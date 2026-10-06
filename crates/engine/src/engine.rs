mod catalog_delta;

#[cfg(test)]
mod catalog_tests;

#[cfg(test)]
mod commit_work_tests;

#[cfg(test)]
mod paired_crc_tests;

#[cfg(test)]
mod payload_tests;

#[cfg(test)]
mod checkpoint_retirement_tests;

#[cfg(test)]
mod revision_tests;

use crate::btree::{
    apply_value_mutations, build_catalog_tree, build_tree_from_values, collect_keys_below,
    drop_tree_payload_references, free_tree, lookup_stored_value, lookup_stored_value_info,
    lookup_stored_value_into, lookup_stored_value_size, lookup_stored_value_state,
    lookup_value_expiry, materialize_pending_stored_value, materialize_pending_value,
    pending_stored_value_info, pending_value_prefix, write_pending_stored_value, BuiltTree, KvPair,
    PageAllocator, PageImages, PendingValue, PointReadBatch, RangeSpec, SortedTreeBuilder,
    TreeIter,
};
use crate::bytes::{read_u32_le, validate_key, validate_store_name as validate_store_name_length};
use crate::catalog::{CatalogMap, CatalogState, ChangeFeedPolicy};
use crate::change_feed::{
    decode_change_record_prefix_ref, encode_after_txid_key, encode_change_log_key,
    encode_change_record_prefix, is_internal_store_name, normalize_store_filter,
    validate_user_store_name, visible_store_count, visible_store_names, ChangeFeed,
    ChangeFeedOptions, ChangeKind, ChangeRecord, CHANGELOG_STORE_FLAGS,
    MAX_CHANGE_RECORD_PREFIX_SIZE, SYSTEM_CHANGELOG_STORE_NAME,
};
use crate::checksum;
use crate::error::{EngineError, Result};
use crate::layout::{StoreMetadata, SuperblockState};
use crate::output::{PackedScanPage, PackedValues, ScanPacket};
use crate::pager::Pager;
use crate::payload::write_payload_unflushed;
use crate::payload_registry::{PayloadRegistry, PAYLOAD_REGISTRY_STORE_NAME};
use crate::prepared_value::{PreparedValue, ValueSource};
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
    BatchOp, BatchOpOutcome, BatchOpRef, IndexOpRef, MutationValue, ReadwriteTx, Snapshot,
    StagedStore, TransactionState, TxInner,
};
use crate::value::{
    store_compression_from_flags, store_flags_for_user_store, store_uses_system_raw_values,
    store_uses_value_envelope, store_value_prefix_len, validate_store_value,
    validate_store_value_max_size, validate_store_value_prefix, StoreCompression, StoredValue,
    ValueRevision, ValueState, COMPRESSION_VALUE_HEADER_SIZE, STORE_FLAG_COMPRESSION_MASK,
    STORE_FLAG_SYSTEM_RAW_VALUES, STORE_FLAG_VALUE_ENVELOPE_V1, STORE_FLAG_VALUE_REVISION,
};
use crate::wal::{append_generated_transaction, CommitRecord};
use catalog_delta::{CatalogDelta, CatalogUpdate};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ops::Bound;
use std::sync::Arc;

pub use crate::txn::TxMode;

pub type ScanRange = RangeSpec;

// Limit pruning work per commit. Later commits remove the remaining records.
const CHANGE_LOG_PRUNE_BATCH: usize = 1024;
const EXTERNAL_PAYLOAD_THRESHOLD: usize = 32 * 1024;

fn validate_store_name(name: &str) -> Result<()> {
    validate_store_name_length(name)?;
    if name == PAYLOAD_REGISTRY_STORE_NAME {
        return Err(EngineError::ReservedStoreName(name.into()));
    }
    Ok(())
}

fn validate_index_entry_binding(
    index_store: &str,
    index_key: &[u8],
    primary_key: &[u8],
) -> Result<()> {
    if !index_store.starts_with("__browserdb:index:") {
        return Err(EngineError::Serialization(
            "checked index store has invalid namespace".into(),
        ));
    }
    let mut part = 0usize;
    let mut matched = 0usize;
    let mut offset = 0usize;
    while offset < index_key.len() {
        let mut byte = index_key[offset];
        offset += 1;
        if byte == 0 {
            let escaped = *index_key
                .get(offset)
                .ok_or_else(|| EngineError::Serialization("truncated index key escape".into()))?;
            offset += 1;
            match escaped {
                0 => {
                    part += 1;
                    if part > 2 || (part == 2 && offset != index_key.len()) {
                        return Err(EngineError::Serialization(
                            "index key must contain exactly two parts".into(),
                        ));
                    }
                    continue;
                }
                255 => byte = 0,
                _ => {
                    return Err(EngineError::Serialization(
                        "invalid index key escape".into(),
                    ))
                }
            }
        }
        if part == 1 {
            if primary_key.get(matched) != Some(&byte) {
                return Err(EngineError::Serialization(
                    "index key does not bind the primary key".into(),
                ));
            }
            matched += 1;
        }
    }
    if part != 2 || matched != primary_key.len() {
        return Err(EngineError::Serialization(
            "index key does not bind the primary key".into(),
        ));
    }
    Ok(())
}

struct PayloadPreparation {
    registry: PayloadRegistry,
    oldest_snapshot_txid: u64,
    pending_writes: bool,
}

impl PayloadPreparation {
    fn externalize<B: FileBackend>(
        &mut self,
        pager: &mut Pager<B>,
        alloc: &mut PageAllocator,
        value: &mut PreparedValue<'_>,
    ) -> Result<()> {
        let body = value.parts().payload();
        if body.len() < EXTERNAL_PAYLOAD_THRESHOLD {
            return Ok(());
        }
        let reservation =
            self.registry
                .reserve(pager, body.len() as u32, self.oldest_snapshot_txid, alloc)?;
        let reference =
            write_payload_unflushed(pager.backend_mut(), reservation.first_page_id, body)?;
        self.pending_writes = true;
        self.registry.register_body(reservation, reference)?;
        value.set_external_reference(reference);
        Ok(())
    }

    fn flush_pending<B: FileBackend>(&mut self, pager: &mut Pager<B>) -> Result<()> {
        if self.pending_writes {
            pager.backend_mut().flush()?;
            self.pending_writes = false;
        }
        Ok(())
    }
}

struct CommitPlan {
    new_txid: u64,
    catalog_update: CatalogUpdate,
    final_schema_version: u64,
    final_change_feed_floor_txid: u64,
    final_change_feed_policy: ChangeFeedPolicy,
    page_images: PageImages,
    catalog_root_page_id: u64,
}

struct PreparedChange<'a> {
    kind: ChangeKind,
    value: PreparedValue<'a>,
}

