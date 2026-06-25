use crate::btree::{
    build_catalog_tree, build_store_tree, lookup, rewrite_store_with_mutations, scan, KvPair,
    PageImages, RangeSpec,
};
use crate::bytes::{key_in_range, validate_key, validate_store_name, validate_value};
use crate::catalog::CatalogMap;
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
    BatchOp, BatchOpOutcome, MutationValue, ReadwriteTx, Snapshot, StagedStore, TransactionState,
    TxInner,
};
use crate::value::{
    store_compression_from_flags, store_flags_for_user_store, store_uses_value_envelope,
    StoreCompression, StoredValue, STORE_FLAG_COMPRESSION_MASK, STORE_FLAG_VALUE_ENVELOPE_V1,
};
use crate::wal::{append_transaction, CommitRecord};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub use crate::txn::TxMode;

pub type ScanRange = RangeSpec;

struct CommitPlan {
    new_txid: u64,
    final_catalog: CatalogMap,
    final_schema_version: u64,
    final_change_feed_floor_txid: u64,
    page_images: PageImages,
    catalog_root_page_id: u64,
    next_page_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenConfig {
    pub create_if_missing: bool,
    pub cache_pages: usize,
}

impl Default for OpenConfig {
    fn default() -> Self {
        Self {
            create_if_missing: true,
            cache_pages: 256,
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

pub struct Engine<B: FileBackend> {
    db_name: String,
    manifest: B,
    pager: Pager<B>,
    wal: B,
    superblock: SuperblockState,
    schema_version: u64,
    catalog: CatalogMap,
    change_feed_floor_txid: u64,
    next_tx_id: u64,
    next_commit_txid: u64,
    txns: HashMap<u64, TransactionState>,
    write_tx_open: Option<u64>,
    next_failpoint: Option<Failpoint>,
    cache_pages: usize,
}

impl<B: FileBackend> std::fmt::Debug for Engine<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("db_name", &self.db_name)
            .field("superblock", &self.superblock)
            .field("schema_version", &self.schema_version)
            .field("catalog", &self.catalog)
            .field("change_feed_floor_txid", &self.change_feed_floor_txid)
            .field("next_tx_id", &self.next_tx_id)
            .field("next_commit_txid", &self.next_commit_txid)
            .field("txns", &self.txns)
            .field("write_tx_open", &self.write_tx_open)
            .field("next_failpoint", &self.next_failpoint)
            .field("cache_pages", &self.cache_pages)
            .finish_non_exhaustive()
    }
}

impl<B: FileBackend> Engine<B> {
    pub fn open(db_name: &str, files: FileSet<B>, config: OpenConfig) -> Result<Self> {
        let cache_pages = config.cache_pages.max(1);
        let mut manifest = files.manifest;
        let mut pager = Pager::new(files.main, cache_pages);
        let mut wal = files.wal;
        let db_id = db_id_from_name(db_name);

        let superblock = ensure_openable_or_initialize(
            &mut manifest,
            &mut pager,
            &mut wal,
            db_id,
            config.create_if_missing,
        )?;
        validate_db_identity(db_name, db_id, &superblock)?;

        let superblock = recover_if_needed(&mut manifest, &mut pager, &mut wal, &superblock)?;
        validate_db_identity(db_name, db_id, &superblock)?;

        let catalog_state = load_catalog_snapshot(&mut pager, &superblock)?;
        let next_commit_txid = superblock.last_committed_txid.saturating_add(1);
        let change_feed_floor_txid = if catalog_state.change_feed_floor_txid == 0
            && !catalog_state
                .stores
                .contains_key(SYSTEM_CHANGELOG_STORE_NAME)
            && superblock.last_committed_txid > 0
        {
            superblock.last_committed_txid
        } else {
            catalog_state.change_feed_floor_txid
        };

        Ok(Self {
            db_name: db_name.to_string(),
            manifest,
            pager,
            wal,
            superblock,
            schema_version: catalog_state.schema_version,
            catalog: catalog_state.stores,
            change_feed_floor_txid,
            next_tx_id: 1,
            next_commit_txid,
            txns: HashMap::new(),
            write_tx_open: None,
            next_failpoint: None,
            cache_pages,
        })
    }

    pub fn close(&mut self) -> Result<()> {
        self.txns.clear();
        self.write_tx_open = None;
        self.pager.close()?;
        self.wal.close()?;
        self.manifest.close()?;
        Ok(())
    }

