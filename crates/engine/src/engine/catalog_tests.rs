use super::*;
use crate::btree::{load_all_entries, read_catalog};
use crate::layout::page_offset;
use crate::storage::memory::{MemoryBackend, MemoryBundle};
use crate::wal::scan_wal_index;

fn store_name(index: usize) -> String {
    format!("store-{index:08}")
}

fn store_meta(created_txid: u64) -> StoreMetadata {
    StoreMetadata {
        store_root_page_id: 0,
        created_txid,
        flags: 0,
    }
}

fn seeded_engine(stores: usize, feed: bool) -> Result<(MemoryBundle, Engine<MemoryBackend>)> {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open(
        "catalog-tests",
        bundle.files(),
        OpenConfig {
            checkpoint_wal_bytes: u64::MAX,
            checkpoint_dirty_pages: usize::MAX,
            ..OpenConfig::default()
        },
    )?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.set_schema_version(tx, 1)?;
    engine.set_change_feed_policy(
        tx,
        ChangeFeedPolicy {
            enabled: feed,
            ..ChangeFeedPolicy::default()
        },
    )?;
    for index in 0..stores {
        let name = store_name(index);
        engine.create_store(tx, &name)?;
        if index < 2 {
            engine.put(tx, &name, b"key", b"before")?;
        }
    }
    engine.commit_tx(tx)?;
    engine.checkpoint()?;
    Ok((bundle, engine))
}

fn assert_current_catalog(engine: &mut Engine<MemoryBackend>) -> Result<()> {
    let state = read_catalog(&mut engine.pager, engine.superblock.catalog_root_page_id)?;
    assert_eq!(&state.stores, engine.catalog.as_ref());
    assert_eq!(state.schema_version, engine.schema_version);
    // Open can synthesize a legacy floor without rewriting the catalog.
    assert!(
        state.change_feed_floor_txid == engine.change_feed_floor_txid
            || (state.change_feed_floor_txid == 0
                && !state.stores.contains_key(SYSTEM_CHANGELOG_STORE_NAME)
                && engine.change_feed_floor_txid == engine.superblock.last_committed_txid)
    );
    assert_eq!(state.change_feed_policy, engine.change_feed_policy);
    Ok(())
}

#[test]
fn begin_and_uncontended_commits_keep_the_same_catalog_allocation() -> Result<()> {
    let (_, mut engine) = seeded_engine(1024, false)?;
    let original = Arc::as_ptr(&engine.catalog);
    for mode in [TxMode::Readonly, TxMode::Readwrite] {
        let tx = engine.begin_tx(mode)?;
        assert!(Arc::ptr_eq(
            &engine.catalog,
            &engine.txns[&tx].snapshot().catalog,
        ));
        engine.rollback_tx(tx)?;
    }
    for value in 0u64..8 {
        let tx = engine.begin_tx(TxMode::Readwrite)?;
        engine.put(tx, &store_name(0), b"key", &value.to_le_bytes())?;
        engine.commit_tx(tx)?;
        // Retaining the writer's snapshot would force a catalog copy.
        assert_eq!(Arc::as_ptr(&engine.catalog), original);
    }
    assert_current_catalog(&mut engine)
}

#[test]
fn live_snapshots_keep_old_roots_without_forcing_every_later_commit_to_copy() -> Result<()> {
    let (_, mut engine) = seeded_engine(256, true)?;
    let old = engine.begin_tx(TxMode::Readonly)?;
    let old_map = Arc::as_ptr(&engine.catalog);
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.clear_store(tx, &store_name(0))?;
    engine.put(tx, &store_name(0), b"key", b"after")?;
    engine.drop_store(tx, &store_name(1))?;
    engine.create_store(tx, "new-store")?;
    engine.put(tx, "new-store", b"key", b"new")?;
    engine.set_schema_version(tx, 7)?;
    engine.commit_tx(tx)?;
    let current_map = Arc::as_ptr(&engine.catalog);
    assert_ne!(current_map, old_map);
    assert_eq!(engine.txns[&old].snapshot().schema_version, 1);
    assert_eq!(engine.schema_version(), 7);

    let current = engine.begin_tx(TxMode::Readonly)?;
    assert_eq!(
        engine.get(current, "new-store", b"key")?,
        Some(b"new".to_vec())
    );
    assert!(matches!(
        engine.get(current, &store_name(1), b"key"),
        Err(EngineError::StoreNotFound(_))
    ));
    engine.rollback_tx(current)?;

    for value in 0u64..24 {
        let tx = engine.begin_tx(TxMode::Readwrite)?;
        engine.put(tx, &store_name(0), b"key", &value.to_le_bytes())?;
        engine.commit_tx(tx)?;
        engine.checkpoint()?;
        assert_eq!(Arc::as_ptr(&engine.catalog), current_map);
        assert_eq!(
            engine.get(old, &store_name(0), b"key")?,
            Some(b"before".to_vec())
        );
        assert_eq!(
            engine.get(old, &store_name(1), b"key")?,
            Some(b"before".to_vec())
        );
        assert!(matches!(
            engine.get(old, "new-store", b"key"),
            Err(EngineError::StoreNotFound(_))
        ));
    }
    engine.rollback_tx(old)?;
    // Closing the old snapshot makes its retired pages available for reuse.
    for value in 24u64..48 {
        let tx = engine.begin_tx(TxMode::Readwrite)?;
        engine.put(tx, &store_name(0), b"key", &value.to_le_bytes())?;
        engine.commit_tx(tx)?;
    }
    assert_current_catalog(&mut engine)
}

