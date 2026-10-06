use moyodb_engine::change_feed::SYSTEM_CHANGELOG_STORE_NAME;
use moyodb_engine::storage::backend::{FileBackend, FileSet};
use moyodb_engine::{
    ChangeFeedOptions, ChangeFeedPolicy, Engine, EngineError, MemoryBackend, OpenConfig, Result,
    TxMode,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const LARGE_VALUE_LEN: usize = 64 * 1024;

thread_local! {
    static VALUE_ALLOCATIONS: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
    static BACKEND_READ_ALLOCATION: Cell<bool> = const { Cell::new(false) };
    static APPEND_OFFSET_CALLS: Cell<usize> = const { Cell::new(0) };
}

struct ValueAllocator;

fn record_allocation(size: usize) {
    if size >= LARGE_VALUE_LEN {
        VALUE_ALLOCATIONS.with(|counter| {
            if let Some((copies, chunks)) = counter.get() {
                counter.set(Some(if BACKEND_READ_ALLOCATION.with(Cell::get) {
                    (copies, chunks + 1)
                } else {
                    (copies + 1, chunks)
                }));
            }
        });
    }
}

// SAFETY: all allocation operations forward their original arguments to System;
// the thread-local counter neither owns nor accesses the allocated memory.
unsafe impl GlobalAlloc for ValueAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation(layout.size());
        // SAFETY: the supplied layout is forwarded unchanged to System.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation(layout.size());
        // SAFETY: the supplied layout is forwarded unchanged to System.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: pointer and layout came from the forwarded System allocation.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record_allocation(size);
        // SAFETY: the original allocation and requested size are forwarded unchanged.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: ValueAllocator = ValueAllocator;

#[derive(Clone)]
struct CountingBackend {
    inner: MemoryBackend,
    length_calls: Arc<AtomicUsize>,
}

impl FileBackend for CountingBackend {
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        struct ResetReadAllocation(bool);
        impl Drop for ResetReadAllocation {
            fn drop(&mut self) {
                BACKEND_READ_ALLOCATION.with(|active| active.set(self.0));
            }
        }
        let previous = BACKEND_READ_ALLOCATION.with(|active| active.replace(true));
        let _reset = ResetReadAllocation(previous);
        self.inner.read_at(offset, len)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        self.inner.write_at(offset, bytes)
    }

    fn flush(&mut self) -> Result<()> {
        self.inner.flush()
    }

    fn len(&self) -> Result<u64> {
        self.length_calls.fetch_add(1, Ordering::Relaxed);
        self.inner.len()
    }

    fn append_offset(&self) -> Result<u64> {
        APPEND_OFFSET_CALLS.with(|calls| calls.set(calls.get() + 1));
        self.len()
    }

    fn truncate(&mut self, size: u64) -> Result<()> {
        self.inner.truncate(size)
    }

    fn close(&mut self) -> Result<()> {
        self.inner.close()
    }
}

fn open_engine(
    config: OpenConfig,
    feed: bool,
) -> Result<(Engine<CountingBackend>, Arc<AtomicUsize>)> {
    let files: [CountingBackend; 3] = std::array::from_fn(|_| CountingBackend {
        inner: MemoryBackend::new(),
        length_calls: Arc::new(AtomicUsize::new(0)),
    });
    let [manifest, main, wal] = files;
    let wal_length_calls = Arc::clone(&wal.length_calls);
    let mut engine = Engine::open("commit-work", FileSet::new(manifest, main, wal), config)?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.set_change_feed_policy(
        tx,
        ChangeFeedPolicy {
            enabled: feed,
            retain_txids: None,
        },
    )?;
    engine.create_store(tx, "source")?;
    engine.create_store(tx, "wanted")?;
    engine.commit_tx(tx)?;
    engine.checkpoint()?;
    Ok((engine, wal_length_calls))
}

fn deferred_config() -> OpenConfig {
    OpenConfig {
        checkpoint_wal_bytes: u64::MAX,
        checkpoint_dirty_pages: usize::MAX,
        ..OpenConfig::default()
    }
}

#[test]
fn eager_commit_uses_the_known_wal_length_for_checkpoint() -> Result<()> {
    let (mut engine, length_calls) = open_engine(
        OpenConfig {
            checkpoint_dirty_pages: 1,
            ..deferred_config()
        },
        false,
    )?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.put(tx, "source", b"key", b"value")?;
    length_calls.store(0, Ordering::Relaxed);
    engine.commit_tx(tx)?;
    assert_eq!(length_calls.load(Ordering::Relaxed), 1);
    let reader = engine.begin_tx(TxMode::Readonly)?;
    assert_eq!(
        engine.get(reader, "source", b"key")?,
        Some(b"value".to_vec())
    );
    engine.rollback_tx(reader)
}

