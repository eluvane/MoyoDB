use super::*;
use crate::layout::{
    decode_superblock_slot, encode_superblock_slot, SuperblockState, ValueKind,
    SUPERBLOCK_CHECKSUM_OFFSET,
};
use crate::page::{decode_page, encode_leaf_page, LeafCell};
use crate::storage::memory::MemoryBackend;
use std::cell::RefCell;

struct TrackedBackend {
    inner: MemoryBackend,
    reads: RefCell<Vec<(u64, usize)>>,
    writes: usize,
    flushes: usize,
    fail_write: Option<usize>,
    fail_flush: bool,
}

impl TrackedBackend {
    fn new() -> Self {
        Self {
            inner: MemoryBackend::new(),
            reads: RefCell::new(Vec::new()),
            writes: 0,
            flushes: 0,
            fail_write: None,
            fail_flush: false,
        }
    }
}

impl FileBackend for TrackedBackend {
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.reads.borrow_mut().push((offset, len));
        self.inner.read_at(offset, len)
    }
    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        self.writes += 1;
        if self.fail_write == Some(self.writes) {
            return Err(EngineError::Storage("payload write failed".into()));
        }
        self.inner.write_at(offset, bytes)
    }
    fn flush(&mut self) -> Result<()> {
        self.flushes += 1;
        if self.fail_flush {
            return Err(EngineError::Storage("payload flush failed".into()));
        }
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
    fn durable_snapshot(&self) -> Option<Vec<u8>> {
        self.inner.durable_snapshot()
    }
}

#[test]
fn one_durable_body_supports_distinct_reference_prefixes() {
    let mut backend = TrackedBackend::new();
    let body = vec![19u8; 20_000];
    let reference = write_payload(&mut backend, 8, &body).unwrap();
    assert_eq!(backend.flushes, 1);
    assert_eq!(
        backend.len().unwrap(),
        (reference.end_page_id().unwrap() - 1) * PAGE_SIZE as u64
    );
    let durable = backend.durable_snapshot().unwrap();
    let primary_prefix = b"ttl";
    let feed_prefix = b"change header";
    let cells = [
        LeafCell {
            key: b"a".to_vec(),
            value: encode_payload_descriptor(&reference, primary_prefix).unwrap(),
            value_kind: ValueKind::External,
            total_value_len: reference.body_len + primary_prefix.len() as u32,
            overflow_head_page_id: reference.first_page_id,
        },
        LeafCell {
            key: b"b".to_vec(),
            value: encode_payload_descriptor(&reference, feed_prefix).unwrap(),
            value_kind: ValueKind::External,
            total_value_len: reference.body_len + feed_prefix.len() as u32,
            overflow_head_page_id: reference.first_page_id,
        },
    ];
    let decoded = decode_page(&encode_leaf_page(1, 0, 0, &cells).unwrap()).unwrap();
    assert_eq!(decoded.leaf_cells, cells);
    for (cell, prefix) in decoded
        .leaf_cells
        .iter()
        .zip([primary_prefix.as_slice(), feed_prefix.as_slice()])
    {
        let (actual, metadata) = decode_payload_descriptor(
            cell.overflow_head_page_id,
            cell.total_value_len,
            &cell.value,
        )
        .unwrap();
        assert_eq!(actual, reference);
        assert_eq!(metadata, prefix);
        assert!(cell.value.len() < 32);
    }
    assert_eq!(backend.durable_snapshot().unwrap(), durable);
    let recovered = MemoryBackend::from_durable(durable);
    assert_eq!(read_payload(&recovered, &reference).unwrap(), body);
}

