mod common;

use moyodb_engine::engine::{Engine, Failpoint, OpenConfig, TxMode};
use moyodb_engine::layout::PAGE_SIZE;
use moyodb_engine::storage::memory::MemoryBackend;
use moyodb_engine::wal::append_page_image_record;
use std::thread::sleep;
use std::time::Duration;

fn seed_kv(engine: &mut Engine<MemoryBackend>) {
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    engine.put(tx, "kv", b"base", b"ok").unwrap();
    engine.commit_tx(tx).unwrap();
}

fn read_value(engine: &mut Engine<MemoryBackend>, key: &[u8]) -> Option<Vec<u8>> {
    let ro = engine.begin_tx(TxMode::Readonly).unwrap();
    let value = engine.get(ro, "kv", key).unwrap();
    engine.rollback_tx(ro).unwrap();
    value
}

fn assert_kv_rows(engine: &mut Engine<MemoryBackend>, expected: &[(&[u8], &[u8])]) {
    let ro = engine.begin_tx(TxMode::Readonly).unwrap();
    let rows = common::scan_all(engine, ro, "kv");
    engine.rollback_tx(ro).unwrap();
    let actual: Vec<(Vec<u8>, Vec<u8>)> = expected
        .iter()
        .map(|(key, value)| (key.to_vec(), value.to_vec()))
        .collect();
    assert_eq!(rows, actual);
}

#[test]
fn crash_before_wal_commit_record() {
    let (bundle, mut engine) = common::open_memory_engine("crash-before-wal-commit-record");
    seed_kv(&mut engine);
    drop(engine);

    let mut wal = bundle.wal.clone();
    let mut offset = wal.len().unwrap();
    append_page_image_record(&mut wal, &mut offset, 99, 123, &vec![0x42; PAGE_SIZE]).unwrap();
    wal.flush().unwrap();

    let mut reopened = common::reopen_memory_engine("crash-before-wal-commit-record", &bundle);
    assert_eq!(read_value(&mut reopened, b"base"), Some(b"ok".to_vec()));
    assert_eq!(read_value(&mut reopened, b"dirty"), None);
}

#[test]
fn crash_after_commit_before_checkpoint() {
    let (bundle, mut engine) = common::open_memory_engine("crash-after-commit-before-checkpoint");
    seed_kv(&mut engine);

    engine.set_failpoint(Some(Failpoint::AfterWalFlush));
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.put(tx, "kv", b"dirty", b"committed-in-wal").unwrap();
    let err = engine.commit_tx(tx).unwrap_err();
    assert_eq!(err.code(), "InjectedFailureError");
    drop(engine);

    let mut reopened =
        common::reopen_memory_engine("crash-after-commit-before-checkpoint", &bundle);
    assert_eq!(
        read_value(&mut reopened, b"dirty"),
        Some(b"committed-in-wal".to_vec())
    );
}

#[test]
fn partial_wal_record_is_ignored() {
    let (bundle, mut engine) = common::open_memory_engine("crash-partial-wal-record");
    seed_kv(&mut engine);
    drop(engine);

    let mut wal = bundle.wal.clone();
    let offset = wal.len().unwrap();
    wal.write_at(offset, &[0x57, 0x41, 0x4c, 0x31, 0x01, 0x02, 0x03])
        .unwrap();
    wal.flush().unwrap();

    let mut reopened = common::reopen_memory_engine("crash-partial-wal-record", &bundle);
    assert_eq!(read_value(&mut reopened, b"base"), Some(b"ok".to_vec()));
}

#[test]
fn corrupt_wal_checksum_stops_replay() {
    let (bundle, mut engine) = common::open_memory_engine("crash-corrupt-wal-checksum");
    seed_kv(&mut engine);

    engine.set_failpoint(Some(Failpoint::AfterWalFlush));
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine
        .put(tx, "kv", b"after", b"should-not-replay")
        .unwrap();
    let err = engine.commit_tx(tx).unwrap_err();
    assert_eq!(err.code(), "InjectedFailureError");
    drop(engine);

    let mut wal = bundle.wal.clone();
    let len = wal.len().unwrap();
    assert!(len > 0);
    let mut last = wal.read_at(len - 1, 1).unwrap();
    last[0] ^= 0x5a;
    wal.write_at(len - 1, &last).unwrap();
    wal.flush().unwrap();

    let mut reopened = common::reopen_memory_engine("crash-corrupt-wal-checksum", &bundle);
    assert_eq!(read_value(&mut reopened, b"base"), Some(b"ok".to_vec()));
    assert_eq!(read_value(&mut reopened, b"after"), None);
}

#[test]
fn multiple_commits_replay_in_order() {
    let (bundle, mut engine) = common::open_memory_engine("crash-multiple-commits");
    seed_kv(&mut engine);

    for value in [
        b"first".as_slice(),
        b"second".as_slice(),
        b"third".as_slice(),
    ] {
        engine.set_failpoint(Some(Failpoint::AfterWalFlush));
        let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
        engine.put(tx, "kv", b"ordered", value).unwrap();
        let err = engine.commit_tx(tx).unwrap_err();
        assert_eq!(err.code(), "InjectedFailureError");
    }
    drop(engine);

    let mut reopened = common::reopen_memory_engine("crash-multiple-commits", &bundle);
    assert_eq!(
        read_value(&mut reopened, b"ordered"),
        Some(b"third".to_vec())
    );
}

