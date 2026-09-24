use crate::btree::{
    apply_mutations, build_catalog_tree, build_tree_from_sorted, collect_keys_below, free_tree,
    lookup, lookup_prefix, materialize_pending_value, pending_value_prefix, BuiltTree, KvPair,
    Mutation, PageAllocator, PageImages, PendingValue, RangeSpec, SortedTreeBuilder, TreeIter,
};
use crate::bytes::{validate_key, validate_store_name, validate_value};
use crate::catalog::{CatalogMap, CatalogState, ChangeFeedPolicy};
use crate::change_feed::{
    decode_change_record_payload, encode_after_txid_key, encode_change_log_key,
    encode_change_record_payload, is_internal_store_name, normalize_store_filter,
    validate_user_store_name, visible_store_count, visible_store_names, ChangeFeed,
    ChangeFeedOptions, ChangeKind, CHANGELOG_STORE_FLAGS, SYSTEM_CHANGELOG_STORE_NAME,
};
use crate::checksum;
use crate::error::{EngineError, Result};
use crate::layout::{StoreMetadata, SuperblockState};
use crate::pager::Pager;
use crate::recovery::{
    ensure_openable_or_initialize, load_catalog_snapshot, recover_if_needed, write_superblock,
};
use crate::snapshot::{
    collect_snapshot_contents, decode_snapshot, encode_snapshot, SnapshotContents, SnapshotEntry,
    SnapshotStore,
};
use crate::storage::backend::{FileBackend, FileSet};
use crate::time::now_unix_ms;
use crate::txn::{
    BatchOp, BatchOpOutcome, BatchOpRef, MutationValue, ReadwriteTx, Snapshot, StagedStore,
    TransactionState, TxInner,
};
use crate::value::{
    decode_envelope_expiry, store_compression_from_flags, store_flags_for_user_store,
    store_uses_system_raw_values, store_uses_value_envelope, stored_value_expired,
    StoreCompression, StoredValue, STORE_FLAG_COMPRESSION_MASK, STORE_FLAG_VALUE_ENVELOPE_V1,
    VALUE_ENVELOPE_HEADER_SIZE,
};
use crate::wal::{append_transaction, CommitRecord};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ops::Bound;

pub use crate::txn::TxMode;

pub type ScanRange = RangeSpec;

// Commit-time pruning work per transaction; the rest continues on later commits.
const CHANGE_LOG_PRUNE_BATCH: usize = 1024;

