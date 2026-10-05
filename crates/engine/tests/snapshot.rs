mod common;

use moyodb_engine::bytes::write_u32_le;
use moyodb_engine::checksum::checksum_with_zeroed_region;
use moyodb_engine::engine::TxMode;
use moyodb_engine::snapshot::{SNAPSHOT_CHECKSUM_OFFSET, SNAPSHOT_HEADER_SIZE, SNAPSHOT_VERSION};
use moyodb_engine::EngineError;

fn seed_source(engine: &mut moyodb_engine::engine::Engine<moyodb_engine::MemoryBackend>) {
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "alpha").unwrap();
    engine.create_store(tx, "beta").unwrap();
    engine.create_store(tx, "empty").unwrap();
    engine.put(tx, "alpha", b"a", b"1").unwrap();
    engine.put(tx, "alpha", b"b", b"2").unwrap();
    engine.put(tx, "beta", &[0, 1, 2], &[9, 8, 7, 6]).unwrap();
    engine.commit_tx(tx).unwrap();
}

fn seed_target_with_old_state(
    engine: &mut moyodb_engine::engine::Engine<moyodb_engine::MemoryBackend>,
) {
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "junk").unwrap();
    engine.put(tx, "junk", b"x", b"old").unwrap();
    engine.commit_tx(tx).unwrap();
}

#[test]
fn export_import_roundtrip_replaces_visible_state() {
    let (_source_bundle, mut source) = common::open_memory_engine("snapshot-source-roundtrip");
    seed_source(&mut source);
    let snapshot = source.export_snapshot().unwrap();

    let (_target_bundle, mut target) = common::open_memory_engine("snapshot-target-roundtrip");
    seed_target_with_old_state(&mut target);

    let imported_txid = target.import_snapshot(&snapshot).unwrap();
    assert!(imported_txid > 0);

    let ro = target.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(
        common::scan_all(&mut target, ro, "alpha"),
        vec![
            (b"a".to_vec(), b"1".to_vec()),
            (b"b".to_vec(), b"2".to_vec())
        ]
    );
    assert_eq!(
        common::scan_all(&mut target, ro, "beta"),
        vec![(vec![0, 1, 2], vec![9, 8, 7, 6])]
    );
    assert!(common::scan_all(&mut target, ro, "empty").is_empty());
    let err = target.get(ro, "junk", b"x").unwrap_err();
    assert!(matches!(err, EngineError::StoreNotFound(_)));
    target.rollback_tx(ro).unwrap();
}

#[test]
fn export_snapshot_ignores_uncommitted_write_state() {
    let (_bundle, mut source) = common::open_memory_engine("snapshot-consistency");
    seed_source(&mut source);

    let rw = source.begin_tx(TxMode::Readwrite).unwrap();
    source.put(rw, "alpha", b"pending", b"nope").unwrap();
    source.delete(rw, "alpha", b"a").unwrap();

    let snapshot = source.export_snapshot().unwrap();
    source.rollback_tx(rw).unwrap();

    let (_target_bundle, mut target) = common::open_memory_engine("snapshot-consistency-import");
    target.import_snapshot(&snapshot).unwrap();

    let ro = target.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(target.get(ro, "alpha", b"a").unwrap(), Some(b"1".to_vec()));
    assert_eq!(target.get(ro, "alpha", b"pending").unwrap(), None);
    target.rollback_tx(ro).unwrap();
}

#[test]
fn snapshot_checksum_is_validated_before_import() {
    let (_source_bundle, mut source) = common::open_memory_engine("snapshot-source-checksum");
    seed_source(&mut source);
    let mut snapshot = source.export_snapshot().unwrap();
    snapshot[SNAPSHOT_HEADER_SIZE] ^= 0x5a;

    let (_target_bundle, mut target) = common::open_memory_engine("snapshot-target-checksum");
    seed_target_with_old_state(&mut target);
    let stats_before = target.stats().unwrap();

    let err = target.import_snapshot(&snapshot).unwrap_err();
    assert_eq!(err.code(), "CorruptionError");
    assert!(err.to_string().contains("checksum"));

    let stats_after = target.stats().unwrap();
    assert_eq!(
        stats_after.last_committed_txid,
        stats_before.last_committed_txid
    );

    let ro = target.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(target.get(ro, "junk", b"x").unwrap(), Some(b"old".to_vec()));
    let err = target.get(ro, "alpha", b"a").unwrap_err();
    assert!(matches!(err, EngineError::StoreNotFound(_)));
    target.rollback_tx(ro).unwrap();
}

#[test]
fn snapshot_version_mismatch_is_rejected() {
    let (_source_bundle, mut source) = common::open_memory_engine("snapshot-source-version");
    seed_source(&mut source);
    let mut snapshot = source.export_snapshot().unwrap();
    write_u32_le(&mut snapshot, 8, SNAPSHOT_VERSION + 1).unwrap();
    let checksum = checksum_with_zeroed_region(&snapshot, SNAPSHOT_CHECKSUM_OFFSET, 4);
    write_u32_le(&mut snapshot, SNAPSHOT_CHECKSUM_OFFSET, checksum).unwrap();

    let (_target_bundle, mut target) = common::open_memory_engine("snapshot-target-version");
    seed_target_with_old_state(&mut target);
    let stats_before = target.stats().unwrap();

    let err = target.import_snapshot(&snapshot).unwrap_err();
    assert_eq!(err.code(), "CorruptionError");
    assert!(err.to_string().contains("unsupported snapshot version"));

    let stats_after = target.stats().unwrap();
    assert_eq!(
        stats_after.last_committed_txid,
        stats_before.last_committed_txid
    );

    let ro = target.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(target.get(ro, "junk", b"x").unwrap(), Some(b"old".to_vec()));
    let err = target.get(ro, "alpha", b"a").unwrap_err();
    assert!(matches!(err, EngineError::StoreNotFound(_)));
    target.rollback_tx(ro).unwrap();
}

#[test]
fn snapshot_roundtrip_preserves_live_ttl_metadata_and_skips_expired_rows() {
    let (_source_bundle, mut source) = common::open_memory_engine("snapshot-ttl-source");

    let tx = source.begin_tx(TxMode::Readwrite).unwrap();
    source.create_store(tx, "ttl").unwrap();
    source
        .put_with_ttl(tx, "ttl", b"live", b"v", Some(60_000))
        .unwrap();
    source
        .put_with_ttl(tx, "ttl", b"expired", b"gone", Some(0))
        .unwrap();
    source.commit_tx(tx).unwrap();

    let snapshot = source.export_snapshot().unwrap();
    let decoded = moyodb_engine::snapshot::decode_snapshot(&snapshot).unwrap();
    let ttl_store = decoded
        .stores
        .iter()
        .find(|store| store.name == "ttl")
        .unwrap();
    assert_eq!(ttl_store.entries.len(), 1);
    assert_eq!(ttl_store.entries[0].key, b"live".to_vec());
    assert_eq!(ttl_store.entries[0].value, b"v".to_vec());
    assert!(ttl_store.entries[0].expires_at_ms.is_some());

    let (_target_bundle, mut target) = common::open_memory_engine("snapshot-ttl-target");
    target.import_snapshot(&snapshot).unwrap();

    let ro = target.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(target.get(ro, "ttl", b"live").unwrap(), Some(b"v".to_vec()));
    assert_eq!(target.get(ro, "ttl", b"expired").unwrap(), None);
    target.rollback_tx(ro).unwrap();
}
