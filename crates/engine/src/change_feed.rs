use crate::bytes::{
    read_u16_le, read_u32_le, try_u16_len, try_u32_len, validate_key, validate_store_name,
    MAX_KEY_BYTES, MAX_STORE_NAME_BYTES,
};
use crate::catalog::CatalogMap;
use crate::error::{EngineError, Result};
use crate::value::STORE_FLAG_SYSTEM_RAW_VALUES;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub type TxId = u64;

// Keep this stored namespace for format compatibility. Renaming requires a migration.
pub const INTERNAL_STORE_PREFIX: &str = "__browserdb:";
pub const SYSTEM_CHANGELOG_STORE_NAME: &str = "__browserdb:changes";
pub const CHANGELOG_STORE_FLAGS: u64 = STORE_FLAG_SYSTEM_RAW_VALUES;

const CHANGE_RECORD_MAGIC: [u8; 4] = *b"CHG1";
const CHANGE_RECORD_HEADER_SIZE: usize = 14;
pub(crate) const MAX_CHANGE_RECORD_PREFIX_SIZE: usize =
    CHANGE_RECORD_HEADER_SIZE + MAX_STORE_NAME_BYTES + MAX_KEY_BYTES;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChangeFeedOptions {
    pub stores: Option<Vec<String>>,
    pub limit: Option<usize>,
}

/// `Clear` and `Drop` each use one store-level record with an empty key.
/// Their log size does not depend on the number of keys in the store.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    Put,
    Delete,
    Clear,
    Drop,
}

impl ChangeKind {
    fn to_tag(self) -> u8 {
        match self {
            ChangeKind::Put => 1,
            ChangeKind::Delete => 2,
            ChangeKind::Clear => 3,
            ChangeKind::Drop => 4,
        }
    }

    fn from_tag(tag: u8) -> Result<Self> {
        match tag {
            1 => Ok(Self::Put),
            2 => Ok(Self::Delete),
            3 => Ok(Self::Clear),
            4 => Ok(Self::Drop),
            other => Err(EngineError::Corruption(format!(
                "unknown change log kind tag {other}"
            ))),
        }
    }

    pub fn is_store_level(self) -> bool {
        matches!(self, ChangeKind::Clear | ChangeKind::Drop)
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

pub(crate) struct ChangeRecordRef<'a> {
    pub(crate) store: &'a str,
    key: &'a [u8],
    kind: ChangeKind,
    value: Option<&'a [u8]>,
}

pub(crate) struct ChangeRecordPrefixRef<'a> {
    pub(crate) store: &'a str,
    pub(crate) key: &'a [u8],
    pub(crate) kind: ChangeKind,
    pub(crate) value_prefix: &'a [u8],
    pub(crate) value_len: usize,
}

impl ChangeRecordRef<'_> {
    pub(crate) fn into_owned(self, txid: TxId) -> ChangeRecord {
        ChangeRecord {
            tx_id: txid,
            store: self.store.to_owned(),
            key: self.key.to_vec(),
            kind: self.kind,
            value: self.value.map(|value| value.to_vec()),
        }
    }
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
    encode_change_record(store, key, kind, value, true)
}

pub(crate) fn encode_change_record_prefix(
    store: &str,
    key: &[u8],
    kind: ChangeKind,
    value: Option<&[u8]>,
) -> Result<Vec<u8>> {
    encode_change_record(store, key, kind, value, false)
}

fn encode_change_record(
    store: &str,
    key: &[u8],
    kind: ChangeKind,
    value: Option<&[u8]>,
    include_value: bool,
) -> Result<Vec<u8>> {
    validate_user_store_name(store)?;
    validate_key(key)?;

    match (kind, value) {
        (ChangeKind::Put, Some(_)) | (ChangeKind::Delete, None) => {}
        (ChangeKind::Clear | ChangeKind::Drop, None) if key.is_empty() => {}
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
        (ChangeKind::Clear | ChangeKind::Drop, _) => {
            return Err(EngineError::Serialization(
                "store-level change records carry no key or value".into(),
            ))
        }
    }

    let store_bytes = store.as_bytes();
    let store_len = try_u16_len(store_bytes.len(), "change log store name too long")?;
    let key_len = try_u16_len(key.len(), "change log key too long")?;
    let value_bytes = value.unwrap_or(&[]);
    let value_len = try_u32_len(value_bytes.len(), "change log value too large to encode")?;

    let mut out = Vec::with_capacity(
        CHANGE_RECORD_HEADER_SIZE
            + store_bytes.len()
            + key.len()
            + if include_value { value_bytes.len() } else { 0 },
    );
    out.extend_from_slice(&CHANGE_RECORD_MAGIC);
    out.push(kind.to_tag());
    out.push(0);
    out.extend_from_slice(&store_len.to_le_bytes());
    out.extend_from_slice(&key_len.to_le_bytes());
    out.extend_from_slice(&value_len.to_le_bytes());
    out.extend_from_slice(store_bytes);
    out.extend_from_slice(key);
    if include_value {
        out.extend_from_slice(value_bytes);
    }
    Ok(out)
}

