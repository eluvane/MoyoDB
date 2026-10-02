use moyodb_engine::change_feed::{encode_change_log_key, SYSTEM_CHANGELOG_STORE_NAME};
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
    static VALUE_ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
}

struct ValueAllocator;

fn record_allocation(size: usize) {
    if size == LARGE_VALUE_LEN {
        VALUE_ALLOCATIONS.with(|counter| {
            if let Some(count) = counter.get() {
                counter.set(Some(count + 1));
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
fn filtered_change_feed_does_not_allocate_excluded_value_copies() -> Result<()> {
    let (mut engine, _) = open_engine(deferred_config(), true)?;
    let before = engine.stats()?.last_committed_txid;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    let value = vec![0x47; LARGE_VALUE_LEN];
    for key in 0u32..4 {
        engine.put(tx, "source", &key.to_be_bytes(), &value)?;
    }
    let latest = engine.commit_tx(tx)?;
    // Warm the same tree and overflow pages outside the allocation measurement.
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
    VALUE_ALLOCATIONS.with(|counter| counter.set(Some(0)));
    let result = engine.changes_since(before, options);
    let allocations = VALUE_ALLOCATIONS
        .with(|counter| counter.replace(None))
        .unwrap_or_default();
    let feed = result?;
    assert!(feed.changes.is_empty());
    assert_eq!(feed.latest_tx_id, latest);
    assert_eq!(
        allocations, 0,
        "excluded values must not be cloned into change records"
    );
    let feed = engine.changes_since(before, ChangeFeedOptions::default())?;
    assert!(feed
        .changes
        .iter()
        .all(|record| record.value.as_deref() == Some(value.as_slice())));
    Ok(())
}

#[test]
fn filtered_change_feed_still_reports_corrupt_excluded_payloads() -> Result<()> {
    let (mut engine, _) = open_engine(deferred_config(), true)?;
    let before = engine.stats()?.last_committed_txid;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.put(tx, "source", b"key", b"value")?;
    let logged = engine.commit_tx(tx)?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.put(
        tx,
        SYSTEM_CHANGELOG_STORE_NAME,
        &encode_change_log_key(logged, 0),
        b"broken",
    )?;
    engine.commit_tx(tx)?;
    let error = engine
        .changes_since(
            before,
            ChangeFeedOptions {
                stores: Some(vec!["wanted".to_string()]),
                limit: None,
            },
        )
        .expect_err("excluded payloads still require validation");
    assert!(matches!(error, EngineError::Corruption(_)));
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