    pub fn begin_tx(&mut self, mode: TxMode) -> Result<u64> {
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

    pub fn rollback_tx(&mut self, tx_id: u64) -> Result<()> {
        let mut tx = self.take_tx(tx_id)?;
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
        let result = self.commit_staged(write_tx.stores, write_tx.staged_schema_version);
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
        let result = (|| match &mut tx.inner {
            TxInner::Readonly(readonly) => {
                let meta = readonly
                    .snapshot
                    .catalog
                    .get(store)
                    .ok_or_else(|| EngineError::StoreNotFound(store.into()))?;
                get_committed_visible(
                    &mut self.pager,
                    meta.store_root_page_id,
                    meta.flags,
                    key,
                    now_ms,
                )
            }
            TxInner::Readwrite(rw) => get_with_staged(&mut self.pager, rw, store, key, now_ms),
        })();
        self.put_tx(tx);
        result
    }

    pub fn has(&mut self, tx_id: u64, store: &str, key: &[u8]) -> Result<bool> {
        Ok(self.get(tx_id, store, key)?.is_some())
    }

    pub fn get_many(
        &mut self,
        tx_id: u64,
        store: &str,
        keys: &[Vec<u8>],
    ) -> Result<Vec<Option<Vec<u8>>>> {
        validate_store_name(store)?;
        let now_ms = now_unix_ms()?;
        let mut tx = self.take_tx(tx_id)?;
        let result = (|| match &mut tx.inner {
            TxInner::Readonly(readonly) => {
                let meta = readonly
                    .snapshot
                    .catalog
                    .get(store)
                    .ok_or_else(|| EngineError::StoreNotFound(store.into()))?;
                for key in keys {
                    validate_key(key)?;
                }
                let mut order: Vec<usize> = (0..keys.len()).collect();
                order.sort_by(|left, right| keys[*left].cmp(&keys[*right]));
                let mut values = vec![None; keys.len()];
                for index in order {
                    values[index] = get_committed_visible(
                        &mut self.pager,
                        meta.store_root_page_id,
                        meta.flags,
                        &keys[index],
                        now_ms,
                    )?;
                }
                Ok(values)
            }
            TxInner::Readwrite(rw) => {
                ensure_readwrite_store_visible(rw, store)?;
                for key in keys {
                    validate_key(key)?;
                }
                let mut order: Vec<usize> = (0..keys.len()).collect();
                order.sort_by(|left, right| keys[*left].cmp(&keys[*right]));
                let mut values = vec![None; keys.len()];
                for index in order {
                    values[index] =
                        get_with_staged(&mut self.pager, rw, store, &keys[index], now_ms)?;
                }
                Ok(values)
            }
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
        validate_store_name(store)?;
        validate_key(key)?;
        validate_value(value)?;
        let operation_now_ms = now_unix_ms()?;
        let expires_at_ms = absolute_expiry_from_ttl_at(ttl_ms, operation_now_ms)?;
        let mut tx = self.take_tx(tx_id)?;
        let result = (|| {
            let rw = tx.readwrite_mut()?;
            put_with_staged_at(
                &mut self.pager,
                rw,
                store,
                key,
                StoredValue::with_expiry(value.to_vec(), expires_at_ms),
                operation_now_ms,
            )?;
            Ok(())
        })();
        self.put_tx(tx);
        result
    }

    pub fn put_many_report(
        &mut self,
        tx_id: u64,
        store: &str,
        entries: &[(Vec<u8>, Vec<u8>)],
    ) -> BatchExecutionReport<bool> {
        self.put_many_with_ttl_report(tx_id, store, entries, None)
    }

    pub fn put_many_with_ttl_report(
        &mut self,
        tx_id: u64,
        store: &str,
        entries: &[(Vec<u8>, Vec<u8>)],
        ttl_ms: Option<u64>,
    ) -> BatchExecutionReport<bool> {
        match validate_user_store_name(store) {
            Ok(()) => {}
            Err(error) => return BatchExecutionReport::failure(Vec::new(), error),
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
            if let Err(error) = validate_key(key) {
                return self.batch_failure(tx, completed, error);
            }
            if let Err(error) = validate_value(value) {
                return self.batch_failure(tx, completed, error);
            }

            let result = tx.readwrite_mut().and_then(|rw| {
                put_with_staged_at(
                    &mut self.pager,
                    rw,
                    store,
                    key,
                    StoredValue::with_expiry(value.clone(), expires_at_ms),
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

    pub fn put_many(
        &mut self,
        tx_id: u64,
        store: &str,
        entries: &[(Vec<u8>, Vec<u8>)],
    ) -> Result<Vec<bool>> {
        self.put_many_report(tx_id, store, entries).into_result()
    }

    pub fn put_many_with_ttl(
        &mut self,
        tx_id: u64,
        store: &str,
        entries: &[(Vec<u8>, Vec<u8>)],
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
        let result = (|| {
            let rw = tx.readwrite_mut()?;
            delete_with_staged_at(&mut self.pager, rw, store, key, operation_now_ms)
        })();
        self.put_tx(tx);
        result
    }

    pub fn delete_many_report(
        &mut self,
        tx_id: u64,
        store: &str,
        keys: &[Vec<u8>],
    ) -> BatchExecutionReport<bool> {
        match validate_user_store_name(store) {
            Ok(()) => {}
            Err(error) => return BatchExecutionReport::failure(Vec::new(), error),
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
            if let Err(error) = validate_key(key) {
                return self.batch_failure(tx, completed, error);
            }

            match tx.readwrite_mut().and_then(|rw| {
                delete_with_staged_at(&mut self.pager, rw, store, key, operation_now_ms)
            }) {
                Ok(deleted) => completed.push(deleted),
                Err(error) => return self.batch_failure(tx, completed, error),
            }
        }

        self.put_tx(tx);
        BatchExecutionReport::success(completed)
    }

    pub fn delete_many(&mut self, tx_id: u64, store: &str, keys: &[Vec<u8>]) -> Result<Vec<bool>> {
        self.delete_many_report(tx_id, store, keys).into_result()
    }

    pub fn apply_batch_report(
        &mut self,
        tx_id: u64,
        store: &str,
        ops: &[BatchOp],
    ) -> BatchExecutionReport<BatchOpOutcome> {
        match validate_user_store_name(store) {
            Ok(()) => {}
            Err(error) => return BatchExecutionReport::failure(Vec::new(), error),
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
            match op {
                BatchOp::Put { key, value } => {
                    if let Err(error) = validate_key(key) {
                        return self.batch_failure(tx, completed, error);
                    }
                    if let Err(error) = validate_value(value) {
                        return self.batch_failure(tx, completed, error);
                    }

                    let result = tx.readwrite_mut().and_then(|rw| {
                        put_with_staged_at(
                            &mut self.pager,
                            rw,
                            store,
                            key,
                            StoredValue::plain(value.clone()),
                            operation_now_ms,
                        )
                    });
                    match result {
                        Ok(baseline_exists) => {
                            completed.push(BatchOpOutcome::Put { baseline_exists })
                        }
                        Err(error) => return self.batch_failure(tx, completed, error),
                    }
                }
                BatchOp::Delete { key } => {
                    if let Err(error) = validate_key(key) {
                        return self.batch_failure(tx, completed, error);
                    }

                    match tx.readwrite_mut().and_then(|rw| {
                        delete_with_staged_at(&mut self.pager, rw, store, key, operation_now_ms)
                    }) {
                        Ok(deleted) => completed.push(BatchOpOutcome::Delete { deleted }),
                        Err(error) => return self.batch_failure(tx, completed, error),
                    }
                }
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
        let result = (|| match &mut tx.inner {
            TxInner::Readonly(readonly) => {
                let meta = readonly
                    .snapshot
                    .catalog
                    .get(store)
                    .ok_or_else(|| EngineError::StoreNotFound(store.into()))?;
                scan_committed_visible(
                    &mut self.pager,
                    meta.store_root_page_id,
                    meta.flags,
                    range,
                    now_ms,
                )
            }
            TxInner::Readwrite(rw) => scan_with_staged(&mut self.pager, rw, store, range, now_ms),
        })();
        self.put_tx(tx);
        result
    }

    pub fn stats(&mut self) -> Result<DbStats> {
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
        })
    }

    pub fn changes_since(&mut self, txid: u64, options: ChangeFeedOptions) -> Result<ChangeFeed> {
        let latest_txid = self.superblock.last_committed_txid;
        if txid > latest_txid {
            return Err(EngineError::InvalidRange(format!(
                "change feed cursor {txid} exceeds latest committed txid {latest_txid}"
            )));
        }
        if txid < self.change_feed_floor_txid {
            return Err(EngineError::ChangeFeedCompacted(format!(
                "change feed cursor {txid} is older than retained floor {}",
                self.change_feed_floor_txid,
            )));
        }

        let store_filter = normalize_store_filter(options.stores.as_deref())?;
        let limit = options.limit.unwrap_or(usize::MAX);
        if limit == 0 {
            return Ok(ChangeFeed {
                changes: Vec::new(),
                latest_tx_id: latest_txid,
            });
        }

        let Some(change_log_meta) = self.catalog.get(SYSTEM_CHANGELOG_STORE_NAME).cloned() else {
            return Ok(ChangeFeed {
                changes: Vec::new(),
                latest_tx_id: latest_txid,
            });
        };

        let mut range = RangeSpec {
            gt: Some(encode_after_txid_key(txid)),
            gte: None,
            lt: None,
            lte: None,
            reverse: false,
            limit: None,
        };
        if store_filter.is_none() {
            range.limit = Some(limit);
        }

        let mut changes = Vec::new();
        for pair in scan(&mut self.pager, change_log_meta.store_root_page_id, &range)? {
            let record_txid = decode_change_log_record_txid(&pair.key)?;
            let record = decode_change_record_payload(record_txid, &pair.value)?;
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
        if !self.txns.is_empty() {
            return Err(EngineError::DatabaseBusy(
                "cannot import snapshot while transactions are open".into(),
            ));
        }
        let snapshot = decode_snapshot(bytes)?;
        self.apply_snapshot_contents(snapshot)
    }

    pub fn reset(&mut self) -> Result<u64> {
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

    fn commit_staged(
        &mut self,
        mut staged: BTreeMap<String, StagedStore>,
        staged_schema_version: Option<u64>,
    ) -> Result<u64> {
        let new_txid = self.reserve_commit_txid()?;
        let commit_now_ms = now_unix_ms()?;
        let mut next_page_id = self.superblock.next_page_id;
        let mut final_catalog = self.catalog.clone();
        let final_schema_version = staged_schema_version.unwrap_or(self.schema_version);
        let mut page_images = Vec::new();
        let mut pending_change_records = Vec::new();

        for (store_name, stage) in staged.iter_mut() {
            normalize_expired_stage_mutations(stage, commit_now_ms);
            if !is_internal_store_name(store_name) {
                collect_change_records_for_stage(
                    &mut self.pager,
                    store_name,
                    stage,
                    new_txid,
                    commit_now_ms,
                    &mut pending_change_records,
                )?;
            }

            if stage.dropped {
                final_catalog.remove(store_name);
                continue;
            }

            let built = if stage.created
                || stage.cleared
                || stage.base_meta.is_none()
                || stage.force_full_rewrite
            {
                let entries =
                    materialize_store_entries_for_commit(&mut self.pager, stage, commit_now_ms)?;
                let encoded_entries = encode_store_entries(stage.flags, &entries)?;
                build_store_tree(&encoded_entries, &mut next_page_id)?
            } else if stage.mutations.is_empty() {
                continue;
            } else {
                let base_meta = stage
                    .base_meta
                    .as_ref()
                    .ok_or_else(|| EngineError::Internal("missing base metadata".into()))?;
                let encoded_mutations = encode_store_mutations(stage.flags, &stage.mutations)?;
                rewrite_store_with_mutations(
                    &mut self.pager,
                    base_meta.store_root_page_id,
                    &encoded_mutations,
                    &mut next_page_id,
                )?
            };

            let root_page_id = built.root_page_id;
            page_images.extend(built.page_images);
            let created_txid = if stage.created {
                new_txid
            } else {
                stage
                    .base_meta
                    .as_ref()
                    .map(|meta| meta.created_txid)
                    .unwrap_or(new_txid)
            };
            final_catalog.insert(
                store_name.clone(),
                StoreMetadata {
                    store_root_page_id: root_page_id,
                    created_txid,
                    flags: stage.flags,
                },
            );
        }

        if !pending_change_records.is_empty() {
            let change_log_base_meta = self.catalog.get(SYSTEM_CHANGELOG_STORE_NAME).cloned();
            let change_log_store_created_txid = change_log_base_meta
                .as_ref()
                .map(|meta| meta.created_txid)
                .unwrap_or(new_txid);
            let change_log_built = build_change_log_store_commit(
                &mut self.pager,
                change_log_base_meta.as_ref(),
                &pending_change_records,
                &mut next_page_id,
            )?;
            page_images.extend(change_log_built.page_images);
            final_catalog.insert(
                SYSTEM_CHANGELOG_STORE_NAME.to_string(),
                StoreMetadata {
                    store_root_page_id: change_log_built.root_page_id,
                    created_txid: change_log_store_created_txid,
                    flags: CHANGELOG_STORE_FLAGS,
                },
            );
        }

        let catalog_tree = build_catalog_tree(
            &final_catalog,
            final_schema_version,
            self.change_feed_floor_txid,
            &mut next_page_id,
        )?;
        let catalog_root_page_id = catalog_tree.root_page_id;
        page_images.extend(catalog_tree.page_images);

        self.finish_commit(CommitPlan {
            new_txid,
            final_catalog,
            final_schema_version,
            final_change_feed_floor_txid: self.change_feed_floor_txid,
            page_images,
            catalog_root_page_id,
            next_page_id,
        })
    }

    fn apply_snapshot_contents(&mut self, snapshot: SnapshotContents) -> Result<u64> {
        let new_txid = self
            .reserve_commit_txid_at_least(snapshot.source_last_committed_txid.saturating_add(1))?;
        let apply_now_ms = now_unix_ms()?;
        let final_schema_version = snapshot.schema_version;
        let final_change_feed_floor_txid = snapshot.source_last_committed_txid;
        let mut next_page_id = self.superblock.next_page_id;
        let mut final_catalog = CatalogMap::new();
        let mut page_images = Vec::new();

        for store in snapshot.stores {
            validate_user_store_name(&store.name)?;
            let mut entries = BTreeMap::new();
            let flags = normalize_snapshot_store_flags(store.flags, &store.entries)?;
            for entry in store.entries {
                let stored = StoredValue::with_expiry(entry.value, entry.expires_at_ms);
                if stored.is_expired_at(apply_now_ms) {
                    continue;
                }
                if entries.insert(entry.key, stored).is_some() {
                    return Err(EngineError::Corruption(format!(
                        "duplicate snapshot key in store {}",
                        store.name
                    )));
                }
            }

            let encoded_entries = encode_store_entries(flags, &entries)?;
            let built = build_store_tree(&encoded_entries, &mut next_page_id)?;
            let root_page_id = built.root_page_id;
            page_images.extend(built.page_images);
            final_catalog.insert(
                store.name,
                StoreMetadata {
                    store_root_page_id: root_page_id,
                    created_txid: new_txid,
                    flags,
                },
            );
        }

        let catalog_tree = build_catalog_tree(
            &final_catalog,
            final_schema_version,
            final_change_feed_floor_txid,
            &mut next_page_id,
        )?;
        let catalog_root_page_id = catalog_tree.root_page_id;
        page_images.extend(catalog_tree.page_images);

        self.finish_commit(CommitPlan {
            new_txid,
            final_catalog,
            final_schema_version,
            final_change_feed_floor_txid,
            page_images,
            catalog_root_page_id,
            next_page_id,
        })
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

    fn finish_commit(&mut self, plan: CommitPlan) -> Result<u64> {
        let CommitPlan {
            new_txid,
            final_catalog,
            final_schema_version,
            final_change_feed_floor_txid,
            page_images,
            catalog_root_page_id,
            next_page_id,
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
        append_transaction(
            &mut self.wal,
            &mut wal_offset,
            new_txid,
            &page_images,
            &commit,
        )?;
        self.wal.flush()?;
        if self.consume_failpoint(Failpoint::AfterWalFlush) {
            return Err(EngineError::InjectedFailure(
                Failpoint::AfterWalFlush.as_str().into(),
            ));
        }

        for (page_id, bytes) in &page_images {
            self.pager.write_page_image(*page_id, bytes)?;
        }
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

        let new_superblock = SuperblockState {
            generation: self.superblock.generation + 1,
            db_id: self.superblock.db_id,
            page_size: self.superblock.page_size,
            catalog_root_page_id,
            next_page_id,
            last_committed_txid: new_txid,
            last_replayed_wal_offset: wal_offset,
            active_slot: if self.superblock.active_slot == 0 {
                1
            } else {
                0
            },
        };
        write_superblock(&mut self.manifest, &new_superblock)?;

        self.superblock = new_superblock;
        self.schema_version = final_schema_version;
        self.catalog = final_catalog;
        self.change_feed_floor_txid = final_change_feed_floor_txid;
        Ok(new_txid)
    }

    fn take_tx(&mut self, tx_id: u64) -> Result<TransactionState> {
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
        let result = (|| {
            let rw = tx.readwrite_mut()?;
            ensure_readwrite_store_visible(rw, store)
        })();
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

#[derive(Debug, Clone)]
struct PendingChangeRecord {
    tx_id: u64,
    store: String,
    key: Vec<u8>,
    kind: ChangeKind,
    value: Option<Vec<u8>>,
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

fn collect_change_records_for_stage<B: FileBackend>(
    pager: &mut Pager<B>,
    store_name: &str,
    stage: &StagedStore,
    tx_id: u64,
    now_ms: u64,
    out: &mut Vec<PendingChangeRecord>,
) -> Result<()> {
    if !stage.has_changes() {
        return Ok(());
    }

    if stage.dropped {
        if let Some(base_meta) = stage.base_meta.as_ref() {
            for pair in scan_committed_visible(
                pager,
                base_meta.store_root_page_id,
                base_meta.flags,
                &RangeSpec::default(),
                now_ms,
            )? {
                out.push(PendingChangeRecord {
                    tx_id,
                    store: store_name.to_string(),
                    key: pair.key,
                    kind: ChangeKind::Delete,
                    value: None,
                });
            }
        }
        return Ok(());
    }

    if stage.cleared {
        let overwritten_keys: BTreeSet<Vec<u8>> = stage
            .mutations
            .iter()
            .filter_map(|(key, mutation)| match mutation {
                MutationValue::Put(_) => Some(key.clone()),
                MutationValue::Delete => None,
            })
            .collect();

        if let Some(base_meta) = stage.base_meta.as_ref() {
            for pair in scan_committed_visible(
                pager,
                base_meta.store_root_page_id,
                base_meta.flags,
                &RangeSpec::default(),
                now_ms,
            )? {
                if overwritten_keys.contains(&pair.key) {
                    continue;
                }
                out.push(PendingChangeRecord {
                    tx_id,
                    store: store_name.to_string(),
                    key: pair.key,
                    kind: ChangeKind::Delete,
                    value: None,
                });
            }
        }

        for (key, mutation) in &stage.mutations {
            if let MutationValue::Put(stored) = mutation {
                out.push(PendingChangeRecord {
                    tx_id,
                    store: store_name.to_string(),
                    key: key.clone(),
                    kind: ChangeKind::Put,
                    value: Some(stored.value.clone()),
                });
            }
        }
        return Ok(());
    }

    for (key, mutation) in &stage.mutations {
        match mutation {
            MutationValue::Put(stored) => {
                out.push(PendingChangeRecord {
                    tx_id,
                    store: store_name.to_string(),
                    key: key.clone(),
                    kind: ChangeKind::Put,
                    value: Some(stored.value.clone()),
                });
            }
            MutationValue::Delete => {
                let Some(base_meta) = stage.base_meta.as_ref() else {
                    continue;
                };
                if !committed_visible_entry_exists(pager, base_meta, key, now_ms)? {
                    continue;
                }
                out.push(PendingChangeRecord {
                    tx_id,
                    store: store_name.to_string(),
                    key: key.clone(),
                    kind: ChangeKind::Delete,
                    value: None,
                });
            }
        }
    }

    Ok(())
}

fn committed_visible_entry_exists<B: FileBackend>(
    pager: &mut Pager<B>,
    meta: &StoreMetadata,
    key: &[u8],
    now_ms: u64,
) -> Result<bool> {
    Ok(get_committed_visible(pager, meta.store_root_page_id, meta.flags, key, now_ms)?.is_some())
}

fn build_change_log_store_commit<B: FileBackend>(
    pager: &mut Pager<B>,
    base_meta: Option<&StoreMetadata>,
    records: &[PendingChangeRecord],
    next_page_id: &mut u64,
) -> Result<crate::btree::BuiltTree> {
    let mut encoded_entries = BTreeMap::new();
    for (sequence, record) in records.iter().enumerate() {
        let sequence = u32::try_from(sequence)
            .map_err(|_| EngineError::Serialization("change log sequence overflow".into()))?;
        let key = encode_change_log_key(record.tx_id, sequence);
        let value = encode_change_record_payload(
            &record.store,
            &record.key,
            record.kind,
            record.value.as_deref(),
        )?;
        encoded_entries.insert(key, value);
    }

    match base_meta {
        Some(base_meta) => {
            let mutations: BTreeMap<Vec<u8>, MutationValue> = encoded_entries
                .into_iter()
                .map(|(key, value)| (key, MutationValue::Put(StoredValue::plain(value))))
                .collect();
            let encoded_mutations = encode_store_mutations(CHANGELOG_STORE_FLAGS, &mutations)?;
            rewrite_store_with_mutations(
                pager,
                base_meta.store_root_page_id,
                &encoded_mutations,
                next_page_id,
            )
        }
        None => build_store_tree(&encoded_entries, next_page_id),
    }
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
    let expired_keys: Vec<Vec<u8>> = stage
        .mutations
        .iter()
        .filter_map(|(key, mutation)| match mutation {
            MutationValue::Put(stored) if stored.is_expired_at(now_ms) => Some(key.clone()),
            MutationValue::Put(_) | MutationValue::Delete => None,
        })
        .collect();

    for key in expired_keys {
        stage.mutations.insert(key, MutationValue::Delete);
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
            let stored = StoredValue::decode_for_store(store_flags, &raw_value)?;
            if stored.is_expired_at(now_ms) {
                Ok(None)
            } else {
                Ok(Some(stored.value))
            }
        }
        None => Ok(None),
    }
}

fn get_with_staged<B: FileBackend>(
    pager: &mut Pager<B>,
    rw: &mut ReadwriteTx,
    store: &str,
    key: &[u8],
    now_ms: u64,
) -> Result<Option<Vec<u8>>> {
    if let Some(stage) = rw.stores.get_mut(store) {
        if stage.dropped {
            return Err(EngineError::StoreNotFound(store.into()));
        }

        if let Some(mutation) = stage.mutations.get(key).cloned() {
            return match mutation {
                MutationValue::Put(stored) => {
                    if stored.is_expired_at(now_ms) {
                        stage.mutations.insert(key.to_vec(), MutationValue::Delete);
                        Ok(None)
                    } else {
                        Ok(Some(stored.value))
                    }
                }
                MutationValue::Delete => Ok(None),
            };
        }

        if stage.created || stage.cleared {
            return Ok(None);
        }

        if let Some(base_meta) = stage.base_meta.clone() {
            return match lookup(pager, base_meta.store_root_page_id, key)? {
                Some(raw_value) => {
                    let stored = StoredValue::decode_for_store(base_meta.flags, &raw_value)?;
                    if stored.is_expired_at(now_ms) {
                        mark_key_expired(stage, key);
                        Ok(None)
                    } else {
                        Ok(Some(stored.value))
                    }
                }
                None => Ok(None),
            };
        }

        return Ok(None);
    }

    let base_meta = rw
        .snapshot
        .catalog
        .get(store)
        .cloned()
        .ok_or_else(|| EngineError::StoreNotFound(store.into()))?;

    match lookup(pager, base_meta.store_root_page_id, key)? {
        Some(raw_value) => {
            let stored = StoredValue::decode_for_store(base_meta.flags, &raw_value)?;
            if stored.is_expired_at(now_ms) {
                let stage = ensure_stage_for_write(rw, store)?;
                mark_key_expired(stage, key);
                Ok(None)
            } else {
                Ok(Some(stored.value))
            }
        }
        None => Ok(None),
    }
}

fn put_with_staged_at<B: FileBackend>(
    pager: &mut Pager<B>,
    rw: &mut ReadwriteTx,
    store: &str,
    key: &[u8],
    value: StoredValue,
    now_ms: u64,
) -> Result<bool> {
    let existed = get_with_staged(pager, rw, store, key, now_ms)?.is_some();
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
    let existed = get_with_staged(pager, rw, store, key, now_ms)?.is_some();
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
    if !range.reverse {
        if let Some(limit) = range.limit {
            return scan_committed_visible_forward_limited(
                pager,
                root_page_id,
                store_flags,
                range,
                now_ms,
                limit,
            );
        }
    }

    let mut physical_range = range.clone();
    physical_range.limit = None;
    physical_range.reverse = false;

    let mut rows = Vec::new();
    for pair in scan(pager, root_page_id, &physical_range)? {
        let stored = StoredValue::decode_for_store(store_flags, &pair.value)?;
        if stored.is_expired_at(now_ms) {
            continue;
        }
        rows.push(KvPair {
            key: pair.key,
            value: stored.value,
        });
    }

    apply_range_ordering_and_limit(&mut rows, range);
    Ok(rows)
}

fn scan_committed_visible_forward_limited<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    store_flags: u64,
    range: &RangeSpec,
    now_ms: u64,
    limit: usize,
) -> Result<Vec<KvPair>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut physical_range = range.clone();
    physical_range.reverse = false;
    let mut rows = Vec::with_capacity(limit.min(1024));

    loop {
        let remaining = limit.saturating_sub(rows.len());
        if remaining == 0 {
            return Ok(rows);
        }

        physical_range.limit = Some(remaining);
        let raw_rows = scan(pager, root_page_id, &physical_range)?;
        if raw_rows.is_empty() {
            return Ok(rows);
        }

        let raw_count = raw_rows.len();
        let mut last_key = None;
        for pair in raw_rows {
            last_key = Some(pair.key.clone());
            let stored = StoredValue::decode_for_store(store_flags, &pair.value)?;
            if stored.is_expired_at(now_ms) {
                continue;
            }
            rows.push(KvPair {
                key: pair.key,
                value: stored.value,
            });
            if rows.len() >= limit {
                return Ok(rows);
            }
        }

        if raw_count < remaining {
            return Ok(rows);
        }

        let Some(last_key) = last_key else {
            return Ok(rows);
        };
        let reached_upper_bound = match (range.lt.as_deref(), range.lte.as_deref()) {
            (Some(upper), _) => last_key.as_slice() >= upper,
            (None, Some(upper)) => last_key.as_slice() >= upper,
            (None, None) => false,
        };
        if reached_upper_bound {
            return Ok(rows);
        }
        physical_range.gt = Some(last_key);
        physical_range.gte = None;
    }
}

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

    let mut physical_range = range.clone();
    physical_range.limit = None;
    physical_range.reverse = false;

    if let Some(stage) = rw.stores.get(store) {
        if stage.dropped {
            return Err(EngineError::StoreNotFound(store.into()));
        }

        let base_meta = if stage.created || stage.cleared {
            None
        } else {
            stage.base_meta.clone()
        };
        let staged_mutations: Vec<(Vec<u8>, MutationValue)> = stage
            .mutations
            .iter()
            .map(|(key, mutation)| (key.clone(), mutation.clone()))
            .collect();

        let mut merged = BTreeMap::new();
        let mut expired_base_keys = Vec::new();

        if let Some(base_meta) = base_meta {
            for pair in scan(pager, base_meta.store_root_page_id, &physical_range)? {
                let stored = StoredValue::decode_for_store(base_meta.flags, &pair.value)?;
                if stored.is_expired_at(now_ms) {
                    expired_base_keys.push(pair.key);
                    continue;
                }
                merged.insert(pair.key, stored.value);
            }
        }

        if !expired_base_keys.is_empty() {
            let stage = rw
                .stores
                .get_mut(store)
                .ok_or_else(|| EngineError::Internal("missing staged store".into()))?;
            for key in &expired_base_keys {
                mark_key_expired(stage, key);
            }
        }

        for (key, mutation) in staged_mutations {
            if !key_in_range(
                &key,
                range.gt.as_deref(),
                range.gte.as_deref(),
                range.lt.as_deref(),
                range.lte.as_deref(),
            ) {
                continue;
            }

            match mutation {
                MutationValue::Put(stored) => {
                    if !stored.is_expired_at(now_ms) {
                        merged.insert(key, stored.value);
                    } else {
                        merged.remove(key.as_slice());
                    }
                }
                MutationValue::Delete => {
                    merged.remove(key.as_slice());
                }
            }
        }

        let mut rows: Vec<KvPair> = merged
            .into_iter()
            .map(|(key, value)| KvPair { key, value })
            .collect();
        apply_range_ordering_and_limit(&mut rows, range);
        return Ok(rows);
    }

    let base_meta = rw
        .snapshot
        .catalog
        .get(store)
        .cloned()
        .ok_or_else(|| EngineError::StoreNotFound(store.into()))?;

    let mut rows = Vec::new();
    let mut expired_keys = Vec::new();
    for pair in scan(pager, base_meta.store_root_page_id, &physical_range)? {
        let stored = StoredValue::decode_for_store(base_meta.flags, &pair.value)?;
        if stored.is_expired_at(now_ms) {
            expired_keys.push(pair.key);
            continue;
        }
        rows.push(KvPair {
            key: pair.key,
            value: stored.value,
        });
    }

    if !expired_keys.is_empty() {
        let stage = ensure_stage_for_write(rw, store)?;
        for key in &expired_keys {
            mark_key_expired(stage, key);
        }
    }

    apply_range_ordering_and_limit(&mut rows, range);
    Ok(rows)
}

fn apply_range_ordering_and_limit(rows: &mut Vec<KvPair>, range: &RangeSpec) {
    if range.reverse {
        rows.reverse();
    }
    if let Some(limit) = range.limit {
        rows.truncate(limit);
    }
}

fn materialize_store_entries_for_commit<B: FileBackend>(
    pager: &mut Pager<B>,
    stage: &StagedStore,
    now_ms: u64,
) -> Result<BTreeMap<Vec<u8>, StoredValue>> {
    let mut entries = BTreeMap::new();

    if !stage.created && !stage.cleared {
        if let Some(base_meta) = stage.base_meta.as_ref() {
            for pair in crate::btree::load_all_entries(pager, base_meta.store_root_page_id)? {
                let stored = StoredValue::decode_for_store(base_meta.flags, &pair.value)?;
                if stored.is_expired_at(now_ms) {
                    continue;
                }
                entries.insert(pair.key, stored);
            }
        }
    }

    for (key, mutation) in &stage.mutations {
        match mutation {
            MutationValue::Put(stored) => {
                if stored.is_expired_at(now_ms) {
                    entries.remove(key.as_slice());
                } else {
                    entries.insert(key.clone(), stored.clone());
                }
            }
            MutationValue::Delete => {
                entries.remove(key.as_slice());
            }
        }
    }

    Ok(entries)
}

fn encode_store_entries(
    store_flags: u64,
    entries: &BTreeMap<Vec<u8>, StoredValue>,
) -> Result<BTreeMap<Vec<u8>, Vec<u8>>> {
    entries
        .iter()
        .map(|(key, stored)| Ok((key.clone(), stored.encode_for_store(store_flags)?)))
        .collect()
}

fn encode_store_mutations(
    store_flags: u64,
    mutations: &BTreeMap<Vec<u8>, MutationValue>,
) -> Result<BTreeMap<Vec<u8>, MutationValue>> {
    mutations
        .iter()
        .map(|(key, mutation)| {
            let encoded = match mutation {
                MutationValue::Put(stored) => {
                    MutationValue::Put(StoredValue::plain(stored.encode_for_store(store_flags)?))
                }
                MutationValue::Delete => MutationValue::Delete,
            };
            Ok((key.clone(), encoded))
        })
        .collect()
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
