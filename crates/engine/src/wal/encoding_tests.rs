use super::*;
use crate::page::encode_overflow_page;
use crate::storage::backend::FileSet;
use crate::storage::memory::{MemoryBackend, MemoryBundle};
use crate::{Engine, EngineHealth, OpenConfig, TxMode};
use std::cell::Cell;
use std::sync::{Arc, Mutex};

const ENCODING_BUDGET: usize = 256 * 1024;
const PAGE_RECORD_LEN: usize = WAL_RECORD_HEADER_SIZE + WAL_PAGE_IMAGE_BODY_HEADER_SIZE + PAGE_SIZE;
const COMMIT_RECORD_LEN: usize = WAL_RECORD_HEADER_SIZE + WAL_COMMIT_BODY_SIZE;

thread_local! {
    static PEAK_BATCH: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
}

pub(super) fn observe_batch(batch_len: usize, batch_capacity: usize) {
    PEAK_BATCH.with(|peak| {
        let (len, capacity) = peak.get();
        peak.set((len.max(batch_len), capacity.max(batch_capacity)));
    });
}

#[derive(Default)]
struct Trace {
    writes: Vec<(u64, usize)>,
    flushes: usize,
    fail_write: Option<(usize, usize)>,
}

#[derive(Default)]
struct CountingBackend {
    inner: MemoryBackend,
    trace: Arc<Mutex<Trace>>,
}

