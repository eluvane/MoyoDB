mod common;

use moyodb_engine::btree::KvPair;
use moyodb_engine::engine::{Failpoint, ScanRange, TxMode};
use moyodb_engine::{BatchOp, EngineError};
use std::thread::sleep;
use std::time::Duration;

#[test]
fn commit_and_rollback_work() {
    let (_bundle, mut engine) = common::open_memory_engine("txn-a");

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    engine.put(tx, "kv", b"a", b"1").unwrap();
    engine.commit_tx(tx).unwrap();

    let tx2 = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.put(tx2, "kv", b"b", b"2").unwrap();
    engine.rollback_tx(tx2).unwrap();

    let ro = engine.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(engine.get(ro, "kv", b"a").unwrap(), Some(b"1".to_vec()));
    assert_eq!(engine.get(ro, "kv", b"b").unwrap(), None);
    engine.rollback_tx(ro).unwrap();
}

#[test]
fn snapshot_semantics_hold() {
    let (_bundle, mut engine) = common::open_memory_engine("txn-b");
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    engine.put(tx, "kv", b"a", b"1").unwrap();
    engine.commit_tx(tx).unwrap();

    let ro = engine.begin_tx(TxMode::Readonly).unwrap();
    let rw = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.put(rw, "kv", b"b", b"2").unwrap();
    engine.commit_tx(rw).unwrap();

    assert_eq!(engine.get(ro, "kv", b"b").unwrap(), None);
    engine.rollback_tx(ro).unwrap();

    let ro2 = engine.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(engine.get(ro2, "kv", b"b").unwrap(), Some(b"2".to_vec()));
    engine.rollback_tx(ro2).unwrap();
}

#[test]
fn missing_deletes_keep_roots_and_still_commit() -> Result<(), EngineError> {
    let (_bundle, mut engine) = common::open_memory_engine("txn-missing-delete");
    let seed = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(seed, "kv")?;
    engine.put(seed, "kv", b"kept", b"original")?;
    engine.commit_tx(seed)?;
    let root_before = engine
        .catalog()
        .get("kv")
        .ok_or_else(|| EngineError::Internal("missing seed store".into()))?
        .store_root_page_id;
    let stats_before = engine.stats()?;
    let reader = engine.begin_tx(TxMode::Readonly)?;
    let writer = engine.begin_tx(TxMode::Readwrite)?;
    assert!(!engine.delete(writer, "kv", b"missing")?);
    assert!(!engine.delete(writer, "kv", b"missing")?);
    let committed = engine.commit_tx(writer)?;
    let stats_after = engine.stats()?;
    assert!(committed > stats_before.last_committed_txid);
    assert_eq!(
        stats_after.catalog_root_page_id,
        stats_before.catalog_root_page_id
    );
    let root_after = engine
        .catalog()
        .get("kv")
        .ok_or_else(|| EngineError::Internal("missing seed store".into()))?
        .store_root_page_id;
    assert_eq!(root_after, root_before);
    assert_eq!(
        engine.get(reader, "kv", b"kept")?,
        Some(b"original".to_vec())
    );
    engine.rollback_tx(reader)?;
    Ok(())
}

#[test]
fn absent_delete_preserves_staged_ttl_cleanup_and_later_puts() -> Result<(), EngineError> {
    let (_bundle, mut engine) = common::open_memory_engine("txn-delete-expired-stage");
    let seed = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(seed, "kv")?;
    engine.put(seed, "kv", b"expired", b"original")?;
    engine.commit_tx(seed)?;
    let reader = engine.begin_tx(TxMode::Readonly)?;
    let writer = engine.begin_tx(TxMode::Readwrite)?;
    engine.put_with_ttl(writer, "kv", b"expired", b"temporary", Some(0))?;
    assert!(!engine.delete(writer, "kv", b"expired")?);
    assert!(!engine.delete(writer, "kv", b"expired")?);
    engine.put(writer, "kv", b"later", b"temporary")?;
    assert!(engine.delete(writer, "kv", b"later")?);
    assert!(!engine.delete(writer, "kv", b"later")?);
    engine.put(writer, "kv", b"later", b"fresh")?;
    engine.commit_tx(writer)?;
    assert_eq!(
        engine.get(reader, "kv", b"expired")?,
        Some(b"original".to_vec())
    );
    engine.rollback_tx(reader)?;
    let current = engine.begin_tx(TxMode::Readonly)?;
    assert_eq!(engine.get(current, "kv", b"expired")?, None);
    assert_eq!(
        engine.get(current, "kv", b"later")?,
        Some(b"fresh".to_vec())
    );
    engine.rollback_tx(current)?;
    Ok(())
}

