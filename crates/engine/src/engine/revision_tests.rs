use super::*;
use crate::snapshot::decode_snapshot;
use crate::storage::memory::{MemoryBackend, MemoryBundle};
use crate::value::{decode_value_envelope, STORE_FLAG_VALUE_REVISION};

const NAME: &str = "native-value-revisions";
const INDEX: &str = "__browserdb:index:rows:value";
const INDEX_KEY: &[u8] = b"value\0\0key\0\0";

fn open(bundle: &MemoryBundle) -> Result<Engine<MemoryBackend>> {
    Engine::open(
        NAME,
        bundle.files(),
        OpenConfig {
            checkpoint_wal_bytes: u64::MAX,
            checkpoint_dirty_pages: usize::MAX,
            ..OpenConfig::default()
        },
    )
}

fn state(
    engine: &mut Engine<MemoryBackend>,
    tx: u64,
    store: &str,
    key: &[u8],
) -> Result<ValueState> {
    engine.get_many_value_states(tx, store, &[key])?[0]
        .clone()
        .ok_or_else(|| EngineError::Internal("missing fixture state".into()))
}

fn revision(
    engine: &mut Engine<MemoryBackend>,
    tx: u64,
    store: &str,
    key: &[u8],
) -> Result<ValueRevision> {
    state(engine, tx, store, key)?
        .revision
        .ok_or_else(|| EngineError::Internal("missing fixture revision".into()))
}

fn seed(engine: &mut Engine<MemoryBackend>, value: &[u8]) -> Result<(u64, ValueRevision)> {
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(tx, "rows")?;
    engine.create_store(tx, INDEX)?;
    engine.put_many_indexed(
        tx,
        "rows",
        &[(b"key".as_slice(), value)],
        &[vec![IndexOpRef::Put {
            store: INDEX,
            key: INDEX_KEY,
        }]],
        None,
    )?;
    let reference = revision(engine, tx, "rows", b"key")?;
    assert_eq!(revision(engine, tx, INDEX, INDEX_KEY)?, reference);
    let committed = engine.commit_tx(tx)?;
    assert_eq!(reference.epoch, committed);
    Ok((committed, reference))
}

#[test]
fn generic_writes_cannot_forge_managed_index_proof_and_versions_span_all_stores() -> Result<()> {
    let mut engine = open(&MemoryBundle::new())?;
    let (_, initial) = seed(&mut engine, b"old")?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.put(tx, INDEX, INDEX_KEY, b"IDXREV01-fake-native-version")?;
    let raw_index = revision(&mut engine, tx, INDEX, INDEX_KEY)?;
    assert_ne!(raw_index, initial);
    engine.put(tx, "rows", b"key", b"old")?;
    let raw_primary = revision(&mut engine, tx, "rows", b"key")?;
    assert_eq!(raw_index.epoch, raw_primary.epoch);
    assert!(raw_primary.ordinal > raw_index.ordinal);
    assert!(matches!(
        engine.put_index_entry_checked(
            tx,
            "rows",
            INDEX,
            b"key",
            INDEX_KEY,
            initial.epoch,
            initial.ordinal
        ),
        Err(EngineError::ValueRevisionConflict)
    ));
    assert_eq!(revision(&mut engine, tx, INDEX, INDEX_KEY)?, raw_index);
    engine.put_index_entry_checked(
        tx,
        "rows",
        INDEX,
        b"key",
        INDEX_KEY,
        raw_primary.epoch,
        raw_primary.ordinal,
    )?;
    assert_eq!(revision(&mut engine, tx, INDEX, INDEX_KEY)?, raw_primary);
    assert_eq!(engine.get(tx, INDEX, INDEX_KEY)?, Some(Vec::new()));
    for invalid in [
        b"value\0\0other\0\0".as_slice(),
        b"value\0\0key\0".as_slice(),
        b"value\0\0key\0\x01".as_slice(),
        b"value\0\0key\0\0tail".as_slice(),
    ] {
        assert!(matches!(
            engine.put_index_entry_checked(
                tx,
                "rows",
                INDEX,
                b"key",
                invalid,
                raw_primary.epoch,
                raw_primary.ordinal
            ),
            Err(EngineError::Serialization(_))
        ));
    }
    assert!(matches!(
        engine.put_index_entry_checked(
            tx,
            "rows",
            "rows",
            b"key",
            INDEX_KEY,
            raw_primary.epoch,
            raw_primary.ordinal
        ),
        Err(EngineError::Serialization(_))
    ));
    engine.commit_tx(tx)?;
    Ok(())
}

