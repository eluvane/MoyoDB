use moyodb_engine::catalog::ChangeFeedPolicy;
use moyodb_engine::engine::{Engine, OpenConfig, TxMode};
use moyodb_engine::error::{EngineError, Result};
use moyodb_engine::layout::{page_offset, PageKind, PAGE_SIZE};
use moyodb_engine::page::{decode_page, encode_overflow_page};
use moyodb_engine::storage::backend::{FileBackend, FileSet};
use moyodb_engine::storage::memory::MemoryBackend;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default)]
struct AllocationWork {
    allocations: usize,
    bytes: usize,
}

thread_local! {
    static ALLOCATIONS: Cell<Option<AllocationWork>> = const { Cell::new(None) };
}

struct WorkAllocator;

fn count_allocation(bytes: usize) {
    let _ = ALLOCATIONS.try_with(|counter| {
        if let Some(mut work) = counter.get() {
            work.allocations += 1;
            work.bytes += bytes;
            counter.set(Some(work));
        }
    });
}

// SAFETY: all operations delegate to System with their original arguments;
// the thread-local counter does not access the allocated memory.
unsafe impl GlobalAlloc for WorkAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_allocation(layout.size());
        // SAFETY: GlobalAlloc provides a valid layout, forwarded to System.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_allocation(layout.size());
        // SAFETY: GlobalAlloc provides a valid layout, forwarded to System.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: pointer and layout are forwarded to the original allocator.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, bytes: usize) -> *mut u8 {
        count_allocation(bytes);
        // SAFETY: GlobalAlloc provides an allocated pointer and its layout.
        unsafe { System.realloc(pointer, layout, bytes) }
    }
}

#[global_allocator]
static ALLOCATOR: WorkAllocator = WorkAllocator;

fn allocation_work<T>(operation: impl FnOnce() -> T) -> (T, AllocationWork) {
    struct StopTracking;
    impl Drop for StopTracking {
        fn drop(&mut self) {
            ALLOCATIONS.with(|counter| counter.set(None));
        }
    }
    ALLOCATIONS.with(|counter| {
        assert!(counter.get().is_none(), "nested allocation measurement");
        counter.set(Some(AllocationWork::default()));
    });
    let stop = StopTracking;
    let result = operation();
    let work = ALLOCATIONS.with(|counter| counter.get().unwrap_or_default());
    drop(stop);
    (result, work)
}

const MAX_FIXTURE_PAGES: usize = 256;

struct ReadWork {
    reads: [AtomicUsize; MAX_FIXTURE_PAGES],
    failures: [AtomicBool; MAX_FIXTURE_PAGES],
}

impl ReadWork {
    fn new() -> Self {
        Self {
            reads: std::array::from_fn(|_| AtomicUsize::new(0)),
            failures: std::array::from_fn(|_| AtomicBool::new(false)),
        }
    }

    fn reset(&self) {
        for count in &self.reads {
            count.store(0, Ordering::Relaxed);
        }
    }

    fn reads(&self, page: u64) -> usize {
        self.reads[page as usize].load(Ordering::Relaxed)
    }

    fn fail(&self, page: u64, enabled: bool) {
        self.failures[page as usize].store(enabled, Ordering::Relaxed);
    }
}

struct CountingBackend {
    inner: MemoryBackend,
    work: Option<Arc<ReadWork>>,
}

impl FileBackend for CountingBackend {
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        if let Some(work) = &self.work {
            assert_eq!(len, PAGE_SIZE, "main-file reads must be complete pages");
            assert_eq!(offset % PAGE_SIZE as u64, 0);
            let page = offset / PAGE_SIZE as u64 + 1;
            assert!((page as usize) < MAX_FIXTURE_PAGES);
            work.reads[page as usize].fetch_add(1, Ordering::Relaxed);
            if work.failures[page as usize].load(Ordering::Relaxed) {
                return Err(EngineError::Storage(format!("injected main page {page}")));
            }
        }
        self.inner.read_at(offset, len)
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

    fn durable_snapshot(&self) -> Option<Vec<u8>> {
        self.inner.durable_snapshot()
    }
}

