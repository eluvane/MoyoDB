mod common;

use moyodb_engine::engine::{Failpoint, TxMode};

#[test]
fn recovery_replays_committed_wal_after_after_wal_flush_failpoint() {
    let (bundle, mut engine) = common::open_memory_engine("recovery-a");
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    engine.put(tx, "kv", b"a", b"1").unwrap();
    engine.commit_tx(tx).unwrap();

    engine.set_failpoint(Some(Failpoint::AfterWalFlush));
    let tx2 = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.put(tx2, "kv", b"b", b"2").unwrap();
    let err = engine.commit_tx(tx2).unwrap_err();
    assert_eq!(err.code(), "InjectedFailureError");

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
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    engine.put(tx, "kv", b"base", b"ok").unwrap();
    engine.commit_tx(tx).unwrap();

    engine.set_failpoint(Some(Failpoint::BeforeSuperblockFlush));
    let tx2 = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.put(tx2, "kv", b"after", b"yes").unwrap();
    let err = engine.commit_tx(tx2).unwrap_err();
    assert_eq!(err.code(), "InjectedFailureError");

    drop(engine);
    let mut reopened = common::reopen_memory_engine("recovery-before-superblock", &bundle);
    let ro = reopened.begin_tx(TxMode::Readonly).unwrap();
    let after = reopened.get(ro, "kv", b"after").unwrap();
    reopened.rollback_tx(ro).unwrap();
    assert_eq!(after, Some(b"yes".to_vec()));
}

#[test]
fn latest_wal_durable_commit_wins_across_repeated_failpoints() {
    let (bundle, mut engine) = common::open_memory_engine("recovery-repeated-failpoints");
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    engine.put(tx, "kv", b"base", b"ok").unwrap();
    engine.commit_tx(tx).unwrap();

    engine.set_failpoint(Some(Failpoint::AfterWalFlush));
    let tx2 = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.put(tx2, "kv", b"after", b"first").unwrap();
    let err = engine.commit_tx(tx2).unwrap_err();
    assert_eq!(err.code(), "InjectedFailureError");

    engine.set_failpoint(Some(Failpoint::AfterWalFlush));
    let tx3 = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.put(tx3, "kv", b"after", b"second").unwrap();
    let err = engine.commit_tx(tx3).unwrap_err();
    assert_eq!(err.code(), "InjectedFailureError");

    drop(engine);
    let mut reopened = common::reopen_memory_engine("recovery-repeated-failpoints", &bundle);
    let ro = reopened.begin_tx(TxMode::Readonly).unwrap();
    let after = reopened.get(ro, "kv", b"after").unwrap();
    reopened.rollback_tx(ro).unwrap();
    assert_eq!(after, Some(b"second".to_vec()));
}

#[test]
fn incomplete_wal_tail_is_ignored() {
    let (bundle, mut engine) = common::open_memory_engine("recovery-b");
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    engine.put(tx, "kv", b"a", b"1").unwrap();
    engine.commit_tx(tx).unwrap();
    drop(engine);

    let mut wal = bundle.wal.clone();
    let offset = wal.len().unwrap();
    wal.write_at(offset, &[1, 2, 3, 4, 5, 6]).unwrap();
    wal.flush().unwrap();

    let mut reopened = common::reopen_memory_engine("recovery-b", &bundle);
    let tx = reopened.begin_tx(TxMode::Readonly).unwrap();
    let a = reopened.get(tx, "kv", b"a").unwrap();
    reopened.rollback_tx(tx).unwrap();
    assert_eq!(a, Some(b"1".to_vec()));
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
