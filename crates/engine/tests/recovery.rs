mod common;

use moyodb_engine::btree::{build_catalog_tree, read_catalog, PageAllocator};
use moyodb_engine::catalog::{
    encode_store_metadata, CatalogState, ChangeFeedPolicy, CATALOG_SCHEMA_VERSION_KEY,
};
use moyodb_engine::engine::{Engine, Failpoint, OpenConfig, TxMode};
use moyodb_engine::layout::{
    page_offset, PageKind, StoreMetadata, SuperblockState, ValueKind, PAGE_SIZE,
};
use moyodb_engine::page::{decode_page, encode_leaf_page, LeafCell};
use moyodb_engine::pager::Pager;
use moyodb_engine::recovery::load_catalog_snapshot;
use moyodb_engine::storage::memory::{MemoryBackend, MemoryBundle};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;

// Thread-local counters exclude other tests. Setup and reporting are untracked.
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

fn allocation_work<T>(operation: impl FnOnce() -> T) -> (T, usize, usize) {
    ALLOCATION_WORK.with(|work| work.set(Some((0, 0))));
    let value = operation();
    let (count, bytes) = ALLOCATION_WORK.with(|work| work.replace(None).unwrap());
    (value, count, bytes)
}

fn seed_kv(engine: &mut Engine<MemoryBackend>, key: &[u8], value: &[u8]) {
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    engine.put(tx, "kv", key, value).unwrap();
    engine.commit_tx(tx).unwrap();
}

fn commit_fails(
    engine: &mut Engine<MemoryBackend>,
    failpoint: Failpoint,
    key: &[u8],
    value: &[u8],
) {
    engine.set_failpoint(Some(failpoint));
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.put(tx, "kv", key, value).unwrap();
    let err = engine.commit_tx(tx).unwrap_err();
    assert_eq!(err.code(), "InjectedFailureError");
}

fn recovered_value(name: &str, bundle: &MemoryBundle, key: &[u8]) -> Option<Vec<u8>> {
    let mut reopened = common::reopen_memory_engine(name, bundle);
    let ro = reopened.begin_tx(TxMode::Readonly).unwrap();
    let value = reopened.get(ro, "kv", key).unwrap();
    reopened.rollback_tx(ro).unwrap();
    value
}

#[test]
fn recovery_replays_committed_wal_after_after_wal_flush_failpoint() {
    let (bundle, mut engine) = common::open_memory_engine("recovery-a");
    seed_kv(&mut engine, b"a", b"1");
    commit_fails(&mut engine, Failpoint::AfterWalFlush, b"b", b"2");

    drop(engine);
    let mut reopened = common::reopen_memory_engine("recovery-a", &bundle);
    let tx = reopened.begin_tx(TxMode::Readonly).unwrap();
    let a = reopened.get(tx, "kv", b"a").unwrap();
    let b = reopened.get(tx, "kv", b"b").unwrap();
    reopened.rollback_tx(tx).unwrap();
    assert_eq!(a, Some(b"1".to_vec()));
    assert_eq!(b, Some(b"2".to_vec()));
}

#[test]
fn recovery_replays_committed_wal_before_superblock_flush_failpoint() {
    let (bundle, mut engine) = common::open_memory_engine("recovery-before-superblock");
    seed_kv(&mut engine, b"base", b"ok");
    commit_fails(
        &mut engine,
        Failpoint::BeforeSuperblockFlush,
        b"after",
        b"yes",
    );

    drop(engine);
    assert_eq!(
        recovered_value("recovery-before-superblock", &bundle, b"after"),
        Some(b"yes".to_vec())
    );
}

#[test]
fn latest_wal_durable_commit_wins_across_repeated_failpoints() {
    let (bundle, mut engine) = common::open_memory_engine("recovery-repeated-failpoints");
    seed_kv(&mut engine, b"base", b"ok");
    commit_fails(&mut engine, Failpoint::AfterWalFlush, b"after", b"first");
    assert!(engine.recover().unwrap().pending_committed);
    commit_fails(&mut engine, Failpoint::AfterWalFlush, b"after", b"second");

    drop(engine);
    assert_eq!(
        recovered_value("recovery-repeated-failpoints", &bundle, b"after"),
        Some(b"second".to_vec())
    );
}

