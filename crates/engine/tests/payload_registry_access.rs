mod common;

use moyodb_engine::engine::TxMode;
use moyodb_engine::{EngineError, ScanRange};

#[test]
fn payload_registry_is_reserved_for_all_transaction_operations() {
    let (_, mut engine) = common::open_memory_engine("payload-registry-access");
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "docs").unwrap();
    engine.put(tx, "docs", b"key", &vec![7; 32 * 1024]).unwrap();
    engine.commit_tx(tx).unwrap();

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    let registry = "__browserdb:payloads";
    let results = [
        engine.create_store(tx, registry),
        engine.drop_store(tx, registry),
        engine.clear_store(tx, registry),
        engine.put(tx, registry, b"key", b"value"),
        engine.delete(tx, registry, b"key").map(|_| ()),
        engine.get(tx, registry, b"key").map(|_| ()),
        engine.has(tx, registry, b"key").map(|_| ()),
        engine.get_many(tx, registry, &[b"key"]).map(|_| ()),
        engine.get_many_packed(tx, registry, &[b"key"]).map(|_| ()),
        engine.scan(tx, registry, &ScanRange::default()).map(|_| ()),
        engine
            .put_many(tx, registry, &[(b"key", b"value")])
            .map(|_| ()),
        engine.delete_many(tx, registry, &[b"key"]).map(|_| ()),
        engine.apply_batch(tx, registry, &[]).map(|_| ()),
    ];
    for result in results {
        assert!(matches!(result, Err(EngineError::ReservedStoreName(name)) if name == registry));
    }
    assert_eq!(
        engine.get(tx, "docs", b"key").unwrap(),
        Some(vec![7; 32 * 1024])
    );
    engine.rollback_tx(tx).unwrap();
}
