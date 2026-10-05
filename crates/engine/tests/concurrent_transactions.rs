mod common;

use moyodb_engine::catalog::ChangeFeedPolicy;
use moyodb_engine::engine::{EngineHealth, Failpoint, OpenConfig, ScanRange, TxMode};
use moyodb_engine::storage::memory::MemoryBundle;
use moyodb_engine::{Engine, EngineError};

#[test]
fn overlapping_writers_keep_private_changes_and_independent_handles() -> Result<(), EngineError> {
    let (_bundle, mut engine) = common::open_memory_engine("concurrent-private");
    let seed = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(seed, "kv")?;
    engine.put(seed, "kv", b"key", b"base")?;
    engine.commit_tx(seed)?;

    let first = engine.begin_tx(TxMode::Readwrite)?;
    let second = engine.begin_tx(TxMode::Readwrite)?;
    let reader = engine.begin_tx(TxMode::Readonly)?;
    engine.put(first, "kv", b"key", b"first")?;
    engine.put(second, "kv", b"key", b"second")?;
    assert_eq!(engine.get(first, "kv", b"key")?, Some(b"first".to_vec()));
    assert_eq!(engine.get(second, "kv", b"key")?, Some(b"second".to_vec()));
    assert_eq!(engine.get(reader, "kv", b"key")?, Some(b"base".to_vec()));
    assert_eq!(engine.stats()?.active_txns, 3);

    engine.rollback_tx(first)?;
    assert!(engine.stats()?.write_tx_open);
    engine.commit_tx(second)?;
    assert!(!engine.stats()?.write_tx_open);
    assert_eq!(engine.get(reader, "kv", b"key")?, Some(b"base".to_vec()));
    engine.rollback_tx(reader)?;
    let current = engine.begin_tx(TxMode::Readonly)?;
    assert_eq!(engine.get(current, "kv", b"key")?, Some(b"second".to_vec()));
    engine.rollback_tx(current)?;
    Ok(())
}

#[test]
fn stale_commit_is_consumed_without_wal_writes_or_lost_updates() -> Result<(), EngineError> {
    let (bundle, mut engine) = common::open_memory_engine("concurrent-conflict");
    let seed = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(seed, "kv")?;
    engine.put(seed, "kv", b"key", b"base")?;
    let snapshot_txid = engine.commit_tx(seed)?;
    let first = engine.begin_tx(TxMode::Readwrite)?;
    let second = engine.begin_tx(TxMode::Readwrite)?;
    engine.put(first, "kv", b"key", b"winner")?;
    engine.put(second, "kv", b"key", b"loser")?;
    engine.put(second, "kv", b"discarded", b"loser")?;
    let current_txid = engine.commit_tx(first)?;
    let before = engine.stats()?;
    let durable_wal = bundle.files().wal.durable_snapshot().unwrap();

    let conflict = engine.commit_tx(second).unwrap_err();
    assert_eq!(
        conflict,
        EngineError::TransactionConflict {
            snapshot_txid,
            current_txid,
        }
    );
    assert_eq!(conflict.code(), "TransactionConflictError");
    assert_eq!(engine.health(), &EngineHealth::Healthy);
    let after = engine.stats()?;
    assert_eq!(after.active_txns, 0);
    assert!(!after.write_tx_open);
    assert_eq!(after.wal_len, before.wal_len);
    assert_eq!(after.next_page_id, before.next_page_id);
    assert_eq!(after.catalog_root_page_id, before.catalog_root_page_id);
    assert_eq!(after.last_committed_txid, current_txid);
    assert_eq!(bundle.files().wal.durable_snapshot().unwrap(), durable_wal);
    assert_eq!(
        engine.commit_tx(second),
        Err(EngineError::TransactionClosed)
    );
    assert_eq!(
        engine.rollback_tx(second),
        Err(EngineError::TransactionClosed)
    );
    assert_eq!(
        engine.get(second, "kv", b"key"),
        Err(EngineError::TransactionClosed)
    );

    let mut reopened = common::reopen_memory_engine("concurrent-conflict", &bundle);
    let reader = reopened.begin_tx(TxMode::Readonly)?;
    assert_eq!(
        reopened.get(reader, "kv", b"key")?,
        Some(b"winner".to_vec())
    );
    assert_eq!(reopened.get(reader, "kv", b"discarded")?, None);
    reopened.rollback_tx(reader)?;

    let retry = engine.begin_tx(TxMode::Readwrite)?;
    assert_eq!(engine.get(retry, "kv", b"key")?, Some(b"winner".to_vec()));
    engine.put(retry, "kv", b"key", b"retry")?;
    assert_eq!(engine.commit_tx(retry)?, current_txid + 1);
    Ok(())
}