#[test]
fn second_write_tx_can_open_and_rollback_independently() {
    let (_bundle, mut engine) = common::open_memory_engine("txn-c");
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    let second = engine.begin_tx(TxMode::Readwrite).unwrap();
    assert_eq!(engine.stats().unwrap().active_txns, 2);
    engine.rollback_tx(tx).unwrap();
    assert!(engine.stats().unwrap().write_tx_open);
    engine.rollback_tx(second).unwrap();
    assert!(!engine.stats().unwrap().write_tx_open);
}

#[test]
fn readonly_commit_is_rejected() {
    let (_bundle, mut engine) = common::open_memory_engine("txn-d");
    let ro = engine.begin_tx(TxMode::Readonly).unwrap();
    let err = engine.commit_tx(ro).unwrap_err();
    assert!(matches!(err, EngineError::ReadonlyTransaction));
}

#[test]
fn ambiguous_commit_requires_recovery_before_more_work() {
    let (_bundle, mut engine) = common::open_memory_engine("txn-failed-commit");
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    engine.put(tx, "kv", b"a", b"1").unwrap();
    engine.commit_tx(tx).unwrap();
    let reader = engine.begin_tx(TxMode::Readonly).unwrap();

    engine.set_failpoint(Some(Failpoint::AfterWalFlush));
    let tx2 = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.put(tx2, "kv", b"b", b"2").unwrap();
    let err = engine.commit_tx(tx2).unwrap_err();
    assert!(matches!(err, EngineError::InjectedFailure(_)));
    assert!(engine.needs_recovery());

    let err = engine.begin_tx(TxMode::Readwrite).unwrap_err();
    assert_eq!(err.code(), "RecoveryRequiredError");
    let err = engine.get(reader, "kv", b"a").unwrap_err();
    assert_eq!(err.code(), "RecoveryRequiredError");
    engine.rollback_tx(reader).unwrap();

    let report = engine.recover().unwrap();
    assert!(report.pending_committed);
    assert_eq!(report.pending_txid, Some(report.last_committed_txid));

    let ro = engine.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(engine.get(ro, "kv", b"b").unwrap(), Some(b"2".to_vec()));
    engine.rollback_tx(ro).unwrap();

    let tx3 = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.put(tx3, "kv", b"c", b"3").unwrap();
    engine.commit_tx(tx3).unwrap();

    let ro = engine.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(engine.get(ro, "kv", b"c").unwrap(), Some(b"3".to_vec()));
    engine.rollback_tx(ro).unwrap();
}