#[test]
fn reads_stream_bounded_chunks_and_prefix_reads_stop_early() {
    let mut backend = TrackedBackend::new();
    let body: Vec<u8> = (0..PAYLOAD_READ_CHUNK_BYTES * 2 + 17)
        .map(|index| (index % 251) as u8)
        .collect();
    let reference = write_payload(&mut backend, 1, &body).unwrap();
    let mut result = Vec::new();
    read_payload_into(&backend, &reference, |chunk| {
        assert!(chunk.len() <= PAYLOAD_READ_CHUNK_BYTES);
        result.extend_from_slice(chunk);
        Ok(())
    })
    .unwrap();
    assert_eq!(result, body);
    assert_eq!(
        backend
            .reads
            .borrow()
            .iter()
            .map(|(_, count)| *count)
            .collect::<Vec<_>>(),
        [
            PAYLOAD_HEADER_SIZE,
            PAYLOAD_READ_CHUNK_BYTES,
            PAYLOAD_READ_CHUNK_BYTES,
            17
        ]
    );
    backend.reads.borrow_mut().clear();
    assert_eq!(
        read_payload_prefix(&backend, &reference, 19).unwrap(),
        body[..19]
    );
    assert_eq!(
        backend
            .reads
            .borrow()
            .iter()
            .map(|(_, count)| *count)
            .collect::<Vec<_>>(),
        [PAYLOAD_HEADER_SIZE, 19]
    );
}

#[test]
fn metadata_prefix_does_not_hash_or_validate_a_complete_short_body() {
    let mut backend = TrackedBackend::new();
    let reference = write_payload(&mut backend, 1, &[31; 18]).unwrap();
    backend
        .inner
        .write_at(PAYLOAD_HEADER_SIZE as u64, &[47])
        .unwrap();
    crate::checksum::work::reset();
    let bytes = read_payload_metadata_prefix(&backend, &reference, 30).unwrap();
    assert_eq!(bytes.len(), 18);
    assert_eq!(bytes[0], 47);
    assert_eq!(crate::checksum::work::hashed_bytes(), PAYLOAD_HEADER_SIZE);
    assert_eq!(
        backend
            .reads
            .borrow()
            .iter()
            .map(|(_, len)| *len)
            .collect::<Vec<_>>(),
        [PAYLOAD_HEADER_SIZE, 18]
    );
    assert!(matches!(
        read_payload_prefix(&backend, &reference, 18),
        Err(EngineError::Corruption(_))
    ));
}

#[test]
fn malformed_references_and_descriptor_lengths_are_rejected() {
    let reference = PayloadRef {
        first_page_id: 1,
        body_len: 16,
        checksum: 7,
    };
    let encoded = encode_payload_descriptor(&reference, b"ttl").unwrap();
    for first in [0, u64::MAX] {
        assert!(matches!(
            decode_payload_descriptor(first, 19, &encoded),
            Err(EngineError::Corruption(_))
        ));
    }
    assert!(matches!(
        decode_payload_descriptor(1, 18, &encoded),
        Err(EngineError::Corruption(_))
    ));
    assert!(matches!(
        decode_payload_descriptor(1, 19, &encoded[..7]),
        Err(EngineError::Corruption(_))
    ));
    assert!(payload_page_count(0).is_err());
    assert!(payload_page_count(MAX_STORED_VALUE_BYTES as u32 + 1).is_err());
    let mut cell = LeafCell {
        key: b"a".to_vec(),
        value: encoded,
        value_kind: ValueKind::External,
        total_value_len: 19,
        overflow_head_page_id: 1,
    };
    cell.total_value_len = 18;
    assert!(matches!(
        encode_leaf_page(1, 0, 0, &[cell]),
        Err(EngineError::Corruption(_))
    ));
}

#[test]
fn header_reference_body_and_truncated_extent_corruption_are_detected() {
    let mut backend = MemoryBackend::new();
    let reference = write_payload(&mut backend, 1, b"immutable body").unwrap();
    let original = backend.durable_snapshot().unwrap();
    for changed in [
        PayloadRef {
            body_len: reference.body_len - 1,
            ..reference
        },
        PayloadRef {
            checksum: reference.checksum ^ 1,
            ..reference
        },
    ] {
        assert!(matches!(
            read_payload(&backend, &changed),
            Err(EngineError::Corruption(_))
        ));
    }
    backend.write_at(16, &2u64.to_le_bytes()).unwrap();
    assert!(matches!(
        read_payload(&backend, &reference),
        Err(EngineError::Corruption(_))
    ));
    backend = MemoryBackend::from_durable(original.clone());
    backend
        .write_at(PAYLOAD_HEADER_SIZE as u64 + 5, &[99])
        .unwrap();
    assert!(matches!(
        read_payload(&backend, &reference),
        Err(EngineError::Corruption(_))
    ));
    backend = MemoryBackend::from_durable(original);
    backend.truncate(PAGE_SIZE as u64 - 1).unwrap();
    assert!(matches!(
        read_payload(&backend, &reference),
        Err(EngineError::Corruption(_))
    ));
}

