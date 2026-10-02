use moyodb_engine::catalog::ChangeFeedPolicy;
use moyodb_engine::engine::{Engine, OpenConfig, TxMode};
use moyodb_engine::error::Result;
use moyodb_engine::storage::backend::{FileBackend, FileSet};
use moyodb_engine::storage::memory::MemoryBackend;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

// Allocation counters belong to this test thread, so parallel test harness
// activity does not enter a measurement. Setup and reporting stay untracked.
thread_local! {
    static ALLOCATION_WORK: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
}

struct WorkAllocator;

fn count_allocation(size: usize) {
    ALLOCATION_WORK.with(|work| {
        if let Some((count, bytes)) = work.get() {
            work.set(Some((count + 1, bytes + size)));
        }
    });
}

// SAFETY: every allocation operation is forwarded unchanged to System; the
// thread-local counter neither owns nor modifies the allocated memory.
unsafe impl GlobalAlloc for WorkAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_allocation(layout.size());
        // SAFETY: GlobalAlloc supplies a valid layout, forwarded to System.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_allocation(layout.size());
        // SAFETY: GlobalAlloc supplies a valid layout, forwarded to System.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: pointer and layout came from this allocator's System calls.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count_allocation(size);
        // SAFETY: GlobalAlloc supplies an allocated pointer and its layout;
        // reallocation and the resulting ownership remain with System.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: WorkAllocator = WorkAllocator;

#[derive(Default)]
struct IoWork {
    reads: AtomicUsize,
    read_bytes: AtomicUsize,
    writes: AtomicUsize,
    write_bytes: AtomicUsize,
    flushes: AtomicUsize,
}

impl IoWork {
    fn reset(&self) {
        self.reads.store(0, Ordering::Relaxed);
        self.read_bytes.store(0, Ordering::Relaxed);
        self.writes.store(0, Ordering::Relaxed);
        self.write_bytes.store(0, Ordering::Relaxed);
        self.flushes.store(0, Ordering::Relaxed);
    }
}

struct CountingBackend {
    inner: MemoryBackend,
    work: Arc<IoWork>,
}