struct CommitPlan {
    new_txid: u64,
    final_catalog: CatalogMap,
    final_schema_version: u64,
    final_change_feed_floor_txid: u64,
    final_change_feed_policy: ChangeFeedPolicy,
    page_images: PageImages,
    catalog_root_page_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenConfig {
    pub create_if_missing: bool,
    pub cache_pages: usize,
    /// Checkpoint once this many WAL bytes are durable but not installed.
    pub checkpoint_wal_bytes: u64,
    /// Checkpoint once this many committed pages wait in memory. Dirty pages
    /// are pinned in the cache, so this bounds memory between checkpoints.
    pub checkpoint_dirty_pages: usize,
}

impl Default for OpenConfig {
    fn default() -> Self {
        Self {
            create_if_missing: true,
            cache_pages: 256,
            checkpoint_wal_bytes: 16 * 1024 * 1024,
            checkpoint_dirty_pages: 4096,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Failpoint {
    AfterWalFlush,
    AfterMainFlush,
    BeforeSuperblockFlush,
}

impl Failpoint {
    pub fn as_str(&self) -> &'static str {
        match self {
            Failpoint::AfterWalFlush => "after_wal_flush",
            Failpoint::AfterMainFlush => "after_main_flush",
            Failpoint::BeforeSuperblockFlush => "before_superblock_flush",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "after_wal_flush" => Ok(Failpoint::AfterWalFlush),
            "after_main_flush" => Ok(Failpoint::AfterMainFlush),
            "before_superblock_flush" => Ok(Failpoint::BeforeSuperblockFlush),
            other => Err(EngineError::Internal(format!("unknown failpoint {other}"))),
        }
    }
}

/// Whether the in-memory state may still be trusted.
///
/// Any failure after WAL bytes of a commit start hitting storage leaves the
/// outcome of that commit unknown and the WAL tail possibly torn. From then on
/// the engine refuses work until [`Engine::recover`] rebuilds its state from
/// the files, or it is closed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum EngineHealth {
    Healthy,
    #[serde(rename_all = "camelCase")]
    RecoveryRequired {
        reason: String,
        pending_txid: Option<u64>,
    },
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryReport {
    pub last_committed_txid: u64,
    /// The commit whose outcome was unknown when the engine was poisoned.
    pub pending_txid: Option<u64>,
    /// Whether that commit turned out to be durable.
    pub pending_committed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbStats {
    pub db_name: String,
    pub db_id: u64,
    pub page_size: u32,
    pub catalog_root_page_id: u64,
    pub next_page_id: u64,
    pub last_committed_txid: u64,
    pub last_replayed_wal_offset: u64,
    pub store_count: usize,
    pub manifest_len: u64,
    pub main_len: u64,
    pub wal_len: u64,
    pub active_txns: usize,
    pub write_tx_open: bool,
    pub cache_pages: usize,
    pub dirty_pages: usize,
    pub reusable_pages: usize,
    pub retired_pages: usize,
    pub health: EngineHealth,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VisibleStoreConfig {
    pub name: String,
    pub flags: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchExecutionReport<T> {
    pub completed: Vec<T>,
    pub error: Option<EngineError>,
}

impl<T> BatchExecutionReport<T> {
    pub fn success(completed: Vec<T>) -> Self {
        Self {
            completed,
            error: None,
        }
    }

    pub fn failure(completed: Vec<T>, error: EngineError) -> Self {
        Self {
            completed,
            error: Some(error),
        }
    }

    pub fn into_result(self) -> Result<Vec<T>> {
        match self.error {
            Some(error) => Err(error),
            None => Ok(self.completed),
        }
    }
}

/// Pages retired by commits, reused once no open snapshot can reach them.
///
/// A page retired by commit T is reachable only from trees older than T, so
/// it becomes reusable when every open snapshot is at T or newer. The pool
/// lives in memory; pages still retired at close are reclaimed by compaction.
#[derive(Debug, Default)]
struct FreePagePool {
    ready: Vec<u64>,
    retired: VecDeque<(u64, Vec<u64>)>,
}

impl FreePagePool {
    fn promote(&mut self, oldest_snapshot_txid: u64) {
        while let Some((retired_at, _)) = self.retired.front() {
            if *retired_at > oldest_snapshot_txid {
                break;
            }
            if let Some((_, pages)) = self.retired.pop_front() {
                self.ready.extend(pages);
            }
        }
    }

    fn take_ready(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.ready)
    }

    fn restore(&mut self, pages: Vec<u64>) {
        self.ready.extend(pages);
    }

    fn retire(&mut self, txid: u64, pages: Vec<u64>) {
        if !pages.is_empty() {
            self.retired.push_back((txid, pages));
        }
    }

    fn clear(&mut self) {
        self.ready.clear();
        self.retired.clear();
    }

    fn retired_len(&self) -> usize {
        self.retired.iter().map(|(_, pages)| pages.len()).sum()
    }
}

struct LoadedState {
    superblock: SuperblockState,
    catalog: CatalogState,
}

pub struct Engine<B: FileBackend> {
    db_name: String,
    manifest: B,
    pager: Pager<B>,
    wal: B,
    superblock: SuperblockState,
    // Highest txid flushed to the WAL. Checkpoint must not truncate past a
    // txid memory has not applied.
    wal_durable_txid: u64,
    schema_version: u64,
    catalog: CatalogMap,
    change_feed_floor_txid: u64,
    change_feed_policy: ChangeFeedPolicy,
    next_tx_id: u64,
    next_commit_txid: u64,
    txns: HashMap<u64, TransactionState>,
    write_tx_open: Option<u64>,
    next_failpoint: Option<Failpoint>,
    cache_pages: usize,
    checkpoint_wal_bytes: u64,
    checkpoint_dirty_pages: usize,
    health: EngineHealth,
    free_pages: FreePagePool,
}

impl<B: FileBackend> std::fmt::Debug for Engine<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("db_name", &self.db_name)
            .field("superblock", &self.superblock)
            .field("wal_durable_txid", &self.wal_durable_txid)
            .field("schema_version", &self.schema_version)
            .field("catalog", &self.catalog)
            .field("change_feed_floor_txid", &self.change_feed_floor_txid)
            .field("change_feed_policy", &self.change_feed_policy)
            .field("next_tx_id", &self.next_tx_id)
            .field("next_commit_txid", &self.next_commit_txid)
            .field("txns", &self.txns)
            .field("write_tx_open", &self.write_tx_open)
            .field("next_failpoint", &self.next_failpoint)
            .field("cache_pages", &self.cache_pages)
            .field("health", &self.health)
            .finish_non_exhaustive()
    }
}

fn load_state<B: FileBackend>(
    manifest: &mut B,
    pager: &mut Pager<B>,
    wal: &mut B,
    db_name: &str,
    create_if_missing: bool,
) -> Result<LoadedState> {
    let db_id = db_id_from_name(db_name);
    let superblock = ensure_openable_or_initialize(manifest, pager, wal, db_id, create_if_missing)?;
    validate_db_identity(db_name, db_id, &superblock)?;
    let superblock = recover_if_needed(manifest, pager, wal, &superblock)?;
    validate_db_identity(db_name, db_id, &superblock)?;
    let catalog = load_catalog_snapshot(pager, &superblock)?;
    Ok(LoadedState {
        superblock,
        catalog,
    })
}

impl<B: FileBackend> Engine<B> {
    pub fn open(db_name: &str, files: FileSet<B>, config: OpenConfig) -> Result<Self> {
        let cache_pages = config.cache_pages.max(1);
        let mut manifest = files.manifest;
        let mut pager = Pager::new(files.main, cache_pages);
        let mut wal = files.wal;
        let loaded = load_state(
            &mut manifest,
            &mut pager,
            &mut wal,
            db_name,
            config.create_if_missing,
        )?;

        let mut engine = Self {
            db_name: db_name.to_string(),
            manifest,
            pager,
            wal,
            superblock: loaded.superblock.clone(),
            wal_durable_txid: 0,
            schema_version: 0,
            catalog: CatalogMap::new(),
            change_feed_floor_txid: 0,
            change_feed_policy: ChangeFeedPolicy::default(),
            next_tx_id: 1,
            next_commit_txid: 1,
            txns: HashMap::new(),
            write_tx_open: None,
            next_failpoint: None,
            cache_pages,
            checkpoint_wal_bytes: config.checkpoint_wal_bytes.max(1),
            checkpoint_dirty_pages: config.checkpoint_dirty_pages.max(1),
            health: EngineHealth::Healthy,
            free_pages: FreePagePool::default(),
        };
        engine.install_loaded(loaded);
        Ok(engine)
    }

    fn install_loaded(&mut self, loaded: LoadedState) {
        let LoadedState {
            superblock,
            catalog,
        } = loaded;
        // Databases written before the change log existed have no history to serve.
        let change_feed_floor_txid = if catalog.change_feed_floor_txid == 0
            && !catalog.stores.contains_key(SYSTEM_CHANGELOG_STORE_NAME)
            && superblock.last_committed_txid > 0
        {
            superblock.last_committed_txid
        } else {
            catalog.change_feed_floor_txid
        };
        self.wal_durable_txid = superblock.last_committed_txid;
        self.next_commit_txid = superblock.last_committed_txid.saturating_add(1);
        self.schema_version = catalog.schema_version;
        self.catalog = catalog.stores;
        self.change_feed_floor_txid = change_feed_floor_txid;
        self.change_feed_policy = catalog.change_feed_policy;
        self.superblock = superblock;
    }

    pub fn health(&self) -> &EngineHealth {
        &self.health
    }

    pub fn needs_recovery(&self) -> bool {
        matches!(self.health, EngineHealth::RecoveryRequired { .. })
    }

    fn ensure_healthy(&self) -> Result<()> {
        match &self.health {
            EngineHealth::Healthy => Ok(()),
            EngineHealth::RecoveryRequired { reason, .. } => {
                Err(EngineError::RecoveryRequired(reason.clone()))
            }
            EngineHealth::Closed => Err(EngineError::Closed),
        }
    }

    fn poison(&mut self, reason: String, pending_txid: Option<u64>) {
        if self.health == EngineHealth::Healthy {
            self.health = EngineHealth::RecoveryRequired {
                reason,
                pending_txid,
            };
        }
    }

    /// Throws away all in-memory state and reloads it from the files: selects
    /// the superblock, replays the durable WAL, reloads the catalog.
    ///
    /// Reports whether the commit that poisoned the engine made it to disk, so
    /// callers can turn an ambiguous commit into a definite answer.
    pub fn recover(&mut self) -> Result<RecoveryReport> {
        let pending_txid = match &self.health {
            EngineHealth::Closed => return Err(EngineError::Closed),
            EngineHealth::Healthy => None,
            EngineHealth::RecoveryRequired { pending_txid, .. } => *pending_txid,
        };
        self.txns.clear();
        self.write_tx_open = None;
        self.next_failpoint = None;
        self.free_pages.clear();
        self.pager.discard_cache();
        // The failed operation may have left written-but-unflushed bytes, and
        // load_state reads them as the truth: a superblock whose flush tore
        // would let it truncate WAL records the durable superblock still
        // needs. Make the files durable first so every state recovery reports
        // survives a crash. Main goes before the manifest, which may point
        // into it; a torn WAL tail made durable is dropped by its checksum.
        self.pager.flush()?;
        self.manifest.flush()?;
        self.wal.flush()?;
        let loaded = load_state(
            &mut self.manifest,
            &mut self.pager,
            &mut self.wal,
            &self.db_name,
            false,
        )?;
        self.install_loaded(loaded);
        self.health = EngineHealth::Healthy;
        let last_committed_txid = self.superblock.last_committed_txid;
        Ok(RecoveryReport {
            last_committed_txid,
            pending_txid,
            pending_committed: pending_txid.is_some_and(|txid| last_committed_txid >= txid),
        })
    }

    /// Installs committed pages and closes the files. A poisoned engine is
    /// closed without a checkpoint: its memory is not trusted, and the WAL
    /// already holds everything that was durable.
    pub fn close(&mut self) -> Result<()> {
        if self.health == EngineHealth::Closed {
            return Ok(());
        }
        self.txns.clear();
        self.write_tx_open = None;
        let checkpoint = if self.health == EngineHealth::Healthy {
            self.checkpoint_inner()
        } else {
            Ok(())
        };
        let closed = self.close_files();
        self.health = EngineHealth::Closed;
        checkpoint.and(closed)
    }

    /// Closes the files without installing anything. Durable commits stay in
    /// the WAL and are replayed by the next open.
    pub fn abandon(&mut self) -> Result<()> {
        if self.health == EngineHealth::Closed {
            return Ok(());
        }
        self.txns.clear();
        self.write_tx_open = None;
        let closed = self.close_files();
        self.health = EngineHealth::Closed;
        closed
    }

    fn close_files(&mut self) -> Result<()> {
        let pager = self.pager.close();
        let wal = self.wal.close();
        let manifest = self.manifest.close();
        pager.and(wal).and(manifest)
    }

    /// Writes dirty pages to the main file and publishes the superblock.
    ///
    /// Commit durability is the WAL flush. This is the later install step: one
    /// main-file flush for every page staged since the previous checkpoint.
    /// The WAL is truncated only when memory has applied every flushed commit.
    pub fn checkpoint(&mut self) -> Result<()> {
        self.ensure_healthy()?;
        self.checkpoint_inner().inspect_err(|err| {
            self.poison(format!("checkpoint failed: {err}"), None);
        })
    }

    fn checkpoint_inner(&mut self) -> Result<()> {
        if !self.pager.has_dirty() {
            return Ok(());
        }
        self.pager.write_back_dirty()?;
        self.pager.flush()?;
        if self.consume_failpoint(Failpoint::AfterMainFlush) {
            return Err(EngineError::InjectedFailure(
                Failpoint::AfterMainFlush.as_str().into(),
            ));
        }
        if self.consume_failpoint(Failpoint::BeforeSuperblockFlush) {
            return Err(EngineError::InjectedFailure(
                Failpoint::BeforeSuperblockFlush.as_str().into(),
            ));
        }

        let wal_len = self.wal.len()?;
        let generation = self
            .superblock
            .generation
            .checked_add(1)
            .ok_or_else(|| EngineError::Internal("superblock generation overflow".into()))?;
        let published = SuperblockState {
            generation,
            db_id: self.superblock.db_id,
            page_size: self.superblock.page_size,
            catalog_root_page_id: self.superblock.catalog_root_page_id,
            next_page_id: self.superblock.next_page_id,
            last_committed_txid: self.superblock.last_committed_txid,
            last_replayed_wal_offset: wal_len,
            active_slot: if self.superblock.active_slot == 0 {
                1
            } else {
                0
            },
        };
        write_superblock(&mut self.manifest, &published)?;
        self.superblock.generation = published.generation;
        self.superblock.active_slot = published.active_slot;
        self.superblock.last_replayed_wal_offset = published.last_replayed_wal_offset;

        if self.wal_durable_txid == self.superblock.last_committed_txid {
            self.wal.truncate(0)?;
            self.wal.flush()?;
        }
        self.pager.mark_dirty_clean();
        Ok(())
    }

    pub fn begin_tx(&mut self, mode: TxMode) -> Result<u64> {
        self.ensure_healthy()?;
        if mode == TxMode::Readwrite && self.write_tx_open.is_some() {
            return Err(EngineError::WriteTransactionAlreadyOpen);
        }
        let snapshot = Snapshot::new(
            self.schema_version,
            self.superblock.catalog_root_page_id,
            self.superblock.last_committed_txid,
            &self.catalog,
        );
        let tx_id = self.next_tx_id;
        self.next_tx_id = self
            .next_tx_id
            .checked_add(1)
            .ok_or_else(|| EngineError::Internal("transaction id overflow".into()))?;
        let tx = match mode {
            TxMode::Readonly => TransactionState::new_readonly(tx_id, snapshot),
            TxMode::Readwrite => {
                self.write_tx_open = Some(tx_id);
                TransactionState::new_readwrite(tx_id, snapshot)
            }
        };
        self.txns.insert(tx_id, tx);
        Ok(tx_id)
    }

    /// Always allowed, including on a poisoned engine, so callers can release
    /// their handles before recovering.
    pub fn rollback_tx(&mut self, tx_id: u64) -> Result<()> {
        let mut tx = self
            .txns
            .remove(&tx_id)
            .ok_or(EngineError::TransactionClosed)?;
        tx.ensure_open()?;
        tx.closed = true;
        if self.write_tx_open == Some(tx_id) {
            self.write_tx_open = None;
        }
        Ok(())
    }

    pub fn commit_tx(&mut self, tx_id: u64) -> Result<u64> {
        let tx = self.take_tx(tx_id)?;
        tx.ensure_open()?;
        if tx.mode == TxMode::Readonly {
            self.put_tx(tx);
            return Err(EngineError::ReadonlyTransaction);
        }

        let TxInner::Readwrite(write_tx) = tx.inner else {
            return Err(EngineError::ReadonlyTransaction);
        };
        let result = self.commit_staged(
            write_tx.stores,
            write_tx.staged_schema_version,
            write_tx.staged_change_feed_policy,
        );
        if self.write_tx_open == Some(tx_id) {
            self.write_tx_open = None;
        }
        result
    }

    pub fn schema_version(&self) -> u64 {
        self.schema_version
    }

    pub fn set_schema_version(&mut self, tx_id: u64, version: u64) -> Result<()> {
        let mut tx = self.take_tx(tx_id)?;
        let result = (|| {
            let rw = tx.readwrite_mut()?;
            rw.staged_schema_version = Some(version);
            Ok(())
        })();
        self.put_tx(tx);
        result
    }

    pub fn change_feed_policy(&self) -> ChangeFeedPolicy {
        self.change_feed_policy
    }

    pub fn set_change_feed_policy(&mut self, tx_id: u64, policy: ChangeFeedPolicy) -> Result<()> {
        if policy.retain_txids == Some(0) {
            return Err(EngineError::InvalidRange(
                "change feed retention must keep at least one transaction".into(),
            ));
        }
        let mut tx = self.take_tx(tx_id)?;
        let result = (|| {
            let rw = tx.readwrite_mut()?;
            rw.staged_change_feed_policy = Some(policy);
            Ok(())
        })();
        self.put_tx(tx);
        result
    }

    pub fn create_store(&mut self, tx_id: u64, name: &str) -> Result<()> {
        self.create_store_with_compression(tx_id, name, StoreCompression::None)
    }

    pub fn create_store_with_compression(
        &mut self,
        tx_id: u64,
        name: &str,
        compression: StoreCompression,
    ) -> Result<()> {
        validate_store_name(name)?;
        if name.as_bytes().first() == Some(&0xff) {
            return Err(EngineError::ReservedStoreName(name.into()));
        }
        let mut tx = self.take_tx(tx_id)?;
        let result = (|| {
            let rw = tx.readwrite_mut()?;
            if let Some(stage) = rw.stores.get(name) {
                if !stage.dropped {
                    return Err(EngineError::StoreExists(name.into()));
                }
            }
            if rw.snapshot.catalog.contains_key(name) {
                return Err(EngineError::StoreExists(name.into()));
            }
            rw.stores.insert(
                name.to_string(),
                StagedStore::created(store_flags_for_user_store(compression)),
            );
            Ok(())
        })();
        self.put_tx(tx);
        result
    }

    pub fn drop_store(&mut self, tx_id: u64, name: &str) -> Result<()> {
        validate_store_name(name)?;
        let mut tx = self.take_tx(tx_id)?;
        let result = (|| {
            let rw = tx.readwrite_mut()?;
            if let Some(stage) = rw.stores.get_mut(name) {
                if stage.dropped {
                    return Err(EngineError::StoreNotFound(name.into()));
                }
                stage.dropped = true;
                stage.mutations.clear();
                return Ok(());
            }
            let base_meta = rw
                .snapshot
                .catalog
                .get(name)
                .cloned()
                .ok_or_else(|| EngineError::StoreNotFound(name.into()))?;
            rw.stores
                .insert(name.to_string(), StagedStore::dropped_existing(base_meta));
            Ok(())
        })();
        self.put_tx(tx);
        result
    }

    pub fn clear_store(&mut self, tx_id: u64, name: &str) -> Result<()> {
        validate_store_name(name)?;
        let mut tx = self.take_tx(tx_id)?;
        let result = (|| {
            let rw = tx.readwrite_mut()?;
            let stage = ensure_stage_for_write(rw, name)?;
            if stage.dropped {
                return Err(EngineError::StoreNotFound(name.into()));
            }
            stage.mutations.clear();
            stage.cleared = true;
            Ok(())
        })();
        self.put_tx(tx);
        result
    }

    pub fn get(&mut self, tx_id: u64, store: &str, key: &[u8]) -> Result<Option<Vec<u8>>> {
        validate_store_name(store)?;
        validate_key(key)?;
        let now_ms = now_unix_ms()?;
        let mut tx = self.take_tx(tx_id)?;
        let result = match &mut tx.inner {
            TxInner::Readonly(readonly) => match readonly.snapshot.catalog.get(store) {
                Some(meta) => get_committed_visible(
                    &mut self.pager,
                    meta.store_root_page_id,
                    meta.flags,
                    key,
                    now_ms,
                ),
                None => Err(EngineError::StoreNotFound(store.into())),
            },
            TxInner::Readwrite(rw) => get_with_staged(&mut self.pager, rw, store, key, now_ms),
        };
        self.put_tx(tx);
        result
    }

    /// Existence check that never reads a value beyond its TTL header.
    pub fn has(&mut self, tx_id: u64, store: &str, key: &[u8]) -> Result<bool> {
        validate_store_name(store)?;
        validate_key(key)?;
        let now_ms = now_unix_ms()?;
        let mut tx = self.take_tx(tx_id)?;
        let result = match &mut tx.inner {
            TxInner::Readonly(readonly) => match readonly.snapshot.catalog.get(store) {
                Some(meta) => exists_committed_visible(
                    &mut self.pager,
                    meta.store_root_page_id,
                    meta.flags,
                    key,
                    now_ms,
                ),
                None => Err(EngineError::StoreNotFound(store.into())),
            },
            TxInner::Readwrite(rw) => exists_with_staged(&mut self.pager, rw, store, key, now_ms),
        };
        self.put_tx(tx);
        result
    }

    pub fn get_many<K: AsRef<[u8]>>(
        &mut self,
        tx_id: u64,
        store: &str,
        keys: &[K],
    ) -> Result<Vec<Option<Vec<u8>>>> {
        validate_store_name(store)?;
        for key in keys {
            validate_key(key.as_ref())?;
        }
        let now_ms = now_unix_ms()?;
        let mut tx = self.take_tx(tx_id)?;
        // Sorted lookups walk neighbouring leaves back to back and hit the cache.
        let mut order: Vec<usize> = (0..keys.len()).collect();
        order.sort_by(|left, right| keys[*left].as_ref().cmp(keys[*right].as_ref()));
        let result = (|| {
            let mut values = vec![None; keys.len()];
            match &mut tx.inner {
                TxInner::Readonly(readonly) => {
                    let meta = readonly
                        .snapshot
                        .catalog
                        .get(store)
                        .ok_or_else(|| EngineError::StoreNotFound(store.into()))?;
                    for index in order {
                        values[index] = get_committed_visible(
                            &mut self.pager,
                            meta.store_root_page_id,
                            meta.flags,
                            keys[index].as_ref(),
                            now_ms,
                        )?;
                    }
                }
                TxInner::Readwrite(rw) => {
                    ensure_readwrite_store_visible(rw, store)?;
                    for index in order {
                        values[index] = get_with_staged(
                            &mut self.pager,
                            rw,
                            store,
                            keys[index].as_ref(),
                            now_ms,
                        )?;
                    }
                }
            }
            Ok(values)
        })();
        self.put_tx(tx);
        result
    }

    pub fn put(&mut self, tx_id: u64, store: &str, key: &[u8], value: &[u8]) -> Result<()> {
        self.put_with_ttl(tx_id, store, key, value, None)
    }

    pub fn put_with_ttl(
        &mut self,
        tx_id: u64,
        store: &str,
        key: &[u8],
        value: &[u8],
        ttl_ms: Option<u64>,
    ) -> Result<()> {
        self.put_reporting_baseline(tx_id, store, key, value, ttl_ms)
            .map(|_| ())
    }

    /// Like [`Engine::put_with_ttl`], but reports whether a live value existed
    /// for `key` before this write. The answer comes from key metadata, so the
    /// previous value is never materialized.
    pub fn put_reporting_baseline(
        &mut self,
        tx_id: u64,
        store: &str,
        key: &[u8],
        value: &[u8],
        ttl_ms: Option<u64>,
    ) -> Result<bool> {
        validate_store_name(store)?;
        validate_key(key)?;
        validate_value(value)?;
        let operation_now_ms = now_unix_ms()?;
        let expires_at_ms = absolute_expiry_from_ttl_at(ttl_ms, operation_now_ms)?;
        let mut tx = self.take_tx(tx_id)?;
        let result = tx.readwrite_mut().and_then(|rw| {
            put_with_staged_at(
                &mut self.pager,
                rw,
                store,
                key,
                StoredValue::with_expiry(value.to_vec(), expires_at_ms),
                operation_now_ms,
            )
        });
        self.put_tx(tx);
        result
    }

    pub fn put_many_report<K: AsRef<[u8]>, V: AsRef<[u8]>>(
        &mut self,
        tx_id: u64,
        store: &str,
        entries: &[(K, V)],
    ) -> BatchExecutionReport<bool> {
        self.put_many_with_ttl_report(tx_id, store, entries, None)
    }

    pub fn put_many_with_ttl_report<K: AsRef<[u8]>, V: AsRef<[u8]>>(
        &mut self,
        tx_id: u64,
        store: &str,
        entries: &[(K, V)],
        ttl_ms: Option<u64>,
    ) -> BatchExecutionReport<bool> {
        if let Err(error) = validate_user_store_name(store) {
            return BatchExecutionReport::failure(Vec::new(), error);
        }
        let operation_now_ms = match now_unix_ms() {
            Ok(now_ms) => now_ms,
            Err(error) => return BatchExecutionReport::failure(Vec::new(), error),
        };
        let expires_at_ms = match absolute_expiry_from_ttl_at(ttl_ms, operation_now_ms) {
            Ok(expires_at_ms) => expires_at_ms,
            Err(error) => return BatchExecutionReport::failure(Vec::new(), error),
        };
        let mut tx = match self.take_visible_readwrite_tx(tx_id, store) {
            Ok(tx) => tx,
            Err(error) => return BatchExecutionReport::failure(Vec::new(), error),
        };

        let mut completed = Vec::with_capacity(entries.len());
        for (key, value) in entries {
            let (key, value) = (key.as_ref(), value.as_ref());
            let result = validate_key(key)
                .and_then(|_| validate_value(value))
                .and_then(|_| tx.readwrite_mut())
                .and_then(|rw| {
                    put_with_staged_at(
                        &mut self.pager,
                        rw,
                        store,
                        key,
                        StoredValue::with_expiry(value.to_vec(), expires_at_ms),
                        operation_now_ms,
                    )
                });
            match result {
                Ok(baseline_exists) => completed.push(baseline_exists),
                Err(error) => return self.batch_failure(tx, completed, error),
            }
        }

        self.put_tx(tx);
        BatchExecutionReport::success(completed)
    }

    pub fn put_many<K: AsRef<[u8]>, V: AsRef<[u8]>>(
        &mut self,
        tx_id: u64,
        store: &str,
        entries: &[(K, V)],
    ) -> Result<Vec<bool>> {
        self.put_many_report(tx_id, store, entries).into_result()
    }

    pub fn put_many_with_ttl<K: AsRef<[u8]>, V: AsRef<[u8]>>(
        &mut self,
        tx_id: u64,
        store: &str,
        entries: &[(K, V)],
        ttl_ms: Option<u64>,
    ) -> Result<Vec<bool>> {
        self.put_many_with_ttl_report(tx_id, store, entries, ttl_ms)
            .into_result()
    }

    pub fn delete(&mut self, tx_id: u64, store: &str, key: &[u8]) -> Result<bool> {
        validate_store_name(store)?;
        validate_key(key)?;
        let operation_now_ms = now_unix_ms()?;
        let mut tx = self.take_tx(tx_id)?;
        let result = tx.readwrite_mut().and_then(|rw| {
            delete_with_staged_at(&mut self.pager, rw, store, key, operation_now_ms)
        });
        self.put_tx(tx);
        result
    }

    pub fn delete_many_report<K: AsRef<[u8]>>(
        &mut self,
        tx_id: u64,
        store: &str,
        keys: &[K],
    ) -> BatchExecutionReport<bool> {
        if let Err(error) = validate_user_store_name(store) {
            return BatchExecutionReport::failure(Vec::new(), error);
        }
        let operation_now_ms = match now_unix_ms() {
            Ok(now_ms) => now_ms,
            Err(error) => return BatchExecutionReport::failure(Vec::new(), error),
        };
        let mut tx = match self.take_visible_readwrite_tx(tx_id, store) {
            Ok(tx) => tx,
            Err(error) => return BatchExecutionReport::failure(Vec::new(), error),
        };

        let mut completed = Vec::with_capacity(keys.len());
        for key in keys {
            let key = key.as_ref();
            let result = validate_key(key)
                .and_then(|_| tx.readwrite_mut())
                .and_then(|rw| {
                    delete_with_staged_at(&mut self.pager, rw, store, key, operation_now_ms)
                });
            match result {
                Ok(deleted) => completed.push(deleted),
                Err(error) => return self.batch_failure(tx, completed, error),
            }
        }

        self.put_tx(tx);
        BatchExecutionReport::success(completed)
    }

    pub fn delete_many<K: AsRef<[u8]>>(
        &mut self,
        tx_id: u64,
        store: &str,
        keys: &[K],
    ) -> Result<Vec<bool>> {
        self.delete_many_report(tx_id, store, keys).into_result()
    }

    pub fn apply_batch_report(
        &mut self,
        tx_id: u64,
        store: &str,
        ops: &[BatchOp],
    ) -> BatchExecutionReport<BatchOpOutcome> {
        let refs: Vec<BatchOpRef<'_>> = ops.iter().map(BatchOpRef::from).collect();
        self.apply_batch_refs_report(tx_id, store, &refs)
    }

    pub fn apply_batch_refs_report(
        &mut self,
        tx_id: u64,
        store: &str,
        ops: &[BatchOpRef<'_>],
    ) -> BatchExecutionReport<BatchOpOutcome> {
        if let Err(error) = validate_user_store_name(store) {
            return BatchExecutionReport::failure(Vec::new(), error);
        }
        let operation_now_ms = match now_unix_ms() {
            Ok(now_ms) => now_ms,
            Err(error) => return BatchExecutionReport::failure(Vec::new(), error),
        };
        let mut tx = match self.take_visible_readwrite_tx(tx_id, store) {
            Ok(tx) => tx,
            Err(error) => return BatchExecutionReport::failure(Vec::new(), error),
        };

        let mut completed = Vec::with_capacity(ops.len());
        for op in ops {
            let result = match *op {
                BatchOpRef::Put { key, value } => validate_key(key)
                    .and_then(|_| validate_value(value))
                    .and_then(|_| tx.readwrite_mut())
                    .and_then(|rw| {
                        put_with_staged_at(
                            &mut self.pager,
                            rw,
                            store,
                            key,
                            StoredValue::plain(value.to_vec()),
                            operation_now_ms,
                        )
                    })
                    .map(|baseline_exists| BatchOpOutcome::Put { baseline_exists }),
                BatchOpRef::Delete { key } => validate_key(key)
                    .and_then(|_| tx.readwrite_mut())
                    .and_then(|rw| {
                        delete_with_staged_at(&mut self.pager, rw, store, key, operation_now_ms)
                    })
                    .map(|deleted| BatchOpOutcome::Delete { deleted }),
            };
            match result {
                Ok(outcome) => completed.push(outcome),
                Err(error) => return self.batch_failure(tx, completed, error),
            }
        }

        self.put_tx(tx);
        BatchExecutionReport::success(completed)
    }

    pub fn apply_batch(
        &mut self,
        tx_id: u64,
        store: &str,
        ops: &[BatchOp],
    ) -> Result<Vec<BatchOpOutcome>> {
        self.apply_batch_report(tx_id, store, ops).into_result()
    }

    pub fn scan(&mut self, tx_id: u64, store: &str, range: &ScanRange) -> Result<Vec<KvPair>> {
        validate_store_name(store)?;
        range.validate()?;
        let now_ms = now_unix_ms()?;
        let mut tx = self.take_tx(tx_id)?;
        let result = match &mut tx.inner {
            TxInner::Readonly(readonly) => match readonly.snapshot.catalog.get(store) {
                Some(meta) => scan_committed_visible(
                    &mut self.pager,
                    meta.store_root_page_id,
                    meta.flags,
                    range,
                    now_ms,
                ),
                None => Err(EngineError::StoreNotFound(store.into())),
            },
            TxInner::Readwrite(rw) => scan_with_staged(&mut self.pager, rw, store, range, now_ms),
        };
        self.put_tx(tx);
        result
    }

    pub fn stats(&mut self) -> Result<DbStats> {
        if self.health == EngineHealth::Closed {
            return Err(EngineError::Closed);
        }
        Ok(DbStats {
            db_name: self.db_name.clone(),
            db_id: self.superblock.db_id,
            page_size: self.superblock.page_size,
            catalog_root_page_id: self.superblock.catalog_root_page_id,
            next_page_id: self.superblock.next_page_id,
            last_committed_txid: self.superblock.last_committed_txid,
            last_replayed_wal_offset: self.superblock.last_replayed_wal_offset,
            store_count: visible_store_count(&self.catalog),
            manifest_len: self.manifest.len()?,
            main_len: self.pager.len()?,
            wal_len: self.wal.len()?,
            active_txns: self.txns.len(),
            write_tx_open: self.write_tx_open.is_some(),
            cache_pages: self.cache_pages,
            dirty_pages: self.pager.dirty_page_count(),
            reusable_pages: self.free_pages.ready.len(),
            retired_pages: self.free_pages.retired_len(),
            health: self.health.clone(),
        })
    }

    fn effective_change_feed_floor(&self) -> u64 {
        if self.change_feed_policy.enabled {
            self.change_feed_floor_txid
        } else {
            self.superblock.last_committed_txid
        }
    }

    pub fn changes_since(&mut self, txid: u64, options: ChangeFeedOptions) -> Result<ChangeFeed> {
        self.ensure_healthy()?;
        let latest_txid = self.superblock.last_committed_txid;
        if txid > latest_txid {
            return Err(EngineError::InvalidRange(format!(
                "change feed cursor {txid} exceeds latest committed txid {latest_txid}"
            )));
        }
        let floor = self.effective_change_feed_floor();
        if txid < floor {
            return Err(EngineError::ChangeFeedCompacted(format!(
                "change feed cursor {txid} is older than retained floor {floor}",
            )));
        }

        let store_filter = normalize_store_filter(options.stores.as_deref())?;
        let limit = options.limit.unwrap_or(usize::MAX);
        let mut changes = Vec::new();
        let Some(change_log_meta) = self.catalog.get(SYSTEM_CHANGELOG_STORE_NAME).cloned() else {
            return Ok(ChangeFeed {
                changes,
                latest_tx_id: latest_txid,
            });
        };
        if limit == 0 {
            return Ok(ChangeFeed {
                changes,
                latest_tx_id: latest_txid,
            });
        }

        let range = RangeSpec {
            gt: Some(encode_after_txid_key(txid)),
            ..RangeSpec::default()
        };
        let mut iter = TreeIter::new(&mut self.pager, change_log_meta.store_root_page_id, &range)?;
        while let Some(pair) = iter.next(&mut self.pager)? {
            let record_txid = decode_change_log_record_txid(&pair.key)?;
            let payload = materialize_pending_value(&mut self.pager, pair.value)?;
            let record = decode_change_record_payload(record_txid, &payload)?;
            if let Some(filter) = store_filter.as_ref() {
                if !filter.contains(&record.store) {
                    continue;
                }
            }
            changes.push(record);
            if changes.len() >= limit {
                break;
            }
        }

        Ok(ChangeFeed {
            changes,
            latest_tx_id: latest_txid,
        })
    }

    pub fn export_snapshot(&mut self) -> Result<Vec<u8>> {
        self.ensure_healthy()?;
        let snapshot = Snapshot::new(
            self.schema_version,
            self.superblock.catalog_root_page_id,
            self.superblock.last_committed_txid,
            &self.catalog,
        );
        let contents = collect_snapshot_contents(&mut self.pager, &snapshot, now_unix_ms()?)?;
        encode_snapshot(&contents)
    }

    pub fn import_snapshot(&mut self, bytes: &[u8]) -> Result<u64> {
        self.ensure_healthy()?;
        if !self.txns.is_empty() {
            return Err(EngineError::DatabaseBusy(
                "cannot import snapshot while transactions are open".into(),
            ));
        }
        let snapshot = decode_snapshot(bytes)?;
        self.apply_snapshot_contents(snapshot)
    }

    pub fn reset(&mut self) -> Result<u64> {
        self.ensure_healthy()?;
        if !self.txns.is_empty() {
            return Err(EngineError::DatabaseBusy(
                "cannot reset database while transactions are open".into(),
            ));
        }

        let stores = self
            .catalog
            .iter()
            .filter(|(name, _)| !is_internal_store_name(name))
            .map(|(name, meta)| SnapshotStore {
                name: name.clone(),
                flags: meta.flags,
                entries: Vec::new(),
            })
            .collect();

        self.apply_snapshot_contents(SnapshotContents {
            source_last_committed_txid: self.superblock.last_committed_txid,
            schema_version: self.schema_version,
            stores,
        })
    }

    /// Bulk-loads the live contents of this database into `target`, a freshly
    /// created empty database, writing its pages directly instead of through
    /// the WAL. One store at a time streams through the target's builder, so
    /// memory stays proportional to a page run rather than the database.
    ///
    /// Crash safety comes from the caller: the target generation is only
    /// published (control file swap) after this returns.
    pub fn compact_into(&mut self, target: &mut Engine<B>) -> Result<u64> {
        self.ensure_healthy()?;
        target.ensure_healthy()?;
        if !self.txns.is_empty() || !target.txns.is_empty() {
            return Err(EngineError::DatabaseBusy(
                "cannot compact while transactions are open".into(),
            ));
        }
        if target.superblock.last_committed_txid != 0
            || !target.catalog.is_empty()
            || target.pager.has_dirty()
        {
            return Err(EngineError::Internal(
                "compaction target must be a new, empty database".into(),
            ));
        }
        let result = self.compact_into_inner(target);
        if let Err(err) = &result {
            target.poison(format!("compaction failed: {err}"), None);
        }
        result
    }

    fn compact_into_inner(&mut self, target: &mut Engine<B>) -> Result<u64> {
        let now_ms = now_unix_ms()?;
        let new_txid = self
            .superblock
            .last_committed_txid
            .checked_add(1)
            .ok_or_else(|| EngineError::Internal("commit txid overflow".into()))?;
        let mut alloc = PageAllocator::new(target.superblock.next_page_id);
        let mut stores = CatalogMap::new();
        // SDK-owned internal stores (secondary indexes, index metadata) are
        // copied as they are; only the change log is left behind, and the
        // target's feed floor is set past it.
        let sources: Vec<(String, StoreMetadata)> = self
            .catalog
            .iter()
            .filter(|(name, _)| name.as_str() != SYSTEM_CHANGELOG_STORE_NAME)
            .map(|(name, meta)| (name.clone(), meta.clone()))
            .collect();

        for (name, meta) in sources {
            let mut builder = SortedTreeBuilder::new();
            let mut iter = TreeIter::new(
                &mut self.pager,
                meta.store_root_page_id,
                &RangeSpec::default(),
            )?;
            while let Some(pair) = iter.next(&mut self.pager)? {
                if pending_value_expired(&mut self.pager, meta.flags, &pair.value, now_ms)? {
                    continue;
                }
                let value = materialize_pending_value(&mut self.pager, pair.value)?;
                builder.push(&pair.key, &value, &mut alloc)?;
                for (page_id, bytes) in builder.drain_images() {
                    target.pager.write_page_image(page_id, &bytes)?;
                }
            }
            let built = builder.finish(&mut alloc)?;
            for (page_id, bytes) in &built.page_images {
                target.pager.write_page_image(*page_id, bytes)?;
            }
            stores.insert(
                name,
                StoreMetadata {
                    store_root_page_id: built.root_page_id,
                    created_txid: new_txid,
                    flags: meta.flags,
                },
            );
        }

        let catalog_state = CatalogState {
            schema_version: self.schema_version,
            change_feed_floor_txid: self.superblock.last_committed_txid,
            change_feed_policy: self.change_feed_policy,
            stores,
        };
        let catalog = build_catalog_tree(&catalog_state, &mut alloc)?;
        for (page_id, bytes) in &catalog.page_images {
            target.pager.write_page_image(*page_id, bytes)?;
        }
        target.pager.flush()?;

        let generation = target
            .superblock
            .generation
            .checked_add(1)
            .ok_or_else(|| EngineError::Internal("superblock generation overflow".into()))?;
        let published = SuperblockState {
            generation,
            db_id: target.superblock.db_id,
            page_size: target.superblock.page_size,
            catalog_root_page_id: catalog.root_page_id,
            next_page_id: alloc.next_page_id(),
            last_committed_txid: new_txid,
            last_replayed_wal_offset: 0,
            active_slot: if target.superblock.active_slot == 0 {
                1
            } else {
                0
            },
        };
        write_superblock(&mut target.manifest, &published)?;
        target.pager.set_page_limit(published.next_page_id);
        target.install_loaded(LoadedState {
            superblock: published,
            catalog: catalog_state,
        });
        Ok(new_txid)
    }

    pub fn store_names(&self) -> Vec<String> {
        visible_store_names(&self.catalog)
    }

    pub fn visible_store_configs(&self) -> Vec<VisibleStoreConfig> {
        self.catalog
            .iter()
            .filter(|(name, _)| !is_internal_store_name(name))
            .map(|(name, meta)| VisibleStoreConfig {
                name: name.clone(),
                flags: meta.flags,
            })
            .collect()
    }

    pub fn set_failpoint(&mut self, failpoint: Option<Failpoint>) {
        self.next_failpoint = failpoint;
    }

    pub fn catalog(&self) -> &CatalogMap {
        &self.catalog
    }

    fn oldest_snapshot_txid(&self) -> u64 {
        self.txns
            .values()
            .map(|tx| tx.snapshot().last_committed_txid)
            .min()
            .unwrap_or(self.superblock.last_committed_txid)
            .min(self.superblock.last_committed_txid)
    }

    fn committed_view(&self) -> CommittedView<'_> {
        CommittedView {
            catalog: &self.catalog,
            catalog_root_page_id: self.superblock.catalog_root_page_id,
            schema_version: self.schema_version,
            change_feed_floor_txid: self.change_feed_floor_txid,
            change_feed_policy: self.change_feed_policy,
        }
    }

    fn commit_staged(
        &mut self,
        mut staged: BTreeMap<String, StagedStore>,
        staged_schema_version: Option<u64>,
        staged_policy: Option<ChangeFeedPolicy>,
    ) -> Result<u64> {
        let commit_now_ms = now_unix_ms()?;
        for stage in staged.values_mut() {
            normalize_expired_stage_mutations(stage, commit_now_ms);
        }
        let new_txid = self.reserve_commit_txid()?;
        self.free_pages.promote(self.oldest_snapshot_txid());
        let reusable = self.free_pages.take_ready();
        let mut alloc =
            PageAllocator::with_reusable(self.superblock.next_page_id, reusable.clone());

        let view = CommittedView {
            catalog: &self.catalog,
            catalog_root_page_id: self.superblock.catalog_root_page_id,
            schema_version: self.schema_version,
            change_feed_floor_txid: self.change_feed_floor_txid,
            change_feed_policy: self.change_feed_policy,
        };
        let plan = plan_commit(
            &mut self.pager,
            &view,
            &staged,
            staged_schema_version,
            staged_policy,
            new_txid,
            commit_now_ms,
            &mut alloc,
        );
        let plan = match plan {
            Ok(plan) => plan,
            Err(err) => {
                self.free_pages.restore(reusable);
                return Err(err);
            }
        };
        self.publish(plan, alloc, reusable)
    }

    fn apply_snapshot_contents(&mut self, snapshot: SnapshotContents) -> Result<u64> {
        let new_txid = self
            .reserve_commit_txid_at_least(snapshot.source_last_committed_txid.saturating_add(1))?;
        let apply_now_ms = now_unix_ms()?;
        self.free_pages.promote(self.oldest_snapshot_txid());
        let reusable = self.free_pages.take_ready();
        let mut alloc =
            PageAllocator::with_reusable(self.superblock.next_page_id, reusable.clone());
        let plan = plan_snapshot_apply(
            &self.committed_view(),
            snapshot,
            new_txid,
            apply_now_ms,
            &mut alloc,
        );
        let plan = match plan {
            Ok(plan) => plan,
            Err(err) => {
                self.free_pages.restore(reusable);
                return Err(err);
            }
        };
        self.publish(plan, alloc, reusable)
    }

    fn publish(
        &mut self,
        plan: CommitPlan,
        alloc: PageAllocator,
        reusable: Vec<u64>,
    ) -> Result<u64> {
        let new_txid = plan.new_txid;
        let (next_page_id, unused, freed) = alloc.into_parts();
        match self.finish_commit(plan, next_page_id) {
            Ok(txid) => {
                self.free_pages.restore(unused);
                self.free_pages.retire(new_txid, freed);
                Ok(txid)
            }
            Err(err) => {
                if self.health == EngineHealth::Healthy {
                    self.free_pages.restore(reusable);
                }
                Err(err)
            }
        }
    }

    fn reserve_commit_txid(&mut self) -> Result<u64> {
        self.reserve_commit_txid_at_least(self.next_commit_txid)
    }

    fn reserve_commit_txid_at_least(&mut self, minimum: u64) -> Result<u64> {
        let new_txid = self.next_commit_txid.max(minimum);
        self.next_commit_txid = new_txid
            .checked_add(1)
            .ok_or_else(|| EngineError::Internal("commit txid overflow".into()))?;
        Ok(new_txid)
    }

    fn finish_commit(&mut self, plan: CommitPlan, next_page_id: u64) -> Result<u64> {
        let CommitPlan {
            new_txid,
            final_catalog,
            final_schema_version,
            final_change_feed_floor_txid,
            final_change_feed_policy,
            page_images,
            catalog_root_page_id,
        } = plan;
        let changed_page_count = u32::try_from(page_images.len())
            .map_err(|_| EngineError::Serialization("commit page count overflow".into()))?;
        let commit = CommitRecord {
            txid: new_txid,
            new_catalog_root_page_id: catalog_root_page_id,
            new_next_page_id: next_page_id,
            changed_page_count,
        };

        let mut wal_offset = self.wal.len()?;
        // From the first WAL byte on, a failure leaves the log tail and the
        // outcome of this commit unknown. Only recovery can say which it was.
        let appended = append_transaction(
            &mut self.wal,
            &mut wal_offset,
            new_txid,
            &page_images,
            &commit,
        )
        .and_then(|_| self.wal.flush());
        if let Err(err) = appended {
            self.poison(format!("wal append failed: {err}"), Some(new_txid));
            return Err(err);
        }
        self.wal_durable_txid = new_txid;
        if self.consume_failpoint(Failpoint::AfterWalFlush) {
            let err = EngineError::InjectedFailure(Failpoint::AfterWalFlush.as_str().into());
            self.poison(err.to_string(), Some(new_txid));
            return Err(err);
        }

        for (page_id, bytes) in page_images {
            if let Err(err) = self.pager.stage_page_image(page_id, bytes) {
                self.poison(
                    format!("staging commit pages failed: {err}"),
                    Some(new_txid),
                );
                return Err(err);
            }
        }
        self.superblock.catalog_root_page_id = catalog_root_page_id;
        self.superblock.next_page_id = next_page_id;
        self.superblock.last_committed_txid = new_txid;
        self.pager.set_page_limit(next_page_id);
        self.schema_version = final_schema_version;
        self.catalog = final_catalog;
        self.change_feed_floor_txid = final_change_feed_floor_txid;
        self.change_feed_policy = final_change_feed_policy;
        if let Err(err) = self.maybe_checkpoint() {
            self.poison(
                format!("checkpoint after commit failed: {err}"),
                Some(new_txid),
            );
            return Err(err);
        }
        Ok(new_txid)
    }

    fn maybe_checkpoint(&mut self) -> Result<()> {
        if !self.pager.has_dirty() {
            return Ok(());
        }
        if !self.checkpoint_failpoint_armed()
            && self.pager.dirty_page_count() < self.checkpoint_dirty_pages
            && self.wal.len()? < self.checkpoint_wal_bytes
        {
            return Ok(());
        }
        self.checkpoint_inner()
    }

    fn checkpoint_failpoint_armed(&self) -> bool {
        matches!(
            self.next_failpoint,
            Some(Failpoint::AfterMainFlush | Failpoint::BeforeSuperblockFlush)
        )
    }

    fn take_tx(&mut self, tx_id: u64) -> Result<TransactionState> {
        self.ensure_healthy()?;
        self.txns
            .remove(&tx_id)
            .ok_or(EngineError::TransactionClosed)
    }

    fn put_tx(&mut self, tx: TransactionState) {
        if !tx.closed {
            self.txns.insert(tx.id, tx);
        }
    }

    fn take_visible_readwrite_tx(&mut self, tx_id: u64, store: &str) -> Result<TransactionState> {
        let mut tx = self.take_tx(tx_id)?;
        let result = tx
            .readwrite_mut()
            .and_then(|rw| ensure_readwrite_store_visible(rw, store));
        if let Err(error) = result {
            self.put_tx(tx);
            return Err(error);
        }
        Ok(tx)
    }

    fn batch_failure<T>(
        &mut self,
        tx: TransactionState,
        completed: Vec<T>,
        error: EngineError,
    ) -> BatchExecutionReport<T> {
        self.put_tx(tx);
        BatchExecutionReport::failure(completed, error)
    }

    fn consume_failpoint(&mut self, failpoint: Failpoint) -> bool {
        if self.next_failpoint == Some(failpoint) {
            self.next_failpoint = None;
            true
        } else {
            false
        }
    }
}

struct CommittedView<'a> {
    catalog: &'a CatalogMap,
    catalog_root_page_id: u64,
    schema_version: u64,
    change_feed_floor_txid: u64,
    change_feed_policy: ChangeFeedPolicy,
}

#[allow(clippy::too_many_arguments)]
fn plan_commit<B: FileBackend>(
    pager: &mut Pager<B>,
    view: &CommittedView<'_>,
    staged: &BTreeMap<String, StagedStore>,
    staged_schema_version: Option<u64>,
    staged_policy: Option<ChangeFeedPolicy>,
    new_txid: u64,
    now_ms: u64,
    alloc: &mut PageAllocator,
) -> Result<CommitPlan> {
    let mut final_catalog = view.catalog.clone();
    let final_schema_version = staged_schema_version.unwrap_or(view.schema_version);
    let final_policy = staged_policy.unwrap_or(view.change_feed_policy);
    let mut page_images = Vec::new();
    let mut change_payloads = Vec::new();

    for (name, stage) in staged {
        if !stage.has_changes() {
            continue;
        }
        if final_policy.enabled && !is_internal_store_name(name) {
            collect_change_payloads(pager, name, stage, now_ms, &mut change_payloads)?;
        }
        if stage.dropped {
            if let Some(meta) = stage.base_meta.as_ref() {
                free_tree(pager, meta.store_root_page_id, alloc)?;
            }
            final_catalog.remove(name);
            continue;
        }
        let Some(built) = build_store_commit(pager, stage, now_ms, alloc)? else {
            continue;
        };
        page_images.extend(built.page_images);
        let created_txid = match (stage.created, stage.base_meta.as_ref()) {
            (false, Some(meta)) => meta.created_txid,
            _ => new_txid,
        };
        final_catalog.insert(
            name.clone(),
            StoreMetadata {
                store_root_page_id: built.root_page_id,
                created_txid,
                flags: stage.flags,
            },
        );
    }

    let mut floor = view.change_feed_floor_txid;
    if final_policy.enabled {
        if !view.change_feed_policy.enabled {
            // History restarts with this commit.
            floor = new_txid.saturating_sub(1);
        }
        let log_meta = final_catalog.get(SYSTEM_CHANGELOG_STORE_NAME).cloned();
        let mut prune_keys = Vec::new();
        if let (Some(meta), Some(retain)) = (log_meta.as_ref(), final_policy.retain_txids) {
            let target = new_txid.saturating_sub(retain);
            if target > floor {
                let bound = encode_change_log_key(target.saturating_add(1), 0);
                prune_keys = collect_keys_below(
                    pager,
                    meta.store_root_page_id,
                    &bound,
                    CHANGE_LOG_PRUNE_BATCH,
                )?;
                floor = if prune_keys.len() < CHANGE_LOG_PRUNE_BATCH {
                    target
                } else {
                    match prune_keys.last() {
                        Some(key) => decode_change_log_record_txid(key)?.max(floor),
                        None => target,
                    }
                };
            }
        }
        if !change_payloads.is_empty() || !prune_keys.is_empty() {
            let append_keys: Vec<Vec<u8>> = (0..change_payloads.len())
                .map(|sequence| {
                    u32::try_from(sequence)
                        .map(|sequence| encode_change_log_key(new_txid, sequence))
                        .map_err(|_| {
                            EngineError::Serialization("change log sequence overflow".into())
                        })
                })
                .collect::<Result<_>>()?;
            let built = match log_meta.as_ref() {
                Some(meta) => {
                    let mut mutations: Vec<Mutation<'_>> = prune_keys
                        .iter()
                        .map(|key| (key.as_slice(), None))
                        .collect();
                    mutations.extend(
                        append_keys
                            .iter()
                            .zip(&change_payloads)
                            .map(|(key, payload)| (key.as_slice(), Some(payload.as_slice()))),
                    );
                    apply_mutations(pager, meta.store_root_page_id, &mutations, alloc)?
                }
                None => build_tree_from_sorted(
                    append_keys
                        .iter()
                        .zip(&change_payloads)
                        .map(|(key, payload)| (key.as_slice(), payload.as_slice())),
                    alloc,
                )?,
            };
            page_images.extend(built.page_images);
            final_catalog.insert(
                SYSTEM_CHANGELOG_STORE_NAME.to_string(),
                StoreMetadata {
                    store_root_page_id: built.root_page_id,
                    created_txid: log_meta.map(|meta| meta.created_txid).unwrap_or(new_txid),
                    flags: CHANGELOG_STORE_FLAGS,
                },
            );
        }
    } else if let Some(meta) = final_catalog.remove(SYSTEM_CHANGELOG_STORE_NAME) {
        free_tree(pager, meta.store_root_page_id, alloc)?;
    }

    let catalog_changed = final_catalog != *view.catalog
        || final_schema_version != view.schema_version
        || floor != view.change_feed_floor_txid
        || final_policy != view.change_feed_policy;
    let catalog_root_page_id = if catalog_changed {
        free_tree(pager, view.catalog_root_page_id, alloc)?;
        let built = build_catalog_tree(
            &CatalogState {
                schema_version: final_schema_version,
                change_feed_floor_txid: floor,
                change_feed_policy: final_policy,
                stores: final_catalog.clone(),
            },
            alloc,
        )?;
        page_images.extend(built.page_images);
        built.root_page_id
    } else {
        view.catalog_root_page_id
    };

    Ok(CommitPlan {
        new_txid,
        final_catalog,
        final_schema_version,
        final_change_feed_floor_txid: floor,
        final_change_feed_policy: final_policy,
        page_images,
        catalog_root_page_id,
    })
}

/// Builds the new tree of one changed store, or `None` if it is unchanged.
fn build_store_commit<B: FileBackend>(
    pager: &mut Pager<B>,
    stage: &StagedStore,
    now_ms: u64,
    alloc: &mut PageAllocator,
) -> Result<Option<BuiltTree>> {
    let base = stage.base_meta.as_ref();
    if stage.created || stage.cleared || base.is_none() {
        if stage.cleared {
            if let Some(base) = base {
                free_tree(pager, base.store_root_page_id, alloc)?;
            }
        }
        let encoded = encode_stage_puts(stage)?;
        return build_tree_from_sorted(
            encoded.iter().map(|(key, value)| (*key, value.as_slice())),
            alloc,
        )
        .map(Some);
    }
    let base = base.ok_or_else(|| EngineError::Internal("missing base metadata".into()))?;
    if stage.force_full_rewrite {
        return rewrite_store_fully(pager, base, stage, now_ms, alloc).map(Some);
    }
    if stage.mutations.is_empty() {
        return Ok(None);
    }
    let encoded = encode_stage_mutations(stage)?;
    let mutations: Vec<Mutation<'_>> = encoded
        .iter()
        .map(|(key, value)| (*key, value.as_deref()))
        .collect();
    apply_mutations(pager, base.store_root_page_id, &mutations, alloc).map(Some)
}

/// Re-encodes a whole store after its value format changed (a TTL put into a
/// store created before value envelopes). Streams the old tree and the staged
/// mutations in key order into a new tree, then retires the old one.
fn rewrite_store_fully<B: FileBackend>(
    pager: &mut Pager<B>,
    base: &StoreMetadata,
    stage: &StagedStore,
    now_ms: u64,
    alloc: &mut PageAllocator,
) -> Result<BuiltTree> {
    let mut builder = SortedTreeBuilder::new();
    let mut staged = stage.mutations.iter().peekable();
    let mut iter = TreeIter::new(pager, base.store_root_page_id, &RangeSpec::default())?;
    let mut base_next = iter.next(pager)?;
    loop {
        let take_staged = match (base_next.as_ref(), staged.peek()) {
            (None, None) => break,
            (Some(_), None) => false,
            (None, Some(_)) => true,
            (Some(pair), Some((key, _))) => pair.key.as_slice() >= key.as_slice(),
        };
        if take_staged {
            let Some((key, mutation)) = staged.next() else {
                break;
            };
            if matches!(base_next.as_ref(), Some(pair) if pair.key == *key) {
                base_next = iter.next(pager)?;
            }
            if let MutationValue::Put(stored) = mutation {
                builder.push(key, &stored.encode_for_store(stage.flags)?, alloc)?;
            }
        } else if let Some(pair) = base_next.take() {
            let raw = materialize_pending_value(pager, pair.value)?;
            let stored = StoredValue::decode_owned_for_store(base.flags, raw)?;
            if !stored.is_expired_at(now_ms) {
                builder.push(&pair.key, &stored.encode_for_store(stage.flags)?, alloc)?;
            }
            base_next = iter.next(pager)?;
        }
    }
    free_tree(pager, base.store_root_page_id, alloc)?;
    builder.finish(alloc)
}

fn plan_snapshot_apply(
    view: &CommittedView<'_>,
    snapshot: SnapshotContents,
    new_txid: u64,
    now_ms: u64,
    alloc: &mut PageAllocator,
) -> Result<CommitPlan> {
    // The previous trees are not walked and retired here: reset and import
    // replace the whole database, and compaction reclaims their pages.
    let mut final_catalog = CatalogMap::new();
    let mut page_images = Vec::new();

    for store in snapshot.stores {
        validate_user_store_name(&store.name)?;
        let flags = normalize_snapshot_store_flags(store.flags, &store.entries)?;
        let mut entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(store.entries.len());
        for entry in store.entries {
            let stored = StoredValue::with_expiry(entry.value, entry.expires_at_ms);
            if stored.is_expired_at(now_ms) {
                continue;
            }
            entries.push((entry.key, stored.encode_for_store(flags)?));
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        if entries.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(EngineError::Corruption(format!(
                "duplicate snapshot key in store {}",
                store.name
            )));
        }
        let built = build_tree_from_sorted(
            entries
                .iter()
                .map(|(key, value)| (key.as_slice(), value.as_slice())),
            alloc,
        )?;
        page_images.extend(built.page_images);
        final_catalog.insert(
            store.name,
            StoreMetadata {
                store_root_page_id: built.root_page_id,
                created_txid: new_txid,
                flags,
            },
        );
    }

    let floor = snapshot.source_last_committed_txid;
    let catalog_tree = build_catalog_tree(
        &CatalogState {
            schema_version: snapshot.schema_version,
            change_feed_floor_txid: floor,
            change_feed_policy: view.change_feed_policy,
            stores: final_catalog.clone(),
        },
        alloc,
    )?;
    page_images.extend(catalog_tree.page_images);

    Ok(CommitPlan {
        new_txid,
        final_catalog,
        final_schema_version: snapshot.schema_version,
        final_change_feed_floor_txid: floor,
        final_change_feed_policy: view.change_feed_policy,
        page_images,
        catalog_root_page_id: catalog_tree.root_page_id,
    })
}

fn encode_stage_puts(stage: &StagedStore) -> Result<Vec<(&[u8], Vec<u8>)>> {
    stage
        .mutations
        .iter()
        .filter_map(|(key, mutation)| match mutation {
            MutationValue::Put(stored) => Some(
                stored
                    .encode_for_store(stage.flags)
                    .map(|encoded| (key.as_slice(), encoded)),
            ),
            MutationValue::Delete => None,
        })
        .collect()
}

type EncodedMutation<'a> = (&'a [u8], Option<Vec<u8>>);

fn encode_stage_mutations(stage: &StagedStore) -> Result<Vec<EncodedMutation<'_>>> {
    stage
        .mutations
        .iter()
        .map(|(key, mutation)| {
            let encoded = match mutation {
                MutationValue::Put(stored) => Some(stored.encode_for_store(stage.flags)?),
                MutationValue::Delete => None,
            };
            Ok((key.as_slice(), encoded))
        })
        .collect()
}