#[test]
fn database_conflicts_cover_disjoint_stores_and_phantom_reads() -> Result<(), EngineError> {
    let (_bundle, mut engine) = common::open_memory_engine("concurrent-phantom");
    let seed = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(seed, "source")?;
    engine.create_store(seed, "result")?;
    engine.commit_tx(seed)?;
    let stale = engine.begin_tx(TxMode::Readwrite)?;
    let writer = engine.begin_tx(TxMode::Readwrite)?;
    assert!(engine
        .scan(stale, "source", &ScanRange::default())?
        .is_empty());
    assert!(!engine.has(stale, "source", b"new")?);
    engine.put(stale, "result", b"count", b"0")?;
    engine.put(writer, "source", b"new", b"value")?;
    engine.commit_tx(writer)?;
    assert!(engine
        .scan(stale, "source", &ScanRange::default())?
        .is_empty());
    assert!(matches!(
        engine.commit_tx(stale),
        Err(EngineError::TransactionConflict { .. })
    ));
    let reader = engine.begin_tx(TxMode::Readonly)?;
    assert_eq!(engine.get(reader, "result", b"count")?, None);
    assert_eq!(
        engine.get(reader, "source", b"new")?,
        Some(b"value".to_vec())
    );
    engine.rollback_tx(reader)?;
    Ok(())
}

#[test]
fn stale_schema_and_store_changes_do_not_override_the_winner() -> Result<(), EngineError> {
    let (_bundle, mut engine) = common::open_memory_engine("concurrent-schema");
    let seed = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(seed, "kv")?;
    engine.put(seed, "kv", b"key", b"base")?;
    engine.commit_tx(seed)?;
    let creator = engine.begin_tx(TxMode::Readwrite)?;
    let dropper = engine.begin_tx(TxMode::Readwrite)?;
    let clearer = engine.begin_tx(TxMode::Readwrite)?;
    let writer = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(creator, "created")?;
    engine.set_schema_version(creator, 99)?;
    engine.set_change_feed_policy(
        creator,
        ChangeFeedPolicy {
            enabled: false,
            retain_txids: None,
        },
    )?;
    engine.drop_store(dropper, "kv")?;
    engine.clear_store(clearer, "kv")?;
    engine.set_schema_version(writer, 7)?;
    engine.put(writer, "kv", b"key", b"winner")?;
    engine.commit_tx(writer)?;
    for stale in [creator, dropper, clearer] {
        assert!(matches!(
            engine.commit_tx(stale),
            Err(EngineError::TransactionConflict { .. })
        ));
    }
    assert_eq!(engine.schema_version(), 7);
    assert!(engine.change_feed_policy().enabled);
    assert!(!engine.catalog().contains_key("created"));
    let reader = engine.begin_tx(TxMode::Readonly)?;
    assert_eq!(engine.get(reader, "kv", b"key")?, Some(b"winner".to_vec()));
    engine.rollback_tx(reader)?;
    Ok(())
}

