use moyodb_engine::storage::backend::FileBackend;
use moyodb_engine::storage::memory::MemoryBackend;
use moyodb_engine::{EngineError, Result};

#[test]
fn default_append_offset_observes_changes_through_a_shared_backend() {
    let file = MemoryBackend::new();
    let mut writer = file.clone();
    assert_eq!(file.append_offset().unwrap(), 0);

    writer.write_at(0, b"abcdef").unwrap();
    assert_eq!(file.append_offset().unwrap(), 6);
    writer.truncate(2).unwrap();
    assert_eq!(file.append_offset().unwrap(), 2);
    writer.write_at(8, b"x").unwrap();
    assert_eq!(file.append_offset().unwrap(), 9);
}

#[test]
fn truncate_without_flush_does_not_change_durable_snapshot() {
    let mut file = MemoryBackend::new();
    file.write_at(0, b"abcdef").unwrap();
    file.flush().unwrap();

    file.truncate(0).unwrap();

    let recovered = MemoryBackend::from_durable(file.durable_snapshot().unwrap_or_default());
    assert_eq!(recovered.read_at(0, 6).unwrap(), b"abcdef");
}

#[test]
fn read_at_into_fills_caller_buffer_and_zeroes_the_tail() {
    let mut backend = MemoryBackend::new();
    backend.write_at(4, &[1, 2]).unwrap();

    let mut buf = [0xFF; 8];
    backend.read_at_into(0, &mut buf).unwrap();
    assert_eq!(buf, [0, 0, 0, 0, 1, 2, 0, 0]);

    let mut past = [0xFF; 3];
    backend.read_at_into(64, &mut past).unwrap();
    assert_eq!(past, [0, 0, 0]);

    let mut exact = [0xFF; 2];
    backend.read_at_into(4, &mut exact).unwrap();
    assert_eq!(exact, [1, 2]);
    assert_eq!(backend.read_at(0, 8).unwrap(), buf);
}

#[test]
fn read_at_into_vec_reuses_spare_capacity() {
    let backend = MemoryBackend::from_durable(b"abcdef".to_vec());
    let mut buf = vec![0xFF; 32];
    assert!(buf.capacity() >= 32);

    backend.read_at_into_vec(1, 4, &mut buf).unwrap();
    assert_eq!(buf, b"bcde");
    assert!(buf.capacity() >= 32);

    let mut stale = b"stale-bytes".to_vec();
    let err = backend
        .read_at_into_vec(usize::MAX as u64, 1, &mut stale)
        .unwrap_err();
    assert!(matches!(err, EngineError::Storage(_)));
    assert_eq!(stale, b"stale-bytes");
}

#[test]
fn write_extend_over_tail_and_sparse_gap_keep_zero_holes() {
    let mut backend = MemoryBackend::new();
    backend.write_at(0, b"aaaa").unwrap();
    backend.write_at(2, b"bbbb").unwrap();
    assert_eq!(backend.read_at(0, 6).unwrap(), b"aabbbb");

    let mut file = MemoryBackend::from_durable(vec![0x55; 32]);
    file.truncate(4).unwrap();
    file.write_at(8, b"").unwrap();
    assert_eq!(
        file.read_at(0, 8).unwrap(),
        vec![0x55, 0x55, 0x55, 0x55, 0, 0, 0, 0]
    );
    file.write_at(8, b"xy").unwrap();
    assert_eq!(
        file.read_at(0, 10).unwrap(),
        vec![0x55, 0x55, 0x55, 0x55, 0, 0, 0, 0, b'x', b'y']
    );
}

struct ShortRead {
    inner: MemoryBackend,
}

impl FileBackend for ShortRead {
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let mut bytes = self.inner.read_at(offset, len)?;
        if !bytes.is_empty() {
            bytes.pop();
        }
        Ok(bytes)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        self.inner.write_at(offset, bytes)
    }

    fn flush(&mut self) -> Result<()> {
        self.inner.flush()
    }

    fn len(&self) -> Result<u64> {
        self.inner.len()
    }

    fn truncate(&mut self, size: u64) -> Result<()> {
        self.inner.truncate(size)
    }

    fn close(&mut self) -> Result<()> {
        self.inner.close()
    }
}

#[test]
fn default_read_into_rejects_short_reads_without_clobbering_the_buffer() {
    let backend = ShortRead {
        inner: MemoryBackend::from_durable(b"abcdef".to_vec()),
    };
    let mut bytes = [0xEE; 4];
    assert!(matches!(
        backend.read_at_into(0, &mut bytes).unwrap_err(),
        EngineError::Storage(_)
    ));
    assert_eq!(bytes, [0xEE; 4]);

    let mut owned = b"keep".to_vec();
    assert!(matches!(
        backend.read_at_into_vec(0, 4, &mut owned).unwrap_err(),
        EngineError::Storage(_)
    ));
    assert_eq!(owned, b"keep");
}