fn decode_change_log_record_txid(key: &[u8]) -> Result<u64> {
    if key.len() != 12 {
        return Err(EngineError::Corruption(format!(
            "change log key length mismatch: expected 12 bytes, got {}",
            key.len()
        )));
    }

    let mut txid_bytes = [0u8; 8];
    txid_bytes.copy_from_slice(&key[..8]);
    Ok(u64::from_be_bytes(txid_bytes))
}

/// Encodes this stage's change records. Clearing or dropping a store is one
/// store-level record; deletes are recorded only for keys that existed, which
/// is checked from metadata without reading values.
fn collect_change_payloads<B: FileBackend>(
    pager: &mut Pager<B>,
    store_name: &str,
    stage: &StagedStore,
    now_ms: u64,
    out: &mut Vec<Vec<u8>>,
) -> Result<()> {
    if stage.dropped {
        if stage.base_meta.is_some() && !stage.created {
            out.push(encode_change_record_payload(
                store_name,
                &[],
                ChangeKind::Drop,
                None,
            )?);
        }
        return Ok(());
    }

    let base = if stage.created {
        None
    } else {
        stage.base_meta.as_ref()
    };
    if stage.cleared && base.is_some() {
        out.push(encode_change_record_payload(
            store_name,
            &[],
            ChangeKind::Clear,
            None,
        )?);
    }
    for (key, mutation) in &stage.mutations {
        match mutation {
            MutationValue::Put(stored) => out.push(encode_change_record_payload(
                store_name,
                key,
                ChangeKind::Put,
                Some(&stored.value),
            )?),
            MutationValue::Delete => {
                if stage.cleared {
                    continue;
                }
                let Some(base) = base else {
                    continue;
                };
                if exists_committed_visible(
                    pager,
                    base.store_root_page_id,
                    base.flags,
                    key,
                    now_ms,
                )? {
                    out.push(encode_change_record_payload(
                        store_name,
                        key,
                        ChangeKind::Delete,
                        None,
                    )?);
                }
            }
        }
    }
    Ok(())
}