#[test]
fn revision_epochs_survive_wal_reopen_and_conflicting_writers() -> Result<()> {
    let bundle = MemoryBundle::new();
    let mut engine = open(&bundle)?;
    let (committed, initial) = seed(&mut engine, b"old")?;
    engine.abandon()?;
    let files = bundle.crash_recovered_files();
    let bundle = MemoryBundle {
        manifest: files.manifest,
        main: files.main,
        wal: files.wal,
    };
    let mut engine = open(&bundle)?;
    let readonly = engine.begin_tx(TxMode::Readonly)?;
    assert_eq!(revision(&mut engine, readonly, "rows", b"key")?, initial);
    engine.rollback_tx(readonly)?;
    let early = engine.begin_tx(TxMode::Readwrite)?;
    let later = engine.begin_tx(TxMode::Readwrite)?;
    engine.put(early, "rows", b"key", b"early")?;
    let early_revision = revision(&mut engine, early, "rows", b"key")?;
    engine.put(later, "rows", b"key", b"later")?;
    let later_revision = revision(&mut engine, later, "rows", b"key")?;
    assert!(early_revision.epoch > committed);
    assert_eq!(later_revision.epoch, early_revision.epoch);
    assert!(later_revision.ordinal > early_revision.ordinal);
    assert!(matches!(
        engine.put_index_entry_checked(
            later,
            "rows",
            INDEX,
            b"key",
            INDEX_KEY,
            early_revision.epoch,
            early_revision.ordinal
        ),
        Err(EngineError::ValueRevisionConflict)
    ));
    engine.put_index_entry_checked(
        later,
        "rows",
        INDEX,
        b"key",
        INDEX_KEY,
        later_revision.epoch,
        later_revision.ordinal,
    )?;
    assert_eq!(engine.commit_tx(later)?, later_revision.epoch);
    assert!(matches!(
        engine.commit_tx(early),
        Err(EngineError::TransactionConflict { .. })
    ));
    engine.checkpoint()?;
    engine.abandon()?;
    let files = bundle.crash_recovered_files();
    let bundle = MemoryBundle {
        manifest: files.manifest,
        main: files.main,
        wal: files.wal,
    };
    let mut engine = open(&bundle)?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    assert_eq!(revision(&mut engine, tx, "rows", b"key")?, later_revision);
    assert_eq!(revision(&mut engine, tx, INDEX, INDEX_KEY)?, later_revision);
    engine.put(tx, "rows", b"key", b"next")?;
    let next = revision(&mut engine, tx, "rows", b"key")?;
    assert!(next.epoch > later_revision.epoch);
    engine.commit_tx(tx)?;
    Ok(())
}

#[test]
fn managed_ttl_zero_stamps_physical_revision_without_hiding_snapshot() -> Result<()> {
    let mut engine = open(&MemoryBundle::new())?;
    let (_, initial) = seed(&mut engine, b"old")?;
    let snapshot = engine.begin_tx(TxMode::Readonly)?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.put_many_indexed(
        tx,
        "rows",
        &[(b"key".as_slice(), b"expired".as_slice())],
        &[vec![IndexOpRef::Put {
            store: INDEX,
            key: INDEX_KEY,
        }]],
        Some(0),
    )?;
    let expired = state(&mut engine, tx, "rows", b"key")?;
    assert!(!expired.exists && expired.expired && expired.max_length == 0);
    assert_eq!(
        revision(&mut engine, tx, INDEX, INDEX_KEY)?,
        expired.revision.unwrap()
    );
    let rw = engine.txns.get(&tx).unwrap().clone();
    assert!(
        matches!(&rw.inner, TxInner::Readwrite(rw) if matches!(rw.stores["rows"].mutations[b"key".as_slice()], MutationValue::Put(_)))
    );
    engine.put_index_entry_checked(
        tx,
        "rows",
        INDEX,
        b"key",
        INDEX_KEY,
        expired.revision.unwrap().epoch,
        expired.revision.unwrap().ordinal,
    )?;
    engine.commit_tx(tx)?;
    assert_eq!(revision(&mut engine, snapshot, "rows", b"key")?, initial);
    let latest = engine.begin_tx(TxMode::Readonly)?;
    assert!(engine.get_many_value_states(latest, "rows", &[b"key"])?[0].is_none());
    engine.rollback_tx(latest)?;
    engine.rollback_tx(snapshot)?;
    Ok(())
}