struct Fixture {
    engine: Engine<CountingBackend>,
    work: Arc<ReadWork>,
    images: [Vec<u8>; 3],
    cache_pages: usize,
}

impl Fixture {
    fn new(entries: &[(Vec<u8>, Vec<u8>)], cache_pages: usize) -> Result<Self> {
        assert!(entries.len() <= 32);
        assert!(entries.iter().all(|(_, value)| value.len() <= 16 * 1024));
        let files: [MemoryBackend; 3] = std::array::from_fn(|_| MemoryBackend::new());
        let mut engine = Engine::open(
            "point-read-work",
            FileSet::new(files[0].clone(), files[1].clone(), files[2].clone()),
            OpenConfig::default(),
        )?;
        let tx = engine.begin_tx(TxMode::Readwrite)?;
        engine.set_change_feed_policy(
            tx,
            ChangeFeedPolicy {
                enabled: false,
                ..ChangeFeedPolicy::default()
            },
        )?;
        engine.create_store(tx, "kv")?;
        for (key, value) in entries {
            engine.put(tx, "kv", key, value)?;
        }
        engine.commit_tx(tx)?;
        engine.checkpoint()?;
        let images =
            std::array::from_fn(|index| files[index].durable_snapshot().expect("durable fixture"));
        Self::open_images(images, cache_pages)
    }

    fn open_images(images: [Vec<u8>; 3], cache_pages: usize) -> Result<Self> {
        let work = Arc::new(ReadWork::new());
        let files = std::array::from_fn(|index| CountingBackend {
            inner: MemoryBackend::from_durable(images[index].clone()),
            work: (index == 1).then(|| Arc::clone(&work)),
        });
        let [manifest, main, wal] = files;
        let engine = Engine::open(
            "point-read-work",
            FileSet::new(manifest, main, wal),
            OpenConfig {
                cache_pages,
                ..OpenConfig::default()
            },
        )?;
        Ok(Self {
            engine,
            work,
            images,
            cache_pages,
        })
    }

    fn root(&self) -> u64 {
        self.engine.catalog()["kv"].store_root_page_id
    }

    fn path(&self, key: &[u8]) -> Vec<u64> {
        let mut path = Vec::new();
        let mut page = self.root();
        loop {
            path.push(page);
            let node = decode_page(page_bytes(&self.images[1], page)).unwrap();
            if node.header.page_kind == PageKind::Leaf {
                return path;
            }
            let index = node
                .internal_cells
                .iter()
                .rposition(|cell| cell.separator.as_slice() <= key)
                .unwrap_or(0);
            page = node.internal_cells[index].child_page_id;
        }
    }

    fn overflow_pages(&self, key: &[u8]) -> Vec<u64> {
        let leaf = *self.path(key).last().unwrap();
        let node = decode_page(page_bytes(&self.images[1], leaf)).unwrap();
        let mut page = node
            .leaf_cells
            .iter()
            .find(|cell| cell.key == key)
            .unwrap()
            .overflow_head_page_id;
        let mut pages = Vec::new();
        while page != 0 {
            assert!(pages.len() < MAX_FIXTURE_PAGES, "fixture overflow loop");
            pages.push(page);
            page = decode_page(page_bytes(&self.images[1], page))
                .unwrap()
                .overflow
                .unwrap()
                .next_overflow_page_id;
        }
        pages
    }

    // A fixed expired timestamp avoids timers and commit-time TTL removal.
    fn expire_overflow(mut self, key: &[u8]) -> Result<Self> {
        let head = self.overflow_pages(key)[0];
        let body = decode_page(page_bytes(&self.images[1], head))?
            .overflow
            .unwrap();
        let mut chunk = body.chunk;
        chunk[8..16].copy_from_slice(&1u64.to_le_bytes());
        let image = encode_overflow_page(head, body.next_overflow_page_id, &chunk)?;
        let start = page_offset(head) as usize;
        self.images[1][start..start + PAGE_SIZE].copy_from_slice(&image);
        Self::open_images(self.images, self.cache_pages)
    }
}