fn ensure_stage_for_write<'a>(rw: &'a mut ReadwriteTx, store: &str) -> Result<&'a mut StagedStore> {
    use std::collections::btree_map::Entry;

    match rw.stores.entry(store.to_string()) {
        Entry::Occupied(entry) => Ok(entry.into_mut()),
        Entry::Vacant(vacant) => {
            let base_meta = rw
                .snapshot
                .catalog
                .get(store)
                .cloned()
                .ok_or_else(|| EngineError::StoreNotFound(store.into()))?;
            Ok(vacant.insert(StagedStore::existing(base_meta)))
        }
    }
}

fn ensure_readwrite_store_visible(rw: &ReadwriteTx, store: &str) -> Result<()> {
    if let Some(stage) = rw.stores.get(store) {
        if stage.dropped {
            return Err(EngineError::StoreNotFound(store.into()));
        }
        return Ok(());
    }
    if rw.snapshot.catalog.contains_key(store) {
        Ok(())
    } else {
        Err(EngineError::StoreNotFound(store.into()))
    }
}

fn absolute_expiry_from_ttl_at(ttl_ms: Option<u64>, now_ms: u64) -> Result<Option<u64>> {
    ttl_ms
        .map(|ttl_ms| {
            now_ms
                .checked_add(ttl_ms)
                .ok_or_else(|| EngineError::Serialization("ttl expiry timestamp overflow".into()))
        })
        .transpose()
}