#[test]
fn compaction_preserves_revisions_and_snapshot_import_mints_new_versions() -> Result<()> {
    let bundle = MemoryBundle::new();
    let mut source = open(&bundle)?;
    let (_, initial) = seed(&mut source, &vec![0x5a; 65_537])?;
    let snapshot = source.export_snapshot()?;
    let decoded = decode_snapshot(&snapshot)?;
    assert!(decoded
        .stores
        .iter()
        .all(|store| store.flags & STORE_FLAG_VALUE_REVISION == 0));
    assert_eq!(decoded.stores.len(), 1);
    let target_bundle = MemoryBundle::new();
    let mut target = open(&target_bundle)?;
    source.compact_into(&mut target)?;
    let reader = target.begin_tx(TxMode::Readonly)?;
    assert_eq!(revision(&mut target, reader, "rows", b"key")?, initial);
    assert_eq!(revision(&mut target, reader, INDEX, INDEX_KEY)?, initial);
    assert_eq!(
        target.get(reader, "rows", b"key")?,
        Some(vec![0x5a; 65_537])
    );
    target.rollback_tx(reader)?;
    let imported_txid = target.import_snapshot(&snapshot)?;
    let tx = target.begin_tx(TxMode::Readonly)?;
    let imported = revision(&mut target, tx, "rows", b"key")?;
    assert_eq!(imported.epoch, imported_txid);
    assert_ne!(imported, initial);
    assert!(matches!(
        target.get_many_value_states(tx, INDEX, &[INDEX_KEY]),
        Err(EngineError::StoreNotFound(_))
    ));
    target.rollback_tx(tx)?;
    Ok(())
}

#[test]
fn historical_snapshot_raw_magic_body_is_not_mistaken_for_revision_header() -> Result<()> {
    let contents = SnapshotContents {
        source_last_committed_txid: 7,
        schema_version: 0,
        stores: vec![SnapshotStore {
            name: "rows".into(),
            flags: 0,
            entries: vec![SnapshotEntry {
                key: b"key".to_vec(),
                value: b"BDREV001\0\0BDTTL001-raw-user-body".to_vec(),
                expires_at_ms: None,
            }],
        }],
    };
    let bytes = encode_snapshot(&contents)?;
    assert_eq!(decode_snapshot(&bytes)?, contents);
    let mut engine = open(&MemoryBundle::new())?;
    let imported = engine.import_snapshot(&bytes)?;
    let tx = engine.begin_tx(TxMode::Readonly)?;
    assert_eq!(
        engine.get(tx, "rows", b"key")?,
        Some(contents.stores[0].entries[0].value.clone())
    );
    let native = revision(&mut engine, tx, "rows", b"key")?;
    assert_eq!(native.epoch, imported);
    engine.rollback_tx(tx)?;
    Ok(())
}