fn page_bytes(image: &[u8], page: u64) -> &[u8] {
    let start = page_offset(page) as usize;
    &image[start..start + PAGE_SIZE]
}

#[test]
fn has_warm_inline_and_overflow_do_not_allocate_headers() -> Result<()> {
    let mut fixture = Fixture::new(
        &[
            (b"inline".to_vec(), vec![0x31; 128]),
            (b"overflow".to_vec(), vec![0x42; 16 * 1024]),
        ],
        32,
    )?;
    let tx = fixture.engine.begin_tx(TxMode::Readonly)?;
    for key in [b"inline".as_slice(), b"overflow".as_slice()] {
        // Warm both page bytes and the bounded LRU queue before counting.
        for _ in 0..128 {
            assert!(fixture.engine.has(tx, "kv", key)?);
        }
        let (exists, work) = allocation_work(|| fixture.engine.has(tx, "kv", key));
        assert!(exists?);
        assert_eq!(work.allocations, 0, "has allocated metadata: {work:?}");
    }
    Ok(())
}

#[test]
fn get_warm_point_allocates_only_the_returned_payload() -> Result<()> {
    let mut fixture = Fixture::new(
        &[
            (b"empty".to_vec(), Vec::new()),
            (b"inline".to_vec(), vec![0x31; 128]),
            (b"overflow".to_vec(), vec![0x42; 16 * 1024]),
        ],
        32,
    )?;
    let tx = fixture.engine.begin_tx(TxMode::Readonly)?;
    for (key, expected) in [
        (b"empty".as_slice(), Vec::new()),
        (b"inline".as_slice(), vec![0x31; 128]),
        (b"overflow".as_slice(), vec![0x42; 16 * 1024]),
    ] {
        for _ in 0..32 {
            assert_eq!(
                fixture.engine.get(tx, "kv", key)?.as_deref(),
                Some(expected.as_slice())
            );
        }
        let (actual, work) = allocation_work(|| fixture.engine.get(tx, "kv", key));
        assert_eq!(actual?.as_deref(), Some(expected.as_slice()));
        assert_eq!(
            work.bytes,
            expected.len(),
            "temporary envelope allocation: {work:?}"
        );
        assert_eq!(work.allocations, usize::from(!expected.is_empty()));
    }
    Ok(())
}

#[test]
fn duplicate_get_many_reads_each_overflow_chain_once() -> Result<()> {
    let mut fixture = Fixture::new(
        &[
            (b"a".to_vec(), vec![0x31; 16 * 1024]),
            (b"b".to_vec(), vec![0x42; 16 * 1024]),
        ],
        1,
    )?;
    let pages_a = fixture.overflow_pages(b"a");
    let pages_b = fixture.overflow_pages(b"b");
    let tx = fixture.engine.begin_tx(TxMode::Readonly)?;
    fixture.work.reset();
    let mut actual = fixture.engine.get_many(
        tx,
        "kv",
        &[b"b".as_slice(), b"a", b"b", b"missing", b"a", b"b"],
    )?;
    assert_eq!(
        actual,
        vec![
            Some(vec![0x42; 16 * 1024]),
            Some(vec![0x31; 16 * 1024]),
            Some(vec![0x42; 16 * 1024]),
            None,
            Some(vec![0x31; 16 * 1024]),
            Some(vec![0x42; 16 * 1024])
        ]
    );
    actual[0].as_mut().unwrap()[0] = 0x99;
    assert_eq!(
        actual[2].as_ref().unwrap()[0],
        0x42,
        "duplicate outputs must own independent buffers"
    );
    for page in pages_a.iter().chain(&pages_b) {
        assert_eq!(fixture.work.reads(*page), 1, "re-read overflow page {page}");
    }
    Ok(())
}

