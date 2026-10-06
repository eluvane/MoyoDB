mod common;

use moyodb_engine::change_feed::{
    decode_change_record_payload, encode_change_record_payload, normalize_store_filter,
};
use moyodb_engine::engine::TxMode;
use moyodb_engine::{ChangeFeedOptions, ChangeKind, EngineError};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

#[derive(Clone, Copy, Debug)]
struct AllocCounts {
    allocs: usize,
    bytes: usize,
}

thread_local! {
    static ALLOC_COUNTS: Cell<Option<AllocCounts>> = const { Cell::new(None) };
}

struct AllocProbe;

// SAFETY: each request is forwarded unchanged to System. The thread-local
// counter records sizes only and does not own the memory.
unsafe impl GlobalAlloc for AllocProbe {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_alloc(layout.size());
        // SAFETY: GlobalAlloc supplies the valid layout forwarded to System.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note_alloc(layout.size());
        // SAFETY: GlobalAlloc supplies the valid layout forwarded to System.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: The pointer and layout came from this allocator's System calls.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note_alloc(new_size);
        // SAFETY: GlobalAlloc supplies the live pointer, its layout, and the new size.
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static ALLOC_PROBE: AllocProbe = AllocProbe;

fn note_alloc(size: usize) {
    if size == 0 {
        return;
    }
    ALLOC_COUNTS.with(|counts| {
        if let Some(mut current) = counts.get() {
            current.allocs += 1;
            current.bytes += size;
            counts.set(Some(current));
        }
    });
}

fn measure<T>(operation: impl FnOnce() -> T) -> (T, AllocCounts) {
    // Touch thread-local storage before the window so its setup is not counted.
    ALLOC_COUNTS.with(|counts| counts.set(None));
    ALLOC_COUNTS.with(|counts| {
        counts.set(Some(AllocCounts {
            allocs: 0,
            bytes: 0,
        }))
    });
    let value = operation();
    let counts = ALLOC_COUNTS.with(|counts| counts.take().unwrap());
    (value, counts)
}

#[test]
fn change_feed_reports_committed_changes_in_order_and_supports_filters() {
    let (_bundle, mut engine) = common::open_memory_engine("change-feed-basic");

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "docs").unwrap();
    engine.create_store(tx, "meta").unwrap();
    engine.put(tx, "docs", b"a", b"one").unwrap();
    engine.put(tx, "meta", b"m", b"seed").unwrap();
    let seed_txid = engine.commit_tx(tx).unwrap();

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.put(tx, "docs", b"a", b"two").unwrap();
    engine.put(tx, "docs", b"b", b"three").unwrap();
    engine.delete(tx, "meta", b"m").unwrap();
    let latest_txid = engine.commit_tx(tx).unwrap();

    let feed = engine
        .changes_since(
            seed_txid,
            ChangeFeedOptions {
                stores: Some(vec!["docs".into()]),
                limit: None,
            },
        )
        .unwrap();
    assert_eq!(feed.latest_tx_id, latest_txid);
    assert_eq!(feed.changes.len(), 2);
    assert_eq!(feed.changes[0].tx_id, latest_txid);
    assert_eq!(feed.changes[0].store, "docs");
    assert_eq!(feed.changes[0].key, b"a".to_vec());
    assert_eq!(feed.changes[0].kind, ChangeKind::Put);
    assert_eq!(feed.changes[0].value, Some(b"two".to_vec()));
    assert_eq!(feed.changes[1].key, b"b".to_vec());
    assert_eq!(feed.changes[1].kind, ChangeKind::Put);
    assert_eq!(feed.changes[1].value, Some(b"three".to_vec()));

    let limited = engine
        .changes_since(
            seed_txid,
            ChangeFeedOptions {
                stores: None,
                limit: Some(1),
            },
        )
        .unwrap();
    assert_eq!(limited.latest_tx_id, latest_txid);
    assert_eq!(limited.changes.len(), 1);
    assert_eq!(limited.changes[0].tx_id, latest_txid);
}

#[test]
fn change_feed_survives_reopen_and_snapshot_import_advances_retention_floor() {
    let (bundle, mut engine) = common::open_memory_engine("change-feed-retention");

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "docs").unwrap();
    engine.put(tx, "docs", b"a", b"one").unwrap();
    let seed_txid = engine.commit_tx(tx).unwrap();

    let mut reopened = common::reopen_memory_engine("change-feed-retention", &bundle);
    let persisted_feed = reopened
        .changes_since(0, ChangeFeedOptions::default())
        .unwrap();
    assert_eq!(persisted_feed.latest_tx_id, seed_txid);
    assert_eq!(persisted_feed.changes.len(), 1);
    assert_eq!(persisted_feed.changes[0].store, "docs");
    assert_eq!(persisted_feed.changes[0].key, b"a".to_vec());
    assert_eq!(persisted_feed.changes[0].kind, ChangeKind::Put);
    assert_eq!(persisted_feed.changes[0].value, Some(b"one".to_vec()));

    let snapshot = reopened.export_snapshot().unwrap();
    let imported_txid = reopened.import_snapshot(&snapshot).unwrap();
    assert!(imported_txid > seed_txid);

    let retained = reopened
        .changes_since(seed_txid, ChangeFeedOptions::default())
        .unwrap();
    assert_eq!(retained.latest_tx_id, imported_txid);
    assert!(retained.changes.is_empty());

    let err = reopened
        .changes_since(seed_txid.saturating_sub(1), ChangeFeedOptions::default())
        .unwrap_err();
    assert!(matches!(err, EngineError::ChangeFeedCompacted(_)));
}

