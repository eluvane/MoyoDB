use crate::bytes::{read_u16_le, read_u32_le, validate_key, validate_store_name};
use crate::catalog::CatalogMap;
use crate::error::{EngineError, Result};
use crate::value::STORE_FLAG_SYSTEM_RAW_VALUES;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub type TxId = u64;

// Legacy internal store namespace preserved for storage-format compatibility; do not rename without a migration.
pub const INTERNAL_STORE_PREFIX: &str = "__browserdb:";
pub const SYSTEM_CHANGELOG_STORE_NAME: &str = "__browserdb:changes";
pub const CHANGELOG_STORE_FLAGS: u64 = STORE_FLAG_SYSTEM_RAW_VALUES;

const CHANGE_RECORD_MAGIC: [u8; 4] = *b"CHG1";
const CHANGE_RECORD_HEADER_SIZE: usize = 14;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChangeFeedOptions {
    pub stores: Option<Vec<String>>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    Put,
    Delete,
}

impl ChangeKind {
    fn to_tag(self) -> u8 {
        match self {
            ChangeKind::Put => 1,
            ChangeKind::Delete => 2,
        }
    }

    fn from_tag(tag: u8) -> Result<Self> {
        match tag {
            1 => Ok(Self::Put),
            2 => Ok(Self::Delete),
            other => Err(EngineError::Corruption(format!(
                "unknown change log kind tag {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChangeRecord {
    pub tx_id: TxId,
    pub store: String,
    pub key: Vec<u8>,
    pub kind: ChangeKind,
    pub value: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChangeFeed {
    pub changes: Vec<ChangeRecord>,
    pub latest_tx_id: TxId,
}

pub fn is_internal_store_name(name: &str) -> bool {
    name.starts_with(INTERNAL_STORE_PREFIX)
}

pub fn validate_user_store_name(name: &str) -> Result<()> {
    validate_store_name(name)?;
    if is_internal_store_name(name) {
        return Err(EngineError::ReservedStoreName(name.into()));
    }
    Ok(())
}

pub fn visible_store_names(catalog: &CatalogMap) -> Vec<String> {
    catalog
        .keys()
        .filter(|name| !is_internal_store_name(name))
        .cloned()
        .collect()
}

pub fn visible_store_count(catalog: &CatalogMap) -> usize {
    catalog
        .keys()
        .filter(|name| !is_internal_store_name(name))
        .count()
}

pub fn normalize_store_filter(stores: Option<&[String]>) -> Result<Option<BTreeSet<String>>> {
    let Some(stores) = stores else {
        return Ok(None);
    };

    let mut normalized = BTreeSet::new();
    for store in stores {
        validate_user_store_name(store)?;
        normalized.insert(store.clone());
    }
    Ok(Some(normalized))
}

pub fn encode_change_log_key(txid: TxId, sequence: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(12);
    out.extend_from_slice(&txid.to_be_bytes());
    out.extend_from_slice(&sequence.to_be_bytes());
    out
}

pub fn encode_after_txid_key(txid: TxId) -> Vec<u8> {
    encode_change_log_key(txid, u32::MAX)
}

pub fn encode_change_record_payload(
    store: &str,
    key: &[u8],
    kind: ChangeKind,
    value: Option<&[u8]>,
) -> Result<Vec<u8>> {
    validate_user_store_name(store)?;
    validate_key(key)?;

    match (kind, value) {
        (ChangeKind::Put, Some(_)) | (ChangeKind::Delete, None) => {}
        (ChangeKind::Put, None) => {
            return Err(EngineError::Serialization(
                "change log put record is missing a value".into(),
            ))
        }
        (ChangeKind::Delete, Some(_)) => {
            return Err(EngineError::Serialization(
                "change log delete record unexpectedly included a value".into(),
            ))
        }
    }

    let store_bytes = store.as_bytes();
    let store_len = u16::try_from(store_bytes.len()).map_err(|_| {
        EngineError::Serialization(format!(
            "change log store name too long: {}",
            store_bytes.len()
        ))
    })?;
    let key_len = u16::try_from(key.len()).map_err(|_| {
        EngineError::Serialization(format!("change log key too long: {}", key.len()))
    })?;
    let value_bytes = value.unwrap_or(&[]);
    let value_len = u32::try_from(value_bytes.len()).map_err(|_| {
        EngineError::Serialization(format!(
            "change log value too large to encode: {}",
            value_bytes.len()
        ))
    })?;

    let mut out = Vec::with_capacity(
        CHANGE_RECORD_HEADER_SIZE + store_bytes.len() + key.len() + value_bytes.len(),
    );
    out.extend_from_slice(&CHANGE_RECORD_MAGIC);
    out.push(kind.to_tag());
    out.push(0);
    out.extend_from_slice(&store_len.to_le_bytes());
    out.extend_from_slice(&key_len.to_le_bytes());
    out.extend_from_slice(&value_len.to_le_bytes());
    out.extend_from_slice(store_bytes);
    out.extend_from_slice(key);
    out.extend_from_slice(value_bytes);
    Ok(out)
}

pub fn decode_change_record_payload(txid: TxId, payload: &[u8]) -> Result<ChangeRecord> {
    if payload.len() < CHANGE_RECORD_HEADER_SIZE {
        return Err(EngineError::Corruption(format!(
            "change log payload too short: expected at least {CHANGE_RECORD_HEADER_SIZE} bytes, got {}",
            payload.len()
        )));
    }
    if payload[..4] != CHANGE_RECORD_MAGIC {
        return Err(EngineError::Corruption(
            "change log payload magic mismatch".into(),
        ));
    }

    let kind = ChangeKind::from_tag(payload[4])?;
    let store_len = usize::from(read_u16_le(payload, 6)?);
    let key_len = usize::from(read_u16_le(payload, 8)?);
    let value_len = usize::try_from(read_u32_le(payload, 10)?)
        .map_err(|_| EngineError::Corruption("change log value length overflow".into()))?;

    let header_end = CHANGE_RECORD_HEADER_SIZE;
    let store_end = header_end
        .checked_add(store_len)
        .ok_or_else(|| EngineError::Corruption("change log store length overflow".into()))?;
    let key_end = store_end
        .checked_add(key_len)
        .ok_or_else(|| EngineError::Corruption("change log key length overflow".into()))?;
    let value_end = key_end
        .checked_add(value_len)
        .ok_or_else(|| EngineError::Corruption("change log value length overflow".into()))?;

    if payload.len() != value_end {
        return Err(EngineError::Corruption(format!(
            "change log payload length mismatch: expected {value_end} bytes, got {}",
            payload.len()
        )));
    }

    let store = String::from_utf8(payload[header_end..store_end].to_vec())
        .map_err(|err| EngineError::Corruption(format!("change log store name utf8: {err}")))?;
    validate_user_store_name(&store)?;

    let key = payload[store_end..key_end].to_vec();
    validate_key(&key)?;

    let value = match kind {
        ChangeKind::Put => Some(payload[key_end..value_end].to_vec()),
        ChangeKind::Delete => {
            if value_len != 0 {
                return Err(EngineError::Corruption(
                    "change log delete record unexpectedly stored value bytes".into(),
                ));
            }
            None
        }
    };

    Ok(ChangeRecord {
        tx_id: txid,
        store,
        key,
        kind,
        value,
    })
}