#[test]
fn get_many_shares_ancestors_without_scanning_unrequested_leaves() -> Result<()> {
    let entries: Vec<_> = (0u8..32)
        .map(|index| {
            let mut key = vec![0x51; 384];
            key.push(index);
            (key, vec![index; 256])
        })
        .collect();
    let mut fixture = Fixture::new(&entries, 1)?;
    let first = fixture.path(&entries[1].0);
    let last = fixture.path(&entries[30].0);
    assert!(first.len() > 1 && first.last() != last.last());
    let root = fixture.root();
    let tx = fixture.engine.begin_tx(TxMode::Readonly)?;
    fixture.work.reset();
    let actual = fixture
        .engine
        .get_many(tx, "kv", &[&entries[30].0, &entries[1].0])?;
    assert_eq!(
        actual,
        vec![Some(entries[30].1.clone()), Some(entries[1].1.clone())]
    );
    assert_eq!(
        fixture.work.reads(root),
        1,
        "batch restarted its root descent"
    );
    for entry in &entries {
        let leaf = *fixture.path(&entry.0).last().unwrap();
        if leaf != *first.last().unwrap() && leaf != *last.last().unwrap() {
            assert_eq!(
                fixture.work.reads(leaf),
                0,
                "visited an unrequested leaf {leaf}"
            );
        }
    }
    Ok(())
}

#[test]
fn get_many_preserves_sorted_error_order_and_transaction_after_failure() -> Result<()> {
    let mut fixture = Fixture::new(
        &[
            (b"a".to_vec(), vec![0x31; 16 * 1024]),
            (b"b".to_vec(), vec![0x42; 16 * 1024]),
        ],
        1,
    )?;
    let tail_a = *fixture.overflow_pages(b"a").last().unwrap();
    let head_b = fixture.overflow_pages(b"b")[0];
    fixture.work.fail(tail_a, true);
    fixture.work.fail(head_b, true);
    let tx = fixture.engine.begin_tx(TxMode::Readonly)?;
    let error = fixture
        .engine
        .get_many(tx, "kv", &[b"b".as_slice(), b"a"])
        .unwrap_err();
    assert!(
        matches!(error, EngineError::Storage(message) if message == format!("injected main page {tail_a}"))
    );
    fixture.work.fail(tail_a, false);
    fixture.work.fail(head_b, false);
    assert_eq!(
        fixture.engine.get(tx, "kv", b"a")?,
        Some(vec![0x31; 16 * 1024])
    );
    fixture.engine.rollback_tx(tx)?;
    Ok(())
}

#[test]
fn get_checks_expired_overflow_tail_and_readwrite_still_cleans_it_up() -> Result<()> {
    let mut fixture = Fixture::new(&[(b"expired".to_vec(), vec![0x31; 16 * 1024])], 1)?
        .expire_overflow(b"expired")?;
    let tail = *fixture.overflow_pages(b"expired").last().unwrap();
    let tx = fixture.engine.begin_tx(TxMode::Readonly)?;
    fixture.work.fail(tail, true);
    assert!(matches!(
        fixture.engine.get(tx, "kv", b"expired"),
        Err(EngineError::Storage(_))
    ));
    assert!(!fixture.engine.has(tx, "kv", b"expired")?);
    fixture.work.fail(tail, false);
    assert_eq!(fixture.engine.get(tx, "kv", b"expired")?, None);
    fixture.engine.rollback_tx(tx)?;
    let old_root = fixture.root();
    let tx = fixture.engine.begin_tx(TxMode::Readwrite)?;
    assert_eq!(
        fixture
            .engine
            .get_many(tx, "kv", &[b"expired".as_slice(), b"expired"])?,
        vec![None, None]
    );
    fixture.engine.commit_tx(tx)?;
    assert_ne!(
        fixture.root(),
        old_root,
        "readwrite get must stage expired-key deletion"
    );
    Ok(())
}