#[test]
fn invalid_range_is_rejected_without_closing_transaction() {
    let (_bundle, mut engine) = common::open_memory_engine("txn-invalid-range");
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    engine.put(tx, "kv", b"a", b"1").unwrap();
    engine.commit_tx(tx).unwrap();

    let ro = engine.begin_tx(TxMode::Readonly).unwrap();
    let err = engine
        .scan(
            ro,
            "kv",
            &ScanRange {
                gt: Some(b"a".to_vec()),
                gte: Some(b"a".to_vec()),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(matches!(err, EngineError::InvalidRange(_)));
    assert_eq!(engine.get(ro, "kv", b"a").unwrap(), Some(b"1".to_vec()));
    engine.rollback_tx(ro).unwrap();
}

#[test]
fn rw_scan_limit_remains_correct_after_delete_overlay() {
    let (_bundle, mut engine) = common::open_memory_engine("txn-overlay-limit");
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    for i in 0u32..20u32 {
        engine.put(tx, "kv", &i.to_be_bytes(), &[i as u8]).unwrap();
    }
    engine.commit_tx(tx).unwrap();

    let rw = engine.begin_tx(TxMode::Readwrite).unwrap();
    assert!(engine.delete(rw, "kv", &0u32.to_be_bytes()).unwrap());
    let rows = engine
        .scan(
            rw,
            "kv",
            &ScanRange {
                gte: Some(0u32.to_be_bytes().to_vec()),
                limit: Some(10),
                ..Default::default()
            },
        )
        .unwrap();
    let keys: Vec<u32> = rows
        .into_iter()
        .map(|pair| u32::from_be_bytes(pair.key.as_slice().try_into().unwrap()))
        .collect();
    assert_eq!(keys, (1u32..=10u32).collect::<Vec<_>>());
    engine.rollback_tx(rw).unwrap();
}

#[test]
fn small_update_reuses_untouched_leaf_runs() {
    let (_bundle, mut engine) = common::open_memory_engine("txn-leaf-reuse");
    let seed_value = vec![0x55; 200];

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    for i in 0u32..2048u32 {
        engine.put(tx, "kv", &i.to_be_bytes(), &seed_value).unwrap();
    }
    engine.commit_tx(tx).unwrap();

    let before = engine.stats().unwrap().next_page_id;

    let tx2 = engine.begin_tx(TxMode::Readwrite).unwrap();
    let mut updated_value = seed_value.clone();
    updated_value[0] = 0x99;
    engine
        .put(tx2, "kv", &1024u32.to_be_bytes(), &updated_value)
        .unwrap();
    engine.commit_tx(tx2).unwrap();

    let after = engine.stats().unwrap().next_page_id;
    let allocated = after - before;
    assert!(
        allocated <= 8,
        "expected sparse rewrite to allocate only a handful of pages, got {allocated}"
    );

    let ro = engine.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(
        engine.get(ro, "kv", &0u32.to_be_bytes()).unwrap(),
        Some(seed_value.clone())
    );
    assert_eq!(
        engine.get(ro, "kv", &1024u32.to_be_bytes()).unwrap(),
        Some(updated_value)
    );
    assert_eq!(
        engine.get(ro, "kv", &2047u32.to_be_bytes()).unwrap(),
        Some(seed_value)
    );
    engine.rollback_tx(ro).unwrap();
}

#[test]
fn retired_pages_are_reused_only_after_snapshots_move_on() {
    let (_bundle, mut engine) = common::open_memory_engine("txn-page-reuse");
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    for i in 0u32..2048 {
        engine
            .put(tx, "kv", &i.to_be_bytes(), &[0x11; 100])
            .unwrap();
    }
    engine.commit_tx(tx).unwrap();

    let update = |engine: &mut moyodb_engine::Engine<moyodb_engine::MemoryBackend>, byte: u8| {
        let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
        engine
            .put(tx, "kv", &7u32.to_be_bytes(), &[byte; 100])
            .unwrap();
        engine.commit_tx(tx).unwrap();
    };

    for byte in 0..4 {
        update(&mut engine, byte);
    }
    let settled = engine.stats().unwrap().next_page_id;
    for byte in 4..32 {
        update(&mut engine, byte);
    }
    let recycled = engine.stats().unwrap().next_page_id;
    // Only change-log growth may extend the file; without reuse each of the
    // 28 commits would append its whole root-to-leaf path and catalog.
    assert!(
        recycled - settled <= 6,
        "steady single-key updates must recycle retired pages, file grew by {}",
        recycled - settled
    );

    let reader = engine.begin_tx(TxMode::Readonly).unwrap();
    for byte in 32..40 {
        update(&mut engine, byte);
    }
    assert!(engine.stats().unwrap().next_page_id > recycled + 8);
    assert_eq!(
        engine.get(reader, "kv", &7u32.to_be_bytes()).unwrap(),
        Some(vec![31; 100]),
        "an open snapshot keeps reading the pages it started with"
    );
    engine.rollback_tx(reader).unwrap();
}

#[test]
fn reverse_scan_with_limit_reads_from_the_end_and_merges_staged_writes() {
    let (_bundle, mut engine) = common::open_memory_engine("txn-reverse-scan");
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    for i in 0u32..3000 {
        engine.put(tx, "kv", &i.to_be_bytes(), b"v").unwrap();
    }
    engine.commit_tx(tx).unwrap();

    let ro = engine.begin_tx(TxMode::Readonly).unwrap();
    let rows = engine
        .scan(
            ro,
            "kv",
            &ScanRange {
                lt: Some(2000u32.to_be_bytes().to_vec()),
                reverse: true,
                limit: Some(3),
                ..ScanRange::default()
            },
        )
        .unwrap();
    engine.rollback_tx(ro).unwrap();
    let keys: Vec<Vec<u8>> = rows.into_iter().map(|row| row.key).collect();
    assert_eq!(
        keys,
        vec![
            1999u32.to_be_bytes().to_vec(),
            1998u32.to_be_bytes().to_vec(),
            1997u32.to_be_bytes().to_vec(),
        ]
    );

    let rw = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.delete(rw, "kv", &2999u32.to_be_bytes()).unwrap();
    engine
        .put(rw, "kv", &5000u32.to_be_bytes(), b"new")
        .unwrap();
    let rows = engine
        .scan(
            rw,
            "kv",
            &ScanRange {
                reverse: true,
                limit: Some(2),
                ..ScanRange::default()
            },
        )
        .unwrap();
    engine.rollback_tx(rw).unwrap();
    assert_eq!(rows[0].key, 5000u32.to_be_bytes().to_vec());
    assert_eq!(rows[0].value, b"new".to_vec());
    assert_eq!(rows[1].key, 2998u32.to_be_bytes().to_vec());
}

#[test]
fn compact_into_streams_live_rows_into_a_fresh_database() {
    let (_source_bundle, mut source) = common::open_memory_engine("txn-compact");
    let tx = source.begin_tx(TxMode::Readwrite).unwrap();
    source.create_store(tx, "kv").unwrap();
    for i in 0u32..1500 {
        source.put(tx, "kv", &i.to_be_bytes(), &[0x22; 64]).unwrap();
    }
    source.put(tx, "kv", b"large", &vec![0x33; 20_000]).unwrap();
    let source_txid = source.commit_tx(tx).unwrap();

    let target_bundle = moyodb_engine::MemoryBundle::new();
    let mut target = moyodb_engine::Engine::open(
        "txn-compact",
        target_bundle.files(),
        moyodb_engine::OpenConfig::default(),
    )
    .unwrap();
    let published = source.compact_into(&mut target).unwrap();
    assert_eq!(published, source_txid + 1);
    drop(target);

    let mut reopened = common::reopen_memory_engine("txn-compact", &target_bundle);
    let ro = reopened.begin_tx(TxMode::Readonly).unwrap();
    let rows = common::scan_all(&mut reopened, ro, "kv");
    assert_eq!(rows.len(), 1501);
    assert_eq!(
        reopened.get(ro, "kv", b"large").unwrap(),
        Some(vec![0x33; 20_000])
    );
    reopened.rollback_tx(ro).unwrap();
}

#[test]
fn batch_operations_work_and_preserve_order() {
    let (_bundle, mut engine) = common::open_memory_engine("txn-batch");

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    engine
        .put_many(
            tx,
            "kv",
            &[
                (b"a".to_vec(), b"1".to_vec()),
                (b"b".to_vec(), b"2".to_vec()),
                (b"c".to_vec(), b"3".to_vec()),
            ],
        )
        .unwrap();
    engine.commit_tx(tx).unwrap();

    let ro = engine.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(
        engine
            .get_many(
                ro,
                "kv",
                &[
                    b"c".to_vec(),
                    b"a".to_vec(),
                    b"missing".to_vec(),
                    b"b".to_vec()
                ],
            )
            .unwrap(),
        vec![
            Some(b"3".to_vec()),
            Some(b"1".to_vec()),
            None,
            Some(b"2".to_vec()),
        ]
    );
    engine.rollback_tx(ro).unwrap();

    let tx2 = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine
        .delete_many(tx2, "kv", &[b"b".to_vec(), b"missing".to_vec()])
        .unwrap();
    engine
        .apply_batch(
            tx2,
            "kv",
            &[
                BatchOp::Delete { key: b"a".to_vec() },
                BatchOp::Put {
                    key: b"c".to_vec(),
                    value: b"30".to_vec(),
                },
                BatchOp::Put {
                    key: b"d".to_vec(),
                    value: b"4".to_vec(),
                },
            ],
        )
        .unwrap();
    engine.commit_tx(tx2).unwrap();

    let ro2 = engine.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(
        engine
            .get_many(
                ro2,
                "kv",
                &[b"a".to_vec(), b"b".to_vec(), b"c".to_vec(), b"d".to_vec()]
            )
            .unwrap(),
        vec![None, None, Some(b"30".to_vec()), Some(b"4".to_vec())]
    );
    engine.rollback_tx(ro2).unwrap();
}

#[test]
fn batch_reports_completed_prefix_on_error() {
    let (_bundle, mut engine) = common::open_memory_engine("txn-batch-partial");

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();

    let report = engine.apply_batch_report(
        tx,
        "kv",
        &[
            BatchOp::Put {
                key: b"a".to_vec(),
                value: b"1".to_vec(),
            },
            BatchOp::Put {
                key: vec![0u8; 1025],
                value: b"boom".to_vec(),
            },
        ],
    );

    assert_eq!(
        report.completed,
        vec![moyodb_engine::BatchOpOutcome::Put {
            baseline_exists: false,
        }]
    );
    assert!(matches!(report.error, Some(EngineError::KeyTooLarge(1025))));
    assert_eq!(engine.get(tx, "kv", b"a").unwrap(), Some(b"1".to_vec()));
    engine.commit_tx(tx).unwrap();

    let ro = engine.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(engine.get(ro, "kv", b"a").unwrap(), Some(b"1".to_vec()));
    engine.rollback_tx(ro).unwrap();
}

#[test]
fn empty_write_batch_still_validates_transaction_mode_and_store() {
    let (_bundle, mut engine) = common::open_memory_engine("txn-batch-empty-checks");

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    engine.commit_tx(tx).unwrap();

    let ro = engine.begin_tx(TxMode::Readonly).unwrap();
    let err = engine
        .put_many::<Vec<u8>, Vec<u8>>(ro, "kv", &[])
        .unwrap_err();
    assert!(matches!(err, EngineError::ReadonlyTransaction));
    engine.rollback_tx(ro).unwrap();

    let rw = engine.begin_tx(TxMode::Readwrite).unwrap();
    let err = engine
        .delete_many::<Vec<u8>>(rw, "missing", &[])
        .unwrap_err();
    assert!(matches!(err, EngineError::StoreNotFound(name) if name == "missing"));
    engine.rollback_tx(rw).unwrap();
}

#[test]
fn ttl_expired_keys_are_hidden_from_reads_and_scans() {
    let (_bundle, mut engine) = common::open_memory_engine("txn-ttl-hidden");

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    engine.put_with_ttl(tx, "kv", b"a", b"1", Some(0)).unwrap();
    engine.commit_tx(tx).unwrap();

    let ro = engine.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(engine.get(ro, "kv", b"a").unwrap(), None);
    assert!(!engine.has(ro, "kv", b"a").unwrap());
    assert!(engine
        .scan(ro, "kv", &ScanRange::default())
        .unwrap()
        .is_empty());
    engine.rollback_tx(ro).unwrap();
}

#[test]
fn readwrite_commit_cleans_up_expired_keys_seen_during_reads() {
    let (_bundle, mut engine) = common::open_memory_engine("txn-ttl-cleanup");

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    engine.put_with_ttl(tx, "kv", b"a", b"1", Some(1)).unwrap();
    engine.commit_tx(tx).unwrap();
    sleep(Duration::from_millis(20));

    let root_before = engine.catalog().get("kv").unwrap().store_root_page_id;

    let rw = engine.begin_tx(TxMode::Readwrite).unwrap();
    assert_eq!(engine.get(rw, "kv", b"a").unwrap(), None);
    engine.commit_tx(rw).unwrap();

    let root_after = engine.catalog().get("kv").unwrap().store_root_page_id;
    assert_ne!(
        root_after, root_before,
        "cleanup should rewrite the store root"
    );

    let ro = engine.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(engine.get(ro, "kv", b"a").unwrap(), None);
    engine.rollback_tx(ro).unwrap();
}

#[test]
fn staged_put_survives_lazy_cleanup_of_same_expired_base_key() {
    let (_bundle, mut engine) = common::open_memory_engine("txn-ttl-overlay");

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    engine
        .put_with_ttl(tx, "kv", b"a", b"stale", Some(1))
        .unwrap();
    engine.commit_tx(tx).unwrap();
    sleep(Duration::from_millis(20));

    let rw = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.put(rw, "kv", b"a", b"fresh").unwrap();
    let rows = engine.scan(rw, "kv", &ScanRange::default()).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].key, b"a".to_vec());
    assert_eq!(rows[0].value, b"fresh".to_vec());
    engine.commit_tx(rw).unwrap();

    let ro = engine.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(engine.get(ro, "kv", b"a").unwrap(), Some(b"fresh".to_vec()));
    engine.rollback_tx(ro).unwrap();
}