#[test]
fn failed_write_or_flush_returns_no_reference_and_preserves_older_body() {
    for fail_flush in [false, true] {
        let mut backend = TrackedBackend::new();
        let reference = write_payload(&mut backend, 1, b"committed body").unwrap();
        if fail_flush {
            backend.fail_flush = true;
        } else {
            backend.fail_write = Some(backend.writes + 2);
        }
        assert!(matches!(
            write_payload(&mut backend, 2, b"orphan body"),
            Err(EngineError::Storage(_))
        ));
        let recovered = MemoryBackend::from_durable(backend.durable_snapshot().unwrap());
        assert_eq!(
            read_payload(&recovered, &reference).unwrap(),
            b"committed body"
        );
        assert_eq!(recovered.len().unwrap(), PAGE_SIZE as u64);
    }
}

#[test]
fn callback_failure_stops_reading_at_first_chunk() {
    let mut backend = TrackedBackend::new();
    let reference = write_payload(&mut backend, 1, &vec![1; PAYLOAD_READ_CHUNK_BYTES * 3]).unwrap();
    let error = read_payload_into(&backend, &reference, |_| {
        Err(EngineError::Storage("sink failed".into()))
    })
    .unwrap_err();
    assert_eq!(error, EngineError::Storage("sink failed".into()));
    assert_eq!(backend.reads.borrow().len(), 2);
}

#[test]
fn valid_legacy_superblock_is_rejected_without_migration() {
    let state = SuperblockState {
        generation: 1,
        db_id: 1,
        page_size: PAGE_SIZE as u32,
        catalog_root_page_id: 1,
        next_page_id: 2,
        last_committed_txid: 1,
        last_replayed_wal_offset: 0,
        active_slot: 0,
    };
    let mut slot = encode_superblock_slot(&state);
    slot[8..12].copy_from_slice(&1u32.to_le_bytes());
    let checksum = checksum_with_zeroed_region(&slot, SUPERBLOCK_CHECKSUM_OFFSET, 4);
    slot[SUPERBLOCK_CHECKSUM_OFFSET..SUPERBLOCK_CHECKSUM_OFFSET + 4]
        .copy_from_slice(&checksum.to_le_bytes());
    assert_eq!(
        decode_superblock_slot(0, &slot).unwrap_err(),
        EngineError::Corruption("unsupported format version 1".into())
    );
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn native_extent_is_durable_after_files_reopen() {
    use crate::storage::file::NativeFileBackend;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
    struct TestDirectory {
        path: PathBuf,
        parent: PathBuf,
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            assert_eq!(self.path.parent(), Some(self.parent.as_path()));
            fs::remove_dir_all(&self.path).unwrap();
        }
    }
    let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.tmp");
    fs::create_dir_all(&parent).unwrap();
    let parent = fs::canonicalize(parent).unwrap();
    let path = parent.join(format!(
        "payload-native-{}-{}",
        std::process::id(),
        NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).unwrap();
    let directory = TestDirectory { path, parent };
    let body = vec![113; PAYLOAD_READ_CHUNK_BYTES + 23];
    let mut files = NativeFileBackend::open_db(&directory.path, "payload", true).unwrap();
    let reference = write_payload(&mut files.main, 4, &body).unwrap();
    drop(files);
    let files = NativeFileBackend::open_db(&directory.path, "payload", false).unwrap();
    assert_eq!(read_payload(&files.main, &reference).unwrap(), body);
    drop(files);
}