#[test]
fn empty_and_scalar_only_commits_do_not_detach_a_shared_store_map() -> Result<()> {
    let (bundle, mut engine) = seeded_engine(128, false)?;
    let reader = engine.begin_tx(TxMode::Readonly)?;
    let pointer = Arc::as_ptr(&engine.catalog);
    let old_root = engine.superblock.catalog_root_page_id;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.set_schema_version(tx, 2)?;
    engine.commit_tx(tx)?;
    assert_ne!(engine.superblock.catalog_root_page_id, old_root);
    assert_eq!(Arc::as_ptr(&engine.catalog), pointer);
    assert_eq!(engine.txns[&reader].snapshot().schema_version, 1);
    let root = engine.superblock.catalog_root_page_id;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(tx, "transient")?;
    engine.drop_store(tx, "transient")?;
    engine.set_schema_version(tx, 2)?;
    engine.commit_tx(tx)?;
    assert_eq!(engine.superblock.catalog_root_page_id, root);
    assert_eq!(Arc::as_ptr(&engine.catalog), pointer);
    let wal = scan_wal_index(&bundle.wal)?;
    let last_commit = wal
        .last()
        .ok_or_else(|| EngineError::Internal("expected committed WAL transaction".into()))?;
    assert_eq!(last_commit.commit.changed_page_count, 0);
    engine.rollback_tx(reader)?;
    assert_current_catalog(&mut engine)
}

#[test]
fn catalog_delta_cancels_net_noops_and_hides_tombstoned_base_entries() {
    let meta = store_meta(1);
    let base = CatalogMap::from([("existing".to_string(), meta.clone())]);
    let mut delta = CatalogDelta::default();
    delta.set(&base, "existing", Some(meta.clone()));
    assert!(delta.is_empty());
    assert_eq!(delta.remove(&base, "existing"), Some(meta.clone()));
    assert_eq!(delta.get(&base, "existing"), None);
    delta.set(&base, "existing", Some(meta.clone()));
    assert!(delta.is_empty());
    delta.set(&base, "new", Some(meta));
    assert!(delta.remove(&base, "new").is_some());
    assert!(delta.is_empty());
}

fn catalog_view(state: &CatalogState, root: u64) -> CommittedView<'_> {
    CommittedView {
        catalog: &state.stores,
        catalog_root_page_id: root,
        schema_version: state.schema_version,
        change_feed_floor_txid: state.change_feed_floor_txid,
        change_feed_policy: state.change_feed_policy,
        oldest_snapshot_txid: u64::MAX,
    }
}

fn install_tree(pager: &mut Pager<MemoryBackend>, built: BuiltTree) -> Result<u64> {
    let root = built.root_page_id;
    for (id, bytes) in built.page_images {
        pager.stage_page_image(id, bytes)?;
    }
    Ok(root)
}