impl<'a> PreparedChange<'a> {
    fn new(store: &str, key: &[u8], kind: ChangeKind, value: Option<&'a [u8]>) -> Result<Self> {
        let prefix = encode_change_record_prefix(store, key, kind, value)?;
        Ok(Self {
            kind,
            value: PreparedValue::prefixed(prefix, value.unwrap_or(&[])),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenConfig {
    pub create_if_missing: bool,
    pub cache_pages: usize,
    /// Checkpoint once this many WAL bytes are durable but not installed.
    pub checkpoint_wal_bytes: u64,
    /// Checkpoint at this many committed dirty pages. Dirty pages are pinned
    /// in the cache, so the threshold limits memory between checkpoints.
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
/// A failure after a commit starts writing to the WAL leaves its outcome
/// unknown and may leave an incomplete WAL tail. Further reads and writes
/// require [`Engine::recover`] to rebuild state from the files.
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
    /// The commit whose outcome was unknown when recovery became required.
    pub pending_txid: Option<u64>,
    /// Whether recovery found that commit durable.
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
/// A page retired by commit T is reachable only from trees older than T.
/// It can be reused when every open snapshot is at T or newer. The pool is
/// not persisted. Compaction reclaims pages not reused before close.
#[derive(Debug, Default)]
struct FreePagePool {
    ready: Vec<u64>,
    retired: VecDeque<RetiredPageBatch>,
}

#[derive(Debug)]
struct RetiredPageBatch {
    txid: u64,
    pages: Vec<u64>,
    dirty_images: Vec<(u64, u64)>,
}

impl FreePagePool {
    fn promote(
        &mut self,
        oldest_snapshot_txid: u64,
        mut discard_dirty_image: impl FnMut(u64, u64),
    ) {
        while let Some(batch) = self.retired.front() {
            if batch.txid > oldest_snapshot_txid {
                break;
            }
            if let Some(batch) = self.retired.pop_front() {
                for (page_id, generation) in batch.dirty_images {
                    discard_dirty_image(page_id, generation);
                }
                self.ready.extend(batch.pages);
            }
        }
    }

    fn take_ready(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.ready)
    }

    fn restore(&mut self, pages: Vec<u64>) {
        self.ready.extend(pages);
    }

    fn retire(&mut self, txid: u64, pages: Vec<u64>, dirty_images: Vec<(u64, u64)>) {
        if !pages.is_empty() {
            self.retired.push_back(RetiredPageBatch {
                txid,
                pages,
                dirty_images,
            });
        }
    }

    fn clear(&mut self) {
        self.ready.clear();
        self.retired.clear();
    }

    fn retired_len(&self) -> usize {
        self.retired.iter().map(|batch| batch.pages.len()).sum()
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
    // Highest txid flushed to the WAL. Checkpoint must retain WAL commits
    // that memory has not applied.
    wal_durable_txid: u64,
    // A commit with no dirty pages still requires checkpoint publication.
    checkpoint_txid: u64,
    schema_version: u64,
    catalog: Arc<CatalogMap>,
    change_feed_floor_txid: u64,
    change_feed_policy: ChangeFeedPolicy,
    next_tx_id: u64,
    next_commit_txid: u64,
    next_value_revision_ordinal: u64,
    txns: HashMap<u64, TransactionState>,
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
            .field("checkpoint_txid", &self.checkpoint_txid)
            .field("schema_version", &self.schema_version)
            .field("catalog", &self.catalog)
            .field("change_feed_floor_txid", &self.change_feed_floor_txid)
            .field("change_feed_policy", &self.change_feed_policy)
            .field("next_tx_id", &self.next_tx_id)
            .field("next_commit_txid", &self.next_commit_txid)
            .field("txns", &self.txns)
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
            checkpoint_txid: 0,
            schema_version: 0,
            catalog: Arc::new(CatalogMap::new()),
            change_feed_floor_txid: 0,
            change_feed_policy: ChangeFeedPolicy::default(),
            next_tx_id: 1,
            next_commit_txid: 1,
            next_value_revision_ordinal: 0,
            txns: HashMap::new(),
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
        self.checkpoint_txid = superblock.last_committed_txid;
        self.next_commit_txid = superblock.last_committed_txid.saturating_add(1);
        self.next_value_revision_ordinal = 0;
        self.schema_version = catalog.schema_version;
        self.catalog = Arc::new(catalog.stores);
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

    /// Discards transactions and cached state. Reloads the superblock,
    /// replays the WAL as needed, then reloads the catalog.
    ///
    /// Reports whether the commit with an unknown outcome is durable.
    pub fn recover(&mut self) -> Result<RecoveryReport> {
        let pending_txid = match &self.health {
            EngineHealth::Closed => return Err(EngineError::Closed),
            EngineHealth::Healthy => None,
            EngineHealth::RecoveryRequired { pending_txid, .. } => *pending_txid,
        };
        self.txns.clear();
        self.next_failpoint = None;
        self.free_pages.clear();
        self.pager.discard_cache();
        // Flush visible bytes before recovery so its result survives a crash.
        // Otherwise, an unflushed superblock could cause recovery to discard
        // WAL records still needed by the durable superblock. Flush main before
        // the manifest that references it. Checksums reject an incomplete WAL tail.
        let loaded = (|| {
            self.pager.flush()?;
            self.manifest.flush()?;
            self.wal.flush()?;
            load_state(
                &mut self.manifest,
                &mut self.pager,
                &mut self.wal,
                &self.db_name,
                false,
            )
        })()
        .inspect_err(|err| {
            // Recovery discarded the dirty cache and may have replayed only
            // part of the WAL. The engine requires recovery even if it was healthy.
            self.poison(format!("recovery failed: {err}"), pending_txid);
        })?;
        self.install_loaded(loaded);
        self.health = EngineHealth::Healthy;
        let last_committed_txid = self.superblock.last_committed_txid;
        Ok(RecoveryReport {
            last_committed_txid,
            pending_txid,
            pending_committed: pending_txid.is_some_and(|txid| last_committed_txid >= txid),
        })
    }

    /// Installs committed pages and closes the files. If recovery is required,
    /// skips the checkpoint because memory cannot be trusted. Durable commits
    /// remain recoverable from the files.
    pub fn close(&mut self) -> Result<()> {
        if self.health == EngineHealth::Closed {
            return Ok(());
        }
        self.txns.clear();
        let checkpoint = if self.health == EngineHealth::Healthy {
            self.checkpoint_inner(None)
        } else {
            Ok(())
        };
        let closed = self.close_files();
        self.health = EngineHealth::Closed;
        checkpoint.and(closed)
    }

    /// Closes the files without a checkpoint. Uninstalled durable commits stay in
    /// the WAL and are replayed by the next open.
    pub fn abandon(&mut self) -> Result<()> {
        if self.health == EngineHealth::Closed {
            return Ok(());
        }
        self.txns.clear();
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
    /// The WAL flush makes commits durable. Checkpoint flushes the main file
    /// once for all pages staged since the previous checkpoint.
    /// The WAL is truncated only when memory has applied every flushed commit.
    pub fn checkpoint(&mut self) -> Result<()> {
        self.ensure_healthy()?;
        self.checkpoint_inner(None).inspect_err(|err| {
            self.poison(format!("checkpoint failed: {err}"), None);
        })
    }

    /// Next superblock image. Callers still choose when to write it.
    /// Generation overflow stays `Internal`; recovery uses `Corruption`.
    #[inline]
    fn next_published_superblock(
        current: &SuperblockState,
        catalog_root_page_id: u64,
        next_page_id: u64,
        last_committed_txid: u64,
        last_replayed_wal_offset: u64,
    ) -> Result<SuperblockState> {
        let generation = current
            .generation
            .checked_add(1)
            .ok_or_else(|| EngineError::Internal("superblock generation overflow".into()))?;
        Ok(SuperblockState {
            generation,
            db_id: current.db_id,
            page_size: current.page_size,
            catalog_root_page_id,
            next_page_id,
            last_committed_txid,
            last_replayed_wal_offset,
            active_slot: if current.active_slot == 0 { 1 } else { 0 },
        })
    }

    fn checkpoint_inner(&mut self, known_wal_len: Option<u64>) -> Result<()> {
        self.promote_free_pages();
        if !self.pager.has_dirty() && self.checkpoint_txid == self.superblock.last_committed_txid {
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

        let wal_len = match known_wal_len {
            Some(len) => len,
            None => self.wal.len()?,
        };
        let published = Self::next_published_superblock(
            &self.superblock,
            self.superblock.catalog_root_page_id,
            self.superblock.next_page_id,
            self.superblock.last_committed_txid,
            wal_len,
        )?;
        write_superblock(&mut self.manifest, &published)?;
        self.superblock.generation = published.generation;
        self.superblock.active_slot = published.active_slot;
        self.superblock.last_replayed_wal_offset = published.last_replayed_wal_offset;

        if self.wal_durable_txid == self.superblock.last_committed_txid {
            self.wal.truncate(0)?;
            self.wal.flush()?;
        }
        self.pager.mark_dirty_clean();
        self.checkpoint_txid = published.last_committed_txid;
        Ok(())
    }

    pub fn begin_tx(&mut self, mode: TxMode) -> Result<u64> {
        self.ensure_healthy()?;
        let snapshot = Snapshot::new_shared(
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
            TxMode::Readwrite => TransactionState::new_readwrite(tx_id, snapshot),
        };
        self.txns.insert(tx_id, tx);
        Ok(tx_id)
    }

    /// Allows callers to release transaction handles even when recovery is required.
    pub fn rollback_tx(&mut self, tx_id: u64) -> Result<()> {
        let mut tx = self
            .txns
            .remove(&tx_id)
            .ok_or(EngineError::TransactionClosed)?;
        tx.ensure_open()?;
        tx.closed = true;
        Ok(())
    }

    /// Rejects the commit if any commit advanced this transaction's snapshot.
    /// A conflict closes the transaction before WAL writes. Retry with a new transaction.
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
        let ReadwriteTx {
            snapshot,
            stores,
            staged_schema_version,
            staged_change_feed_policy,
            revision_epoch: _,
        } = write_tx;
        if snapshot.last_committed_txid != self.superblock.last_committed_txid {
            return Err(EngineError::TransactionConflict {
                snapshot_txid: snapshot.last_committed_txid,
                current_txid: self.superblock.last_committed_txid,
            });
        }
        // Release the writer's snapshot before publication. Keeping it would
        // force a catalog copy even with no readers of the current version.
        drop(snapshot);
        self.commit_staged(stores, staged_schema_version, staged_change_feed_policy)
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
        // Other `__browserdb:` names are real stores. This one is the change
        // log: an enabled feed appends to it on later commits.
        if name.as_bytes().first() == Some(&0xff) || name == SYSTEM_CHANGELOG_STORE_NAME {
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
                stage.has_expiring_mutations = Some(false);
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
            stage.has_expiring_mutations = Some(false);
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
            TxInner::Readonly(readonly) => match store_meta(&readonly.snapshot.catalog, store) {
                Ok(meta) => get_committed_visible(
                    &mut self.pager,
                    meta.store_root_page_id,
                    meta.flags,
                    key,
                    now_ms,
                ),
                Err(error) => Err(error),
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
            TxInner::Readonly(readonly) => match store_meta(&readonly.snapshot.catalog, store) {
                Ok(meta) => exists_committed_visible(
                    &mut self.pager,
                    meta.store_root_page_id,
                    meta.flags,
                    key,
                    now_ms,
                ),
                Err(error) => Err(error),
            },
            TxInner::Readwrite(rw) => exists_with_staged(&mut self.pager, rw, store, key, now_ms),
        };
        self.put_tx(tx);
        result
    }

    /// Checks a batch using one TTL timestamp and returns flags in input order.
    /// Committed expiry cleanup is applied only after a successful batch.
    pub fn has_many<K: AsRef<[u8]>>(
        &mut self,
        tx_id: u64,
        store: &str,
        keys: &[K],
    ) -> Result<Vec<bool>> {
        validate_store_name(store)?;
        for key in keys {
            validate_key(key.as_ref())?;
        }
        let now_ms = now_unix_ms()?;
        let mut tx = self.take_tx(tx_id)?;
        let mut order: Vec<usize> = (0..keys.len()).collect();
        order.sort_by(|left, right| keys[*left].as_ref().cmp(keys[*right].as_ref()));
        let mut batch = PointReadBatch::default();
        let result = (|| match &mut tx.inner {
            TxInner::Readonly(readonly) => {
                let meta = store_meta(&readonly.snapshot.catalog, store)?;
                read_many_in_order(keys, &order, |key| {
                    exists_committed_visible_in_batch(
                        &mut self.pager,
                        meta.store_root_page_id,
                        meta.flags,
                        key,
                        now_ms,
                        Some(&mut batch),
                    )
                })
            }
            TxInner::Readwrite(rw) => {
                ensure_readwrite_store_visible(rw, store)?;
                if let Some(stage) = rw.stores.get_mut(store) {
                    normalize_expired_stage_mutations(stage, now_ms);
                }
                let mut expired_base_keys = Vec::new();
                let values = read_many_in_order(keys, &order, |key| {
                    exists_with_staged_deferred_base(
                        &mut self.pager,
                        rw,
                        store,
                        key,
                        now_ms,
                        &mut batch,
                        &mut expired_base_keys,
                    )
                })?;
                for key in expired_base_keys {
                    expire_staged_key(rw, store, &key)?;
                }
                Ok(values)
            }
        })();
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
        let mut order: Vec<usize> = (0..keys.len()).collect();
        order.sort_by(|left, right| keys[*left].as_ref().cmp(keys[*right].as_ref()));
        let mut batch = PointReadBatch::default();
        let result = (|| match &mut tx.inner {
            TxInner::Readonly(readonly) => {
                let meta = store_meta(&readonly.snapshot.catalog, store)?;
                read_many_in_order(keys, &order, |key| {
                    get_committed_visible_in_batch(
                        &mut self.pager,
                        meta.store_root_page_id,
                        meta.flags,
                        key,
                        now_ms,
                        Some(&mut batch),
                    )
                })
            }
            TxInner::Readwrite(rw) => {
                ensure_readwrite_store_visible(rw, store)?;
                read_many_in_order(keys, &order, |key| {
                    get_with_staged_in_batch(
                        &mut self.pager,
                        rw,
                        store,
                        key,
                        now_ms,
                        Some(&mut batch),
                    )
                })
            }
        })();
        self.put_tx(tx);
        result
    }

    /// Reads live value sizes from headers. Does not validate bodies or stage TTL deletes.
    pub fn get_many_value_sizes<K: AsRef<[u8]>>(
        &mut self,
        tx_id: u64,
        store: &str,
        keys: &[K],
    ) -> Result<Vec<Option<usize>>> {
        validate_store_name(store)?;
        for key in keys {
            validate_key(key.as_ref())?;
        }
        let now_ms = now_unix_ms()?;
        let tx = self.take_tx(tx_id)?;
        let mut order: Vec<usize> = (0..keys.len()).collect();
        order.sort_by(|left, right| keys[*left].as_ref().cmp(keys[*right].as_ref()));
        let mut batch = PointReadBatch::default();
        let result = (|| match &tx.inner {
            TxInner::Readonly(readonly) => {
                let meta = readonly
                    .snapshot
                    .catalog
                    .get(store)
                    .ok_or_else(|| EngineError::StoreNotFound(store.into()))?;
                read_many_in_order(keys, &order, |key| {
                    lookup_stored_value_size(
                        &mut self.pager,
                        meta.store_root_page_id,
                        key,
                        meta.flags,
                        now_ms,
                        Some(&mut batch),
                    )
                })
            }
            TxInner::Readwrite(rw) => {
                ensure_readwrite_store_visible(rw, store)?;
                read_many_in_order(keys, &order, |key| {
                    stored_value_size_with_staged(
                        &mut self.pager,
                        rw,
                        store,
                        key,
                        now_ms,
                        &mut batch,
                    )
                })
            }
        })();
        self.put_tx(tx);
        result
    }

    /// Retains physical expired rows in metadata. Does not read bodies or stage TTL deletes.
    pub fn get_many_value_states<K: AsRef<[u8]>>(
        &mut self,
        tx_id: u64,
        store: &str,
        keys: &[K],
    ) -> Result<Vec<Option<ValueState>>> {
        validate_store_name(store)?;
        for key in keys {
            validate_key(key.as_ref())?;
        }
        let now_ms = now_unix_ms()?;
        let tx = self.take_tx(tx_id)?;
        let mut order: Vec<usize> = (0..keys.len()).collect();
        order.sort_by(|left, right| keys[*left].as_ref().cmp(keys[*right].as_ref()));
        let mut batch = PointReadBatch::default();
        let result = (|| match &tx.inner {
            TxInner::Readonly(readonly) => {
                let meta = readonly
                    .snapshot
                    .catalog
                    .get(store)
                    .ok_or_else(|| EngineError::StoreNotFound(store.into()))?;
                read_many_in_order(keys, &order, |key| {
                    lookup_stored_value_state(
                        &mut self.pager,
                        meta.store_root_page_id,
                        key,
                        meta.flags,
                        now_ms,
                        Some(&mut batch),
                    )
                })
            }
            TxInner::Readwrite(rw) => {
                ensure_readwrite_store_visible(rw, store)?;
                read_many_in_order(keys, &order, |key| {
                    stored_value_state_with_staged(
                        &mut self.pager,
                        rw,
                        store,
                        key,
                        now_ms,
                        &mut batch,
                    )
                })
            }
        })();
        self.put_tx(tx);
        result
    }

    pub fn get_many_packed<K: AsRef<[u8]>>(
        &mut self,
        tx_id: u64,
        store: &str,
        keys: &[K],
    ) -> Result<Vec<u8>> {
        validate_store_name(store)?;
        for key in keys {
            validate_key(key.as_ref())?;
        }
        let now_ms = now_unix_ms()?;
        let mut tx = self.take_tx(tx_id)?;
        let mut order: Vec<usize> = (0..keys.len()).collect();
        order.sort_by(|left, right| keys[*left].as_ref().cmp(keys[*right].as_ref()));
        let mut batch = PointReadBatch::default();
        let result = (|| {
            // Metadata fixes every output slot before sorted reads fill values in input order.
            let metadata = match &tx.inner {
                TxInner::Readonly(readonly) => {
                    let meta = readonly
                        .snapshot
                        .catalog
                        .get(store)
                        .ok_or_else(|| EngineError::StoreNotFound(store.into()))?;
                    read_many_in_order(keys, &order, |key| {
                        lookup_stored_value_info(
                            &mut self.pager,
                            meta.store_root_page_id,
                            key,
                            meta.flags,
                            Some(&mut batch),
                        )
                    })?
                }
                TxInner::Readwrite(rw) => {
                    ensure_readwrite_store_visible(rw, store)?;
                    read_many_in_order(keys, &order, |key| {
                        stored_value_info_with_staged(&mut self.pager, rw, store, key, &mut batch)
                    })?
                }
            };
            let lengths: Vec<Option<usize>> = metadata
                .iter()
                .map(|info| {
                    info.and_then(|(length, expiry)| {
                        (!matches!(expiry, Some(timestamp) if now_ms >= timestamp))
                            .then_some(length)
                    })
                })
                .collect();
            let mut packet = PackedValues::new(&lengths)?;
            let mut start = 0;
            while start < order.len() {
                let index = order[start];
                let key = keys[index].as_ref();
                let mut end = start + 1;
                while end < order.len() && keys[order[end]].as_ref() == key {
                    end += 1;
                }
                let mut writer = packet.writer(index, lengths[index].unwrap_or(0));
                let mut write = |chunk: &[u8]| {
                    if lengths[index].is_some() {
                        writer.write(chunk)
                    } else {
                        Ok(())
                    }
                };
                let expiry = match &mut tx.inner {
                    TxInner::Readonly(readonly) => {
                        let meta = readonly
                            .snapshot
                            .catalog
                            .get(store)
                            .ok_or_else(|| EngineError::StoreNotFound(store.into()))?;
                        lookup_stored_value_into(
                            &mut self.pager,
                            meta.store_root_page_id,
                            key,
                            meta.flags,
                            Some(&mut batch),
                            &mut write,
                        )?
                    }
                    TxInner::Readwrite(rw) => stored_value_into_with_staged(
                        &mut self.pager,
                        rw,
                        store,
                        key,
                        now_ms,
                        &mut batch,
                        &mut write,
                    )?,
                };
                if expiry != metadata[index].map(|(_, expiry)| expiry) {
                    return Err(EngineError::Corruption(
                        "value metadata changed during packed read".into(),
                    ));
                }
                writer.finish()?;
                if let Some(length) = lengths[index] {
                    for duplicate in &order[start + 1..end] {
                        packet.duplicate(index, *duplicate, length);
                    }
                }
                start = end;
            }
            Ok(packet.bytes)
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

    #[allow(clippy::too_many_arguments)]
    /// Checks the stored revision. Expired rows remain eligible until cleanup.
    /// Read the expected revision from the primary row in this transaction.
    /// Derive the logical index projection from that row. This checks the
    /// primary-key binding and revision, not the document fields.
    pub fn put_index_entry_checked(
        &mut self,
        tx_id: u64,
        primary_store: &str,
        index_store: &str,
        primary_key: &[u8],
        index_key: &[u8],
        expected_epoch: u64,
        expected_ordinal: u64,
    ) -> Result<()> {
        validate_store_name(primary_store)?;
        validate_store_name(index_store)?;
        validate_key(primary_key)?;
        validate_key(index_key)?;
        validate_index_entry_binding(index_store, index_key, primary_key)?;
        let expected = ValueRevision {
            epoch: expected_epoch,
            ordinal: expected_ordinal,
        };
        let now_ms = now_unix_ms()?;
        let mut tx = self.take_tx(tx_id)?;
        let result = tx.readwrite_mut().and_then(|rw| {
            let state = stored_value_state_with_staged(
                &mut self.pager,
                rw,
                primary_store,
                primary_key,
                now_ms,
                &mut PointReadBatch::default(),
            )?;
            if expected.epoch == 0
                || expected.ordinal == 0
                || state.and_then(|state| state.revision) != Some(expected)
            {
                return Err(EngineError::ValueRevisionConflict);
            }
            if expected.epoch > rw.snapshot.last_committed_txid
                && Some(expected.epoch) != rw.revision_epoch
            {
                return Err(EngineError::Corruption(
                    "primary revision is newer than its transaction".into(),
                ));
            }
            validate_readwrite_store_value(rw, index_store, &[])?;
            put_with_staged_at(
                &mut self.pager,
                &mut self.next_value_revision_ordinal,
                rw,
                index_store,
                index_key,
                StoredValue::plain(Vec::new()).with_revision(expected),
                now_ms,
            )
            .map(|_| ())
        });
        self.put_tx(tx);
        result
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
        validate_store_value_max_size(value)?;
        let operation_now_ms = now_unix_ms()?;
        let expires_at_ms = absolute_expiry_from_ttl_at(ttl_ms, operation_now_ms)?;
        let mut tx = self.take_tx(tx_id)?;
        let result = tx.readwrite_mut().and_then(|rw| {
            validate_readwrite_store_value(rw, store, value)?;
            self.ensure_revision_epoch(rw)?;
            put_with_staged_at(
                &mut self.pager,
                &mut self.next_value_revision_ordinal,
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
                .and_then(|_| validate_store_value_max_size(value))
                .and_then(|_| tx.readwrite_mut())
                .and_then(|rw| {
                    validate_readwrite_store_value(rw, store, value)?;
                    self.ensure_revision_epoch(rw)?;
                    put_with_staged_at(
                        &mut self.pager,
                        &mut self.next_value_revision_ordinal,
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

    /// Completes each primary put and its index writes before starting the next row.
    /// On failure, earlier writes in the failed row remain staged, but that row
    /// has no completed outcome.
    pub fn put_many_indexed_report<K: AsRef<[u8]>, V: AsRef<[u8]>>(
        &mut self,
        tx_id: u64,
        store: &str,
        entries: &[(K, V)],
        index_ops: &[Vec<IndexOpRef<'_>>],
        ttl_ms: Option<u64>,
    ) -> BatchExecutionReport<bool> {
        if entries.len() != index_ops.len() {
            return BatchExecutionReport::failure(
                Vec::new(),
                EngineError::Serialization("indexed put row count mismatch".into()),
            );
        }
        let mut tx: Option<TransactionState> = None;
        let mut completed = Vec::with_capacity(entries.len());
        for ((key, value), row_ops) in entries.iter().zip(index_ops) {
            let (key, value) = (key.as_ref(), value.as_ref());
            let result = (|| {
                validate_store_name(store)?;
                validate_key(key)?;
                validate_store_value_max_size(value)?;
                let now_ms = now_unix_ms()?;
                let expires_at_ms = absolute_expiry_from_ttl_at(ttl_ms, now_ms)?;
                if tx.is_none() {
                    tx = Some(self.take_tx(tx_id)?);
                }
                let rw = tx
                    .as_mut()
                    .ok_or_else(|| EngineError::Internal("indexed put transaction missing".into()))?
                    .readwrite_mut()?;
                validate_readwrite_store_value(rw, store, value)?;
                self.ensure_revision_epoch(rw)?;
                let baseline_exists = put_with_staged_at(
                    &mut self.pager,
                    &mut self.next_value_revision_ordinal,
                    rw,
                    store,
                    key,
                    StoredValue::with_expiry(value.to_vec(), expires_at_ms),
                    now_ms,
                )?;
                let revision = match staged_lookup(rw, store, key)? {
                    StagedLookup::Staged(Some(value)) => value.revision,
                    _ => None,
                };
                for op in row_ops {
                    let (index_store, index_key) = match op {
                        IndexOpRef::Put { store, key } | IndexOpRef::Delete { store, key } => {
                            (*store, *key)
                        }
                    };
                    validate_store_name(index_store)?;
                    validate_key(index_key)?;
                    let now_ms = now_unix_ms()?;
                    match op {
                        IndexOpRef::Put { .. } => {
                            validate_readwrite_store_value(rw, index_store, &[])?;
                            let mut index_value = StoredValue::plain(Vec::new());
                            // Only bound SDK index entries may share a primary revision.
                            if index_store.starts_with("__browserdb:index:") {
                                validate_index_entry_binding(index_store, index_key, key)?;
                                index_value =
                                    index_value.with_revision(revision.ok_or_else(|| {
                                        EngineError::Internal(
                                            "indexed primary revision missing".into(),
                                        )
                                    })?);
                            }
                            put_with_staged_at(
                                &mut self.pager,
                                &mut self.next_value_revision_ordinal,
                                rw,
                                index_store,
                                index_key,
                                index_value,
                                now_ms,
                            )?;
                        }
                        IndexOpRef::Delete { .. } => {
                            delete_with_staged_at(
                                &mut self.pager,
                                rw,
                                index_store,
                                index_key,
                                now_ms,
                            )?;
                        }
                    }
                }
                Ok(baseline_exists)
            })();
            match result {
                Ok(baseline_exists) => completed.push(baseline_exists),
                Err(error) => {
                    return match tx {
                        Some(tx) => self.batch_failure(tx, completed, error),
                        None => BatchExecutionReport::failure(completed, error),
                    };
                }
            }
        }
        if let Some(tx) = tx {
            self.put_tx(tx);
        }
        BatchExecutionReport::success(completed)
    }

    pub fn put_many_indexed<K: AsRef<[u8]>, V: AsRef<[u8]>>(
        &mut self,
        tx_id: u64,
        store: &str,
        entries: &[(K, V)],
        index_ops: &[Vec<IndexOpRef<'_>>],
        ttl_ms: Option<u64>,
    ) -> Result<Vec<bool>> {
        self.put_many_indexed_report(tx_id, store, entries, index_ops, ttl_ms)
            .into_result()
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
        let (operation_now_ms, mut tx) = match self.begin_visible_batch(tx_id, store) {
            Ok(ready) => ready,
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
        let (operation_now_ms, mut tx) = match self.begin_visible_batch(tx_id, store) {
            Ok(ready) => ready,
            Err(error) => return BatchExecutionReport::failure(Vec::new(), error),
        };

        let mut completed = Vec::with_capacity(ops.len());
        for op in ops {
            let result = match *op {
                BatchOpRef::Put { key, value } => validate_key(key)
                    .and_then(|_| validate_store_value_max_size(value))
                    .and_then(|_| tx.readwrite_mut())
                    .and_then(|rw| {
                        validate_readwrite_store_value(rw, store, value)?;
                        self.ensure_revision_epoch(rw)?;
                        put_with_staged_at(
                            &mut self.pager,
                            &mut self.next_value_revision_ordinal,
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
            TxInner::Readonly(readonly) => match store_meta(&readonly.snapshot.catalog, store) {
                Ok(meta) => scan_committed_visible(
                    &mut self.pager,
                    meta.store_root_page_id,
                    meta.flags,
                    range,
                    now_ms,
                ),
                Err(error) => Err(error),
            },
            TxInner::Readwrite(rw) => scan_with_staged(&mut self.pager, rw, store, range, now_ms),
        };
        self.put_tx(tx);
        result
    }

    pub fn scan_packed_page(
        &mut self,
        tx_id: u64,
        store: &str,
        range: &ScanRange,
        max_rows: usize,
        max_bytes: usize,
        keys_only: bool,
    ) -> Result<PackedScanPage> {
        validate_store_name(store)?;
        range.validate()?;
        if max_rows == 0 {
            return Err(EngineError::InvalidRange(
                "scan row budget must be positive".into(),
            ));
        }
        let mut bounded = range.clone();
        bounded.limit = Some(range.limit.unwrap_or(usize::MAX).min(max_rows));
        let now_ms = now_unix_ms()?;
        let mut packet = ScanPacket::new(max_bytes)?;
        let mut tx = self.take_tx(tx_id)?;
        let result = (|| {
            let mut append = |pager: &mut Pager<B>, key: Vec<u8>, value: VisibleValue<'_>| {
                let length = if keys_only {
                    0
                } else {
                    match &value {
                        VisibleValue::Staged(stored, _) => stored.value.len(),
                        VisibleValue::Committed(pending, flags) => {
                            pending_stored_value_info(pager, pending, *flags)?.0
                        }
                    }
                };
                let decoded_length = if keys_only {
                    0
                } else {
                    match &value {
                        VisibleValue::Staged(stored, flags) => {
                            decoded_scan_value_length(&stored.value, *flags, length)?
                        }
                        VisibleValue::Committed(pending, flags) => {
                            let offset = store_value_prefix_len(*flags);
                            if store_compression_from_flags(*flags)? == StoreCompression::None {
                                length
                            } else {
                                let prefix = pending_value_prefix(
                                    pager,
                                    pending,
                                    offset + COMPRESSION_VALUE_HEADER_SIZE,
                                )?;
                                decoded_scan_value_length(&prefix[offset..], *flags, length)?
                            }
                        }
                    }
                };
                if !packet.begin_row(&key, length, decoded_length)? {
                    return Ok(false);
                }
                if !keys_only {
                    match value {
                        VisibleValue::Staged(stored, _) => {
                            packet.bytes.extend_from_slice(&stored.value)
                        }
                        VisibleValue::Committed(pending, flags) => {
                            write_pending_stored_value(pager, pending, flags, |chunk| {
                                packet.bytes.extend_from_slice(chunk);
                                Ok(())
                            })?;
                        }
                    }
                }
                packet.finish_row(&key);
                Ok(true)
            };
            let exhausted = match &mut tx.inner {
                TxInner::Readonly(readonly) => {
                    let meta = readonly
                        .snapshot
                        .catalog
                        .get(store)
                        .ok_or_else(|| EngineError::StoreNotFound(store.into()))?;
                    visit_committed_visible(
                        &mut self.pager,
                        meta.store_root_page_id,
                        meta.flags,
                        &bounded,
                        now_ms,
                        &mut append,
                    )?
                }
                TxInner::Readwrite(rw) => visit_with_staged(
                    &mut self.pager,
                    rw,
                    store,
                    &bounded,
                    now_ms,
                    false,
                    &mut append,
                )?,
            };
            packet.finish(exhausted)
        })();
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
            write_tx_open: self.txns.values().any(|tx| tx.mode == TxMode::Readwrite),
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
            let total_len = match &pair.value {
                PendingValue::Inline(value) => value.len(),
                PendingValue::Overflow { total_len, .. } => *total_len,
                PendingValue::External { payload, prefix } => {
                    prefix.len() + payload.body_len as usize
                }
            };
            let mut prefix = Vec::with_capacity(total_len.min(MAX_CHANGE_RECORD_PREFIX_SIZE));
            let mut selected = None;
            let mut value: Option<Vec<u8>> = None;
            let mut metadata_error = None;
            // Complete body validation before skipping records or reporting metadata errors.
            write_pending_stored_value(
                &mut self.pager,
                pair.value,
                CHANGELOG_STORE_FLAGS,
                |chunk| {
                    if let Some(selected) = selected {
                        if selected {
                            if let Some(value) = value.as_mut() {
                                value.extend_from_slice(chunk);
                            }
                        }
                        return Ok(());
                    }
                    if metadata_error.is_some() {
                        return Ok(());
                    }
                    let take = (MAX_CHANGE_RECORD_PREFIX_SIZE - prefix.len()).min(chunk.len());
                    prefix.extend_from_slice(&chunk[..take]);
                    match decode_change_record_prefix_ref(&prefix, total_len) {
                        Ok(Some(record)) => {
                            let include = store_filter
                                .as_ref()
                                .is_none_or(|filter| filter.contains(record.store));
                            selected = Some(include);
                            if include && record.kind == ChangeKind::Put {
                                let mut body = Vec::new();
                                body.try_reserve_exact(record.value_len).map_err(|error| {
                                    EngineError::Storage(format!(
                                        "change value allocation failed: {error}"
                                    ))
                                })?;
                                body.extend_from_slice(record.value_prefix);
                                body.extend_from_slice(&chunk[take..]);
                                value = Some(body);
                            }
                        }
                        Ok(None) => {}
                        Err(error) => metadata_error = Some(error),
                    }
                    Ok(())
                },
            )?;
            if let Some(error) = metadata_error {
                return Err(error);
            }
            let record = decode_change_record_prefix_ref(&prefix, total_len)?.ok_or_else(|| {
                EngineError::Corruption("change log payload prefix is truncated".into())
            })?;
            if selected == Some(false) {
                continue;
            }
            changes.push(ChangeRecord {
                tx_id: record_txid,
                store: record.store.to_owned(),
                key: record.key.to_vec(),
                kind: record.kind,
                value,
            });
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
        let snapshot = Snapshot::new_shared(
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
        self.import_decoded_snapshot(decode_snapshot(bytes)?)
    }

    pub(crate) fn import_decoded_snapshot(&mut self, snapshot: SnapshotContents) -> Result<u64> {
        self.ensure_healthy()?;
        if !self.txns.is_empty() {
            return Err(EngineError::DatabaseBusy(
                "cannot import snapshot while transactions are open".into(),
            ));
        }
        self.apply_snapshot_contents(snapshot)
    }

    /// Imports into a new, empty target. Keeps this engine's commit floor and feed policy.
    /// Activate the target only after the import and all derived store writes succeed.
    pub fn import_snapshot_into(&mut self, target: &mut Engine<B>, bytes: &[u8]) -> Result<u64> {
        self.import_snapshot_contents_into(target, decode_snapshot(bytes)?)
    }

    pub(crate) fn import_snapshot_contents_into(
        &mut self,
        target: &mut Engine<B>,
        snapshot: SnapshotContents,
    ) -> Result<u64> {
        self.ensure_healthy()?;
        target.ensure_healthy()?;
        if !self.txns.is_empty() || !target.txns.is_empty() {
            return Err(EngineError::DatabaseBusy(
                "cannot import snapshot while transactions are open".into(),
            ));
        }
        if target.superblock.last_committed_txid != 0
            || !target.catalog.is_empty()
            || target.pager.has_dirty()
        {
            return Err(EngineError::Internal(
                "snapshot target must be a new, empty database".into(),
            ));
        }
        target.next_commit_txid = self.next_commit_txid;
        target.change_feed_policy = self.change_feed_policy;
        target.apply_snapshot_contents(snapshot)
    }

    #[cfg(any(target_arch = "wasm32", test))]
    pub(crate) fn next_commit_txid(&self) -> u64 {
        self.next_commit_txid
    }

    #[cfg(any(target_arch = "wasm32", test))]
    pub(crate) fn next_transaction_handle(&self) -> u64 {
        self.next_tx_id
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

    /// Bulk-loads live contents into a new, empty `target` without using its WAL.
    /// Streams one store at a time with memory proportional to a page run.
    ///
    /// The caller must activate the target generation only after this succeeds.
    pub fn compact_into(&mut self, target: &mut Engine<B>) -> Result<u64> {
        self.compact_into_skipping_stores(target, &[])
    }

    /// Compacts into a new, empty target. Omits the change log and the named
    /// internal stores. SDK rebuild uses this before regenerating its indexes.
    pub fn compact_into_skipping_stores(
        &mut self,
        target: &mut Engine<B>,
        skip_stores: &[String],
    ) -> Result<u64> {
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
        let mut skipped_stores = std::collections::BTreeSet::new();
        for name in skip_stores {
            validate_store_name(name)?;
            if !is_internal_store_name(name) {
                return Err(EngineError::InvalidRange(format!(
                    "only internal stores may be skipped during compaction: {name}"
                )));
            }
            skipped_stores.insert(name.as_str());
        }
        let result = self.compact_into_inner(target, &skipped_stores);
        if let Err(err) = &result {
            target.poison(format!("compaction failed: {err}"), None);
        }
        result
    }

    fn compact_into_inner(
        &mut self,
        target: &mut Engine<B>,
        skipped_stores: &std::collections::BTreeSet<&str>,
    ) -> Result<u64> {
        let now_ms = now_unix_ms()?;
        let new_txid = self
            .superblock
            .last_committed_txid
            .checked_add(1)
            .ok_or_else(|| EngineError::Internal("commit txid overflow".into()))?;
        let mut alloc = PageAllocator::new(target.superblock.next_page_id);
        let mut payloads = PayloadPreparation {
            registry: PayloadRegistry::new(0),
            oldest_snapshot_txid: 0,
            pending_writes: false,
        };
        let mut stores = CatalogMap::new();
        let mut page_batch = Vec::with_capacity(crate::pager::PAGE_WRITE_BATCH_PAGES);
        let mut page_buffer = Vec::new();
        // Retain internal stores unless explicitly skipped. Omit the system
        // change log. A user store that only shares its name is ordinary data.
        // The target feed floor excludes the source history.
        for (name, meta) in self.catalog.iter() {
            if skipped_stores.contains(name.as_str())
                || name.as_str() == PAYLOAD_REGISTRY_STORE_NAME
                || (name.as_str() == SYSTEM_CHANGELOG_STORE_NAME
                    && store_uses_system_raw_values(meta.flags))
            {
                continue;
            }
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
                let raw = materialize_pending_value(&mut self.pager, pair.value)?;
                let stored = StoredValue::decode_owned_for_store(meta.flags, raw)?;
                let mut value = PreparedValue::stored(&stored, meta.flags)?;
                payloads.externalize(&mut target.pager, &mut alloc, &mut value)?;
                builder.push_value(&pair.key, &value, &mut alloc)?;
                queue_compaction_images(
                    &mut target.pager,
                    &mut page_batch,
                    &mut page_buffer,
                    builder.drain_images(),
                )?;
            }
            let built = builder.finish(&mut alloc)?;
            queue_compaction_images(
                &mut target.pager,
                &mut page_batch,
                &mut page_buffer,
                built.page_images,
            )?;
            stores.insert(
                name.clone(),
                StoreMetadata {
                    store_root_page_id: built.root_page_id,
                    created_txid: new_txid,
                    flags: meta.flags,
                },
            );
        }

        payloads.registry.apply_reference_changes(
            alloc.added_payload_refs(),
            alloc.dropped_payload_refs(),
            new_txid,
            &mut target.pager,
        )?;
        if let Some(built) = payloads.registry.finish(&mut target.pager, &mut alloc)? {
            queue_compaction_images(
                &mut target.pager,
                &mut page_batch,
                &mut page_buffer,
                built.page_images,
            )?;
            stores.insert(
                PAYLOAD_REGISTRY_STORE_NAME.into(),
                StoreMetadata {
                    store_root_page_id: built.root_page_id,
                    created_txid: new_txid,
                    flags: STORE_FLAG_SYSTEM_RAW_VALUES,
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
        queue_compaction_images(
            &mut target.pager,
            &mut page_batch,
            &mut page_buffer,
            catalog.page_images,
        )?;
        target
            .pager
            .write_page_images(&mut page_batch, &mut page_buffer)?;
        // Flush tree pages and pending payload bodies before manifest publication.
        target.pager.flush()?;

        let published = Self::next_published_superblock(
            &target.superblock,
            catalog.root_page_id,
            alloc.next_page_id(),
            new_txid,
            0,
        )?;
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

    fn promote_free_pages(&mut self) {
        let oldest_snapshot_txid = self.oldest_snapshot_txid();
        let pager = &mut self.pager;
        self.free_pages
            .promote(oldest_snapshot_txid, |page_id, generation| {
                pager.discard_dirty_page_if_generation(page_id, generation);
            });
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
        self.promote_free_pages();
        let reusable = self.free_pages.take_ready();
        let mut alloc =
            PageAllocator::with_reusable(self.superblock.next_page_id, reusable.clone());

        let view = CommittedView {
            catalog: &self.catalog,
            catalog_root_page_id: self.superblock.catalog_root_page_id,
            schema_version: self.schema_version,
            change_feed_floor_txid: self.change_feed_floor_txid,
            change_feed_policy: self.change_feed_policy,
            oldest_snapshot_txid: self.oldest_snapshot_txid(),
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
        self.promote_free_pages();
        let reusable = self.free_pages.take_ready();
        let mut alloc =
            PageAllocator::with_reusable(self.superblock.next_page_id, reusable.clone());
        let catalog = self.catalog.clone();
        let view = CommittedView {
            catalog: &catalog,
            catalog_root_page_id: self.superblock.catalog_root_page_id,
            schema_version: self.schema_version,
            change_feed_floor_txid: self.change_feed_floor_txid,
            change_feed_policy: self.change_feed_policy,
            oldest_snapshot_txid: self.oldest_snapshot_txid(),
        };
        let plan = plan_snapshot_apply(
            &mut self.pager,
            &view,
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
        // Match the retired image, even if this commit stages the same page id.
        let dirty_images = freed
            .iter()
            .filter_map(|page_id| {
                self.pager
                    .dirty_page_generation(*page_id)
                    .map(|generation| (*page_id, generation))
            })
            .collect();
        match self.finish_commit(plan, next_page_id) {
            Ok((txid, wal_offset)) => {
                // Roots are published. Checkpoint must see their retirements.
                self.free_pages.restore(unused);
                self.free_pages.retire(new_txid, freed, dirty_images);
                self.promote_free_pages();
                if let Err(err) = self.maybe_checkpoint(wal_offset) {
                    self.poison(
                        format!("checkpoint after commit failed: {err}"),
                        Some(new_txid),
                    );
                    return Err(err);
                }
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

    fn ensure_revision_epoch(&self, rw: &mut ReadwriteTx) -> Result<()> {
        if rw.revision_epoch.is_none() {
            // Peeking keeps rollback out of feed retention. Global ordinals separate writers.
            self.next_commit_txid
                .checked_add(1)
                .ok_or_else(|| EngineError::Internal("commit txid overflow".into()))?;
            rw.revision_epoch = Some(self.next_commit_txid);
        }
        Ok(())
    }

    fn reserve_commit_txid_at_least(&mut self, minimum: u64) -> Result<u64> {
        let new_txid = self.next_commit_txid.max(minimum);
        self.next_commit_txid = new_txid
            .checked_add(1)
            .ok_or_else(|| EngineError::Internal("commit txid overflow".into()))?;
        Ok(new_txid)
    }

    fn finish_commit(&mut self, plan: CommitPlan, next_page_id: u64) -> Result<(u64, u64)> {
        let CommitPlan {
            new_txid,
            catalog_update,
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

        let mut wal_offset = self.wal.append_offset()?;
        // After WAL writes start, a failure makes the commit outcome unknown.
        // Recovery must determine whether the commit is durable.
        let appended = append_generated_transaction(
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
        catalog_update.publish(&mut self.catalog);
        self.change_feed_floor_txid = final_change_feed_floor_txid;
        self.change_feed_policy = final_change_feed_policy;
        Ok((new_txid, wal_offset))
    }

    fn maybe_checkpoint(&mut self, wal_len: u64) -> Result<()> {
        if !self.checkpoint_failpoint_armed()
            && self.pager.dirty_page_count() < self.checkpoint_dirty_pages
            && wal_len < self.checkpoint_wal_bytes
        {
            return Ok(());
        }
        self.checkpoint_inner(Some(wal_len))
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

    /// Store check, one clock read, then the visible writer. TTL expiry stays
    /// outside this helper: `put_many` must reject overflow before taking the tx.
    #[inline]
    fn begin_visible_batch(&mut self, tx_id: u64, store: &str) -> Result<(u64, TransactionState)> {
        validate_user_store_name(store)?;
        let now_ms = now_unix_ms()?;
        let tx = self.take_visible_readwrite_tx(tx_id, store)?;
        Ok((now_ms, tx))
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
    oldest_snapshot_txid: u64,
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
    let mut catalog_delta = CatalogDelta::default();
    let final_schema_version = staged_schema_version.unwrap_or(view.schema_version);
    let final_policy = staged_policy.unwrap_or(view.change_feed_policy);
    let mut page_images = Vec::new();
    let mut change_payloads = Vec::new();
    let registry_meta = view.catalog.get(PAYLOAD_REGISTRY_STORE_NAME);
    if registry_meta.is_some_and(|meta| !store_uses_system_raw_values(meta.flags)) {
        return Err(EngineError::ReservedStoreName(
            PAYLOAD_REGISTRY_STORE_NAME.into(),
        ));
    }
    let mut payloads = PayloadPreparation {
        registry: PayloadRegistry::new(registry_meta.map_or(0, |meta| meta.store_root_page_id)),
        oldest_snapshot_txid: view.oldest_snapshot_txid,
        pending_writes: false,
    };

    for (name, stage) in staged {
        if !stage.has_changes() {
            continue;
        }
        let change_start = change_payloads.len();
        if final_policy.enabled && !is_internal_store_name(name) {
            collect_change_payloads(pager, name, stage, now_ms, &mut change_payloads)?;
        }
        if stage.dropped {
            if let Some(meta) = stage.base_meta.as_ref() {
                free_tree(pager, meta.store_root_page_id, alloc)?;
            }
            catalog_delta.remove(view.catalog, name);
            continue;
        }
        let Some(built) = build_store_commit(
            pager,
            stage,
            &mut change_payloads[change_start..],
            now_ms,
            alloc,
            &mut payloads,
        )?
        else {
            continue;
        };
        page_images.extend(built.page_images);
        let created_txid = match (stage.created, stage.base_meta.as_ref()) {
            (false, Some(meta)) => meta.created_txid,
            _ => new_txid,
        };
        catalog_delta.set(
            view.catalog,
            name,
            Some(StoreMetadata {
                store_root_page_id: built.root_page_id,
                created_txid,
                flags: stage.flags,
            }),
        );
    }

    let mut floor = view.change_feed_floor_txid;
    if final_policy.enabled {
        if !view.change_feed_policy.enabled {
            // History restarts with this commit.
            floor = new_txid.saturating_sub(1);
        }
        let log_meta = catalog_delta
            .get(view.catalog, SYSTEM_CHANGELOG_STORE_NAME)
            .cloned();
        // A legacy user store can already occupy the log name. Appending here
        // would reinterpret its tree and replace its flags.
        if log_meta
            .as_ref()
            .is_some_and(|meta| !store_uses_system_raw_values(meta.flags))
        {
            return Err(EngineError::ReservedStoreName(
                SYSTEM_CHANGELOG_STORE_NAME.into(),
            ));
        }
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
                    let mut mutations: Vec<(&[u8], Option<&PreparedValue<'_>>)> = prune_keys
                        .iter()
                        .map(|key| (key.as_slice(), None))
                        .collect();
                    mutations.extend(
                        append_keys
                            .iter()
                            .zip(&change_payloads)
                            .map(|(key, payload)| (key.as_slice(), Some(&payload.value))),
                    );
                    apply_value_mutations(pager, meta.store_root_page_id, &mutations, alloc)?
                }
                None => build_tree_from_values(
                    append_keys
                        .iter()
                        .zip(&change_payloads)
                        .map(|(key, payload)| (key.as_slice(), &payload.value)),
                    alloc,
                )?,
            };
            page_images.extend(built.page_images);
            catalog_delta.set(
                view.catalog,
                SYSTEM_CHANGELOG_STORE_NAME,
                Some(StoreMetadata {
                    store_root_page_id: built.root_page_id,
                    created_txid: log_meta.map(|meta| meta.created_txid).unwrap_or(new_txid),
                    flags: CHANGELOG_STORE_FLAGS,
                }),
            );
        }
    } else if let Some(meta) = catalog_delta.remove(view.catalog, SYSTEM_CHANGELOG_STORE_NAME) {
        free_tree(pager, meta.store_root_page_id, alloc)?;
    }

    payloads.flush_pending(pager)?;
    payloads.registry.apply_reference_changes(
        alloc.added_payload_refs(),
        alloc.dropped_payload_refs(),
        new_txid,
        pager,
    )?;
    if let Some(built) = payloads.registry.finish(pager, alloc)? {
        page_images.extend(built.page_images);
        catalog_delta.set(
            view.catalog,
            PAYLOAD_REGISTRY_STORE_NAME,
            Some(StoreMetadata {
                store_root_page_id: built.root_page_id,
                created_txid: registry_meta.map_or(new_txid, |meta| meta.created_txid),
                flags: STORE_FLAG_SYSTEM_RAW_VALUES,
            }),
        );
    }

    let catalog_changed = !catalog_delta.is_empty()
        || final_schema_version != view.schema_version
        || floor != view.change_feed_floor_txid
        || final_policy != view.change_feed_policy;
    let catalog_root_page_id = if catalog_changed {
        let built = catalog_delta.build_tree(
            pager,
            view,
            (final_schema_version, floor, final_policy),
            alloc,
        )?;
        page_images.extend(built.page_images);
        built.root_page_id
    } else {
        view.catalog_root_page_id
    };

    Ok(CommitPlan {
        new_txid,
        catalog_update: CatalogUpdate::Delta(catalog_delta),
        final_schema_version,
        final_change_feed_floor_txid: floor,
        final_change_feed_policy: final_policy,
        page_images,
        catalog_root_page_id,
    })
}

/// Builds the new tree of one changed store, or `None` if it is unchanged.
fn build_store_commit<'a, B: FileBackend>(
    pager: &mut Pager<B>,
    stage: &'a StagedStore,
    changes: &mut [PreparedChange<'a>],
    now_ms: u64,
    alloc: &mut PageAllocator,
    payloads: &mut PayloadPreparation,
) -> Result<Option<BuiltTree>> {
    let base = stage.base_meta.as_ref();
    if stage.created || stage.cleared || base.is_none() {
        if stage.cleared {
            if let Some(base) = base {
                free_tree(pager, base.store_root_page_id, alloc)?;
            }
        }
        let encoded = prepare_stage_puts(stage, changes, pager, alloc, payloads)?;
        return build_tree_from_values(encoded.iter().map(|(key, value)| (*key, value)), alloc)
            .map(Some);
    }
    let base = base.ok_or_else(|| EngineError::Internal("missing base metadata".into()))?;
    if stage.force_full_rewrite {
        return rewrite_store_fully(pager, base, stage, changes, now_ms, alloc, payloads).map(Some);
    }
    if stage.mutations.is_empty() {
        return Ok(None);
    }
    let encoded = prepare_stage_mutations(stage, changes, pager, alloc, payloads)?;
    let mutations: Vec<_> = encoded
        .iter()
        .map(|(key, value)| (*key, value.as_ref()))
        .collect();
    apply_value_mutations(pager, base.store_root_page_id, &mutations, alloc).map(Some)
}

/// Re-encodes a legacy store after a TTL write enables value envelopes.
/// Merges the old tree and staged mutations in key order, then retires the old tree.
fn rewrite_store_fully<'a, B: FileBackend>(
    pager: &mut Pager<B>,
    base: &StoreMetadata,
    stage: &'a StagedStore,
    changes: &mut [PreparedChange<'a>],
    now_ms: u64,
    alloc: &mut PageAllocator,
    payloads: &mut PayloadPreparation,
) -> Result<BuiltTree> {
    let mut builder = SortedTreeBuilder::new();
    let mut staged = stage.mutations.iter().peekable();
    let mut change_puts = changes
        .iter_mut()
        .filter(|change| change.kind == ChangeKind::Put);
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
                let value = prepare_staged_value(
                    stored,
                    stage.flags,
                    change_puts.next(),
                    pager,
                    alloc,
                    payloads,
                )?;
                builder.push_value(key, &value, alloc)?;
            }
        } else if let Some(pair) = base_next.take() {
            let raw = materialize_pending_value(pager, pair.value)?;
            let stored = StoredValue::decode_owned_for_store(base.flags, raw)?;
            if !stored.is_expired_at(now_ms) {
                let mut value = PreparedValue::stored(&stored, stage.flags)?;
                payloads.externalize(pager, alloc, &mut value)?;
                builder.push_value(&pair.key, &value, alloc)?;
            }
            base_next = iter.next(pager)?;
        }
    }
    free_tree(pager, base.store_root_page_id, alloc)?;
    builder.finish(alloc)
}

fn queue_compaction_images<B: FileBackend>(
    pager: &mut Pager<B>,
    batch: &mut PageImages,
    buffer: &mut Vec<u8>,
    images: PageImages,
) -> Result<()> {
    for (page_id, bytes) in images {
        if batch.len() == crate::pager::PAGE_WRITE_BATCH_PAGES
            || batch
                .last()
                .is_some_and(|(previous, _)| previous.checked_add(1) != Some(page_id))
        {
            pager.write_page_images(batch, buffer)?;
        }
        batch.push((page_id, bytes));
    }
    Ok(())
}

fn plan_snapshot_apply<B: FileBackend>(
    pager: &mut Pager<B>,
    view: &CommittedView<'_>,
    snapshot: SnapshotContents,
    new_txid: u64,
    now_ms: u64,
    alloc: &mut PageAllocator,
) -> Result<CommitPlan> {
    // Tree pages stay allocated until compaction. External references retire
    // with this commit and remain protected by the oldest active snapshot.
    let mut final_catalog = CatalogMap::new();
    let mut page_images = Vec::new();
    let registry_meta = view.catalog.get(PAYLOAD_REGISTRY_STORE_NAME);
    let mut payloads = PayloadPreparation {
        registry: PayloadRegistry::new(registry_meta.map_or(0, |meta| meta.store_root_page_id)),
        oldest_snapshot_txid: view.oldest_snapshot_txid,
        pending_writes: false,
    };
    if let Some(meta) = registry_meta {
        final_catalog.insert(PAYLOAD_REGISTRY_STORE_NAME.into(), meta.clone());
    }

    for (name, meta) in view.catalog.iter() {
        if name != PAYLOAD_REGISTRY_STORE_NAME {
            drop_tree_payload_references(pager, meta.store_root_page_id, alloc)?;
        }
    }

    let mut revision_ordinal = 0u64;
    for store in snapshot.stores {
        validate_user_store_name(&store.name)?;
        let flags = normalize_snapshot_store_flags(store.flags, &store.entries)?;
        let mut entries: Vec<(Vec<u8>, StoredValue)> = Vec::with_capacity(store.entries.len());
        for entry in store.entries {
            revision_ordinal = revision_ordinal.checked_add(1).ok_or_else(|| {
                EngineError::Internal("snapshot revision ordinal overflow".into())
            })?;
            let stored = StoredValue::with_expiry(entry.value, entry.expires_at_ms).with_revision(
                ValueRevision {
                    epoch: new_txid,
                    ordinal: revision_ordinal,
                },
            );
            if stored.is_expired_at(now_ms) {
                continue;
            }
            entries.push((entry.key, stored));
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        if entries.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(EngineError::Corruption(format!(
                "duplicate snapshot key in store {}",
                store.name
            )));
        }
        let mut prepared = Vec::with_capacity(entries.len());
        for (key, stored) in &entries {
            let mut value = PreparedValue::stored(stored, flags)?;
            payloads.externalize(pager, alloc, &mut value)?;
            prepared.push((key.as_slice(), value));
        }
        let built =
            build_tree_from_values(prepared.iter().map(|(key, value)| (*key, value)), alloc)?;
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

    payloads.flush_pending(pager)?;
    payloads.registry.apply_reference_changes(
        alloc.added_payload_refs(),
        alloc.dropped_payload_refs(),
        new_txid,
        pager,
    )?;
    if let Some(built) = payloads.registry.finish(pager, alloc)? {
        page_images.extend(built.page_images);
        final_catalog.insert(
            PAYLOAD_REGISTRY_STORE_NAME.into(),
            StoreMetadata {
                store_root_page_id: built.root_page_id,
                created_txid: registry_meta.map_or(new_txid, |meta| meta.created_txid),
                flags: STORE_FLAG_SYSTEM_RAW_VALUES,
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
        catalog_update: CatalogUpdate::Replace(final_catalog),
        final_schema_version: snapshot.schema_version,
        final_change_feed_floor_txid: floor,
        final_change_feed_policy: view.change_feed_policy,
        page_images,
        catalog_root_page_id: catalog_tree.root_page_id,
    })
}

fn prepare_staged_value<'a, B: FileBackend>(
    stored: &'a StoredValue,
    flags: u64,
    change: Option<&mut PreparedChange<'a>>,
    pager: &mut Pager<B>,
    alloc: &mut PageAllocator,
    payloads: &mut PayloadPreparation,
) -> Result<PreparedValue<'a>> {
    let mut value = PreparedValue::stored(stored, flags)?;
    payloads.externalize(pager, alloc, &mut value)?;
    if let Some(change) = change {
        if let Some(reference) = value.external_reference() {
            if !std::ptr::eq(value.parts().payload(), change.value.parts().payload()) {
                return Err(EngineError::Internal(
                    "primary and change log payloads do not match".into(),
                ));
            }
            change.value.set_external_reference(reference);
        }
        value.share_payload_checksums(&mut change.value);
    }
    Ok(value)
}

fn prepare_stage_puts<'a, B: FileBackend>(
    stage: &'a StagedStore,
    changes: &mut [PreparedChange<'a>],
    pager: &mut Pager<B>,
    alloc: &mut PageAllocator,
    payloads: &mut PayloadPreparation,
) -> Result<Vec<(&'a [u8], PreparedValue<'a>)>> {
    let mut change_puts = changes
        .iter_mut()
        .filter(|change| change.kind == ChangeKind::Put);
    stage
        .mutations
        .iter()
        .filter_map(|(key, mutation)| match mutation {
            MutationValue::Put(stored) => Some(
                prepare_staged_value(
                    stored,
                    stage.flags,
                    change_puts.next(),
                    pager,
                    alloc,
                    payloads,
                )
                .map(|encoded| (key.as_slice(), encoded)),
            ),
            MutationValue::Delete => None,
        })
        .collect()
}

type PreparedMutation<'a> = (&'a [u8], Option<PreparedValue<'a>>);

fn prepare_stage_mutations<'a, B: FileBackend>(
    stage: &'a StagedStore,
    changes: &mut [PreparedChange<'a>],
    pager: &mut Pager<B>,
    alloc: &mut PageAllocator,
    payloads: &mut PayloadPreparation,
) -> Result<Vec<PreparedMutation<'a>>> {
    let mut change_puts = changes
        .iter_mut()
        .filter(|change| change.kind == ChangeKind::Put);
    stage
        .mutations
        .iter()
        .map(|(key, mutation)| {
            let encoded = match mutation {
                MutationValue::Put(stored) => Some(prepare_staged_value(
                    stored,
                    stage.flags,
                    change_puts.next(),
                    pager,
                    alloc,
                    payloads,
                )?),
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

/// Clear and drop each produce one store-level record. Delete records require
/// a live committed key, checked from metadata without loading value bodies.
fn collect_change_payloads<'a, B: FileBackend>(
    pager: &mut Pager<B>,
    store_name: &str,
    stage: &'a StagedStore,
    now_ms: u64,
    out: &mut Vec<PreparedChange<'a>>,
) -> Result<()> {
    if stage.dropped {
        if stage.base_meta.is_some() && !stage.created {
            out.push(PreparedChange::new(
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
        out.push(PreparedChange::new(
            store_name,
            &[],
            ChangeKind::Clear,
            None,
        )?);
    }
    for (key, mutation) in &stage.mutations {
        match mutation {
            MutationValue::Put(stored) => out.push(PreparedChange::new(
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
                    out.push(PreparedChange::new(
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

    if rw.stores.contains_key(store) {
        return rw
            .stores
            .get_mut(store)
            .ok_or_else(|| EngineError::StoreNotFound(store.into()));
    }

    match rw.stores.entry(store.to_string()) {
        Entry::Occupied(entry) => Ok(entry.into_mut()),
        Entry::Vacant(vacant) => {
            let base_meta = store_meta(&rw.snapshot.catalog, store)?.clone();
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

fn validate_readwrite_store_value(rw: &ReadwriteTx, store: &str, value: &[u8]) -> Result<()> {
    let flags = if let Some(stage) = rw.stores.get(store) {
        if stage.dropped {
            return Err(EngineError::StoreNotFound(store.into()));
        }
        stage.flags
    } else {
        rw.snapshot
            .catalog
            .get(store)
            .ok_or_else(|| EngineError::StoreNotFound(store.into()))?
            .flags
    };
    validate_store_value(value, flags)
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
    Ok(normalized | STORE_FLAG_VALUE_REVISION)
}

fn mark_key_expired(stage: &mut StagedStore, key: &[u8]) {
    if !stage.mutations.contains_key(key) {
        stage.mutations.insert(key.to_vec(), MutationValue::Delete);
    }
}

fn normalize_expired_stage_mutations(stage: &mut StagedStore, now_ms: u64) {
    if stage.has_expiring_mutations == Some(false) {
        return;
    }
    let mut has_expiring_mutations = false;
    for mutation in stage.mutations.values_mut() {
        #[cfg(test)]
        commit_work_tests::record_ttl_examination();
        match mutation {
            MutationValue::Put(stored) if stored.is_expired_at(now_ms) => {
                *mutation = MutationValue::Delete;
            }
            MutationValue::Put(stored) => {
                has_expiring_mutations |= stored.expires_at_ms.is_some();
            }
            MutationValue::Delete => {}
        }
    }
    stage.has_expiring_mutations = Some(has_expiring_mutations);
}

/// Reads each distinct key in sorted order and stops at the first read error.
/// Clones duplicate results so each output value has independent ownership.
fn read_many_in_order<K: AsRef<[u8]>, V: Clone + Default>(
    keys: &[K],
    order: &[usize],
    mut read: impl FnMut(&[u8]) -> Result<V>,
) -> Result<Vec<V>> {
    let mut values = vec![V::default(); keys.len()];
    let mut start = 0;
    while start < order.len() {
        let key = keys[order[start]].as_ref();
        let mut end = start + 1;
        while end < order.len() && keys[order[end]].as_ref() == key {
            end += 1;
        }
        let value = read(key)?;
        for index in &order[start..end - 1] {
            values[*index] = value.clone();
        }
        values[order[end - 1]] = value;
        start = end;
    }
    Ok(values)
}

#[inline]
fn store_meta<'a>(catalog: &'a CatalogMap, store: &str) -> Result<&'a StoreMetadata> {
    catalog
        .get(store)
        .ok_or_else(|| EngineError::StoreNotFound(store.into()))
}

fn get_committed_visible<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    store_flags: u64,
    key: &[u8],
    now_ms: u64,
) -> Result<Option<Vec<u8>>> {
    get_committed_visible_in_batch(pager, root_page_id, store_flags, key, now_ms, None)
}

fn get_committed_visible_in_batch<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    store_flags: u64,
    key: &[u8],
    now_ms: u64,
    batch: Option<&mut PointReadBatch>,
) -> Result<Option<Vec<u8>>> {
    match lookup_stored_value(pager, root_page_id, key, store_flags, batch)? {
        Some(stored) => {
            if stored.is_expired_at(now_ms) {
                Ok(None)
            } else {
                Ok(Some(stored.value))
            }
        }
        None => Ok(None),
    }
}

/// Checks existence from leaf metadata and the TTL header without loading value bodies.
fn exists_committed_visible<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    store_flags: u64,
    key: &[u8],
    now_ms: u64,
) -> Result<bool> {
    exists_committed_visible_in_batch(pager, root_page_id, store_flags, key, now_ms, None)
}

fn exists_committed_visible_in_batch<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    store_flags: u64,
    key: &[u8],
    now_ms: u64,
    batch: Option<&mut PointReadBatch>,
) -> Result<bool> {
    match lookup_value_expiry(pager, root_page_id, key, store_flags, batch)? {
        None => Ok(false),
        Some(expiry) => Ok(!matches!(
            expiry,
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
    Ok(StagedLookup::Committed(
        store_meta(&rw.snapshot.catalog, store)?.clone(),
    ))
}

fn stored_value_state_with_staged<B: FileBackend>(
    pager: &mut Pager<B>,
    rw: &ReadwriteTx,
    store: &str,
    key: &[u8],
    now_ms: u64,
    batch: &mut PointReadBatch,
) -> Result<Option<ValueState>> {
    match staged_lookup(rw, store, key)? {
        StagedLookup::Staged(Some(stored)) => {
            let expired = stored.is_expired_at(now_ms);
            let max_length = if expired {
                0
            } else {
                let flags = rw
                    .stores
                    .get(store)
                    .ok_or_else(|| {
                        EngineError::Internal("staged value without staged store".into())
                    })?
                    .flags;
                let encoded_length = stored.value.len();
                encoded_length.max(decoded_scan_value_length(
                    &stored.value,
                    flags,
                    encoded_length,
                )?)
            };
            Ok(Some(ValueState {
                exists: !expired,
                expired,
                expires_at_ms: stored.expires_at_ms,
                revision: stored.revision.as_ref().map(|revision| ValueRevision {
                    epoch: revision.epoch,
                    ordinal: revision.ordinal,
                }),
                max_length,
            }))
        }
        StagedLookup::Staged(None) | StagedLookup::Absent => Ok(None),
        StagedLookup::Committed(meta) => lookup_stored_value_state(
            pager,
            meta.store_root_page_id,
            key,
            meta.flags,
            now_ms,
            Some(batch),
        ),
    }
}

fn stored_value_size_with_staged<B: FileBackend>(
    pager: &mut Pager<B>,
    rw: &ReadwriteTx,
    store: &str,
    key: &[u8],
    now_ms: u64,
    batch: &mut PointReadBatch,
) -> Result<Option<usize>> {
    match staged_lookup(rw, store, key)? {
        StagedLookup::Staged(Some(stored)) => {
            if stored.is_expired_at(now_ms) {
                return Ok(None);
            }
            let flags = rw
                .stores
                .get(store)
                .ok_or_else(|| EngineError::Internal("staged value without staged store".into()))?
                .flags;
            let encoded_length = stored.value.len();
            let decoded_length = decoded_scan_value_length(&stored.value, flags, encoded_length)?;
            Ok(Some(encoded_length.max(decoded_length)))
        }
        StagedLookup::Staged(None) | StagedLookup::Absent => Ok(None),
        StagedLookup::Committed(meta) => lookup_stored_value_size(
            pager,
            meta.store_root_page_id,
            key,
            meta.flags,
            now_ms,
            Some(batch),
        ),
    }
}

fn stored_value_info_with_staged<B: FileBackend>(
    pager: &mut Pager<B>,
    rw: &ReadwriteTx,
    store: &str,
    key: &[u8],
    batch: &mut PointReadBatch,
) -> Result<Option<(usize, Option<u64>)>> {
    match staged_lookup(rw, store, key)? {
        StagedLookup::Staged(Some(stored)) => Ok(Some((stored.value.len(), stored.expires_at_ms))),
        StagedLookup::Staged(None) | StagedLookup::Absent => Ok(None),
        StagedLookup::Committed(meta) => {
            lookup_stored_value_info(pager, meta.store_root_page_id, key, meta.flags, Some(batch))
        }
    }
}

fn stored_value_into_with_staged<B: FileBackend>(
    pager: &mut Pager<B>,
    rw: &mut ReadwriteTx,
    store: &str,
    key: &[u8],
    now_ms: u64,
    batch: &mut PointReadBatch,
    mut write: impl FnMut(&[u8]) -> Result<()>,
) -> Result<Option<Option<u64>>> {
    let expiry = match staged_lookup(rw, store, key)? {
        StagedLookup::Staged(Some(stored)) => {
            write(&stored.value)?;
            Some(stored.expires_at_ms)
        }
        StagedLookup::Staged(None) | StagedLookup::Absent => None,
        StagedLookup::Committed(meta) => lookup_stored_value_into(
            pager,
            meta.store_root_page_id,
            key,
            meta.flags,
            Some(batch),
            write,
        )?,
    };
    if matches!(expiry, Some(Some(timestamp)) if now_ms >= timestamp) {
        expire_staged_key(rw, store, key)?;
    }
    Ok(expiry)
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
    get_with_staged_in_batch(pager, rw, store, key, now_ms, None)
}

fn get_with_staged_in_batch<B: FileBackend>(
    pager: &mut Pager<B>,
    rw: &mut ReadwriteTx,
    store: &str,
    key: &[u8],
    now_ms: u64,
    batch: Option<&mut PointReadBatch>,
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
        StagedLookup::Committed(meta) => {
            match lookup_stored_value(pager, meta.store_root_page_id, key, meta.flags, batch)? {
                Some(stored) => {
                    if stored.is_expired_at(now_ms) {
                        (None, true)
                    } else {
                        (Some(stored.value), false)
                    }
                }
                None => (None, false),
            }
        }
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
    exists_with_staged_in_batch(pager, rw, store, key, now_ms, None)
}

fn exists_with_staged_in_batch<B: FileBackend>(
    pager: &mut Pager<B>,
    rw: &mut ReadwriteTx,
    store: &str,
    key: &[u8],
    now_ms: u64,
    batch: Option<&mut PointReadBatch>,
) -> Result<bool> {
    let (exists, expired) = match staged_lookup(rw, store, key)? {
        StagedLookup::Staged(Some(stored)) => {
            let expired = stored.is_expired_at(now_ms);
            (!expired, expired)
        }
        StagedLookup::Staged(None) | StagedLookup::Absent => (false, false),
        StagedLookup::Committed(meta) => {
            match lookup_value_expiry(pager, meta.store_root_page_id, key, meta.flags, batch)? {
                None => (false, false),
                Some(expiry) => {
                    let expired = matches!(
                        expiry,
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

/// Converts expired staged values to deletes immediately. Defers committed
/// expiry cleanup until every metadata lookup in the batch succeeds.
fn exists_with_staged_deferred_base<B: FileBackend>(
    pager: &mut Pager<B>,
    rw: &mut ReadwriteTx,
    store: &str,
    key: &[u8],
    now_ms: u64,
    batch: &mut PointReadBatch,
    expired_base_keys: &mut Vec<Vec<u8>>,
) -> Result<bool> {
    let (exists, expired_staged) = match staged_lookup(rw, store, key)? {
        StagedLookup::Staged(Some(stored)) => {
            let expired = stored.is_expired_at(now_ms);
            (!expired, expired)
        }
        StagedLookup::Staged(None) | StagedLookup::Absent => (false, false),
        StagedLookup::Committed(meta) => {
            match lookup_value_expiry(pager, meta.store_root_page_id, key, meta.flags, Some(batch))?
            {
                None => (false, false),
                Some(expiry) => {
                    let expired = matches!(expiry, Some(timestamp) if now_ms >= timestamp);
                    if expired {
                        expired_base_keys.push(key.to_vec());
                    }
                    (!expired, false)
                }
            }
        }
    };
    if expired_staged {
        expire_staged_key(rw, store, key)?;
    }
    Ok(exists)
}

fn put_with_staged_at<B: FileBackend>(
    pager: &mut Pager<B>,
    next_value_revision_ordinal: &mut u64,
    rw: &mut ReadwriteTx,
    store: &str,
    key: &[u8],
    mut value: StoredValue,
    now_ms: u64,
) -> Result<bool> {
    let existed = exists_with_staged(pager, rw, store, key, now_ms)?;
    let flags = ensure_stage_for_write(rw, store)?.flags;
    if !store_uses_system_raw_values(flags) && value.revision.is_none() {
        let epoch = rw
            .revision_epoch
            .ok_or_else(|| EngineError::Internal("write revision epoch missing".into()))?;
        *next_value_revision_ordinal = next_value_revision_ordinal
            .checked_add(1)
            .ok_or_else(|| EngineError::Internal("write revision ordinal overflow".into()))?;
        value.revision = Some(ValueRevision {
            epoch,
            ordinal: *next_value_revision_ordinal,
        });
    }
    let stage = ensure_stage_for_write(rw, store)?;
    if stage.dropped {
        return Err(EngineError::StoreNotFound(store.into()));
    }
    if value.expires_at_ms.is_some() {
        stage.has_expiring_mutations = Some(true);
    }
    if !store_uses_system_raw_values(stage.flags) && stage.flags & STORE_FLAG_VALUE_REVISION == 0 {
        stage.flags |= STORE_FLAG_VALUE_REVISION;
        if !stage.created && !stage.cleared {
            stage.force_full_rewrite = true;
        }
    }
    if value.expires_at_ms.is_some() && !store_uses_system_raw_values(stage.flags) {
        stage.flags |= STORE_FLAG_VALUE_ENVELOPE_V1;
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
    // The lookup already stages expired values as deletes. An absent key
    // needs no new mutation. Keep any existing staged delete.
    if !existed {
        return Ok(false);
    }
    let stage = ensure_stage_for_write(rw, store)?;
    if stage.dropped {
        return Err(EngineError::StoreNotFound(store.into()));
    }
    stage.mutations.insert(key.to_vec(), MutationValue::Delete);
    Ok(existed)
}

fn scan_committed_visible<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    store_flags: u64,
    range: &RangeSpec,
    now_ms: u64,
) -> Result<Vec<KvPair>> {
    let mut rows = Vec::new();
    visit_committed_visible(
        pager,
        root_page_id,
        store_flags,
        range,
        now_ms,
        &mut |pager, key, value| {
            let VisibleValue::Committed(pending, flags) = value else {
                return Err(EngineError::Internal("staged row in committed scan".into()));
            };
            let stored = materialize_pending_stored_value(pager, pending, flags)?;
            rows.push(KvPair {
                key,
                value: stored.value,
            });
            Ok(true)
        },
    )?;
    Ok(rows)
}

fn scan_with_staged<B: FileBackend>(
    pager: &mut Pager<B>,
    rw: &mut ReadwriteTx,
    store: &str,
    range: &RangeSpec,
    now_ms: u64,
) -> Result<Vec<KvPair>> {
    let mut rows = Vec::new();
    visit_with_staged(
        pager,
        rw,
        store,
        range,
        now_ms,
        true,
        &mut |pager, key, value| {
            let value = match value {
                VisibleValue::Staged(stored, _) => stored.value.clone(),
                VisibleValue::Committed(pending, flags) => {
                    materialize_pending_stored_value(pager, pending, flags)?.value
                }
            };
            rows.push(KvPair { key, value });
            Ok(true)
        },
    )?;
    Ok(rows)
}

enum VisibleValue<'a> {
    Staged(&'a StoredValue, u64),
    Committed(PendingValue, u64),
}

fn decoded_scan_value_length(prefix: &[u8], flags: u64, encoded_length: usize) -> Result<usize> {
    // The SDK compression header carries the decoded length without reading the body.
    if store_compression_from_flags(flags)? == StoreCompression::None {
        return Ok(encoded_length);
    }
    validate_store_value_prefix(prefix, encoded_length, flags).map_err(|error| match error {
        EngineError::Corruption(_) => error,
        error => EngineError::Corruption(error.to_string()),
    })?;
    Ok(read_u32_le(prefix, 10)? as usize)
}

fn visit_committed_visible<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    store_flags: u64,
    range: &RangeSpec,
    now_ms: u64,
    visit: &mut impl FnMut(&mut Pager<B>, Vec<u8>, VisibleValue<'_>) -> Result<bool>,
) -> Result<bool> {
    let limit = range.limit.unwrap_or(usize::MAX);
    if limit == 0 {
        return Ok(true);
    }
    let mut rows = 0;
    let mut iter = TreeIter::new(pager, root_page_id, range)?;
    while let Some(pair) = iter.next(pager)? {
        if pending_value_expired(pager, store_flags, &pair.value, now_ms)? {
            continue;
        }
        if !visit(
            pager,
            pair.key,
            VisibleValue::Committed(pair.value, store_flags),
        )? {
            return Ok(false);
        }
        rows += 1;
        if rows >= limit {
            return Ok(false);
        }
    }
    Ok(true)
}

fn visit_with_staged<B: FileBackend>(
    pager: &mut Pager<B>,
    rw: &mut ReadwriteTx,
    store: &str,
    range: &RangeSpec,
    now_ms: u64,
    normalize_all: bool,
    visit: &mut impl FnMut(&mut Pager<B>, Vec<u8>, VisibleValue<'_>) -> Result<bool>,
) -> Result<bool> {
    if let Some(stage) = rw.stores.get_mut(store) {
        if stage.dropped {
            return Err(EngineError::StoreNotFound(store.into()));
        }
        if normalize_all {
            normalize_expired_stage_mutations(stage, now_ms);
        }
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
        None => (Some(store_meta(&rw.snapshot.catalog, store)?.clone()), None),
    };

    let limit = range.limit.unwrap_or(usize::MAX);
    let mut rows = 0;
    let mut exhausted = limit == 0;
    let mut expired_base_keys = Vec::new();
    let mut expired_staged_keys = Vec::new();
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

        while rows < limit {
            let order = match (base_next.as_ref(), staged_next) {
                (None, None) => {
                    exhausted = true;
                    break;
                }
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
                if let MutationValue::Put(stored) = mutation {
                    if !stored.is_expired_at(now_ms) {
                        let flags = stage.map_or(0, |stage| stage.flags);
                        if !visit(pager, key.clone(), VisibleValue::Staged(stored, flags))? {
                            break;
                        }
                        rows += 1;
                    } else {
                        expired_staged_keys.push(key.clone());
                    }
                }
                if rows >= limit {
                    break;
                }
                if order == Ordering::Equal {
                    base_next = match base.as_mut() {
                        Some(iter) => iter.next(pager)?,
                        None => None,
                    };
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
                if !visit(
                    pager,
                    pair.key,
                    VisibleValue::Committed(pair.value, meta.flags),
                )? {
                    break;
                }
                rows += 1;
            }
            if rows >= limit {
                break;
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
    for key in expired_staged_keys {
        expire_staged_key(rw, store, &key)?;
    }
    Ok(exhausted)
}

/// Checks only the envelope header. Expired overflow values need no further
/// chain reads after the header is available.
fn pending_value_expired<B: FileBackend>(
    pager: &mut Pager<B>,
    store_flags: u64,
    value: &PendingValue,
    now_ms: u64,
) -> Result<bool> {
    if !store_uses_value_envelope(store_flags) || store_uses_system_raw_values(store_flags) {
        return Ok(false);
    }
    let (_, expiry) = pending_stored_value_info(pager, value, store_flags)?;
    Ok(matches!(expiry, Some(timestamp) if now_ms >= timestamp))
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