#[test]
fn stale_write_snapshots_pin_pages_until_their_handles_close() -> Result<(), EngineError> {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open(
        "concurrent-reclamation",
        bundle.files(),
        OpenConfig {
            cache_pages: 1,
            checkpoint_dirty_pages: 1,
            ..OpenConfig::default()
        },
    )?;
    let seed = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(seed, "kv")?;
    engine.set_change_feed_policy(
        seed,
        ChangeFeedPolicy {
            enabled: false,
            retain_txids: None,
        },
    )?;
    engine.put(seed, "kv", b"key", &[0; 9000])?;
    engine.commit_tx(seed)?;
    let stale = engine.begin_tx(TxMode::Readwrite)?;
    let stale_root = engine.catalog()["kv"].store_root_page_id;
    for byte in 1..12 {
        let writer = engine.begin_tx(TxMode::Readwrite)?;
        engine.put(writer, "kv", b"key", &[byte; 9000])?;
        engine.commit_tx(writer)?;
        assert_eq!(engine.get(stale, "kv", b"key")?, Some(vec![0; 9000]));
    }
    assert_ne!(engine.catalog()["kv"].store_root_page_id, stale_root);
    let pinned = engine.stats()?;
    assert!(pinned.retired_pages > 0);
    assert_eq!(pinned.reusable_pages, 0);
    assert!(matches!(
        engine.commit_tx(stale),
        Err(EngineError::TransactionConflict { .. })
    ));
    let writer = engine.begin_tx(TxMode::Readwrite)?;
    engine.put(writer, "kv", b"key", &[12; 9000])?;
    engine.commit_tx(writer)?;
    let reclaimed = engine.stats()?;
    assert!(reclaimed.reusable_pages > 0);
    assert_eq!(reclaimed.next_page_id, pinned.next_page_id);
    let reader = engine.begin_tx(TxMode::Readonly)?;
    assert_eq!(engine.get(reader, "kv", b"key")?, Some(vec![12; 9000]));
    engine.rollback_tx(reader)?;
    Ok(())
}

#[test]
fn conflicts_preserve_failpoints_and_unknown_commit_outcomes() -> Result<(), EngineError> {
    let (_bundle, mut engine) = common::open_memory_engine("concurrent-failpoint");
    let stale = engine.begin_tx(TxMode::Readwrite)?;
    let first = engine.begin_tx(TxMode::Readwrite)?;
    assert_eq!(engine.commit_tx(first)?, 1);
    engine.set_failpoint(Some(Failpoint::AfterWalFlush));
    assert!(matches!(
        engine.commit_tx(stale),
        Err(EngineError::TransactionConflict { .. })
    ));
    let writer = engine.begin_tx(TxMode::Readwrite)?;
    let other = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(writer, "durable")?;
    engine.create_store(other, "discarded")?;
    assert!(matches!(
        engine.commit_tx(writer),
        Err(EngineError::InjectedFailure(_))
    ));
    assert!(matches!(
        engine.health(),
        EngineHealth::RecoveryRequired {
            pending_txid: Some(2),
            ..
        }
    ));
    assert!(matches!(
        engine.commit_tx(other),
        Err(EngineError::RecoveryRequired(_))
    ));
    let report = engine.recover()?;
    assert_eq!(report.pending_txid, Some(2));
    assert!(report.pending_committed);
    assert_eq!(engine.commit_tx(other), Err(EngineError::TransactionClosed));
    assert_eq!(engine.stats()?.active_txns, 0);
    assert!(!engine.stats()?.write_tx_open);
    assert!(engine.catalog().contains_key("durable"));
    assert!(!engine.catalog().contains_key("discarded"));
    let next = engine.begin_tx(TxMode::Readwrite)?;
    assert_eq!(engine.commit_tx(next)?, 3);
    Ok(())
}

#[test]
fn maintenance_requires_every_overlapping_writer_to_close() -> Result<(), EngineError> {
    let (_bundle, mut engine) = common::open_memory_engine("concurrent-maintenance");
    let snapshot = engine.export_snapshot()?;
    let (_target_bundle, mut target) = common::open_memory_engine("concurrent-maintenance-target");
    let first = engine.begin_tx(TxMode::Readwrite)?;
    let second = engine.begin_tx(TxMode::Readwrite)?;
    engine.rollback_tx(first)?;
    assert!(matches!(
        engine.import_snapshot(&snapshot),
        Err(EngineError::DatabaseBusy(_))
    ));
    assert!(matches!(engine.reset(), Err(EngineError::DatabaseBusy(_))));
    assert!(matches!(
        engine.compact_into(&mut target),
        Err(EngineError::DatabaseBusy(_))
    ));
    engine.rollback_tx(second)?;
    engine.compact_into(&mut target)?;
    engine.import_snapshot(&snapshot)?;
    Ok(())
}