#[inline]
fn span_end(start: usize, len: usize, overflow: &'static str) -> Result<usize> {
    start
        .checked_add(len)
        .ok_or_else(|| EngineError::Corruption(overflow.into()))
}

pub fn decode_change_record_payload(txid: TxId, payload: &[u8]) -> Result<ChangeRecord> {
    Ok(decode_change_record_payload_ref(payload)?.into_owned(txid))
}

/// Validates the complete payload before callers inspect the borrowed fields.
pub(crate) fn decode_change_record_payload_ref(payload: &[u8]) -> Result<ChangeRecordRef<'_>> {
    let record = decode_change_record_prefix_ref(payload, payload.len())?.ok_or_else(|| {
        EngineError::Corruption(format!(
            "change log payload too short: expected at least {CHANGE_RECORD_HEADER_SIZE} bytes, got {}",
            payload.len()
        ))
    })?;
    Ok(ChangeRecordRef {
        store: record.store,
        key: record.key,
        kind: record.kind,
        value: (record.kind == ChangeKind::Put).then_some(record.value_prefix),
    })
}

/// Returns None until the prefix contains the header, store name, and key.
/// The declared total length includes the body.
pub(crate) fn decode_change_record_prefix_ref(
    payload: &[u8],
    total_len: usize,
) -> Result<Option<ChangeRecordPrefixRef<'_>>> {
    if payload.len() < CHANGE_RECORD_HEADER_SIZE {
        return Ok(None);
    }
    if payload[..4] != CHANGE_RECORD_MAGIC {
        return Err(EngineError::Corruption(
            "change log payload magic mismatch".into(),
        ));
    }
    if payload[5] != 0 {
        return Err(EngineError::Corruption(format!(
            "unsupported change log flags {}",
            payload[5]
        )));
    }

    let kind = ChangeKind::from_tag(payload[4])?;
    let store_len = usize::from(read_u16_le(payload, 6)?);
    let key_len = usize::from(read_u16_le(payload, 8)?);
    let value_len = usize::try_from(read_u32_le(payload, 10)?)
        .map_err(|_| EngineError::Corruption("change log value length overflow".into()))?;

    let header_end = CHANGE_RECORD_HEADER_SIZE;
    let store_end = span_end(header_end, store_len, "change log store length overflow")?;
    let key_end = span_end(store_end, key_len, "change log key length overflow")?;
    let value_end = span_end(key_end, value_len, "change log value length overflow")?;

    if total_len != value_end {
        return Err(EngineError::Corruption(format!(
            "change log payload length mismatch: expected {value_end} bytes, got {}",
            total_len
        )));
    }
    if store_len > MAX_STORE_NAME_BYTES || key_len > MAX_KEY_BYTES {
        return Err(EngineError::Corruption(
            "change log prefix exceeds its length limits".into(),
        ));
    }
    if payload.len() < key_end {
        return Ok(None);
    }
    if payload.len() > value_end {
        return Err(EngineError::Corruption(
            "change log prefix exceeds its payload length".into(),
        ));
    }

    let store = std::str::from_utf8(&payload[header_end..store_end])
        .map_err(|err| EngineError::Corruption(format!("change log store name utf8: {err}")))?;
    validate_user_store_name(store)?;

    let key = &payload[store_end..key_end];
    validate_key(key)?;

    match kind {
        ChangeKind::Put => {}
        ChangeKind::Delete | ChangeKind::Clear | ChangeKind::Drop => {
            if value_len != 0 {
                return Err(EngineError::Corruption(
                    "change log record unexpectedly stored value bytes".into(),
                ));
            }
            if kind.is_store_level() && key_len != 0 {
                return Err(EngineError::Corruption(
                    "store-level change record unexpectedly stored a key".into(),
                ));
            }
        }
    }

    Ok(Some(ChangeRecordPrefixRef {
        store,
        key,
        kind,
        value_prefix: &payload[key_end..],
        value_len,
    }))
}