#[test]
fn incremental_catalog_matches_full_builder_bytes_through_mixed_changes() -> Result<()> {
    let mut expected = CatalogState::default();
    for index in 0..512 {
        expected.stores.insert(store_name(index), store_meta(1));
    }
    let mut pager = Pager::new(MemoryBackend::new(), 8);
    let mut alloc = PageAllocator::new(1);
    let mut root = install_tree(&mut pager, build_catalog_tree(&expected, &mut alloc)?)?;
    for round in 0u64..12 {
        let previous = expected.clone();
        let mut delta = CatalogDelta::default();
        for offset in 0..80 {
            let name = if offset % 7 == 0 {
                format!("é-{offset:04}")
            } else {
                store_name((round as usize * 73 + offset * 11) % 512)
            };
            if offset % 3 == 0 {
                delta.remove(&previous.stores, &name);
                expected.stores.remove(&name);
            } else {
                let meta = store_meta(round + 2);
                delta.set(&previous.stores, &name, Some(meta.clone()));
                expected.stores.insert(name, meta);
            }
        }
        expected.schema_version = round + 2;
        expected.change_feed_floor_txid = round * 2;
        expected.change_feed_policy = match round % 3 {
            0 => ChangeFeedPolicy::default(),
            1 => ChangeFeedPolicy {
                enabled: false,
                retain_txids: Some(7),
            },
            _ => ChangeFeedPolicy {
                enabled: true,
                retain_txids: None,
            },
        };
        let built = delta.build_tree(
            &mut pager,
            &catalog_view(&previous, root),
            (
                expected.schema_version,
                expected.change_feed_floor_txid,
                expected.change_feed_policy,
            ),
            &mut alloc,
        )?;
        let next_root = install_tree(&mut pager, built)?;
        assert_eq!(read_catalog(&mut pager, root)?, previous);
        root = next_root;
        assert_eq!(read_catalog(&mut pager, root)?, expected);

        // The full builder independently checks the stored entries, including
        // reserved key order and omission of the default policy record.
        let mut oracle = Pager::new(MemoryBackend::new(), 8);
        let mut oracle_alloc = PageAllocator::new(1);
        let oracle_root = install_tree(
            &mut oracle,
            build_catalog_tree(&expected, &mut oracle_alloc)?,
        )?;
        assert_eq!(
            load_all_entries(&mut pager, root)?,
            load_all_entries(&mut oracle, oracle_root)?,
        );
    }
    Ok(())
}

#[test]
fn small_catalog_update_emits_and_retires_paths_not_the_catalog() -> Result<()> {
    for count in [1, 128, 1024, 8192] {
        let mut state = CatalogState::default();
        for index in 0..count {
            state.stores.insert(store_name(index), store_meta(1));
        }
        let mut pager = Pager::new(MemoryBackend::new(), 8);
        let mut initial_alloc = PageAllocator::new(1);
        let built = build_catalog_tree(&state, &mut initial_alloc)?;
        let full_pages = built.page_images.len();
        let root = install_tree(&mut pager, built)?;
        let mut delta = CatalogDelta::default();
        delta.set(&state.stores, &store_name(0), Some(store_meta(2)));
        let mut alloc = PageAllocator::new(initial_alloc.next_page_id());
        let update = delta.build_tree(
            &mut pager,
            &catalog_view(&state, root),
            (0, 0, ChangeFeedPolicy::default()),
            &mut alloc,
        )?;
        let incremental_pages = update.page_images.len();
        let retired_pages = alloc.freed().len();
        assert!(
            incremental_pages <= 16,
            "{count} stores: {incremental_pages} images"
        );
        assert!(
            retired_pages <= 16,
            "{count} stores: {retired_pages} retirements"
        );
        if count >= 1024 {
            assert!(incremental_pages < full_pages);
            assert!(retired_pages < full_pages);
        }
        println!(
            "catalog_work stores={count} full_images={full_pages} \
             delta_images={incremental_pages} full_retired={full_pages} \
             delta_retired={retired_pages}"
        );
        let next_root = install_tree(&mut pager, update)?;
        assert_eq!(read_catalog(&mut pager, root)?, state);
        state
            .stores
            .get_mut(&store_name(0))
            .ok_or_else(|| EngineError::Internal("expected seeded catalog store".into()))?
            .created_txid = 2;
        assert_eq!(read_catalog(&mut pager, next_root)?, state);
    }
    Ok(())
}

