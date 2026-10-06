use moyodb_engine::txn::IndexOpRef;
use moyodb_engine::{
    BatchExecutionReport, ChangeFeedOptions, Engine, EngineError, Failpoint, MemoryBackend,
    MemoryBundle, OpenConfig, Result, ScanRange, TxMode,
};

const NAME: &str = "indexed-batch";

fn config() -> OpenConfig {
    OpenConfig {
        checkpoint_wal_bytes: u64::MAX,
        checkpoint_dirty_pages: usize::MAX,
        ..OpenConfig::default()
    }
}

fn seeded() -> Result<(MemoryBundle, Engine<MemoryBackend>)> {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open(NAME, bundle.files(), config())?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    for store in ["records", "idx-left", "idx-right"] {
        engine.create_store(tx, store)?;
    }
    engine.put(tx, "records", b"a", b"original")?;
    engine.put(tx, "idx-left", b"old-a", b"")?;
    engine.put(tx, "idx-left", b"old-b", b"")?;
    engine.commit_tx(tx)?;
    engine.checkpoint()?;
    Ok((bundle, engine))
}

fn scalar_report(
    engine: &mut Engine<MemoryBackend>,
    tx: u64,
    entries: &[(&[u8], &[u8])],
    index_ops: &[Vec<IndexOpRef<'_>>],
    ttl: Option<u64>,
) -> BatchExecutionReport<bool> {
    let mut completed = Vec::new();
    for ((key, value), row_ops) in entries.iter().zip(index_ops) {
        let result = (|| {
            let baseline = engine.put_reporting_baseline(tx, "records", key, value, ttl)?;
            for op in row_ops {
                match op {
                    IndexOpRef::Put { store, key } => engine.put(tx, store, key, b"")?,
                    IndexOpRef::Delete { store, key } => {
                        engine.delete(tx, store, key)?;
                    }
                }
            }
            Ok(baseline)
        })();
        match result {
            Ok(baseline) => completed.push(baseline),
            Err(error) => return BatchExecutionReport::failure(completed, error),
        }
    }
    BatchExecutionReport::success(completed)
}

fn assert_same_staged(
    scalar: &mut Engine<MemoryBackend>,
    scalar_tx: u64,
    batch: &mut Engine<MemoryBackend>,
    batch_tx: u64,
) -> Result<()> {
    for store in ["records", "idx-left", "idx-right"] {
        assert_eq!(
            scalar.scan(scalar_tx, store, &ScanRange::default())?,
            batch.scan(batch_tx, store, &ScanRange::default())?
        );
    }
    Ok(())
}

#[test]
fn indexed_batch_matches_scalar_duplicate_rows_cross_store_state_and_events() -> Result<()> {
    let (scalar_bundle, mut scalar) = seeded()?;
    let (batch_bundle, mut batch) = seeded()?;
    let large = vec![0x47; 64 * 1024];
    let entries: Vec<(&[u8], &[u8])> = vec![
        (b"a", b"first"),
        (b"a", b"second"),
        (b"c", &large),
        (b"empty", b""),
    ];
    let ops = vec![
        vec![
            IndexOpRef::Delete {
                store: "idx-left",
                key: b"old-a",
            },
            IndexOpRef::Put {
                store: "idx-left",
                key: b"first-a",
            },
        ],
        vec![
            IndexOpRef::Delete {
                store: "idx-left",
                key: b"first-a",
            },
            IndexOpRef::Put {
                store: "idx-left",
                key: b"second-a",
            },
        ],
        vec![
            IndexOpRef::Put {
                store: "idx-left",
                key: b"c",
            },
            IndexOpRef::Put {
                store: "idx-right",
                key: b"c",
            },
        ],
        vec![],
    ];
    let scalar_tx = scalar.begin_tx(TxMode::Readwrite)?;
    let batch_tx = batch.begin_tx(TxMode::Readwrite)?;
    let expected = scalar_report(&mut scalar, scalar_tx, &entries, &ops, None);
    let report = batch.put_many_indexed_report(batch_tx, "records", &entries, &ops, None);
    assert_eq!(report.completed, [true, true, false, false]);
    assert_eq!(report.completed, expected.completed);
    assert_eq!(report.error, expected.error);
    assert_same_staged(&mut scalar, scalar_tx, &mut batch, batch_tx)?;
    let scalar_commit = scalar.commit_tx(scalar_tx)?;
    let batch_commit = batch.commit_tx(batch_tx)?;
    assert_eq!(batch_commit, scalar_commit);
    assert_eq!(
        batch.changes_since(1, ChangeFeedOptions::default())?,
        scalar.changes_since(1, ChangeFeedOptions::default())?
    );
    batch.checkpoint()?;
    scalar.checkpoint()?;
    assert_eq!(
        batch_bundle.main.durable_snapshot(),
        scalar_bundle.main.durable_snapshot()
    );
    Ok(())
}

#[test]
fn dead_index_store_error_reports_only_complete_rows_and_preserves_scalar_prefix() -> Result<()> {
    let (_scalar_bundle, mut scalar) = seeded()?;
    let (_batch_bundle, mut batch) = seeded()?;
    let entries: Vec<(&[u8], &[u8])> = vec![
        (b"a", b"updated"),
        (b"b", b"failed-row-primary"),
        (b"later", b"must-not-run"),
    ];
    let ops = vec![
        vec![IndexOpRef::Put {
            store: "idx-right",
            key: b"a",
        }],
        vec![
            IndexOpRef::Delete {
                store: "idx-left",
                key: b"old-b",
            },
            IndexOpRef::Put {
                store: "missing-index",
                key: b"b",
            },
        ],
        vec![IndexOpRef::Put {
            store: "idx-right",
            key: b"later",
        }],
    ];
    let scalar_tx = scalar.begin_tx(TxMode::Readwrite)?;
    let batch_tx = batch.begin_tx(TxMode::Readwrite)?;
    let expected = scalar_report(&mut scalar, scalar_tx, &entries, &ops, None);
    let report = batch.put_many_indexed_report(batch_tx, "records", &entries, &ops, None);
    assert_eq!(report.completed, [true]);
    assert_eq!(report.completed, expected.completed);
    assert_eq!(
        report.error,
        Some(EngineError::StoreNotFound("missing-index".into()))
    );
    assert_eq!(report.error, expected.error);
    assert_same_staged(&mut scalar, scalar_tx, &mut batch, batch_tx)?;
    assert_eq!(
        batch.get(batch_tx, "records", b"b")?.as_deref(),
        Some(b"failed-row-primary".as_slice())
    );
    assert_eq!(batch.get(batch_tx, "records", b"later")?, None);
    assert_eq!(batch.get(batch_tx, "idx-left", b"old-b")?, None);
    scalar.commit_tx(scalar_tx)?;
    batch.commit_tx(batch_tx)?;
    assert_eq!(
        batch.changes_since(1, ChangeFeedOptions::default())?,
        scalar.changes_since(1, ChangeFeedOptions::default())?
    );
    Ok(())
}

#[test]
fn invalid_index_key_is_validated_after_primary_and_prior_index_operations() -> Result<()> {
    let (_bundle, mut engine) = seeded()?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    let invalid = vec![0x47; moyodb_engine::bytes::MAX_KEY_BYTES + 1];
    let entries = [(b"a".as_slice(), b"updated".as_slice())];
    let ops = vec![vec![
        IndexOpRef::Delete {
            store: "idx-left",
            key: b"old-a",
        },
        IndexOpRef::Put {
            store: "idx-right",
            key: &invalid,
        },
    ]];
    let report = engine.put_many_indexed_report(tx, "records", &entries, &ops, None);
    assert!(report.completed.is_empty());
    assert_eq!(report.error, Some(EngineError::KeyTooLarge(invalid.len())));
    assert_eq!(
        engine.get(tx, "records", b"a")?.as_deref(),
        Some(b"updated".as_slice())
    );
    assert_eq!(engine.get(tx, "idx-left", b"old-a")?, None);
    engine.rollback_tx(tx)
}

#[test]
fn indexed_batch_shape_errors_do_not_stage_primary_writes() -> Result<()> {
    let (_bundle, mut engine) = seeded()?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    let entries = [(b"a".as_slice(), b"updated".as_slice())];
    let report = engine.put_many_indexed_report(tx, "records", &entries, &[], None);
    assert!(report.completed.is_empty());
    assert!(matches!(report.error, Some(EngineError::Serialization(_))));
    assert_eq!(
        engine.get(tx, "records", b"a")?.as_deref(),
        Some(b"original".as_slice())
    );
    engine.rollback_tx(tx)
}

#[test]
fn primary_validation_errors_keep_the_scalar_error_precedence() -> Result<()> {
    let (_bundle, mut engine) = seeded()?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    let invalid = vec![0x47; moyodb_engine::bytes::MAX_KEY_BYTES + 1];
    let expected = engine
        .put_reporting_baseline(tx, "missing-primary", &invalid, b"value", None)
        .unwrap_err();
    let entries = [(invalid.as_slice(), b"value".as_slice())];
    let report = engine.put_many_indexed_report(tx, "missing-primary", &entries, &[vec![]], None);
    assert!(report.completed.is_empty());
    assert_eq!(report.error, Some(expected));
    assert_eq!(
        engine.get(tx, "records", b"a")?.as_deref(),
        Some(b"original".as_slice())
    );
    engine.rollback_tx(tx)
}

#[test]
fn a_dropped_index_store_does_not_prevent_staging_the_failed_rows_primary() -> Result<()> {
    let (_scalar_bundle, mut scalar) = seeded()?;
    let (_batch_bundle, mut batch) = seeded()?;
    let scalar_tx = scalar.begin_tx(TxMode::Readwrite)?;
    let batch_tx = batch.begin_tx(TxMode::Readwrite)?;
    scalar.drop_store(scalar_tx, "idx-right")?;
    batch.drop_store(batch_tx, "idx-right")?;
    let entries: Vec<(&[u8], &[u8])> = vec![
        (b"a", b"updated"),
        (b"b", b"staged-before-index-error"),
        (b"later", b"must-not-run"),
    ];
    let ops = vec![
        vec![],
        vec![IndexOpRef::Put {
            store: "idx-right",
            key: b"b",
        }],
        vec![],
    ];
    let expected = scalar_report(&mut scalar, scalar_tx, &entries, &ops, None);
    let report = batch.put_many_indexed_report(batch_tx, "records", &entries, &ops, None);
    assert_eq!(report.completed, [true]);
    assert_eq!(report.completed, expected.completed);
    assert_eq!(
        report.error,
        Some(EngineError::StoreNotFound("idx-right".into()))
    );
    assert_eq!(report.error, expected.error);
    assert_eq!(
        batch.scan(batch_tx, "records", &ScanRange::default())?,
        scalar.scan(scalar_tx, "records", &ScanRange::default())?
    );
    assert_eq!(
        batch.get(batch_tx, "records", b"b")?.as_deref(),
        Some(b"staged-before-index-error".as_slice())
    );
    assert_eq!(batch.get(batch_tx, "records", b"later")?, None);
    scalar.rollback_tx(scalar_tx)?;
    batch.rollback_tx(batch_tx)
}

#[test]
fn indexed_primary_ttl_does_not_expire_the_index_entries() -> Result<()> {
    let (_scalar_bundle, mut scalar) = seeded()?;
    let (_batch_bundle, mut batch) = seeded()?;
    let scalar_tx = scalar.begin_tx(TxMode::Readwrite)?;
    let batch_tx = batch.begin_tx(TxMode::Readwrite)?;
    let entries = [(b"a".as_slice(), b"temporary".as_slice())];
    let ops = vec![vec![IndexOpRef::Put {
        store: "idx-right",
        key: b"a",
    }]];
    let expected = scalar_report(&mut scalar, scalar_tx, &entries, &ops, Some(0));
    let report = batch.put_many_indexed_report(batch_tx, "records", &entries, &ops, Some(0));
    assert_eq!(report.completed, expected.completed);
    assert_eq!(report.error, expected.error);
    assert_same_staged(&mut scalar, scalar_tx, &mut batch, batch_tx)?;
    assert_eq!(batch.get(batch_tx, "records", b"a")?, None);
    assert_eq!(batch.get(batch_tx, "idx-right", b"a")?, Some(Vec::new()));
    scalar.rollback_tx(scalar_tx)?;
    batch.rollback_tx(batch_tx)
}

#[test]
fn indexed_primary_uses_its_store_flags_for_the_eight_mib_logical_limit() -> Result<()> {
    let (_bundle, mut engine) = seeded()?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store_with_compression(
        tx,
        "compressed",
        moyodb_engine::value::StoreCompression::Snappy,
    )?;
    let maximum = moyodb_engine::bytes::MAX_VALUE_BYTES;
    let mut wire = vec![0; maximum + 18];
    wire[..8].copy_from_slice(b"BDBZVAL1");
    wire[8] = 1;
    wire[10..14].copy_from_slice(&(maximum as u32).to_le_bytes());
    let crc = crc32fast::hash(&wire[18..]);
    wire[14..18].copy_from_slice(&crc.to_le_bytes());
    let entries = [(b"large".as_slice(), wire.as_slice())];
    let ops = vec![vec![IndexOpRef::Put {
        store: "idx-right",
        key: b"large",
    }]];
    assert_eq!(
        engine.put_many_indexed(tx, "compressed", &entries, &ops, None)?,
        [false]
    );
    assert_eq!(
        engine
            .get(tx, "compressed", b"large")?
            .map(|value| value.len()),
        Some(maximum + 18)
    );
    assert_eq!(engine.get(tx, "idx-right", b"large")?, Some(Vec::new()));
    let report = engine.put_many_indexed_report(tx, "records", &entries, &ops, None);
    assert!(report.completed.is_empty());
    assert_eq!(report.error, Some(EngineError::ValueTooLarge(maximum + 18)));
    assert_eq!(engine.get(tx, "records", b"large")?, None);
    engine.rollback_tx(tx)
}

#[test]
fn indexed_batch_recovers_primary_and_indexes_at_every_commit_failpoint() -> Result<()> {
    for failpoint in [
        Failpoint::AfterWalFlush,
        Failpoint::AfterMainFlush,
        Failpoint::BeforeSuperblockFlush,
    ] {
        let (bundle, mut engine) = seeded()?;
        let tx = engine.begin_tx(TxMode::Readwrite)?;
        let large = vec![0x91; 64 * 1024];
        let entries = [(b"a".as_slice(), large.as_slice())];
        let ops = vec![vec![
            IndexOpRef::Delete {
                store: "idx-left",
                key: b"old-a",
            },
            IndexOpRef::Put {
                store: "idx-right",
                key: b"new-a",
            },
        ]];
        assert_eq!(
            engine.put_many_indexed(tx, "records", &entries, &ops, None)?,
            [true]
        );
        engine.set_failpoint(Some(failpoint));
        assert!(matches!(
            engine.commit_tx(tx),
            Err(EngineError::InjectedFailure(_))
        ));
        let mut recovered = Engine::open(NAME, bundle.crash_recovered_files(), config())?;
        let reader = recovered.begin_tx(TxMode::Readonly)?;
        assert_eq!(
            recovered.get(reader, "records", b"a")?.as_deref(),
            Some(large.as_slice())
        );
        assert_eq!(recovered.get(reader, "idx-left", b"old-a")?, None);
        assert_eq!(
            recovered.get(reader, "idx-right", b"new-a")?,
            Some(Vec::new())
        );
        recovered.rollback_tx(reader)?;
    }
    Ok(())
}