#[test]
fn deferred_commit_and_explicit_checkpoint_each_read_wal_length_once() -> Result<()> {
    let (mut engine, length_calls) = open_engine(deferred_config(), false)?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.put(tx, "source", b"key", b"value")?;
    length_calls.store(0, Ordering::Relaxed);
    engine.commit_tx(tx)?;
    assert_eq!(length_calls.load(Ordering::Relaxed), 1);
    length_calls.store(0, Ordering::Relaxed);
    engine.checkpoint()?;
    assert_eq!(length_calls.load(Ordering::Relaxed), 1);
    length_calls.store(0, Ordering::Relaxed);
    engine.checkpoint()?;
    assert_eq!(length_calls.load(Ordering::Relaxed), 0);
    Ok(())
}

#[test]
fn commits_use_backend_append_offsets_but_stats_and_recovery_read_length() -> Result<()> {
    let (mut engine, length_calls) = open_engine(deferred_config(), false)?;
    APPEND_OFFSET_CALLS.with(|calls| calls.set(0));
    for key in 0u8..3 {
        let tx = engine.begin_tx(TxMode::Readwrite)?;
        engine.put(tx, "source", &[key], b"value")?;
        engine.commit_tx(tx)?;
    }
    assert_eq!(APPEND_OFFSET_CALLS.with(Cell::get), 3);

    APPEND_OFFSET_CALLS.with(|calls| calls.set(0));
    length_calls.store(0, Ordering::Relaxed);
    assert!(engine.stats()?.wal_len > 0);
    assert_eq!(APPEND_OFFSET_CALLS.with(Cell::get), 0);
    assert_eq!(length_calls.load(Ordering::Relaxed), 1);

    length_calls.store(0, Ordering::Relaxed);
    engine.recover()?;
    assert_eq!(APPEND_OFFSET_CALLS.with(Cell::get), 0);
    assert!(length_calls.load(Ordering::Relaxed) > 0);
    Ok(())
}

#[test]
fn filtered_change_feed_does_not_allocate_excluded_value_copies() -> Result<()> {
    for value_len in [LARGE_VALUE_LEN, 1024 * 1024] {
        let (mut engine, _) = open_engine(deferred_config(), true)?;
        let before = engine.stats()?.last_committed_txid;
        let tx = engine.begin_tx(TxMode::Readwrite)?;
        let value = vec![0x47; value_len];
        for key in 0u32..4 {
            engine.put(tx, "source", &key.to_be_bytes(), &value)?;
        }
        let latest = engine.commit_tx(tx)?;
        // Warm the tree before measuring value and transport allocations separately.
        assert_eq!(
            engine
                .changes_since(before, ChangeFeedOptions::default())?
                .changes
                .len(),
            4
        );
        let options = ChangeFeedOptions {
            stores: Some(vec!["wanted".to_string()]),
            limit: None,
        };
        VALUE_ALLOCATIONS.with(|counter| counter.set(Some((0, 0))));
        let result = engine.changes_since(before, options);
        let (copies, chunks) = VALUE_ALLOCATIONS
            .with(|counter| counter.replace(None))
            .unwrap_or_default();
        let feed = result?;
        assert!(feed.changes.is_empty());
        assert_eq!(feed.latest_tx_id, latest);
        assert_eq!(
            copies, 0,
            "excluded values must not be materialized or cloned into change records"
        );
        let expected_chunks =
            4 * value_len.div_ceil(moyodb_engine::payload::PAYLOAD_READ_CHUNK_BYTES);
        assert_eq!(
            chunks, expected_chunks,
            "CRC validation must stream bounded backend reads"
        );
        VALUE_ALLOCATIONS.with(|counter| counter.set(Some((0, 0))));
        let result = engine.changes_since(before, ChangeFeedOptions::default());
        let (copies, chunks) = VALUE_ALLOCATIONS
            .with(|counter| counter.replace(None))
            .unwrap();
        let feed = result?;
        assert_eq!(
            copies, 4,
            "included values need one owned allocation per record"
        );
        assert_eq!(chunks, expected_chunks);
        assert!(feed
            .changes
            .iter()
            .all(|record| record.value.as_deref() == Some(value.as_slice())));
    }
    Ok(())
}

