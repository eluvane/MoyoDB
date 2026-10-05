mod common;

use moyodb_engine::engine::{Engine, Failpoint, OpenConfig, TxMode};
use moyodb_engine::storage::memory::MemoryBundle;

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
    assert!(engine.recover().unwrap().pending_committed);

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
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    engine.put(tx, "kv", b"k", b"v").unwrap();
    engine.commit_tx(tx).unwrap();
    assert!(engine.stats().unwrap().wal_len > 0);

    engine.close().unwrap();
    assert!(bundle.wal.durable_snapshot().unwrap().is_empty());

    let mut reopened = common::reopen_memory_engine("recovery-checkpoint-close", &bundle);
    let ro = reopened.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(reopened.get(ro, "kv", b"k").unwrap(), Some(b"v".to_vec()));
    reopened.rollback_tx(ro).unwrap();
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