#[test]
fn recovery_is_idempotent() {
    let (bundle, mut engine) = common::open_memory_engine("crash-idempotent");
    seed_kv(&mut engine);

    engine.set_failpoint(Some(Failpoint::AfterWalFlush));
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.put(tx, "kv", b"after", b"once").unwrap();
    let err = engine.commit_tx(tx).unwrap_err();
    assert_eq!(err.code(), "InjectedFailureError");
    drop(engine);

    let mut reopened = common::reopen_memory_engine("crash-idempotent", &bundle);
    assert_eq!(read_value(&mut reopened, b"after"), Some(b"once".to_vec()));
    drop(reopened);

    let mut reopened_again = common::reopen_memory_engine("crash-idempotent", &bundle);
    assert_eq!(
        read_value(&mut reopened_again, b"after"),
        Some(b"once".to_vec())
    );
}

#[test]
fn committed_delete_survives_recovery() {
    let (bundle, mut engine) = common::open_memory_engine("crash-delete");
    seed_kv(&mut engine);

    engine.set_failpoint(Some(Failpoint::AfterWalFlush));
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    assert!(engine.delete(tx, "kv", b"base").unwrap());
    let err = engine.commit_tx(tx).unwrap_err();
    assert_eq!(err.code(), "InjectedFailureError");
    drop(engine);

    let mut reopened = common::reopen_memory_engine("crash-delete", &bundle);
    assert_eq!(read_value(&mut reopened, b"base"), None);
}

#[test]
fn store_drop_survives_recovery() {
    let (bundle, mut engine) = common::open_memory_engine("crash-drop-store");
    seed_kv(&mut engine);

    engine.set_failpoint(Some(Failpoint::AfterWalFlush));
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.drop_store(tx, "kv").unwrap();
    let err = engine.commit_tx(tx).unwrap_err();
    assert_eq!(err.code(), "InjectedFailureError");
    drop(engine);

    let mut reopened = common::reopen_memory_engine("crash-drop-store", &bundle);
    assert!(!reopened.store_names().contains(&"kv".to_string()));
    let ro = reopened.begin_tx(TxMode::Readonly).unwrap();
    let err = reopened.get(ro, "kv", b"base").unwrap_err();
    assert_eq!(err.code(), "StoreNotFoundError");
    reopened.rollback_tx(ro).unwrap();
}

#[test]
#[ignore = "not applicable in Rust engine: managed secondary indexes live in the TypeScript SDK"]
fn index_update_survives_recovery() {}

#[test]
fn ttl_state_after_recovery() {
    let (bundle, mut engine) = common::open_memory_engine("crash-ttl");
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    engine.commit_tx(tx).unwrap();

    engine.set_failpoint(Some(Failpoint::AfterWalFlush));
    let tx2 = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine
        .put_with_ttl(tx2, "kv", b"expired", b"gone", Some(1))
        .unwrap();
    let err = engine.commit_tx(tx2).unwrap_err();
    assert_eq!(err.code(), "InjectedFailureError");
    drop(engine);
    sleep(Duration::from_millis(5));

    let mut reopened = common::reopen_memory_engine("crash-ttl", &bundle);
    assert_eq!(read_value(&mut reopened, b"expired"), None);
}

#[test]
fn large_value_overflow_recovery() {
    let (bundle, mut engine) = common::open_memory_engine("crash-large-value");
    seed_kv(&mut engine);
    let large = vec![0xab; 64 * 1024];

    engine.set_failpoint(Some(Failpoint::AfterWalFlush));
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.put(tx, "kv", b"large", &large).unwrap();
    let err = engine.commit_tx(tx).unwrap_err();
    assert_eq!(err.code(), "InjectedFailureError");
    drop(engine);

    let mut reopened = common::reopen_memory_engine("crash-large-value", &bundle);
    assert_eq!(read_value(&mut reopened, b"large"), Some(large));
}

#[test]
fn clear_store_recovery() {
    let (bundle, mut engine) = common::open_memory_engine("crash-clear-store");
    seed_kv(&mut engine);
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.put(tx, "kv", b"another", b"value").unwrap();
    engine.commit_tx(tx).unwrap();

    engine.set_failpoint(Some(Failpoint::AfterWalFlush));
    let tx2 = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.clear_store(tx2, "kv").unwrap();
    let err = engine.commit_tx(tx2).unwrap_err();
    assert_eq!(err.code(), "InjectedFailureError");
    drop(engine);

    let mut reopened = common::reopen_memory_engine("crash-clear-store", &bundle);
    assert_kv_rows(&mut reopened, &[]);
}

#[test]
fn create_if_missing_false_rejects_missing_database() {
    let bundle = moyodb_engine::storage::memory::MemoryBundle::new();
    let err = Engine::open(
        "missing",
        bundle.files(),
        OpenConfig {
            create_if_missing: false,
            ..OpenConfig::default()
        },
    )
    .unwrap_err();
    assert_eq!(err.code(), "StorageError");
}