#[test]
fn filtered_change_feed_still_reports_corrupt_excluded_external_bodies() -> Result<()> {
    use moyodb_engine::layout::page_offset;
    use moyodb_engine::page::decode_page;
    use moyodb_engine::pager::Pager;
    use moyodb_engine::payload::{decode_payload_descriptor, PAYLOAD_HEADER_SIZE};
    use moyodb_engine::storage::memory::MemoryBundle;

    let mut bundle = MemoryBundle::new();
    let mut engine = Engine::open(
        "excluded-external-corruption",
        bundle.files(),
        deferred_config(),
    )?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(tx, "source")?;
    engine.create_store(tx, "wanted")?;
    let before = engine.commit_tx(tx)?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.put(tx, "source", b"key", &vec![0x47; LARGE_VALUE_LEN])?;
    engine.commit_tx(tx)?;
    engine.checkpoint()?;
    let mut pager = Pager::new(bundle.main.clone(), 1);
    let page = decode_page(&pager.read_page(engine.catalog()["source"].store_root_page_id)?)?;
    let cell = &page.leaf_cells[0];
    let reference = decode_payload_descriptor(
        cell.overflow_head_page_id,
        cell.total_value_len,
        &cell.value,
    )?
    .0;
    let offset = page_offset(reference.first_page_id)
        + PAYLOAD_HEADER_SIZE as u64
        + LARGE_VALUE_LEN as u64 / 2;
    bundle.main.write_at(offset, &[0x48])?;
    let error = engine
        .changes_since(
            before,
            ChangeFeedOptions {
                stores: Some(vec!["wanted".into()]),
                limit: None,
            },
        )
        .expect_err("excluded external bodies still require their complete CRC validation");
    assert!(matches!(error, EngineError::Corruption(_)));
    Ok(())
}

#[test]
fn user_put_cannot_replace_a_change_log_record() -> Result<()> {
    let (mut engine, _) = open_engine(deferred_config(), true)?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.put(tx, "source", b"key", b"value")?;
    engine.commit_tx(tx)?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    let error = engine
        .put(tx, SYSTEM_CHANGELOG_STORE_NAME, b"k", b"broken")
        .expect_err("the change log is not a user store");
    assert!(matches!(error, EngineError::ReservedStoreName(_)));
    engine.rollback_tx(tx)?;
    Ok(())
}
#[test]
fn public_staged_store_keeps_default_struct_update_construction() {
    let stage = moyodb_engine::txn::StagedStore {
        flags: 0,
        ..Default::default()
    };
    assert!(stage.mutations.is_empty());
    assert!(!stage.has_changes());
}

#[test]
fn repeated_puts_keep_baselines_across_one_read_path() -> Result<()> {
    let (mut engine, _) = open_engine(deferred_config(), false)?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    assert!(!engine.put_reporting_baseline(tx, "source", b"a", b"1", None)?);
    assert!(!engine.put_reporting_baseline(tx, "source", b"c", b"3", None)?);
    assert!(engine.put_reporting_baseline(tx, "source", b"a", b"1b", None)?);
    engine.commit_tx(tx)?;

    let tx = engine.begin_tx(TxMode::Readwrite)?;
    assert!(engine.put_reporting_baseline(tx, "source", b"c", b"3b", None)?);
    assert!(!engine.put_reporting_baseline(tx, "source", b"b", b"2", None)?);
    assert!(engine.put_reporting_baseline(tx, "source", b"a", b"1c", None)?);
    assert_eq!(engine.get(tx, "source", b"a")?, Some(b"1c".to_vec()));
    assert_eq!(engine.get(tx, "source", b"b")?, Some(b"2".to_vec()));
    assert!(!engine.has(tx, "source", b"missing")?);
    // Replacing a live key with an already-expired value still reports the old baseline.
    assert!(engine.put_reporting_baseline(tx, "source", b"c", b"gone", Some(0))?);
    engine.commit_tx(tx)?;

    let tx = engine.begin_tx(TxMode::Readonly)?;
    assert_eq!(engine.get(tx, "source", b"a")?, Some(b"1c".to_vec()));
    assert_eq!(engine.get(tx, "source", b"b")?, Some(b"2".to_vec()));
    assert_eq!(engine.get(tx, "source", b"c")?, None);
    engine.rollback_tx(tx)?;

    let tx = engine.begin_tx(TxMode::Readwrite)?;
    assert!(!engine.put_reporting_baseline(tx, "source", b"c", b"fresh", None)?);
    engine.commit_tx(tx)?;
    let tx = engine.begin_tx(TxMode::Readonly)?;
    assert_eq!(engine.get(tx, "source", b"c")?, Some(b"fresh".to_vec()));
    engine.rollback_tx(tx)
}