#[test]
fn put_many_with_shared_ttl_expires_as_one_batch() {
    let (_bundle, mut engine) = common::open_memory_engine("txn-ttl-put-many");

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    engine
        .put_many_with_ttl(
            tx,
            "kv",
            &[
                (b"a".to_vec(), b"1".to_vec()),
                (b"b".to_vec(), b"2".to_vec()),
            ],
            Some(0),
        )
        .unwrap();
    engine.commit_tx(tx).unwrap();

    let ro = engine.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(
        engine
            .get_many(ro, "kv", &[b"a".to_vec(), b"b".to_vec()])
            .unwrap(),
        vec![None, None]
    );
    assert!(engine
        .scan(ro, "kv", &ScanRange::default())
        .unwrap()
        .is_empty());
    engine.rollback_tx(ro).unwrap();
}

#[test]
fn sdk_internal_index_stores_survive_reopen_without_becoming_public() {
    let (bundle, mut engine) = common::open_memory_engine("txn-sdk-index-stores");
    let internal_stores = ["__browserdb:indexes", "__browserdb:index:users:byEmail"];
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "users").unwrap();
    engine.put(tx, "users", b"u:1", b"Ada").unwrap();
    for store in internal_stores {
        engine.create_store(tx, store).unwrap();
        engine.put(tx, store, b"email", b"u:1").unwrap();
        assert_eq!(
            engine.scan(tx, store, &ScanRange::default()).unwrap(),
            vec![KvPair {
                key: b"email".to_vec(),
                value: b"u:1".to_vec(),
            }]
        );
    }
    engine.commit_tx(tx).unwrap();

    let mut reopened = common::reopen_memory_engine("txn-sdk-index-stores", &bundle);
    assert_eq!(reopened.store_names(), vec!["users"]);
    assert_eq!(reopened.stats().unwrap().store_count, 1);
    let feed = reopened.changes_since(0, Default::default()).unwrap();
    assert_eq!(feed.changes.len(), 1);
    assert_eq!(feed.changes[0].store, "users");
    let ro = reopened.begin_tx(TxMode::Readonly).unwrap();
    for store in internal_stores {
        assert_eq!(
            reopened.get(ro, store, b"email").unwrap(),
            Some(b"u:1".to_vec())
        );
        assert!(reopened.has(ro, store, b"email").unwrap());
        assert_eq!(
            reopened.scan(ro, store, &ScanRange::default()).unwrap(),
            vec![KvPair {
                key: b"email".to_vec(),
                value: b"u:1".to_vec(),
            }]
        );
    }
    reopened.rollback_tx(ro).unwrap();

    let tx = reopened.begin_tx(TxMode::Readwrite).unwrap();
    assert!(reopened.delete(tx, internal_stores[0], b"email").unwrap());
    reopened.clear_store(tx, internal_stores[1]).unwrap();
    for store in internal_stores {
        reopened.drop_store(tx, store).unwrap();
    }
    reopened.commit_tx(tx).unwrap();
    assert_eq!(
        reopened
            .changes_since(0, Default::default())
            .unwrap()
            .changes,
        feed.changes
    );
}

#[test]
fn create_store_rejects_the_change_log_name() {
    let (_bundle, mut engine) = common::open_memory_engine("txn-reserved-changelog");
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    let err = engine.create_store(tx, "__browserdb:changes").unwrap_err();
    assert_eq!(err.code(), "ReservedStoreNameError");
    assert_eq!(
        engine
            .get(tx, "__browserdb:changes", b"k")
            .unwrap_err()
            .code(),
        "ReservedStoreNameError"
    );
    assert_eq!(
        engine
            .scan(tx, "__browserdb:changes", &ScanRange::default())
            .unwrap_err()
            .code(),
        "ReservedStoreNameError"
    );
    assert_eq!(
        engine
            .put(tx, "__browserdb:changes", b"k", b"v")
            .unwrap_err()
            .code(),
        "ReservedStoreNameError"
    );
    assert_eq!(
        engine
            .drop_store(tx, "__browserdb:changes")
            .unwrap_err()
            .code(),
        "ReservedStoreNameError"
    );
    engine.rollback_tx(tx).unwrap();
}
