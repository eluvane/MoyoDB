mod common;

use moyodb_engine::engine::{Engine, Failpoint, OpenConfig, TxMode};
use moyodb_engine::storage::memory::{MemoryBackend, MemoryBundle};

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
