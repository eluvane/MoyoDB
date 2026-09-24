use crate::bytes::{read_u64_le, write_u64_le};
use crate::error::{EngineError, Result};
use crate::layout::StoreMetadata;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const STORE_METADATA_ENCODED_LEN: usize = 24;
const SCHEMA_VERSION_ENCODED_LEN: usize = 8;
const CHANGE_FEED_FLOOR_TXID_ENCODED_LEN: usize = 8;

const CHANGE_FEED_POLICY_ENCODED_LEN: usize = 16;
const CHANGE_FEED_POLICY_FLAG_DISABLED: u64 = 1 << 0;

// Legacy on-disk namespace preserved for storage-format compatibility; do not rename without a migration.
pub const CATALOG_SCHEMA_VERSION_KEY: &[u8] = b"\xffbrowserdb:schema_version";
pub const CATALOG_CHANGE_FEED_FLOOR_TXID_KEY: &[u8] = b"\xffbrowserdb:change_feed_floor_txid";
pub const CATALOG_CHANGE_FEED_POLICY_KEY: &[u8] = b"\xffbrowserdb:change_feed_policy";

/// Transactions of history kept when a database has never configured retention.
pub const DEFAULT_CHANGE_FEED_RETAIN_TXIDS: u64 = 100_000;

pub type CatalogMap = BTreeMap<String, StoreMetadata>;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChangeFeedPolicy {
    pub enabled: bool,
    /// Committed transactions kept in the log; `None` keeps everything.
    pub retain_txids: Option<u64>,
}

impl Default for ChangeFeedPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            retain_txids: Some(DEFAULT_CHANGE_FEED_RETAIN_TXIDS),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogState {
    pub schema_version: u64,
    pub change_feed_floor_txid: u64,
    pub change_feed_policy: ChangeFeedPolicy,
    pub stores: CatalogMap,
}

pub fn encode_change_feed_policy(policy: &ChangeFeedPolicy) -> Result<Vec<u8>> {
    let mut out = vec![0u8; CHANGE_FEED_POLICY_ENCODED_LEN];
    let flags = if policy.enabled {
        0
    } else {
        CHANGE_FEED_POLICY_FLAG_DISABLED
    };
    write_u64_le(&mut out, 0, flags)?;
    write_u64_le(&mut out, 8, policy.retain_txids.unwrap_or(0))?;
    Ok(out)
}

pub fn decode_change_feed_policy(bytes: &[u8]) -> Result<ChangeFeedPolicy> {
    if bytes.len() != CHANGE_FEED_POLICY_ENCODED_LEN {
        return Err(EngineError::Corruption(format!(
            "change feed policy length mismatch: expected {CHANGE_FEED_POLICY_ENCODED_LEN}, got {}",
            bytes.len()
        )));
    }
    let flags = read_u64_le(bytes, 0)?;
    if flags & !CHANGE_FEED_POLICY_FLAG_DISABLED != 0 {
        return Err(EngineError::Corruption(format!(
            "change feed policy has unknown flags {flags:#x}"
        )));
    }
    let retain = read_u64_le(bytes, 8)?;
    Ok(ChangeFeedPolicy {
        enabled: flags & CHANGE_FEED_POLICY_FLAG_DISABLED == 0,
        retain_txids: if retain == 0 { None } else { Some(retain) },
    })
}

pub fn encode_store_metadata(meta: &StoreMetadata) -> Result<Vec<u8>> {
    let mut out = vec![0u8; STORE_METADATA_ENCODED_LEN];
    write_u64_le(&mut out, 0, meta.store_root_page_id)?;
    write_u64_le(&mut out, 8, meta.created_txid)?;
    write_u64_le(&mut out, 16, meta.flags)?;
    Ok(out)
}

pub fn decode_store_metadata(bytes: &[u8]) -> Result<StoreMetadata> {
    if bytes.len() != STORE_METADATA_ENCODED_LEN {
        return Err(EngineError::Serialization(format!(
            "store metadata length mismatch: expected {STORE_METADATA_ENCODED_LEN}, got {}",
            bytes.len()
        )));
    }
    Ok(StoreMetadata {
        store_root_page_id: read_u64_le(bytes, 0)?,
        created_txid: read_u64_le(bytes, 8)?,
        flags: read_u64_le(bytes, 16)?,
    })
}

pub fn encode_schema_version(version: u64) -> Result<Vec<u8>> {
    let mut out = vec![0u8; SCHEMA_VERSION_ENCODED_LEN];
    write_u64_le(&mut out, 0, version)?;
    Ok(out)
}

pub fn decode_schema_version(bytes: &[u8]) -> Result<u64> {
    if bytes.len() != SCHEMA_VERSION_ENCODED_LEN {
        return Err(EngineError::Serialization(format!(
            "schema version metadata length mismatch: expected {SCHEMA_VERSION_ENCODED_LEN}, got {}",
            bytes.len()
        )));
    }
    read_u64_le(bytes, 0)
}

pub fn encode_change_feed_floor_txid(txid: u64) -> Result<Vec<u8>> {
    let mut out = vec![0u8; CHANGE_FEED_FLOOR_TXID_ENCODED_LEN];
    write_u64_le(&mut out, 0, txid)?;
    Ok(out)
}

pub fn decode_change_feed_floor_txid(bytes: &[u8]) -> Result<u64> {
    if bytes.len() != CHANGE_FEED_FLOOR_TXID_ENCODED_LEN {
        return Err(EngineError::Serialization(format!(
            "change feed floor metadata length mismatch: expected {CHANGE_FEED_FLOOR_TXID_ENCODED_LEN}, got {}",
            bytes.len()
        )));
    }
    read_u64_le(bytes, 0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogSnapshot {
    pub schema_version: u64,
    pub stores: CatalogMap,
}