#[test]
fn get_many_observes_staged_values_deletes_and_empty_store_errors() -> Result<()> {
    let mut fixture = Fixture::new(
        &[
            (b"a".to_vec(), b"old".to_vec()),
            (b"b".to_vec(), b"kept".to_vec()),
        ],
        8,
    )?;
    let tx = fixture.engine.begin_tx(TxMode::Readwrite)?;
    fixture.engine.put(tx, "kv", b"a", b"new")?;
    fixture.engine.delete(tx, "kv", b"b")?;
    assert_eq!(
        fixture
            .engine
            .get_many(tx, "kv", &[b"b".as_slice(), b"a", b"a", b"missing"])?,
        vec![None, Some(b"new".to_vec()), Some(b"new".to_vec()), None]
    );
    assert!(matches!(
        fixture.engine.get_many(tx, "absent", &[] as &[&[u8]]),
        Err(EngineError::StoreNotFound(_))
    ));
    fixture.engine.clear_store(tx, "kv")?;
    assert_eq!(
        fixture
            .engine
            .get_many(tx, "kv", &[b"a".as_slice(), b"b"])?,
        vec![None, None]
    );
    fixture.engine.create_store(tx, "created")?;
    fixture.engine.put(tx, "created", b"c", b"fresh")?;
    assert_eq!(
        fixture
            .engine
            .get_many(tx, "created", &[b"c".as_slice(), b"c"])?,
        vec![Some(b"fresh".to_vec()), Some(b"fresh".to_vec())]
    );
    fixture.engine.rollback_tx(tx)?;
    Ok(())
}

// Qualified trait calls use the scalar oracle. Ordinary method calls use
// the inherent batch API, so both paths remain available for comparison.
trait HasManyOracle {
    fn has_many<K: AsRef<[u8]>>(&mut self, tx: u64, store: &str, keys: &[K]) -> Result<Vec<bool>>;
}

#[test]
fn get_many_reuses_a_multilevel_path_and_matches_a_byte_oracle() -> Result<()> {
    let entries: Vec<_> = (0u8..32)
        .map(|index| {
            let mut key = vec![0x51; 799];
            key.push(index);
            (key, vec![index; 256])
        })
        .collect();
    let mut fixture = Fixture::new(&entries, 1)?;
    let root = fixture.root();
    let header = decode_page(page_bytes(&fixture.images[1], root))?.header;
    assert_eq!(header.page_kind, PageKind::Internal);
    assert!(
        header.level >= 2,
        "fixture must have at least two internal levels"
    );
    let mut missing = entries[17].0.clone();
    missing.push(0xff);
    let keys = [
        &entries[30].0,
        &entries[1].0,
        &entries[17].0,
        &entries[30].0,
        &missing,
    ];
    let expected: Vec<_> = keys
        .iter()
        .map(|key| {
            entries
                .iter()
                .find(|(stored_key, _)| stored_key.as_slice() == key.as_slice())
                .map(|(_, value)| value.clone())
        })
        .collect();
    let mut internal_pages = std::collections::BTreeSet::new();
    let mut leaves = std::collections::BTreeSet::new();
    for key in &keys {
        let path = fixture.path(key);
        internal_pages.extend(path[..path.len() - 1].iter().copied());
        leaves.insert(*path.last().unwrap());
    }
    let tx = fixture.engine.begin_tx(TxMode::Readonly)?;
    for (key, value) in keys.iter().zip(&expected) {
        assert_eq!(fixture.engine.get(tx, "kv", key)?, *value);
    }
    fixture.work.reset();
    assert_eq!(fixture.engine.get_many(tx, "kv", &keys)?, expected);
    for page in internal_pages {
        assert_eq!(
            fixture.work.reads(page),
            1,
            "re-read internal ancestor {page}"
        );
    }
    for (key, _) in &entries {
        let leaf = *fixture.path(key).last().unwrap();
        if !leaves.contains(&leaf) {
            assert_eq!(
                fixture.work.reads(leaf),
                0,
                "visited unrequested leaf {leaf}"
            );
        }
    }
    fixture.engine.rollback_tx(tx)?;
    Ok(())
}

impl HasManyOracle for Engine<CountingBackend> {
    fn has_many<K: AsRef<[u8]>>(&mut self, tx: u64, store: &str, keys: &[K]) -> Result<Vec<bool>> {
        moyodb_engine::bytes::validate_store_name(store)?;
        for key in keys {
            moyodb_engine::bytes::validate_key(key.as_ref())?;
        }
        let mut order: Vec<_> = (0..keys.len()).collect();
        order.sort_by(|left, right| keys[*left].as_ref().cmp(keys[*right].as_ref()));
        let mut values = vec![false; keys.len()];
        for index in order {
            values[index] = self.has(tx, store, keys[index].as_ref())?;
        }
        Ok(values)
    }
}