#[test]
fn change_feed_clear_and_drop_are_single_store_level_records() {
    let (_bundle, mut engine) = common::open_memory_engine("change-feed-clear-drop");

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "docs").unwrap();
    for index in 0..500u32 {
        engine
            .put(tx, "docs", &index.to_be_bytes(), b"value")
            .unwrap();
    }
    let seed_txid = engine.commit_tx(tx).unwrap();

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.clear_store(tx, "docs").unwrap();
    engine.put(tx, "docs", b"b", b"two-new").unwrap();
    engine.put(tx, "docs", b"c", b"three").unwrap();
    let cleared_txid = engine.commit_tx(tx).unwrap();

    let cleared_feed = engine
        .changes_since(seed_txid, ChangeFeedOptions::default())
        .unwrap();
    assert_eq!(cleared_feed.latest_tx_id, cleared_txid);
    assert_eq!(cleared_feed.changes.len(), 3);
    assert_eq!(cleared_feed.changes[0].kind, ChangeKind::Clear);
    assert!(cleared_feed.changes[0].key.is_empty());
    assert_eq!(cleared_feed.changes[0].value, None);
    assert_eq!(cleared_feed.changes[1].key, b"b".to_vec());
    assert_eq!(cleared_feed.changes[1].kind, ChangeKind::Put);
    assert_eq!(cleared_feed.changes[1].value, Some(b"two-new".to_vec()));
    assert_eq!(cleared_feed.changes[2].key, b"c".to_vec());
    assert_eq!(cleared_feed.changes[2].kind, ChangeKind::Put);
    assert_eq!(cleared_feed.changes[2].value, Some(b"three".to_vec()));

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.drop_store(tx, "docs").unwrap();
    let dropped_txid = engine.commit_tx(tx).unwrap();

    let dropped_feed = engine
        .changes_since(cleared_txid, ChangeFeedOptions::default())
        .unwrap();
    assert_eq!(dropped_feed.latest_tx_id, dropped_txid);
    assert_eq!(dropped_feed.changes.len(), 1);
    assert_eq!(dropped_feed.changes[0].store, "docs");
    assert_eq!(dropped_feed.changes[0].kind, ChangeKind::Drop);
    assert!(dropped_feed.changes[0].key.is_empty());
}

#[test]
fn change_feed_retention_prunes_old_records_and_advances_floor() {
    let (_bundle, mut engine) = common::open_memory_engine("change-feed-retention-policy");

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "docs").unwrap();
    engine
        .set_change_feed_policy(
            tx,
            moyodb_engine::ChangeFeedPolicy {
                enabled: true,
                retain_txids: Some(3),
            },
        )
        .unwrap();
    engine.commit_tx(tx).unwrap();

    let mut last = 0;
    for index in 0..10u8 {
        let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
        engine.put(tx, "docs", &[index], b"v").unwrap();
        last = engine.commit_tx(tx).unwrap();
    }

    let err = engine
        .changes_since(0, ChangeFeedOptions::default())
        .unwrap_err();
    assert!(matches!(err, EngineError::ChangeFeedCompacted(_)));

    let retained = engine
        .changes_since(last - 3, ChangeFeedOptions::default())
        .unwrap();
    assert_eq!(retained.changes.len(), 3);
    assert!(retained
        .changes
        .iter()
        .all(|change| change.tx_id > last - 3));
}

#[test]
fn disabled_change_feed_keeps_no_history() {
    let (_bundle, mut engine) = common::open_memory_engine("change-feed-disabled");

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "docs").unwrap();
    engine.put(tx, "docs", b"a", b"one").unwrap();
    engine.commit_tx(tx).unwrap();

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine
        .set_change_feed_policy(
            tx,
            moyodb_engine::ChangeFeedPolicy {
                enabled: false,
                retain_txids: None,
            },
        )
        .unwrap();
    engine.put(tx, "docs", b"b", b"two").unwrap();
    let disabled_txid = engine.commit_tx(tx).unwrap();
    assert!(!engine
        .catalog()
        .contains_key(moyodb_engine::change_feed::SYSTEM_CHANGELOG_STORE_NAME));

    let err = engine
        .changes_since(0, ChangeFeedOptions::default())
        .unwrap_err();
    assert!(matches!(err, EngineError::ChangeFeedCompacted(_)));
    let empty = engine
        .changes_since(disabled_txid, ChangeFeedOptions::default())
        .unwrap();
    assert!(empty.changes.is_empty());
}

