mod common;

use moyodb_engine::engine::TxMode;
use moyodb_engine::{ChangeFeedOptions, ChangeKind, EngineError};

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