impl FileBackend for CountingBackend {
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.work.reads.fetch_add(1, Ordering::Relaxed);
        self.work.read_bytes.fetch_add(len, Ordering::Relaxed);
        self.inner.read_at(offset, len)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        self.work.writes.fetch_add(1, Ordering::Relaxed);
        self.work
            .write_bytes
            .fetch_add(bytes.len(), Ordering::Relaxed);
        self.inner.write_at(offset, bytes)
    }

    fn flush(&mut self) -> Result<()> {
        self.work.flushes.fetch_add(1, Ordering::Relaxed);
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

struct Profile {
    engine: Engine<CountingBackend>,
    io: [Arc<IoWork>; 3],
}

impl Profile {
    fn new(feed: bool) -> Result<Self> {
        let io = std::array::from_fn(|_| Arc::new(IoWork::default()));
        let files = std::array::from_fn(|index| CountingBackend {
            inner: MemoryBackend::new(),
            work: Arc::clone(&io[index]),
        });
        let [manifest, main, wal] = files;
        let mut engine = Engine::open(
            "work-profile",
            FileSet::new(manifest, main, wal),
            OpenConfig::default(),
        )?;
        let tx = engine.begin_tx(TxMode::Readwrite)?;
        engine.set_change_feed_policy(
            tx,
            ChangeFeedPolicy {
                enabled: feed,
                ..ChangeFeedPolicy::default()
            },
        )?;
        engine.create_store(tx, "kv")?;
        engine.commit_tx(tx)?;
        engine.checkpoint()?;
        Ok(Self { engine, io })
    }

    fn measure(
        &mut self,
        name: &str,
        operation: impl FnOnce(&mut Engine<CountingBackend>) -> Result<()>,
    ) -> Result<()> {
        for file in &self.io {
            file.reset();
        }
        ALLOCATION_WORK.with(|work| work.set(Some((0, 0))));
        let result = operation(&mut self.engine);
        let (allocations, allocated_bytes) = ALLOCATION_WORK
            .with(|work| work.replace(None))
            .unwrap_or_default();
        result?;
        println!("work {name}: allocations={allocations} allocated_bytes={allocated_bytes} main_reads={} main_read_bytes={} main_writes={} main_write_bytes={} main_flushes={} wal_writes={} wal_write_bytes={} wal_flushes={}",
            self.io[1].reads.load(Ordering::Relaxed), self.io[1].read_bytes.load(Ordering::Relaxed),
            self.io[1].writes.load(Ordering::Relaxed), self.io[1].write_bytes.load(Ordering::Relaxed),
            self.io[1].flushes.load(Ordering::Relaxed), self.io[2].writes.load(Ordering::Relaxed),
            self.io[2].write_bytes.load(Ordering::Relaxed), self.io[2].flushes.load(Ordering::Relaxed));
        Ok(())
    }
}

/// Matching deterministic workloads for before/after physical-work comparison.
/// This reports allocator requests (including reallocations), not live memory.
#[test]
fn profile_remaining_work() -> Result<()> {
    for feed in [false, true] {
        for (count, value_len, batch_size) in
            [(10_000u32, 256, 1000), (1000, 65_536, 100), (1000, 128, 1)]
        {
            let mut profile = Profile::new(feed)?;
            let value = vec![0x47; value_len];
            let keys: Vec<_> = (0..count).map(u32::to_be_bytes).collect();
            profile.measure(
                &format!("insert_{count}_values_{value_len}_batch_{batch_size}_feed_{feed}"),
                |engine| {
                    for batch in keys.chunks(batch_size) {
                        let tx = engine.begin_tx(TxMode::Readwrite)?;
                        for key in batch {
                            engine.put(tx, "kv", key, &value)?;
                        }
                        engine.commit_tx(tx)?;
                    }
                    // Count the complete physical work, including the final tail
                    // checkpoint that normal close would otherwise perform.
                    engine.checkpoint()
                },
            )?;
            let tx = profile.engine.begin_tx(TxMode::Readonly)?;
            profile.measure(
                &format!("get_many_{count}_values_{value_len}_feed_{feed}"),
                |engine| {
                    let values = engine.get_many(tx, "kv", &keys)?;
                    assert!(values
                        .iter()
                        .all(|actual| actual.as_deref() == Some(value.as_slice())));
                    Ok(())
                },
            )?;
            profile.engine.rollback_tx(tx)?;
        }
    }
    for value_len in [16, 256, 2048] {
        let mut profile = Profile::new(false)?;
        let tx = profile.engine.begin_tx(TxMode::Readwrite)?;
        profile
            .engine
            .put(tx, "kv", b"key", &vec![0x47; value_len])?;
        profile.engine.commit_tx(tx)?;
        let tx = profile.engine.begin_tx(TxMode::Readonly)?;
        profile.measure(&format!("has_10000_values_{value_len}"), |engine| {
            for _ in 0..10_000 {
                assert!(engine.has(tx, "kv", b"key")?);
            }
            Ok(())
        })?;
        profile.engine.rollback_tx(tx)?;
    }
    Ok(())
}

fn allocation_work<T>(operation: impl FnOnce() -> Result<T>) -> Result<(T, usize, usize)> {
    ALLOCATION_WORK.with(|work| work.set(Some((0, 0))));
    let result = operation();
    let (allocations, bytes) = ALLOCATION_WORK
        .with(|work| work.replace(None))
        .unwrap_or_default();
    result.map(|value| (value, allocations, bytes))
}

#[test]
fn staged_overwrites_do_not_reallocate_the_store_name() -> Result<()> {
    let mut work = Vec::new();
    for name_len in [1, 240] {
        let name = "s".repeat(name_len);
        let mut profile = Profile::new(false)?;
        let tx = profile.engine.begin_tx(TxMode::Readwrite)?;
        profile.engine.create_store(tx, &name)?;
        profile.engine.put(tx, &name, b"key", b"first")?;
        let (_, allocations, bytes) = allocation_work(|| {
            for _ in 0..256 {
                profile.engine.put(tx, &name, b"key", b"next")?;
            }
            Ok(())
        })?;
        assert_eq!(
            profile.engine.get(tx, &name, b"key")?,
            Some(b"next".to_vec())
        );
        profile.engine.rollback_tx(tx)?;
        assert!(!profile.engine.store_names().contains(&name));
        println!("staged overwrite store_len={name_len}: allocations={allocations} bytes={bytes}");
        work.push((allocations, bytes));
    }
    assert_eq!(work[0].0, work[1].0);
    assert!(
        work[1].1 <= work[0].1 + 32,
        "repeated writes copied store names: short={:?}, long={:?}",
        work[0],
        work[1]
    );
    Ok(())
}

#[test]
fn snapshot_encoding_does_not_allocate_a_key_copy_per_entry() -> Result<()> {
    use moyodb_engine::snapshot::{
        decode_snapshot, encode_snapshot, SnapshotContents, SnapshotEntry, SnapshotStore,
    };

    let contents = SnapshotContents {
        source_last_committed_txid: 17,
        schema_version: 3,
        stores: vec![SnapshotStore {
            name: "kv".into(),
            flags: 0,
            entries: (0..256u32)
                .rev()
                .map(|index| SnapshotEntry {
                    key: index.to_be_bytes().to_vec(),
                    value: vec![index as u8; 7],
                    expires_at_ms: None,
                })
                .collect(),
        }],
    };
    let (encoded, allocations, bytes) = allocation_work(|| encode_snapshot(&contents))?;
    assert_eq!(decode_snapshot(&encoded)?, contents);
    println!("snapshot 256 keys: allocations={allocations} bytes={bytes}");
    assert!(
        allocations < 128,
        "snapshot encoding allocated once per key: {allocations} allocations"
    );
    Ok(())
}

#[test]
fn retiring_a_tree_does_not_copy_inline_values() -> Result<()> {
    use moyodb_engine::btree::{build_tree, free_tree, PageAllocator};
    use moyodb_engine::pager::Pager;

    let entries: Vec<_> = (0..512u32)
        .map(|index| (index.to_be_bytes().to_vec(), vec![index as u8; 769]))
        .collect();
    let mut next_page_id = 1;
    let built = build_tree(&entries, &mut next_page_id)?;
    let mut expected: Vec<_> = built.page_images.iter().map(|(id, _)| *id).collect();
    expected.sort_unstable();
    let mut pager = Pager::new(MemoryBackend::new(), built.page_images.len() + 1);
    for (page_id, bytes) in built.page_images {
        pager.write_page_image(page_id, &bytes)?;
    }
    let mut alloc = PageAllocator::new(next_page_id);
    let (_, allocations, bytes) =
        allocation_work(|| free_tree(&mut pager, built.root_page_id, &mut alloc))?;
    let mut freed = alloc.freed().to_vec();
    freed.sort_unstable();
    assert_eq!(freed, expected);
    println!("retire 512 inline values: allocations={allocations} bytes={bytes}");
    assert!(
        bytes < 512 * 769 / 2,
        "retirement copied discarded inline payload: {bytes} allocated bytes"
    );
    Ok(())
}