impl FileBackend for CountingBackend {
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.inner.read_at(offset, len)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        let fail = {
            let mut trace = self.trace.lock().unwrap();
            trace.writes.push((offset, bytes.len()));
            trace
                .fail_write
                .filter(|(write, _)| *write == trace.writes.len())
        };
        if let Some((_, len)) = fail {
            self.inner
                .write_at(offset, &bytes[..bytes.len().min(len)])?;
            return Err(EngineError::Storage("injected partial WAL batch".into()));
        }
        self.inner.write_at(offset, bytes)
    }

    fn flush(&mut self) -> Result<()> {
        self.trace.lock().unwrap().flushes += 1;
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

fn images(count: usize) -> Result<Vec<(u64, Vec<u8>)>> {
    (1..=count)
        .map(|page_id| {
            Ok((
                page_id as u64,
                encode_overflow_page(page_id as u64, 0, &[page_id as u8; 64])?,
            ))
        })
        .collect()
}

fn commit(txid: u64, count: usize) -> CommitRecord {
    CommitRecord {
        txid,
        new_catalog_root_page_id: 1,
        new_next_page_id: count.max(1) as u64 + 1,
        changed_page_count: count as u32,
    }
}

fn reference_bytes(txid: u64, pages: &[(u64, Vec<u8>)], commit: &CommitRecord) -> Result<Vec<u8>> {
    let mut reference = MemoryBackend::new();
    let mut offset = 0;
    for (page_id, bytes) in pages {
        append_page_image_record(&mut reference, &mut offset, txid, *page_id, bytes)?;
    }
    append_commit_record(&mut reference, &mut offset, commit.clone())?;
    reference.read_at(0, offset as usize)
}

#[test]
fn wal_encoding_staging_stays_within_budget_for_large_transactions() -> Result<()> {
    let pages = images(2048)?;
    let commit = commit(7, pages.len());
    let mut wal = CountingBackend::default();
    let mut offset = 0;
    PEAK_BATCH.with(|peak| peak.set((0, 0)));
    append_generated_transaction(&mut wal, &mut offset, 7, &pages, &commit)?;
    let (peak_len, peak_capacity) = PEAK_BATCH.with(Cell::get);
    assert!(
        peak_len <= ENCODING_BUDGET,
        "encoded batch grew to {peak_len} bytes"
    );
    assert!(
        peak_capacity <= ENCODING_BUDGET,
        "staging capacity grew to {peak_capacity} bytes"
    );
    assert_eq!(peak_capacity, ENCODING_BUDGET);
    assert_eq!(
        peak_len,
        ENCODING_BUDGET / PAGE_RECORD_LEN * PAGE_RECORD_LEN
    );
    assert_eq!(
        offset as usize,
        pages.len() * PAGE_RECORD_LEN + COMMIT_RECORD_LEN
    );
    let trace = wal.trace.lock().unwrap();
    assert!(trace.writes.len() > 1);
    assert!(trace.writes.iter().all(|(_, len)| *len <= ENCODING_BUDGET));
    assert_eq!(trace.flushes, 0);
    Ok(())
}

#[test]
fn wal_encoding_batches_preserve_reference_bytes_and_single_small_append() -> Result<()> {
    let pages_per_batch = ENCODING_BUDGET / PAGE_RECORD_LEN;
    for count in [
        0,
        1,
        pages_per_batch - 1,
        pages_per_batch,
        pages_per_batch + 1,
        pages_per_batch * 3 + 1,
    ] {
        let pages = images(count)?;
        let commit = commit(9, count);
        let reference = reference_bytes(9, &pages, &commit)?;
        for generated in [false, true] {
            let mut wal = CountingBackend::default();
            let mut offset = 23;
            wal.inner.write_at(0, &[0xa7; 23])?;
            if generated {
                append_generated_transaction(&mut wal, &mut offset, 9, &pages, &commit)?;
            } else {
                append_transaction(&mut wal, &mut offset, 9, &pages, &commit)?;
            }
            assert_eq!(wal.read_at(23, reference.len())?, reference);
            assert_eq!(offset, 23 + reference.len() as u64);
            let trace = wal.trace.lock().unwrap();
            let expected_writes = count.div_ceil(pages_per_batch).max(1);
            assert_eq!(
                trace.writes.len(),
                expected_writes,
                "pages={count}, generated={generated}"
            );
            assert_eq!(trace.flushes, 0);
            assert_eq!(trace.writes.first().unwrap().0, 23);
            for pair in trace.writes.windows(2) {
                assert_eq!(pair[1].0, pair[0].0 + pair[0].1 as u64);
            }
            let commit_start = 23 + count * PAGE_RECORD_LEN;
            let final_write = trace.writes.last().unwrap();
            assert!(final_write.0 <= commit_start as u64);
            assert_eq!(
                final_write.0 + final_write.1 as u64,
                commit_start as u64 + COMMIT_RECORD_LEN as u64
            );
        }
    }
    Ok(())
}

#[test]
fn invalid_late_page_is_rejected_before_any_bounded_wal_batch() -> Result<()> {
    let mut pages = images(ENCODING_BUDGET / PAGE_RECORD_LEN * 3)?;
    let commit = commit(11, pages.len());
    pages.last_mut().unwrap().1.pop();
    let mut wal = CountingBackend::default();
    let mut offset = 23;
    assert!(matches!(
        append_generated_transaction(&mut wal, &mut offset, 11, &pages, &commit),
        Err(EngineError::Serialization(_))
    ));
    assert!(wal.trace.lock().unwrap().writes.is_empty());
    assert_eq!(offset, 23);
    assert_eq!(wal.len()?, 0);
    Ok(())
}

#[test]
fn failed_middle_wal_batch_leaves_only_the_prior_commit_replayable() -> Result<()> {
    let pages_per_batch = ENCODING_BUDGET / PAGE_RECORD_LEN;
    let pages = images(pages_per_batch * 3 + 1)?;
    let prior = images(1)?;
    for partial_len in [
        0,
        17,
        PAGE_RECORD_LEN - 1,
        PAGE_RECORD_LEN,
        PAGE_RECORD_LEN + 17,
        pages_per_batch * PAGE_RECORD_LEN,
    ] {
        let mut wal = CountingBackend::default();
        let mut offset = 0;
        append_generated_transaction(&mut wal, &mut offset, 1, &prior, &commit(1, 1))?;
        wal.flush()?;
        let prior_end = offset;
        *wal.trace.lock().unwrap() = Trace {
            fail_write: Some((2, partial_len)),
            ..Trace::default()
        };
        assert!(matches!(
            append_generated_transaction(&mut wal, &mut offset, 2, &pages, &commit(2, pages.len())),
            Err(EngineError::Storage(_))
        ));
        assert_eq!(
            offset,
            prior_end + (pages_per_batch * PAGE_RECORD_LEN) as u64
        );
        assert_eq!(wal.trace.lock().unwrap().writes.len(), 2);
        assert_eq!(wal.trace.lock().unwrap().flushes, 0);
        let recovered = scan_wal(&wal)?;
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].txid, 1);
        assert_eq!(recovered[0].end_offset, prior_end);
        wal.inner.flush()?;
        let durable = MemoryBackend::from_durable(wal.inner.durable_snapshot().unwrap());
        assert_eq!(scan_wal(&durable)?, recovered);
    }
    Ok(())
}

