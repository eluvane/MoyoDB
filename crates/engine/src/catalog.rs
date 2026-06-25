use crate::bytes::{read_u64_le, write_u64_le};
use crate::error::{EngineError, Result};
use crate::layout::StoreMetadata;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const STORE_METADATA_ENCODED_LEN: usize = 24;
const SCHEMA_VERSION_ENCODED_LEN: usize = 8;
const CHANGE_FEED_FLOOR_TXID_ENCODED_LEN: usize = 8;

// Legacy on-disk namespace preserved for storage-format compatibility; do not rename without a migration.
pub const CATALOG_SCHEMA_VERSION_KEY: &[u8] = b"\xffbrowserdb:schema_version";
pub const CATALOG_CHANGE_FEED_FLOOR_TXID_KEY: &[u8] = b"\xffbrowserdb:change_feed_floor_txid";

pub type CatalogMap = BTreeMap<String, StoreMetadata>;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogState {
    pub schema_version: u64,
    pub change_feed_floor_txid: u64,
    pub stores: CatalogMap,
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