fn normalize_snapshot_store_flags(flags: u64, entries: &[SnapshotEntry]) -> Result<u64> {
    let compression = store_compression_from_flags(flags)?;
    let mut normalized = flags & (STORE_FLAG_VALUE_ENVELOPE_V1 | STORE_FLAG_COMPRESSION_MASK);
    if compression != StoreCompression::None
        || entries.iter().any(|entry| entry.expires_at_ms.is_some())
    {
        normalized |= STORE_FLAG_VALUE_ENVELOPE_V1;
    }
    Ok(normalized)
}

fn mark_key_expired(stage: &mut StagedStore, key: &[u8]) {
    if !stage.mutations.contains_key(key) {
        stage.mutations.insert(key.to_vec(), MutationValue::Delete);
    }
}

fn normalize_expired_stage_mutations(stage: &mut StagedStore, now_ms: u64) {
    for mutation in stage.mutations.values_mut() {
        if matches!(mutation, MutationValue::Put(stored) if stored.is_expired_at(now_ms)) {
            *mutation = MutationValue::Delete;
        }
    }
}

fn get_committed_visible<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    store_flags: u64,
    key: &[u8],
    now_ms: u64,
) -> Result<Option<Vec<u8>>> {
    match lookup(pager, root_page_id, key)? {
        Some(raw_value) => {
            let stored = StoredValue::decode_owned_for_store(store_flags, raw_value)?;
            if stored.is_expired_at(now_ms) {
                Ok(None)
            } else {
                Ok(Some(stored.value))
            }
        }
        None => Ok(None),
    }
}

