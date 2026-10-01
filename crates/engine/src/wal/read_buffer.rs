use crate::error::{EngineError, Result};
use crate::storage::backend::{ensure_exact_len, FileBackend};
use std::borrow::Cow;

const READ_CHUNK_SIZE: usize = 64 * 1024;

/// Two adjacent chunks let a header and its complete record share the same
/// bytes, even when the header itself straddles a chunk boundary. Ordinary
/// records borrow the chunk; only straddling records need a small assembly copy.
pub(super) struct WalReadBuffer<'a, B: FileBackend> {
    wal: &'a B,
    file_len: u64,
    start: u64,
    current: Vec<u8>,
    next: Option<Vec<u8>>,
}

impl<'a, B: FileBackend> WalReadBuffer<'a, B> {
    pub(super) fn new(wal: &'a B, file_len: u64) -> Self {
        Self {
            wal,
            file_len,
            start: 0,
            current: Vec::new(),
            next: None,
        }
    }

    pub(super) fn read(&mut self, offset: u64, len: usize) -> Result<Cow<'_, [u8]>> {
        let end = offset
            .checked_add(len as u64)
            .ok_or_else(|| EngineError::Storage("wal read range overflow".into()))?;
        if end > self.file_len || len > READ_CHUNK_SIZE {
            return Err(EngineError::Storage("wal read range out of bounds".into()));
        }
        if len == 0 {
            return Ok(Cow::Borrowed(&[]));
        }
        if offset < self.start {
            return Err(EngineError::Internal("wal reader moved backwards".into()));
        }
        let current_end = self.start + self.current.len() as u64;
        if self.current.is_empty() || offset >= current_end {
            // A previous straddling read may already have fetched this chunk.
            // Promote it rather than reading those bytes from the file again.
            match self.next.take() {
                Some(next) if offset < current_end + next.len() as u64 => {
                    self.current = next;
                    self.start = current_end;
                }
                _ => {
                    self.current = self.read_chunk(offset)?;
                    self.start = offset;
                }
            }
        }
        let local_start = (offset - self.start) as usize;
        let local_end = local_start + len;
        if local_end <= self.current.len() {
            return Ok(Cow::Borrowed(&self.current[local_start..local_end]));
        }
        if self.next.is_none() {
            self.next = Some(self.read_chunk(self.start + self.current.len() as u64)?);
        }
        let prefix = &self.current[local_start..];
        let suffix = self
            .next
            .as_deref()
            .and_then(|next| next.get(..len - prefix.len()))
            .ok_or_else(|| EngineError::Storage("short buffered wal record".into()))?;
        let mut record = Vec::with_capacity(len);
        record.extend_from_slice(prefix);
        record.extend_from_slice(suffix);
        Ok(Cow::Owned(record))
    }

    fn read_chunk(&self, offset: u64) -> Result<Vec<u8>> {
        let len = (self.file_len - offset).min(READ_CHUNK_SIZE as u64) as usize;
        ensure_exact_len(self.wal.read_at(offset, len)?, len, "wal read chunk")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{wal_record_total_len, PAGE_SIZE, WAL_COMMIT_BODY_SIZE};
    use crate::page::encode_leaf_page;
    use crate::storage::memory::MemoryBackend;
    use crate::wal::{append_commit_record, append_transaction, scan_wal_index, CommitRecord};
    use std::cell::RefCell;

    struct CountingBackend {
        inner: MemoryBackend,
        reads: RefCell<Vec<(u64, usize)>>,
        short_reads: bool,
    }

    impl CountingBackend {
        fn new(inner: MemoryBackend) -> Self {
            Self {
                inner,
                reads: RefCell::new(Vec::new()),
                short_reads: false,
            }
        }

        fn assert_sequential_reads(&self, file_len: usize) {
            let reads = self.reads.borrow();
            assert_eq!(reads.len(), file_len.div_ceil(READ_CHUNK_SIZE));
            let mut end = 0;
            for &(offset, len) in reads.iter() {
                assert_eq!(offset, end as u64);
                assert!(len <= READ_CHUNK_SIZE);
                end += len;
            }
            assert_eq!(end, file_len);
        }
    }

    impl FileBackend for CountingBackend {
        fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
            self.reads.borrow_mut().push((offset, len));
            let mut bytes = self.inner.read_at(offset, len)?;
            if self.short_reads {
                bytes.truncate(bytes.len().saturating_sub(1));
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

    fn commit(txid: u64, changed_page_count: u32) -> CommitRecord {
        CommitRecord {
            txid,
            new_catalog_root_page_id: 1,
            new_next_page_id: 2,
            changed_page_count,
        }
    }

    #[test]
    fn split_header_and_record_reuse_both_chunks() -> Result<()> {
        let bytes: Vec<u8> = (0..READ_CHUNK_SIZE * 3).map(|i| (i * 31) as u8).collect();
        let wal = CountingBackend::new(MemoryBackend::from_durable(bytes.clone()));
        let mut reader = WalReadBuffer::new(&wal, bytes.len() as u64);
        assert_eq!(reader.read(0, 16)?.as_ref(), &bytes[..16]);
        let offset = READ_CHUNK_SIZE - 8;
        assert_eq!(
            reader.read(offset as u64, 16)?.as_ref(),
            &bytes[offset..offset + 16]
        );
        assert_eq!(
            reader.read(offset as u64, PAGE_SIZE + 40)?.as_ref(),
            &bytes[offset..offset + PAGE_SIZE + 40]
        );
        assert!(matches!(
            reader.read((offset + PAGE_SIZE + 40) as u64, 48)?,
            Cow::Borrowed(_)
        ));
        assert_eq!(
            *wal.reads.borrow(),
            vec![(0, READ_CHUNK_SIZE), (READ_CHUNK_SIZE as u64, READ_CHUNK_SIZE)]
        );
        assert!(reader.current.len() <= READ_CHUNK_SIZE);
        assert!(reader.next.as_ref().is_none_or(|next| next.len() <= READ_CHUNK_SIZE));
        Ok(())
    }

    #[test]
    fn sequential_headers_and_records_do_not_reread_chunks() -> Result<()> {
        let bytes: Vec<u8> = (0..READ_CHUNK_SIZE * 3 + 77).map(|i| (i * 17) as u8).collect();
        let wal = CountingBackend::new(MemoryBackend::from_durable(bytes.clone()));
        let mut reader = WalReadBuffer::new(&wal, bytes.len() as u64);
        let mut offset = 0usize;
        while offset < bytes.len() {
            let len = (PAGE_SIZE + 40).min(bytes.len() - offset);
            let header_len = 16.min(len);
            assert_eq!(
                reader.read(offset as u64, header_len)?.as_ref(),
                &bytes[offset..offset + header_len]
            );
            assert_eq!(
                reader.read(offset as u64, len)?.as_ref(),
                &bytes[offset..offset + len]
            );
            offset += len;
        }
        wal.assert_sequential_reads(bytes.len());
        Ok(())
    }

    #[test]
    fn bounds_and_short_backend_reads_fail_closed() -> Result<()> {
        let mut wal = CountingBackend::new(MemoryBackend::from_durable(vec![0; 100]));
        {
            let mut reader = WalReadBuffer::new(&wal, 100);
            assert!(reader.read(100, 0)?.is_empty());
            assert!(reader.read(99, 2).is_err());
            assert!(reader.read(u64::MAX, 1).is_err());
            assert!(wal.reads.borrow().is_empty());
        }
        wal.short_reads = true;
        let mut reader = WalReadBuffer::new(&wal, 100);
        assert_eq!(reader.read(0, 16).unwrap_err().code(), "StorageError");
        Ok(())
    }

    #[test]
    fn thousand_tiny_commits_read_each_wal_byte_once() -> Result<()> {
        let mut wal = MemoryBackend::new();
        let mut offset = 0;
        let pages = vec![(1, encode_leaf_page(1, 0, 0, &[])?)];
        for txid in 1..=1000 {
            append_transaction(&mut wal, &mut offset, txid, &pages, &commit(txid, 1))?;
        }
        let wal = CountingBackend::new(wal);
        let txs = scan_wal_index(&wal)?;
        assert_eq!(txs.len(), 1000);
        assert_eq!(txs.last().map(|tx| tx.end_offset), Some(offset));
        wal.assert_sequential_reads(offset as usize);
        eprintln!(
            "1000 tiny commits: wal_bytes={offset}, backend_reads={}",
            wal.reads.borrow().len()
        );
        Ok(())
    }

    #[test]
    fn torn_transaction_after_split_header_keeps_committed_prefix() -> Result<()> {
        // 1278 empty commit records and one page/commit pair end eight bytes
        // before a chunk boundary. The next record's header straddles it.
        let mut wal = MemoryBackend::new();
        let mut offset = 0;
        for txid in 1..=1278 {
            append_commit_record(&mut wal, &mut offset, commit(txid, 0))?;
        }
        let pages = vec![(1, encode_leaf_page(1, 0, 0, &[])?)];
        append_transaction(&mut wal, &mut offset, 1279, &pages, &commit(1279, 1))?;
        assert_eq!(offset as usize, READ_CHUNK_SIZE - 8);
        let prefix_len = offset;
        append_transaction(&mut wal, &mut offset, 1280, &pages, &commit(1280, 1))?;
        let bytes = wal.read_at(0, offset as usize)?;
        let tail_len = (offset - prefix_len) as usize;
        let commit_len = wal_record_total_len(WAL_COMMIT_BODY_SIZE);
        for kept in [
            0,
            1,
            7,
            8,
            15,
            16,
            17,
            tail_len - commit_len,
            tail_len - 1,
            tail_len,
        ] {
            let file = MemoryBackend::from_durable(bytes[..prefix_len as usize + kept].to_vec());
            let txs = scan_wal_index(&file)?;
            assert_eq!(
                txs.len(),
                if kept == tail_len { 1280 } else { 1279 },
                "kept={kept}"
            );
        }
        Ok(())
    }

    #[test]
    fn committed_corruption_after_refill_is_not_a_torn_tail() -> Result<()> {
        let mut wal = MemoryBackend::new();
        let mut offset = 0;
        let pages = vec![(1, encode_leaf_page(1, 0, 0, &[])?)];
        for txid in 1..=16 {
            append_transaction(&mut wal, &mut offset, txid, &pages, &commit(txid, 1))?;
        }
        assert!(offset > READ_CHUNK_SIZE as u64);
        append_transaction(
            &mut wal,
            &mut offset,
            17,
            &[(1, vec![0; PAGE_SIZE])],
            &commit(17, 1),
        )?;
        assert_eq!(scan_wal_index(&wal).unwrap_err().code(), "CorruptionError");
        Ok(())
    }
}