#[test]
fn incomplete_wal_tail_is_ignored() {
    let (bundle, mut engine) = common::open_memory_engine("recovery-b");
    seed_kv(&mut engine, b"a", b"1");
    drop(engine);

    let mut wal = bundle.wal.clone();
    let offset = wal.len().unwrap();
    wal.write_at(offset, &[1, 2, 3, 4, 5, 6]).unwrap();
    wal.flush().unwrap();

    assert_eq!(
        recovered_value("recovery-b", &bundle, b"a"),
        Some(b"1".to_vec())
    );
}

#[test]
fn deferred_commits_survive_reopen_without_close() {
    let (bundle, mut engine) = common::open_memory_engine("recovery-deferred-wal");
    for (key, value) in [
        (b"a".as_slice(), b"1".as_slice()),
        (b"b", b"2"),
        (b"c", b"3"),
    ] {
        let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
        if key == b"a" {
            engine.create_store(tx, "kv").unwrap();
        }
        engine.put(tx, "kv", key, value).unwrap();
        engine.commit_tx(tx).unwrap();
    }
    assert!(engine.stats().unwrap().wal_len > 0);
    drop(engine);

    let mut reopened = common::reopen_memory_engine("recovery-deferred-wal", &bundle);
    let ro = reopened.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(reopened.get(ro, "kv", b"a").unwrap(), Some(b"1".to_vec()));
    assert_eq!(reopened.get(ro, "kv", b"c").unwrap(), Some(b"3".to_vec()));
    reopened.rollback_tx(ro).unwrap();
}

#[test]
fn close_checkpoints_wal_and_keeps_rows() {
    let (bundle, mut engine) = common::open_memory_engine("recovery-checkpoint-close");
    seed_kv(&mut engine, b"k", b"v");
    assert!(engine.stats().unwrap().wal_len > 0);

    engine.close().unwrap();
    assert!(bundle.wal.durable_snapshot().unwrap().is_empty());
    assert_eq!(
        recovered_value("recovery-checkpoint-close", &bundle, b"k"),
        Some(b"v".to_vec())
    );
}

#[test]
fn tiny_cache_keeps_dirty_pages_readable() {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open(
        "recovery-dirty-cache",
        bundle.files(),
        OpenConfig {
            cache_pages: 1,
            ..OpenConfig::default()
        },
    )
    .unwrap();
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    for index in 0..80u32 {
        let key = index.to_string();
        engine
            .put(tx, "kv", key.as_bytes(), key.as_bytes())
            .unwrap();
    }
    engine.commit_tx(tx).unwrap();

    let ro = engine.begin_tx(TxMode::Readonly).unwrap();
    for index in 0..80u32 {
        let key = index.to_string();
        assert_eq!(
            engine.get(ro, "kv", key.as_bytes()).unwrap(),
            Some(key.into_bytes())
        );
    }
    engine.rollback_tx(ro).unwrap();
    engine.close().unwrap();

    let mut reopened = common::reopen_memory_engine("recovery-dirty-cache", &bundle);
    let ro = reopened.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(reopened.get(ro, "kv", b"0").unwrap(), Some(b"0".to_vec()));
    assert_eq!(reopened.get(ro, "kv", b"79").unwrap(), Some(b"79".to_vec()));
    reopened.rollback_tx(ro).unwrap();
}

#[test]
fn opening_bundle_under_different_name_is_rejected() {
    let (bundle, mut engine) = common::open_memory_engine("recovery-db-id-a");
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    engine.commit_tx(tx).unwrap();
    drop(engine);

    let err = moyodb_engine::engine::Engine::open(
        "recovery-db-id-b",
        bundle.crash_recovered_files(),
        moyodb_engine::engine::OpenConfig::default(),
    )
    .unwrap_err();
    assert_eq!(err.code(), "CorruptionError");
}

