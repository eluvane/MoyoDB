use moyodb_engine::bytes::MAX_VALUE_BYTES;
use moyodb_engine::snapshot::{
    decode_snapshot, encode_snapshot, SnapshotContents, SnapshotEntry, SnapshotStore,
};
use moyodb_engine::txn::BatchOpRef;
use moyodb_engine::value::{
    store_flags_for_user_store, validate_store_flags, validate_store_value, StoreCompression,
    COMPRESSION_VALUE_HEADER_SIZE,
};
use moyodb_engine::{Engine, EngineError, MemoryBundle, OpenConfig, Result, TxMode};

fn raw_record(value: &[u8]) -> Vec<u8> {
    let mut record = Vec::with_capacity(COMPRESSION_VALUE_HEADER_SIZE + value.len());
    record.extend_from_slice(b"BDBZVAL1");
    record.extend_from_slice(&[1, 0]);
    record.extend_from_slice(&(value.len() as u32).to_le_bytes());
    record.extend_from_slice(&crc32fast::hash(value).to_le_bytes());
    record.extend_from_slice(value);
    record
}

fn noise(length: usize) -> Vec<u8> {
    let mut state = 0x1234_5678u32;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state & 0xff) as u8
        })
        .collect()
}

#[test]
fn maximum_logical_raw_fallback_passes_all_writes_reads_and_snapshot_restore() -> Result<()> {
    let record = raw_record(&noise(MAX_VALUE_BYTES));
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open("compression-maximum", bundle.files(), OpenConfig::default())?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store_with_compression(tx, "s", StoreCompression::Snappy)?;
    engine.create_store(tx, "raw")?;
    engine.put(tx, "s", b"k", &record)?;
    assert_eq!(
        engine.put_many(tx, "s", &[(b"k", record.as_slice())])?,
        vec![true]
    );
    let report = engine.apply_batch_refs_report(
        tx,
        "s",
        &[BatchOpRef::Put {
            key: b"k",
            value: &record,
        }],
    );
    assert!(report.error.is_none());
    assert_eq!(report.completed.len(), 1);
    assert_eq!(engine.get(tx, "s", b"k")?, Some(record.clone()));
    assert!(matches!(
        engine.put(tx, "raw", b"k", &record),
        Err(EngineError::ValueTooLarge(_))
    ));
    engine.commit_tx(tx)?;
    let tx = engine.begin_tx(TxMode::Readonly)?;
    assert_eq!(engine.get(tx, "s", b"k")?, Some(record.clone()));
    engine.rollback_tx(tx)?;
    let snapshot = engine.export_snapshot()?;
    let decoded = decode_snapshot(&snapshot)?;
    assert_eq!(
        decoded
            .stores
            .iter()
            .find(|store| store.name == "s")
            .unwrap()
            .entries[0]
            .value,
        record
    );
    let restored_bundle = MemoryBundle::new();
    let mut restored = Engine::open(
        "compression-restore",
        restored_bundle.files(),
        OpenConfig::default(),
    )?;
    restored.import_snapshot(&snapshot)?;
    let tx = restored.begin_tx(TxMode::Readonly)?;
    assert_eq!(restored.get(tx, "s", b"k")?, Some(record));
    restored.rollback_tx(tx)?;
    Ok(())
}

#[test]
fn malformed_compression_headers_fail_before_their_mutation_is_staged() -> Result<()> {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open("compression-invalid", bundle.files(), OpenConfig::default())?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store_with_compression(tx, "s", StoreCompression::Snappy)?;
    let original = raw_record(b"original");
    engine.put(tx, "s", b"k", &original)?;
    let mut bad_magic = raw_record(b"replacement");
    bad_magic[0] ^= 1;
    let mut bad_version = raw_record(b"replacement");
    bad_version[8] = 2;
    let mut bad_tag = raw_record(b"replacement");
    bad_tag[9] = 1;
    let mut bad_length = raw_record(b"replacement");
    bad_length[10..14].copy_from_slice(&0u32.to_le_bytes());
    for invalid in [
        b"BDBZVAL1".to_vec(),
        bad_magic,
        bad_version,
        bad_tag,
        bad_length,
    ] {
        assert!(matches!(
            engine.put(tx, "s", b"k", &invalid),
            Err(EngineError::Corruption(_))
        ));
        assert_eq!(engine.get(tx, "s", b"k")?, Some(original.clone()));
        let report = engine.put_many_report(tx, "s", &[(b"k", invalid.as_slice())]);
        assert!(matches!(report.error, Some(EngineError::Corruption(_))));
        assert!(report.completed.is_empty());
        let report = engine.apply_batch_refs_report(
            tx,
            "s",
            &[BatchOpRef::Put {
                key: b"k",
                value: &invalid,
            }],
        );
        assert!(matches!(report.error, Some(EngineError::Corruption(_))));
        assert!(report.completed.is_empty());
    }
    let mut oversized = raw_record(b"a");
    oversized[10..14].copy_from_slice(&((MAX_VALUE_BYTES + 1) as u32).to_le_bytes());
    assert_eq!(
        engine.put(tx, "s", b"k", &oversized),
        Err(EngineError::ValueTooLarge(MAX_VALUE_BYTES + 1))
    );
    assert_eq!(engine.get(tx, "s", b"k")?, Some(original));
    engine.rollback_tx(tx)?;
    Ok(())
}

#[test]
fn flags_and_snapshot_reads_keep_strict_bounds_and_corruption_errors() -> Result<()> {
    let flags = store_flags_for_user_store(StoreCompression::Snappy);
    for invalid in [1 << 8, 3, 12, 14, 15] {
        assert!(matches!(
            validate_store_flags(invalid),
            Err(EngineError::Corruption(_))
        ));
        assert!(matches!(
            validate_store_value(&raw_record(b"a"), invalid),
            Err(EngineError::Corruption(_))
        ));
    }
    let contents = SnapshotContents {
        source_last_committed_txid: 1,
        schema_version: 0,
        stores: vec![SnapshotStore {
            name: "s".into(),
            flags,
            entries: vec![SnapshotEntry {
                key: b"k".to_vec(),
                value: raw_record(b"a"),
                expires_at_ms: None,
            }],
        }],
    };
    let original = encode_snapshot(&contents)?;
    for invalid_flags in [1 << 8, 12] {
        let mut snapshot = original.clone();
        snapshot[60..68].copy_from_slice(&(invalid_flags as u64).to_le_bytes());
        snapshot[24..28].fill(0);
        let checksum = crc32fast::hash(&snapshot);
        snapshot[24..28].copy_from_slice(&checksum.to_le_bytes());
        assert!(matches!(
            decode_snapshot(&snapshot),
            Err(EngineError::Corruption(_))
        ));
    }
    let mut oversized = original;
    oversized[104..108].copy_from_slice(&((MAX_VALUE_BYTES + 1) as u32).to_le_bytes());
    oversized[24..28].fill(0);
    let checksum = crc32fast::hash(&oversized);
    oversized[24..28].copy_from_slice(&checksum.to_le_bytes());
    assert!(matches!(
        decode_snapshot(&oversized),
        Err(EngineError::Corruption(_))
    ));
    Ok(())
}