#[test]
fn upgrading_legacy_envelope_preserves_unknown_rows_without_forging_revisions() -> Result<()> {
    let mut engine = open(&MemoryBundle::new())?;
    let setup = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(setup, "rows")?;
    let rw = engine.txns.get_mut(&setup).unwrap().readwrite_mut()?;
    let stage = rw.stores.get_mut("rows").unwrap();
    stage.flags = STORE_FLAG_VALUE_ENVELOPE_V1;
    stage.mutations.insert(
        b"old".to_vec(),
        MutationValue::Put(StoredValue::plain(b"old-body".to_vec())),
    );
    stage.mutations.insert(
        b"key".to_vec(),
        MutationValue::Put(StoredValue::plain(b"before".to_vec())),
    );
    engine.commit_tx(setup)?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    assert_eq!(state(&mut engine, tx, "rows", b"old")?.revision, None);
    engine.put(tx, "rows", b"key", b"after")?;
    let new = revision(&mut engine, tx, "rows", b"key")?;
    engine.commit_tx(tx)?;
    let tx = engine.begin_tx(TxMode::Readonly)?;
    assert_eq!(engine.get(tx, "rows", b"old")?, Some(b"old-body".to_vec()));
    assert_eq!(state(&mut engine, tx, "rows", b"old")?.revision, None);
    assert_eq!(revision(&mut engine, tx, "rows", b"key")?, new);
    let meta = engine.catalog.get("rows").unwrap();
    let raw = crate::btree::lookup(&mut engine.pager, meta.store_root_page_id, b"old")?.unwrap();
    assert_eq!(decode_value_envelope(meta.flags, &raw)?.1, None);
    engine.rollback_tx(tx)?;
    Ok(())
}

#[test]
fn failed_indexed_row_keeps_primary_prefix_without_matching_stale_index_proof() -> Result<()> {
    let mut engine = open(&MemoryBundle::new())?;
    seed(&mut engine, b"original")?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    let report = engine.put_many_indexed_report(
        tx,
        "rows",
        &[
            (b"key".as_slice(), b"first".as_slice()),
            (b"key".as_slice(), b"second".as_slice()),
        ],
        &[
            vec![IndexOpRef::Put {
                store: INDEX,
                key: INDEX_KEY,
            }],
            vec![IndexOpRef::Put {
                store: INDEX,
                key: b"value\0\0other\0\0",
            }],
        ],
        None,
    );
    assert_eq!(report.completed, vec![true]);
    assert!(matches!(report.error, Some(EngineError::Serialization(_))));
    assert_eq!(engine.get(tx, "rows", b"key")?, Some(b"second".to_vec()));
    let primary = revision(&mut engine, tx, "rows", b"key")?;
    let earlier_index = revision(&mut engine, tx, INDEX, INDEX_KEY)?;
    assert_eq!(primary.epoch, earlier_index.epoch);
    assert!(primary.ordinal > earlier_index.ordinal);
    engine.put_index_entry_checked(
        tx,
        "rows",
        INDEX,
        b"key",
        INDEX_KEY,
        primary.epoch,
        primary.ordinal,
    )?;
    assert_eq!(revision(&mut engine, tx, INDEX, INDEX_KEY)?, primary);
    engine.commit_tx(tx)?;
    Ok(())
}

#[test]
fn legacy_indexed_targets_mint_unique_versions_instead_of_copying_user_row_revision() -> Result<()>
{
    let mut engine = open(&MemoryBundle::new())?;
    seed(&mut engine, b"old")?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.put_many_indexed(
        tx,
        "rows",
        &[(b"key".as_slice(), b"new".as_slice())],
        &[vec![IndexOpRef::Put {
            store: "rows",
            key: b"other",
        }]],
        None,
    )?;
    assert_ne!(
        revision(&mut engine, tx, "rows", b"other")?,
        revision(&mut engine, tx, "rows", b"key")?
    );
    engine.rollback_tx(tx)?;
    Ok(())
}