#[test]
fn synthesized_legacy_feed_floor_survives_first_logged_commit_and_reopen() -> Result<()> {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open("legacy-catalog", bundle.files(), OpenConfig::default())?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(tx, "store")?;
    let floor = engine.commit_tx(tx)?; // Store creation produces no change-log record.
    engine.checkpoint()?;
    let recovered = bundle.crash_recovered_files();
    let durable = MemoryBundle {
        manifest: recovered.manifest,
        main: recovered.main,
        wal: recovered.wal,
    };
    let mut engine = Engine::open("legacy-catalog", durable.files(), OpenConfig::default())?;
    assert_eq!(engine.change_feed_floor_txid, floor);
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.put(tx, "store", b"key", b"value")?;
    engine.commit_tx(tx)?;
    let mut reopened = Engine::open(
        "legacy-catalog",
        durable.crash_recovered_files(),
        OpenConfig::default(),
    )?;
    assert_eq!(reopened.change_feed_floor_txid, floor);
    assert!(matches!(
        reopened.changes_since(0, ChangeFeedOptions::default()),
        Err(EngineError::ChangeFeedCompacted(_))
    ));
    assert_eq!(
        reopened
            .changes_since(floor, ChangeFeedOptions::default())?
            .changes
            .len(),
        1
    );
    assert_current_catalog(&mut reopened)
}

#[test]
fn failed_wal_publication_keeps_memory_until_recovery() -> Result<()> {
    let (bundle, mut engine) = seeded_engine(128, true)?;
    let pointer = Arc::as_ptr(&engine.catalog);
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.put(tx, &store_name(0), b"key", b"durable")?;
    engine.drop_store(tx, &store_name(1))?;
    engine.set_schema_version(tx, 9)?;
    engine.set_failpoint(Some(Failpoint::AfterWalFlush));
    assert!(matches!(
        engine.commit_tx(tx),
        Err(EngineError::InjectedFailure(_))
    ));
    assert_eq!(Arc::as_ptr(&engine.catalog), pointer);
    assert!(engine.catalog.contains_key(&store_name(1)));
    assert_eq!(engine.schema_version(), 1);
    assert!(engine.recover()?.pending_committed);
    assert_eq!(engine.schema_version(), 9);
    assert!(!engine.catalog.contains_key(&store_name(1)));
    let read = engine.begin_tx(TxMode::Readonly)?;
    assert_eq!(
        engine.get(read, &store_name(0), b"key")?,
        Some(b"durable".to_vec())
    );
    engine.rollback_tx(read)?;
    assert_current_catalog(&mut engine)?;
    let mut reopened = Engine::open(
        "catalog-tests",
        bundle.crash_recovered_files(),
        OpenConfig::default(),
    )?;
    assert_eq!(reopened.schema_version(), 9);
    assert_current_catalog(&mut reopened)
}

#[test]
fn corrupt_catalog_path_fails_before_wal_or_metadata_publication() -> Result<()> {
    let (mut bundle, mut engine) = seeded_engine(128, false)?;
    let pointer = Arc::as_ptr(&engine.catalog);
    let root = engine.superblock.catalog_root_page_id;
    let original = engine.pager.read_page(root)?;
    let mut damaged = original.clone();
    damaged[0] ^= 1;
    bundle.main.write_at(page_offset(root), &damaged)?;
    engine.pager.discard_cache();
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.put(tx, &store_name(0), b"key", b"after")?;
    assert!(matches!(
        engine.commit_tx(tx),
        Err(EngineError::Corruption(_))
    ));
    assert_eq!(bundle.wal.len()?, 0);
    assert_eq!(Arc::as_ptr(&engine.catalog), pointer);
    assert_eq!(engine.superblock.catalog_root_page_id, root);
    assert_eq!(engine.health(), &EngineHealth::Healthy);
    bundle.main.write_at(page_offset(root), &original)?;
    engine.pager.discard_cache();
    assert_current_catalog(&mut engine)
}

#[test]
fn shared_snapshot_serialization_keeps_the_original_map_shape() -> Result<()> {
    let (_, mut engine) = seeded_engine(4, false)?;
    let tx = engine.begin_tx(TxMode::Readonly)?;
    let snapshot = engine.txns[&tx].snapshot();
    let value = serde_json::to_value(snapshot)
        .map_err(|error| EngineError::Serialization(error.to_string()))?;
    assert_eq!(
        value["catalog"],
        serde_json::to_value(engine.catalog.as_ref())
            .map_err(|error| EngineError::Serialization(error.to_string()))?
    );
    let decoded: Snapshot = serde_json::from_value(value)
        .map_err(|error| EngineError::Serialization(error.to_string()))?;
    assert_eq!(decoded.catalog.as_ref(), engine.catalog.as_ref());
    assert_eq!(decoded.schema_version, snapshot.schema_version);
    assert_eq!(decoded.catalog_root_page_id, snapshot.catalog_root_page_id);
    assert_eq!(decoded.last_committed_txid, snapshot.last_committed_txid);
    engine.rollback_tx(tx)
}