fn sample_catalog(store_count: usize) -> CatalogState {
    let mut stores = BTreeMap::new();
    for index in 0..store_count {
        stores.insert(
            format!("store-{index:04}"),
            StoreMetadata {
                store_root_page_id: 10 + index as u64,
                created_txid: index as u64 + 1,
                flags: index as u64,
            },
        );
    }
    CatalogState {
        schema_version: 4,
        change_feed_floor_txid: 12,
        change_feed_policy: ChangeFeedPolicy {
            enabled: false,
            retain_txids: Some(7),
        },
        stores,
    }
}

fn superblock(root: u64, next_page_id: u64) -> SuperblockState {
    SuperblockState {
        generation: 1,
        db_id: 1,
        page_size: PAGE_SIZE as u32,
        catalog_root_page_id: root,
        next_page_id,
        last_committed_txid: 1,
        last_replayed_wal_offset: 0,
        active_slot: 0,
    }
}

fn install_catalog(state: &CatalogState) -> (Pager<MemoryBackend>, SuperblockState, PageKind) {
    let mut alloc = PageAllocator::new(1);
    let built = build_catalog_tree(state, &mut alloc).unwrap();
    let root_image = built
        .page_images
        .iter()
        .find(|(page_id, _)| *page_id == built.root_page_id)
        .expect("catalog root image");
    let kind = decode_page(&root_image.1).unwrap().header.page_kind;
    let mut pager = Pager::new(MemoryBackend::new(), 256);
    for (page_id, bytes) in built.page_images {
        pager.write_page_image(page_id, &bytes).unwrap();
    }
    (
        pager,
        superblock(built.root_page_id, alloc.next_page_id()),
        kind,
    )
}

fn store_cell(name: &str, root: u64) -> LeafCell {
    let value = encode_store_metadata(&StoreMetadata {
        store_root_page_id: root,
        created_txid: 1,
        flags: 0,
    })
    .unwrap();
    let total_value_len = u32::try_from(value.len()).unwrap();
    LeafCell {
        key: name.as_bytes().to_vec(),
        value,
        value_kind: ValueKind::Inline,
        total_value_len,
        overflow_head_page_id: 0,
    }
}

fn pager_with_pages(pages: &[(u64, Vec<u8>)]) -> Pager<MemoryBackend> {
    let mut pager = Pager::new(MemoryBackend::new(), 8);
    for (page_id, bytes) in pages {
        pager.write_page_image(*page_id, bytes).unwrap();
    }
    pager
}