#[test]
fn unknown_change_log_flags_are_rejected() {
    let mut payload =
        encode_change_record_payload("docs", b"a", ChangeKind::Put, Some(b"v")).unwrap();
    payload[5] = 1;
    let err = decode_change_record_payload(1, &payload).unwrap_err();
    assert_eq!(err.code(), "CorruptionError");
    assert!(err.to_string().contains("unsupported change log flags 1"));
}

#[test]
fn change_feed_filter_skips_earlier_stores_and_keeps_later_matches() {
    let (_bundle, mut engine) = common::open_memory_engine("change-feed-filter-order");

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "aaa").unwrap();
    engine.create_store(tx, "docs").unwrap();
    let seed = engine.commit_tx(tx).unwrap();

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.put(tx, "aaa", b"m", b"one").unwrap();
    engine.put(tx, "docs", b"a", b"two").unwrap();
    engine.put(tx, "aaa", b"n", b"three").unwrap();
    engine.put(tx, "docs", b"b", b"four").unwrap();
    let latest = engine.commit_tx(tx).unwrap();

    let limited = engine
        .changes_since(
            seed,
            ChangeFeedOptions {
                stores: Some(vec!["docs".into(), "docs".into()]),
                limit: Some(1),
            },
        )
        .unwrap();
    assert_eq!(limited.latest_tx_id, latest);
    assert_eq!(limited.changes.len(), 1);
    assert_eq!(limited.changes[0].store, "docs");
    assert_eq!(limited.changes[0].key, b"a");
    assert_eq!(limited.changes[0].kind, ChangeKind::Put);
    assert_eq!(limited.changes[0].value.as_deref(), Some(&b"two"[..]));

    let docs = engine
        .changes_since(
            seed,
            ChangeFeedOptions {
                stores: Some(vec!["docs".into()]),
                limit: None,
            },
        )
        .unwrap();
    assert_eq!(docs.changes.len(), 2);
    assert_eq!(docs.changes[0].key, b"a");
    assert_eq!(docs.changes[1].key, b"b");
    assert_eq!(docs.changes[1].value.as_deref(), Some(&b"four"[..]));

    let empty = engine
        .changes_since(
            seed,
            ChangeFeedOptions {
                stores: Some(vec![]),
                limit: None,
            },
        )
        .unwrap();
    assert!(empty.changes.is_empty());
    assert_eq!(empty.latest_tx_id, latest);

    let reserved = engine
        .changes_since(
            seed,
            ChangeFeedOptions {
                stores: Some(vec!["__browserdb:changes".into()]),
                limit: None,
            },
        )
        .unwrap_err();
    assert_eq!(reserved.code(), "ReservedStoreNameError");

    let too_long = engine
        .changes_since(
            seed,
            ChangeFeedOptions {
                stores: Some(vec!["a".repeat(256)]),
                limit: None,
            },
        )
        .unwrap_err();
    assert_eq!(too_long.code(), "StoreNameTooLongError");
}

#[test]
fn change_record_encode_and_store_filter_allocate_once_or_not_at_all() {
    let names = vec!["docs".to_string(), "meta".to_string()];
    let (filter, counts) = measure(|| normalize_store_filter(Some(&names)).unwrap());
    let filter = filter.expect("store list is a filter");
    assert_eq!(counts.allocs, 0, "filter cloned store names: {counts:?}");
    assert_eq!(counts.bytes, 0);
    assert!(filter.contains("docs"));
    assert!(filter.contains("meta"));
    assert!(!filter.contains("other"));

    let (absent, counts) = measure(|| normalize_store_filter(None).unwrap());
    assert!(absent.is_none());
    assert_eq!(counts.allocs, 0);

    let value = vec![0x5a; 64 * 1024];
    let (encoded, counts) = measure(|| {
        encode_change_record_payload("docs", b"ab", ChangeKind::Put, Some(&value)).unwrap()
    });
    assert_eq!(counts.allocs, 1, "payload encode allocated {counts:?}");
    assert_eq!(counts.bytes, encoded.len());
    assert_eq!(encoded.len(), 14 + "docs".len() + 2 + value.len());
    assert_eq!(&encoded[..4], b"CHG1");
    assert_eq!(encoded[4], 1);
    assert_eq!(encoded[5], 0);
    assert_eq!(&encoded[encoded.len() - value.len()..], value.as_slice());

    let record = decode_change_record_payload(9, &encoded).unwrap();
    assert_eq!(record.tx_id, 9);
    assert_eq!(record.store, "docs");
    assert_eq!(record.key, b"ab");
    assert_eq!(record.kind, ChangeKind::Put);
    assert_eq!(record.value.as_deref(), Some(value.as_slice()));

    let (deleted, counts) =
        measure(|| encode_change_record_payload("docs", b"ab", ChangeKind::Delete, None).unwrap());
    assert_eq!(counts.allocs, 1, "delete encode allocated {counts:?}");
    assert_eq!(counts.bytes, deleted.len());
    assert_eq!(deleted.len(), 14 + "docs".len() + 2);
}