#[test]
fn one_user_write_has_bounded_wal_page_count_with_and_without_change_feed() -> Result<()> {
    for count in [1, 128, 1024, 8192] {
        for feed in [false, true] {
            let (bundle, mut engine) = seeded_engine(count, feed)?;
            let tx = engine.begin_tx(TxMode::Readwrite)?;
            engine.put(tx, &store_name(0), b"key", b"after")?;
            engine.commit_tx(tx)?;
            let transactions = scan_wal_index(&bundle.wal)?;
            assert_eq!(transactions.len(), 1);
            let images = transactions[0].commit.changed_page_count;
            assert!(
                images <= 20,
                "{count} stores, feed={feed}: {images} WAL images"
            );
            assert_current_catalog(&mut engine)?;
            println!(
                "user_write_work stores={count} feed={feed} wal_images={images} wal_bytes={}",
                bundle.wal.len()?,
            );
            let mut recovered = Engine::open(
                "catalog-tests",
                bundle.crash_recovered_files(),
                OpenConfig::default(),
            )?;
            assert_current_catalog(&mut recovered)?;
            let read = recovered.begin_tx(TxMode::Readonly)?;
            assert_eq!(
                recovered.get(read, &store_name(0), b"key")?,
                Some(b"after".to_vec())
            );
            recovered.rollback_tx(read)?;
        }
    }
    Ok(())
}

#[test]
fn policy_changes_and_retention_persist_across_recovery() -> Result<()> {
    let (_, mut engine) = seeded_engine(128, true)?;
    for policy in [
        ChangeFeedPolicy {
            enabled: false,
            retain_txids: Some(2),
        },
        ChangeFeedPolicy {
            enabled: true,
            retain_txids: Some(2),
        },
        ChangeFeedPolicy {
            enabled: true,
            retain_txids: None,
        },
        ChangeFeedPolicy::default(),
    ] {
        let tx = engine.begin_tx(TxMode::Readwrite)?;
        engine.set_change_feed_policy(tx, policy)?;
        engine.put(tx, &store_name(0), b"key", b"after")?;
        let committed = engine.commit_tx(tx)?;
        engine.recover()?;
        assert_eq!(engine.change_feed_policy, policy);
        assert_current_catalog(&mut engine)?;
        assert!(engine
            .changes_since(committed, ChangeFeedOptions::default())?
            .changes
            .is_empty());
        if policy.enabled {
            assert_eq!(
                engine
                    .changes_since(committed - 1, ChangeFeedOptions::default())?
                    .changes
                    .len(),
                1,
            );
        }
    }
    Ok(())
}

#[test]
fn change_log_name_cannot_be_created_and_still_records_user_writes() -> Result<()> {
    let mut engine = Engine::open(
        "reserved-change-log",
        MemoryBundle::new().files(),
        OpenConfig::default(),
    )?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    let error = engine
        .create_store(tx, SYSTEM_CHANGELOG_STORE_NAME)
        .unwrap_err();
    assert_eq!(
        error,
        EngineError::ReservedStoreName(SYSTEM_CHANGELOG_STORE_NAME.into())
    );
    engine.create_store(tx, "docs")?;
    engine.put(tx, "docs", b"key", b"value")?;
    let committed = engine.commit_tx(tx)?;
    let feed = engine.changes_since(committed - 1, ChangeFeedOptions::default())?;
    assert_eq!(feed.changes.len(), 1);
    assert_eq!(feed.changes[0].store, "docs");
    assert!(engine
        .catalog
        .get(SYSTEM_CHANGELOG_STORE_NAME)
        .is_some_and(|meta| store_uses_system_raw_values(meta.flags)));
    Ok(())
}