#[test]
fn has_many_shares_ancestors_and_matches_the_scalar_oracle() -> Result<()> {
    let entries: Vec<_> = (0u8..32)
        .map(|index| {
            let mut key = vec![0x51; 384];
            key.push(index);
            (key, vec![index; 256])
        })
        .collect();
    let mut fixture = Fixture::new(&entries, 1)?;
    let mut missing = entries[15].0.clone();
    missing.push(0xff);
    let keys = [&entries[30].0, &missing, &entries[1].0, &entries[30].0];
    let root = fixture.root();
    let tx = fixture.engine.begin_tx(TxMode::Readonly)?;
    let expected = HasManyOracle::has_many(&mut fixture.engine, tx, "kv", &keys)?;
    assert_eq!(expected, vec![true, false, true, true]);
    fixture.work.reset();
    assert_eq!(fixture.engine.has_many(tx, "kv", &keys)?, expected);
    assert_eq!(
        fixture.work.reads(root),
        1,
        "has batch restarted its root descent"
    );
    fixture.engine.rollback_tx(tx)?;
    Ok(())
}

#[test]
fn has_many_cleans_expired_base_keys_and_preserves_staged_values() -> Result<()> {
    let mut fixture = Fixture::new(
        &[
            (b"a".to_vec(), vec![0x31; 16 * 1024]),
            (b"b".to_vec(), b"old".to_vec()),
        ],
        1,
    )?
    .expire_overflow(b"a")?;
    let old_root = fixture.root();
    let tx = fixture.engine.begin_tx(TxMode::Readwrite)?;
    assert_eq!(
        fixture
            .engine
            .has_many(tx, "kv", &[b"a".as_slice(), b"a", b"missing"])?,
        vec![false, false, false]
    );
    fixture.engine.commit_tx(tx)?;
    assert_ne!(
        fixture.root(),
        old_root,
        "has batch must stage expired-key deletion"
    );

    let tx = fixture.engine.begin_tx(TxMode::Readwrite)?;
    fixture.engine.put(tx, "kv", b"a", b"fresh")?;
    fixture
        .engine
        .put_with_ttl(tx, "kv", b"b", b"temporary", Some(0))?;
    let keys = [b"b".as_slice(), b"a", b"a", b"missing"];
    assert_eq!(
        fixture.engine.has_many(tx, "kv", &keys)?,
        vec![false, true, true, false]
    );
    assert_eq!(
        HasManyOracle::has_many(&mut fixture.engine, tx, "kv", &keys)?,
        vec![false, true, true, false]
    );
    fixture.engine.commit_tx(tx)?;
    let tx = fixture.engine.begin_tx(TxMode::Readonly)?;
    assert_eq!(fixture.engine.get(tx, "kv", b"a")?, Some(b"fresh".to_vec()));
    assert_eq!(fixture.engine.get(tx, "kv", b"b")?, None);
    fixture.engine.rollback_tx(tx)?;
    Ok(())
}

#[test]
fn has_many_validates_all_inputs_before_readwrite_cleanup() -> Result<()> {
    let mut fixture =
        Fixture::new(&[(b"a".to_vec(), vec![0x31; 16 * 1024])], 1)?.expire_overflow(b"a")?;
    let old_root = fixture.root();
    let tx = fixture.engine.begin_tx(TxMode::Readwrite)?;
    let oversized = vec![0; 1025];
    assert!(matches!(
        fixture
            .engine
            .has_many(tx, "kv", &[b"a".as_slice(), oversized.as_slice()]),
        Err(EngineError::KeyTooLarge(1025))
    ));
    fixture.engine.commit_tx(tx)?;
    assert_eq!(
        fixture.root(),
        old_root,
        "validation failure staged TTL cleanup"
    );
    Ok(())
}