#[test]
fn engine_middle_wal_batch_failure_poisoning_recovers_without_the_failed_commit() -> Result<()> {
    let bundle = MemoryBundle::new();
    let files = bundle.files();
    let wrap = |inner| CountingBackend {
        inner,
        ..CountingBackend::default()
    };
    let wal = wrap(files.wal);
    let trace = Arc::clone(&wal.trace);
    let mut engine = Engine::open(
        "bounded-wal-failure",
        FileSet::new(wrap(files.manifest), wrap(files.main), wal),
        OpenConfig {
            checkpoint_wal_bytes: u64::MAX,
            checkpoint_dirty_pages: usize::MAX,
            ..OpenConfig::default()
        },
    )?;
    let setup = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(setup, "kv")?;
    engine.put(setup, "kv", b"prior", b"durable")?;
    let prior_txid = engine.commit_tx(setup)?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    for key in 0u32..500 {
        engine.put(tx, "kv", &key.to_be_bytes(), &[0x5a; 900])?;
    }
    *trace.lock().unwrap() = Trace {
        fail_write: Some((2, 17)),
        ..Trace::default()
    };
    assert!(matches!(engine.commit_tx(tx), Err(EngineError::Storage(_))));
    assert_eq!(trace.lock().unwrap().writes.len(), 2);
    assert_eq!(trace.lock().unwrap().flushes, 0);
    assert!(matches!(
        engine.health(),
        EngineHealth::RecoveryRequired { .. }
    ));
    assert!(matches!(
        engine.begin_tx(TxMode::Readonly),
        Err(EngineError::RecoveryRequired(_))
    ));
    trace.lock().unwrap().fail_write = None;
    let recovery = engine.recover()?;
    assert!(!recovery.pending_committed);
    assert_eq!(recovery.last_committed_txid, prior_txid);
    assert_eq!(engine.health(), &EngineHealth::Healthy);
    let read = engine.begin_tx(TxMode::Readonly)?;
    assert_eq!(engine.get(read, "kv", b"prior")?, Some(b"durable".to_vec()));
    assert_eq!(engine.get(read, "kv", &0u32.to_be_bytes())?, None);
    engine.rollback_tx(read)?;
    let next = engine.begin_tx(TxMode::Readwrite)?;
    for key in 0u32..500 {
        engine.put(next, "kv", &key.to_be_bytes(), &[0x5a; 900])?;
    }
    engine.put(next, "kv", b"after", b"recovered")?;
    *trace.lock().unwrap() = Trace::default();
    assert_eq!(engine.commit_tx(next)?, prior_txid + 1);
    assert!(trace.lock().unwrap().writes.len() > 1);
    assert_eq!(trace.lock().unwrap().flushes, 1);
    let durable = MemoryBackend::from_durable(bundle.wal.durable_snapshot().unwrap());
    assert_eq!(scan_wal(&durable)?.last().unwrap().txid, prior_txid + 1);
    Ok(())
}