#[test]
fn legacy_user_store_under_change_log_name_blocks_feed_append_without_wal() -> Result<()> {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open(
        "legacy-change-log-name",
        bundle.files(),
        OpenConfig::default(),
    )?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.set_change_feed_policy(
        tx,
        ChangeFeedPolicy {
            enabled: false,
            retain_txids: None,
        },
    )?;
    engine.create_store(tx, "docs")?;
    engine.put(tx, "docs", b"key", b"original")?;
    engine.commit_tx(tx)?;
    {
        let catalog = Arc::make_mut(&mut engine.catalog);
        catalog.insert(
            SYSTEM_CHANGELOG_STORE_NAME.to_string(),
            StoreMetadata {
                store_root_page_id: 0,
                created_txid: 1,
                flags: store_flags_for_user_store(StoreCompression::None),
            },
        );
    }
    let wal_len = bundle.wal.len()?;
    let root = engine.superblock.catalog_root_page_id;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.set_change_feed_policy(
        tx,
        ChangeFeedPolicy {
            enabled: true,
            retain_txids: None,
        },
    )?;
    engine.put(tx, "docs", b"key", b"replacement")?;
    let error = engine.commit_tx(tx).unwrap_err();
    assert_eq!(
        error,
        EngineError::ReservedStoreName(SYSTEM_CHANGELOG_STORE_NAME.into())
    );
    assert_eq!(engine.health(), &EngineHealth::Healthy);
    assert!(!engine.change_feed_policy.enabled);
    assert_eq!(bundle.wal.len()?, wal_len);
    assert_eq!(engine.superblock.catalog_root_page_id, root);
    assert!(engine
        .catalog
        .get(SYSTEM_CHANGELOG_STORE_NAME)
        .is_some_and(|meta| !store_uses_system_raw_values(meta.flags)));
    let read = engine.begin_tx(TxMode::Readonly)?;
    assert_eq!(
        engine.get(read, "docs", b"key")?,
        Some(b"original".to_vec())
    );
    engine.rollback_tx(read)?;
    Ok(())
}

#[test]
fn compaction_omits_the_system_log_and_keeps_a_same_named_user_store() -> Result<()> {
    let mut source = Engine::open(
        "compact-log-source",
        MemoryBundle::new().files(),
        OpenConfig::default(),
    )?;
    let tx = source.begin_tx(TxMode::Readwrite)?;
    source.create_store(tx, "docs")?;
    source.put(tx, "docs", b"key", b"value")?;
    source.commit_tx(tx)?;
    let mut target = Engine::open(
        "compact-log-target",
        MemoryBundle::new().files(),
        OpenConfig::default(),
    )?;
    source.compact_into(&mut target)?;
    assert!(!target.catalog.contains_key(SYSTEM_CHANGELOG_STORE_NAME));
    let read = target.begin_tx(TxMode::Readonly)?;
    assert_eq!(target.get(read, "docs", b"key")?, Some(b"value".to_vec()));
    target.rollback_tx(read)?;

    let mut legacy = Engine::open(
        "compact-legacy-log-name",
        MemoryBundle::new().files(),
        OpenConfig::default(),
    )?;
    let tx = legacy.begin_tx(TxMode::Readwrite)?;
    legacy.set_change_feed_policy(
        tx,
        ChangeFeedPolicy {
            enabled: false,
            retain_txids: None,
        },
    )?;
    legacy.create_store(tx, "docs")?;
    legacy.put(tx, "docs", b"key", b"kept")?;
    legacy.commit_tx(tx)?;
    let user_flags = store_flags_for_user_store(StoreCompression::None);
    {
        let catalog = Arc::make_mut(&mut legacy.catalog);
        catalog.insert(
            SYSTEM_CHANGELOG_STORE_NAME.to_string(),
            StoreMetadata {
                store_root_page_id: 0,
                created_txid: 1,
                flags: user_flags,
            },
        );
    }
    let mut legacy_target = Engine::open(
        "compact-legacy-log-target",
        MemoryBundle::new().files(),
        OpenConfig::default(),
    )?;
    legacy.compact_into(&mut legacy_target)?;
    assert_eq!(
        legacy_target
            .catalog
            .get(SYSTEM_CHANGELOG_STORE_NAME)
            .map(|meta| meta.flags),
        Some(user_flags)
    );
    let read = legacy_target.begin_tx(TxMode::Readonly)?;
    assert_eq!(
        legacy_target.get(read, "docs", b"key")?,
        Some(b"kept".to_vec())
    );
    legacy_target.rollback_tx(read)?;
    let source_read = legacy.begin_tx(TxMode::Readonly)?;
    assert_eq!(
        legacy.get(source_read, "docs", b"key")?,
        Some(b"kept".to_vec())
    );
    legacy.rollback_tx(source_read)?;
    Ok(())
}