#[test]
fn durable_failed_publication_recovers_both_versions_before_the_next_epoch() -> Result<()> {
    for failpoint in [
        Failpoint::AfterWalFlush,
        Failpoint::AfterMainFlush,
        Failpoint::BeforeSuperblockFlush,
    ] {
        let bundle = MemoryBundle::new();
        let mut engine = open(&bundle)?;
        seed(&mut engine, &vec![0x47; 65_537])?;
        engine.checkpoint()?;
        let tx = engine.begin_tx(TxMode::Readwrite)?;
        engine.put_many_indexed(
            tx,
            "rows",
            &[(b"key".as_slice(), vec![0x52; 65_537])],
            &[vec![IndexOpRef::Put {
                store: INDEX,
                key: INDEX_KEY,
            }]],
            None,
        )?;
        let durable_revision = revision(&mut engine, tx, "rows", b"key")?;
        engine.set_failpoint(Some(failpoint));
        assert!(matches!(
            engine.commit_tx(tx),
            Err(EngineError::InjectedFailure(_))
        ));
        assert!(matches!(
            engine.health(),
            EngineHealth::RecoveryRequired { .. }
        ));
        engine.abandon()?;
        let mut reopened =
            Engine::open(NAME, bundle.crash_recovered_files(), OpenConfig::default())?;
        let tx = reopened.begin_tx(TxMode::Readwrite)?;
        assert_eq!(
            revision(&mut reopened, tx, "rows", b"key")?,
            durable_revision
        );
        assert_eq!(
            revision(&mut reopened, tx, INDEX, INDEX_KEY)?,
            durable_revision
        );
        assert_eq!(reopened.get(tx, "rows", b"key")?, Some(vec![0x52; 65_537]));
        reopened.put(tx, "rows", b"key", b"after-recovery")?;
        assert!(revision(&mut reopened, tx, "rows", b"key")?.epoch > durable_revision.epoch);
        reopened.commit_tx(tx)?;
    }
    Ok(())
}

#[test]
fn rollback_and_stale_writers_do_not_advance_commit_ids_or_feed_retention() -> Result<()> {
    for stale in [false, true] {
        let bundle = MemoryBundle::new();
        let mut engine = open(&bundle)?;
        let setup = engine.begin_tx(TxMode::Readwrite)?;
        engine.create_store(setup, "rows")?;
        engine.set_change_feed_policy(
            setup,
            ChangeFeedPolicy {
                enabled: true,
                retain_txids: Some(2),
            },
        )?;
        engine.put(setup, "rows", b"first", b"first-body")?;
        assert_eq!(engine.commit_tx(setup)?, 1);
        let abandoned = engine.begin_tx(TxMode::Readwrite)?;
        let writer = engine.begin_tx(TxMode::Readwrite)?;
        engine.put(abandoned, "rows", b"unpublished", b"unpublished-body")?;
        let abandoned_revision = revision(&mut engine, abandoned, "rows", b"unpublished")?;
        engine.put(writer, "rows", b"second", b"second-body")?;
        let committed_revision = revision(&mut engine, writer, "rows", b"second")?;
        assert_eq!(abandoned_revision.epoch, committed_revision.epoch);
        assert_ne!(abandoned_revision.ordinal, committed_revision.ordinal);
        let before_commit = bundle.wal.durable_snapshot().unwrap();
        if !stale {
            engine.rollback_tx(abandoned)?;
            assert_eq!(bundle.wal.durable_snapshot().unwrap(), before_commit);
        }
        assert_eq!(engine.commit_tx(writer)?, 2);
        let durable_wal = bundle.wal.durable_snapshot().unwrap();
        if stale {
            assert!(matches!(
                engine.commit_tx(abandoned),
                Err(EngineError::TransactionConflict { .. })
            ));
            assert_eq!(bundle.wal.durable_snapshot().unwrap(), durable_wal);
        }
        let feed = engine.changes_since(0, ChangeFeedOptions::default())?;
        assert_eq!(
            feed.changes
                .iter()
                .map(|record| (record.tx_id, record.key.as_slice()))
                .collect::<Vec<_>>(),
            vec![(1, b"first".as_slice()), (2, b"second".as_slice())]
        );
        assert_eq!(engine.change_feed_floor_txid, 0);
        let reader = engine.begin_tx(TxMode::Readonly)?;
        assert!(!engine.has(reader, "rows", b"unpublished")?);
        engine.rollback_tx(reader)?;
        let retry = engine.begin_tx(TxMode::Readwrite)?;
        engine.put(retry, "rows", b"third", b"third-body")?;
        assert_eq!(engine.commit_tx(retry)?, 3);
        assert_eq!(engine.change_feed_floor_txid, 1);
        let feed = engine.changes_since(1, ChangeFeedOptions::default())?;
        assert_eq!(
            feed.changes
                .iter()
                .map(|record| (record.tx_id, record.key.as_slice()))
                .collect::<Vec<_>>(),
            vec![(2, b"second".as_slice()), (3, b"third".as_slice())]
        );
    }
    Ok(())
}