#[test]
fn catalog_reload_after_dirty_close_copies_fewer_leaf_entries() {
    let (bundle, mut engine) = common::open_memory_engine("catalog-reload-dirty");
    seed_kv(&mut engine, b"a", b"1");
    commit_fails(&mut engine, Failpoint::AfterWalFlush, b"b", b"2");
    drop(engine);
    assert_eq!(
        recovered_value("catalog-reload-dirty", &bundle, b"a"),
        Some(b"1".to_vec())
    );
    assert_eq!(
        recovered_value("catalog-reload-dirty", &bundle, b"b"),
        Some(b"2".to_vec())
    );

    let leaf = sample_catalog(4);
    let (mut pager, state, kind) = install_catalog(&leaf);
    assert_eq!(kind, PageKind::Leaf);
    let root = state.catalog_root_page_id;
    let _ = load_catalog_snapshot(&mut pager, &state).unwrap();
    let _ = read_catalog(&mut pager, root).unwrap();
    let (fast, fast_allocs, fast_bytes) =
        allocation_work(|| load_catalog_snapshot(&mut pager, &state).unwrap());
    let (slow, slow_allocs, slow_bytes) =
        allocation_work(|| read_catalog(&mut pager, root).unwrap());
    assert_eq!(fast, slow);
    assert_eq!(fast, leaf);
    assert!(
        fast_allocs < slow_allocs,
        "inline leaf reload allocated {fast_allocs} times / {fast_bytes} bytes, scanner allocated {slow_allocs} times / {slow_bytes} bytes"
    );
    assert!(
        fast_bytes < slow_bytes,
        "inline leaf reload copied {fast_bytes} bytes, scanner copied {slow_bytes} bytes"
    );
    println!(
        "catalog leaf reload: inline={fast_allocs} allocs/{fast_bytes} bytes, scan={slow_allocs} allocs/{slow_bytes} bytes"
    );

    let internal = sample_catalog(100);
    let (mut pager, state, kind) = install_catalog(&internal);
    assert_eq!(kind, PageKind::Internal);
    assert_eq!(
        load_catalog_snapshot(&mut pager, &state).unwrap(),
        read_catalog(&mut pager, state.catalog_root_page_id).unwrap()
    );

    let mut bad_checksum = Pager::new(MemoryBackend::new(), 4);
    bad_checksum
        .backend_mut()
        .write_at(page_offset(1), &[0xab; PAGE_SIZE])
        .unwrap();
    let state = superblock(1, 2);
    assert_eq!(
        load_catalog_snapshot(&mut bad_checksum, &state).unwrap_err(),
        read_catalog(&mut bad_checksum, 1).unwrap_err()
    );

    let unknown = encode_leaf_page(
        1,
        0,
        0,
        &[LeafCell {
            key: b"\xffnot-catalog".to_vec(),
            value: vec![0; 8],
            value_kind: ValueKind::Inline,
            total_value_len: 8,
            overflow_head_page_id: 0,
        }],
    )
    .unwrap();
    let mut pager = pager_with_pages(&[(1, unknown)]);
    let state = superblock(1, 2);
    assert_eq!(
        load_catalog_snapshot(&mut pager, &state).unwrap_err(),
        read_catalog(&mut pager, 1).unwrap_err()
    );

    let truncated_schema = encode_leaf_page(
        1,
        0,
        0,
        &[LeafCell {
            key: CATALOG_SCHEMA_VERSION_KEY.to_vec(),
            value: vec![1, 2, 3],
            value_kind: ValueKind::Inline,
            total_value_len: 3,
            overflow_head_page_id: 0,
        }],
    )
    .unwrap();
    let mut pager = pager_with_pages(&[(1, truncated_schema)]);
    assert_eq!(
        load_catalog_snapshot(&mut pager, &state).unwrap_err(),
        read_catalog(&mut pager, 1).unwrap_err()
    );

    let mut name_bytes = store_cell("kv", 4);
    name_bytes.key = vec![0x80];
    let invalid_name = encode_leaf_page(1, 0, 0, &[name_bytes]).unwrap();
    let mut pager = pager_with_pages(&[(1, invalid_name)]);
    assert_eq!(
        load_catalog_snapshot(&mut pager, &state).unwrap_err(),
        read_catalog(&mut pager, 1).unwrap_err()
    );

    let root = encode_leaf_page(1, 0, 2, &[store_cell("kv", 8)]).unwrap();
    let sibling = encode_leaf_page(2, 0, 0, &[store_cell("hidden", 9)]).unwrap();
    let mut pager = pager_with_pages(&[(1, root), (2, sibling)]);
    let loaded = load_catalog_snapshot(&mut pager, &state).unwrap();
    assert_eq!(loaded, read_catalog(&mut pager, 1).unwrap());
    assert!(loaded.stores.contains_key("kv"));
    assert!(!loaded.stores.contains_key("hidden"));

    let overflow = encode_leaf_page(
        1,
        0,
        0,
        &[LeafCell {
            key: b"kv".to_vec(),
            value: Vec::new(),
            value_kind: ValueKind::Overflow,
            total_value_len: 32,
            overflow_head_page_id: 9,
        }],
    )
    .unwrap();
    let mut pager = pager_with_pages(&[(1, overflow)]);
    assert_eq!(
        load_catalog_snapshot(&mut pager, &state).unwrap_err(),
        read_catalog(&mut pager, 1).unwrap_err()
    );
}
