use crate::catalog::CatalogMap;
use crate::error::{EngineError, Result};
use crate::layout::StoreMetadata;
use crate::value::StoredValue;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TxMode {
    Readonly,
    Readwrite,
}

impl TxMode {
    pub fn parse(mode: &str) -> Result<Self> {
        match mode {
            "readonly" => Ok(TxMode::Readonly),
            "readwrite" => Ok(TxMode::Readwrite),
            other => Err(EngineError::Internal(format!("unknown tx mode {other}"))),
        }
    }
}

impl std::str::FromStr for TxMode {
    type Err = EngineError;

    fn from_str(mode: &str) -> Result<Self> {
        Self::parse(mode)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema_version: u64,
    pub catalog_root_page_id: u64,
    pub last_committed_txid: u64,
    pub catalog: CatalogMap,
}

impl Snapshot {
    pub fn new(
        schema_version: u64,
        catalog_root_page_id: u64,
        last_committed_txid: u64,
        catalog: &CatalogMap,
    ) -> Self {
        Self {
            schema_version,
            catalog_root_page_id,
            last_committed_txid,
            catalog: catalog.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MutationValue {
    Put(StoredValue),
    Delete,
}

impl MutationValue {
    pub fn is_delete(&self) -> bool {
        matches!(self, MutationValue::Delete)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum BatchOp {
    Put { key: Vec<u8>, value: Vec<u8> },
    Delete { key: Vec<u8> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum BatchOpOutcome {
    Put { baseline_exists: bool },
    Delete { deleted: bool },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StagedStore {
    pub base_meta: Option<StoreMetadata>,
    pub mutations: BTreeMap<Vec<u8>, MutationValue>,
    pub created: bool,
    pub dropped: bool,
    pub cleared: bool,
    pub flags: u64,
    pub force_full_rewrite: bool,
}

impl StagedStore {
    pub(crate) fn created(flags: u64) -> Self {
        Self {
            created: true,
            flags,
            ..Self::default()
        }
    }

    pub(crate) fn existing(base_meta: StoreMetadata) -> Self {
        let flags = base_meta.flags;
        Self {
            base_meta: Some(base_meta),
            flags,
            ..Self::default()
        }
    }

    pub(crate) fn dropped_existing(base_meta: StoreMetadata) -> Self {
        Self {
            dropped: true,
            ..Self::existing(base_meta)
        }
    }

    pub fn visible(&self) -> bool {
        !self.dropped
    }

    pub fn has_changes(&self) -> bool {
        self.created
            || self.dropped
            || self.cleared
            || self.force_full_rewrite
            || !self.mutations.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadonlyTx {
    pub snapshot: Snapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadwriteTx {
    pub snapshot: Snapshot,
    pub stores: BTreeMap<String, StagedStore>,
    pub staged_schema_version: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TxInner {
    Readonly(ReadonlyTx),
    Readwrite(ReadwriteTx),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionState {
    pub id: u64,
    pub mode: TxMode,
    pub closed: bool,
    pub inner: TxInner,
}

impl TransactionState {
    pub fn new_readonly(id: u64, snapshot: Snapshot) -> Self {
        Self {
            id,
            mode: TxMode::Readonly,
            closed: false,
            inner: TxInner::Readonly(ReadonlyTx { snapshot }),
        }
    }

    pub fn new_readwrite(id: u64, snapshot: Snapshot) -> Self {
        Self {
            id,
            mode: TxMode::Readwrite,
            closed: false,
            inner: TxInner::Readwrite(ReadwriteTx {
                snapshot,
                stores: BTreeMap::new(),
                staged_schema_version: None,
            }),
        }
    }

    pub fn ensure_open(&self) -> Result<()> {
        if self.closed {
            return Err(EngineError::TransactionClosed);
        }
        Ok(())
    }

    pub fn snapshot(&self) -> &Snapshot {
        match &self.inner {
            TxInner::Readonly(tx) => &tx.snapshot,
            TxInner::Readwrite(tx) => &tx.snapshot,
        }
    }

    pub fn readwrite_mut(&mut self) -> Result<&mut ReadwriteTx> {
        self.ensure_open()?;
        match &mut self.inner {
            TxInner::Readwrite(tx) => Ok(tx),
            TxInner::Readonly(_) => Err(EngineError::ReadonlyTransaction),
        }
    }
}