/// Existence from the leaf cell alone, plus the 16-byte TTL header for
/// enveloped stores. Overflow values are never read past their first page.
fn exists_committed_visible<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    store_flags: u64,
    key: &[u8],
    now_ms: u64,
) -> Result<bool> {
    let enveloped =
        store_uses_value_envelope(store_flags) && !store_uses_system_raw_values(store_flags);
    let prefix_len = if enveloped {
        VALUE_ENVELOPE_HEADER_SIZE
    } else {
        0
    };
    match lookup_prefix(pager, root_page_id, key, prefix_len)? {
        None => Ok(false),
        Some(_) if !enveloped => Ok(true),
        Some(prefix) => Ok(!matches!(
            decode_envelope_expiry(&prefix)?,
            Some(expires_at_ms) if now_ms >= expires_at_ms
        )),
    }
}

enum StagedLookup<'a> {
    Staged(Option<&'a StoredValue>),
    Committed(StoreMetadata),
    Absent,
}

fn staged_lookup<'a>(rw: &'a ReadwriteTx, store: &str, key: &[u8]) -> Result<StagedLookup<'a>> {
    if let Some(stage) = rw.stores.get(store) {
        if stage.dropped {
            return Err(EngineError::StoreNotFound(store.into()));
        }
        if let Some(mutation) = stage.mutations.get(key) {
            return Ok(StagedLookup::Staged(match mutation {
                MutationValue::Put(stored) => Some(stored),
                MutationValue::Delete => None,
            }));
        }
        if stage.created || stage.cleared {
            return Ok(StagedLookup::Absent);
        }
        return Ok(match stage.base_meta.clone() {
            Some(meta) => StagedLookup::Committed(meta),
            None => StagedLookup::Absent,
        });
    }
    rw.snapshot
        .catalog
        .get(store)
        .cloned()
        .map(StagedLookup::Committed)
        .ok_or_else(|| EngineError::StoreNotFound(store.into()))
}

fn expire_staged_key(rw: &mut ReadwriteTx, store: &str, key: &[u8]) -> Result<()> {
    let stage = ensure_stage_for_write(rw, store)?;
    match stage.mutations.get(key) {
        Some(MutationValue::Put(_)) => {
            stage.mutations.insert(key.to_vec(), MutationValue::Delete);
        }
        Some(MutationValue::Delete) => {}
        None => mark_key_expired(stage, key),
    }
    Ok(())
}

fn get_with_staged<B: FileBackend>(
    pager: &mut Pager<B>,
    rw: &mut ReadwriteTx,
    store: &str,
    key: &[u8],
    now_ms: u64,
) -> Result<Option<Vec<u8>>> {
    let (value, expired) = match staged_lookup(rw, store, key)? {
        StagedLookup::Staged(Some(stored)) => {
            if stored.is_expired_at(now_ms) {
                (None, true)
            } else {
                (Some(stored.value.clone()), false)
            }
        }
        StagedLookup::Staged(None) | StagedLookup::Absent => (None, false),
        StagedLookup::Committed(meta) => match lookup(pager, meta.store_root_page_id, key)? {
            Some(raw) => {
                let stored = StoredValue::decode_owned_for_store(meta.flags, raw)?;
                if stored.is_expired_at(now_ms) {
                    (None, true)
                } else {
                    (Some(stored.value), false)
                }
            }
            None => (None, false),
        },
    };
    if expired {
        expire_staged_key(rw, store, key)?;
    }
    Ok(value)
}

fn exists_with_staged<B: FileBackend>(
    pager: &mut Pager<B>,
    rw: &mut ReadwriteTx,
    store: &str,
    key: &[u8],
    now_ms: u64,
) -> Result<bool> {
    let (exists, expired) = match staged_lookup(rw, store, key)? {
        StagedLookup::Staged(Some(stored)) => {
            let expired = stored.is_expired_at(now_ms);
            (!expired, expired)
        }
        StagedLookup::Staged(None) | StagedLookup::Absent => (false, false),
        StagedLookup::Committed(meta) => {
            let enveloped =
                store_uses_value_envelope(meta.flags) && !store_uses_system_raw_values(meta.flags);
            let prefix_len = if enveloped {
                VALUE_ENVELOPE_HEADER_SIZE
            } else {
                0
            };
            match lookup_prefix(pager, meta.store_root_page_id, key, prefix_len)? {
                None => (false, false),
                Some(_) if !enveloped => (true, false),
                Some(prefix) => {
                    let expired = matches!(
                        decode_envelope_expiry(&prefix)?,
                        Some(expires_at_ms) if now_ms >= expires_at_ms
                    );
                    (!expired, expired)
                }
            }
        }
    };
    if expired {
        expire_staged_key(rw, store, key)?;
    }
    Ok(exists)
}

fn put_with_staged_at<B: FileBackend>(
    pager: &mut Pager<B>,
    rw: &mut ReadwriteTx,
    store: &str,
    key: &[u8],
    value: StoredValue,
    now_ms: u64,
) -> Result<bool> {
    let existed = exists_with_staged(pager, rw, store, key, now_ms)?;
    let stage = ensure_stage_for_write(rw, store)?;
    if stage.dropped {
        return Err(EngineError::StoreNotFound(store.into()));
    }
    if value.expires_at_ms.is_some() && !store_uses_value_envelope(stage.flags) {
        stage.flags |= STORE_FLAG_VALUE_ENVELOPE_V1;
        if !stage.created && !stage.cleared {
            stage.force_full_rewrite = true;
        }
    }
    stage
        .mutations
        .insert(key.to_vec(), MutationValue::Put(value));
    Ok(existed)
}

fn delete_with_staged_at<B: FileBackend>(
    pager: &mut Pager<B>,
    rw: &mut ReadwriteTx,
    store: &str,
    key: &[u8],
    now_ms: u64,
) -> Result<bool> {
    let existed = exists_with_staged(pager, rw, store, key, now_ms)?;
    let stage = ensure_stage_for_write(rw, store)?;
    if stage.dropped {
        return Err(EngineError::StoreNotFound(store.into()));
    }
    stage.mutations.insert(key.to_vec(), MutationValue::Delete);
    Ok(existed)
}

/// Streams a committed tree in range order, skipping expired values, and
/// stops as soon as `limit` rows are collected, in either direction.
fn scan_committed_visible<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    store_flags: u64,
    range: &RangeSpec,
    now_ms: u64,
) -> Result<Vec<KvPair>> {
    let limit = range.limit.unwrap_or(usize::MAX);
    let mut rows = Vec::new();
    if limit == 0 {
        return Ok(rows);
    }
    let mut iter = TreeIter::new(pager, root_page_id, range)?;
    while let Some(pair) = iter.next(pager)? {
        if pending_value_expired(pager, store_flags, &pair.value, now_ms)? {
            continue;
        }
        let raw = materialize_pending_value(pager, pair.value)?;
        let stored = StoredValue::decode_owned_for_store(store_flags, raw)?;
        rows.push(KvPair {
            key: pair.key,
            value: stored.value,
        });
        if rows.len() >= limit {
            break;
        }
    }
    Ok(rows)
}

/// Merges the committed tree with staged mutations lazily, in range order,
/// so `limit` bounds the work in both directions instead of materializing
/// the whole range first.
fn scan_with_staged<B: FileBackend>(
    pager: &mut Pager<B>,
    rw: &mut ReadwriteTx,
    store: &str,
    range: &RangeSpec,
    now_ms: u64,
) -> Result<Vec<KvPair>> {
    if let Some(stage) = rw.stores.get_mut(store) {
        if stage.dropped {
            return Err(EngineError::StoreNotFound(store.into()));
        }
        normalize_expired_stage_mutations(stage, now_ms);
    }

    let (base_meta, stage) = match rw.stores.get(store) {
        Some(stage) => {
            let base = if stage.created || stage.cleared {
                None
            } else {
                stage.base_meta.clone()
            };
            (base, Some(stage))
        }
        None => (
            Some(
                rw.snapshot
                    .catalog
                    .get(store)
                    .cloned()
                    .ok_or_else(|| EngineError::StoreNotFound(store.into()))?,
            ),
            None,
        ),
    };

    let limit = range.limit.unwrap_or(usize::MAX);
    let mut rows = Vec::new();
    let mut expired_base_keys = Vec::new();
    if limit > 0 {
        let lower = to_bound(range.lower_bound());
        let upper = to_bound(range.upper_bound());
        let mut staged: Box<dyn Iterator<Item = (&Vec<u8>, &MutationValue)>> = match stage {
            Some(stage) => {
                let iter = stage
                    .mutations
                    .range::<[u8], (Bound<&[u8]>, Bound<&[u8]>)>((lower, upper));
                if range.reverse {
                    Box::new(iter.rev())
                } else {
                    Box::new(iter)
                }
            }
            None => Box::new(std::iter::empty()),
        };
        let mut base = match base_meta.as_ref() {
            Some(meta) => Some(TreeIter::new(pager, meta.store_root_page_id, range)?),
            None => None,
        };
        let mut base_next = match base.as_mut() {
            Some(iter) => iter.next(pager)?,
            None => None,
        };
        let mut staged_next = staged.next();

        while rows.len() < limit {
            let order = match (base_next.as_ref(), staged_next) {
                (None, None) => break,
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (Some(pair), Some((key, _))) => {
                    let ordering = pair.key.as_slice().cmp(key.as_slice());
                    if range.reverse {
                        ordering.reverse()
                    } else {
                        ordering
                    }
                }
            };
            if order != Ordering::Less {
                let Some((key, mutation)) = staged_next else {
                    break;
                };
                if order == Ordering::Equal {
                    base_next = match base.as_mut() {
                        Some(iter) => iter.next(pager)?,
                        None => None,
                    };
                }
                if let MutationValue::Put(stored) = mutation {
                    if !stored.is_expired_at(now_ms) {
                        rows.push(KvPair {
                            key: key.clone(),
                            value: stored.value.clone(),
                        });
                    }
                }
                staged_next = staged.next();
                continue;
            }
            let Some(pair) = base_next.take() else {
                break;
            };
            let meta = base_meta
                .as_ref()
                .ok_or_else(|| EngineError::Internal("base row without a base tree".into()))?;
            if pending_value_expired(pager, meta.flags, &pair.value, now_ms)? {
                expired_base_keys.push(pair.key);
            } else {
                let raw = materialize_pending_value(pager, pair.value)?;
                let stored = StoredValue::decode_owned_for_store(meta.flags, raw)?;
                rows.push(KvPair {
                    key: pair.key,
                    value: stored.value,
                });
            }
            base_next = match base.as_mut() {
                Some(iter) => iter.next(pager)?,
                None => None,
            };
        }
    }

    if !expired_base_keys.is_empty() {
        let stage = ensure_stage_for_write(rw, store)?;
        for key in &expired_base_keys {
            mark_key_expired(stage, key);
        }
    }
    Ok(rows)
}

/// Expiry from the envelope header only, so expired overflow values are
/// skipped without reading their chains.
fn pending_value_expired<B: FileBackend>(
    pager: &mut Pager<B>,
    store_flags: u64,
    value: &PendingValue,
    now_ms: u64,
) -> Result<bool> {
    if !store_uses_value_envelope(store_flags) || store_uses_system_raw_values(store_flags) {
        return Ok(false);
    }
    let prefix = pending_value_prefix(pager, value, VALUE_ENVELOPE_HEADER_SIZE)?;
    stored_value_expired(store_flags, &prefix, now_ms)
}

fn to_bound(bound: Option<(&[u8], bool)>) -> Bound<&[u8]> {
    match bound {
        None => Bound::Unbounded,
        Some((key, true)) => Bound::Included(key),
        Some((key, false)) => Bound::Excluded(key),
    }
}

fn db_id_from_name(name: &str) -> u64 {
    checksum::crc32(name.as_bytes()) as u64
}

fn validate_db_identity(
    db_name: &str,
    expected_db_id: u64,
    superblock: &SuperblockState,
) -> Result<()> {
    if superblock.db_id != expected_db_id {
        return Err(EngineError::Corruption(format!(
            "db identity mismatch for {db_name}: manifest db_id={} expected={expected_db_id}",
            superblock.db_id,
        )));
    }
    Ok(())
}
