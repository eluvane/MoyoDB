use crate::btree::load_all_entries;
use crate::bytes::{
    read_u16_le, read_u32_le, read_u64_le, validate_key, validate_value, write_u32_le, write_u64_le,
};
use crate::change_feed::{is_internal_store_name, validate_user_store_name};
use crate::checksum::checksum_with_zeroed_region;
use crate::error::{EngineError, Result};
use crate::pager::Pager;
use crate::storage::backend::FileBackend;
use crate::txn::Snapshot;
use crate::value::StoredValue;
use std::collections::BTreeSet;
use std::convert::TryFrom;

pub const SNAPSHOT_MAGIC: [u8; 8] = *b"BDSNAP01";
pub const SNAPSHOT_VERSION: u32 = 3;
pub const SNAPSHOT_HEADER_SIZE: usize = 32;
pub const SNAPSHOT_BODY_PREFIX_SIZE: usize = 24;
pub const SNAPSHOT_STORE_HEADER_SIZE: usize = 20;
pub const SNAPSHOT_ENTRY_HEADER_SIZE_V1_V2: usize = 8;
pub const SNAPSHOT_ENTRY_HEADER_SIZE: usize = 16;
pub const SNAPSHOT_CHECKSUM_OFFSET: usize = 24;
pub const SNAPSHOT_ENTRY_FLAG_HAS_EXPIRY: u16 = 1 << 0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotEntry {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    pub expires_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotStore {
    pub name: String,
    pub flags: u64,
    pub entries: Vec<SnapshotEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotContents {
    pub source_last_committed_txid: u64,
    pub schema_version: u64,
    pub stores: Vec<SnapshotStore>,
}

pub fn collect_snapshot_contents<B: FileBackend>(
    pager: &mut Pager<B>,
    snapshot: &Snapshot,
    now_ms: u64,
) -> Result<SnapshotContents> {
    let mut stores = Vec::with_capacity(snapshot.catalog.len());
    for (name, meta) in snapshot.catalog.iter() {
        if is_internal_store_name(name) {
            continue;
        }
        validate_user_store_name(name)?;
        let mut entries = Vec::new();
        for pair in load_all_entries(pager, meta.store_root_page_id)? {
            let stored = StoredValue::decode_for_store(meta.flags, &pair.value)?;
            if stored.is_expired_at(now_ms) {
                continue;
            }
            entries.push(SnapshotEntry {
                key: pair.key,
                value: stored.value,
                expires_at_ms: stored.expires_at_ms,
            });
        }
        stores.push(SnapshotStore {
            name: name.clone(),
            flags: meta.flags,
            entries,
        });
    }
    Ok(SnapshotContents {
        source_last_committed_txid: snapshot.last_committed_txid,
        schema_version: snapshot.schema_version,
        stores,
    })
}

pub fn encode_snapshot(contents: &SnapshotContents) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    body.extend_from_slice(&contents.source_last_committed_txid.to_le_bytes());
    body.extend_from_slice(&contents.schema_version.to_le_bytes());
    body.extend_from_slice(
        &u32::try_from(contents.stores.len())
            .map_err(|_| EngineError::Serialization("snapshot store count overflow".into()))?
            .to_le_bytes(),
    );
    body.extend_from_slice(&0u32.to_le_bytes());

    for store in &contents.stores {
        encode_store(&mut body, store)?;
    }

    let mut snapshot = vec![0u8; SNAPSHOT_HEADER_SIZE];
    snapshot[..8].copy_from_slice(&SNAPSHOT_MAGIC);
    write_u32_le(&mut snapshot, 8, SNAPSHOT_VERSION)?;
    write_u32_le(&mut snapshot, 12, 0)?;
    write_u64_le(
        &mut snapshot,
        16,
        u64::try_from(body.len())
            .map_err(|_| EngineError::Serialization("snapshot body too large".into()))?,
    )?;
    write_u32_le(&mut snapshot, SNAPSHOT_CHECKSUM_OFFSET, 0)?;
    write_u32_le(&mut snapshot, 28, 0)?;
    snapshot.extend_from_slice(&body);

    let checksum = checksum_with_zeroed_region(&snapshot, SNAPSHOT_CHECKSUM_OFFSET, 4);
    write_u32_le(&mut snapshot, SNAPSHOT_CHECKSUM_OFFSET, checksum)?;
    Ok(snapshot)
}

pub fn decode_snapshot(bytes: &[u8]) -> Result<SnapshotContents> {
    let version = validate_snapshot_header(bytes)?;

    let body_len = usize_from_u64(
        read_u64_le(bytes, 16).map_err(corruption_from_engine_error)?,
        "snapshot body length",
    )?;
    let expected_total_len = SNAPSHOT_HEADER_SIZE
        .checked_add(body_len)
        .ok_or_else(|| corruption("snapshot length overflow"))?;
    if bytes.len() != expected_total_len {
        return Err(corruption(format!(
            "snapshot length mismatch: expected {expected_total_len} bytes, got {}",
            bytes.len()
        )));
    }

    let mut offset = SNAPSHOT_HEADER_SIZE;
    let source_last_committed_txid =
        read_u64_le(bytes, offset).map_err(corruption_from_engine_error)?;
    offset += 8;
    let schema_version = if version >= 2 {
        let version = read_u64_le(bytes, offset).map_err(corruption_from_engine_error)?;
        offset += 8;
        version
    } else {
        0
    };
    let store_count = usize_from_u32(
        read_u32_le(bytes, offset).map_err(corruption_from_engine_error)?,
        "snapshot store count",
    )?;
    offset += 4;
    offset += 4;

    let mut stores = Vec::with_capacity(store_count.min(1024));
    let mut seen_store_names = BTreeSet::new();
    for _ in 0..store_count {
        let name_len =
            usize::from(read_u16_le(bytes, offset).map_err(corruption_from_engine_error)?);
        offset += 2;
        offset += 2;
        let flags = read_u64_le(bytes, offset).map_err(corruption_from_engine_error)?;
        offset += 8;
        let entry_count = usize_from_u64(
            read_u64_le(bytes, offset).map_err(corruption_from_engine_error)?,
            "snapshot entry count",
        )?;
        offset += 8;

        let name_bytes = take_slice(bytes, &mut offset, name_len, "snapshot store name")?;
        let name = String::from_utf8(name_bytes.to_vec())
            .map_err(|err| corruption(format!("snapshot store name utf8: {err}")))?;
        validate_user_store_name(&name).map_err(corruption_from_engine_error)?;
        if !seen_store_names.insert(name.clone()) {
            return Err(corruption(format!("duplicate snapshot store {name}")));
        }

        let mut entries = Vec::with_capacity(entry_count.min(4096));
        let mut seen_keys = BTreeSet::new();
        for _ in 0..entry_count {
            let key_len =
                usize::from(read_u16_le(bytes, offset).map_err(corruption_from_engine_error)?);
            offset += 2;
            let entry_flags = if version >= 3 {
                let entry_flags =
                    read_u16_le(bytes, offset).map_err(corruption_from_engine_error)?;
                offset += 2;
                entry_flags
            } else {
                offset += 2;
                0
            };
            let value_len = usize_from_u32(
                read_u32_le(bytes, offset).map_err(corruption_from_engine_error)?,
                "snapshot value length",
            )?;
            offset += 4;
            let expires_at_ms = if version >= 3 {
                let expires_at_ms =
                    read_u64_le(bytes, offset).map_err(corruption_from_engine_error)?;
                offset += 8;
                if entry_flags & SNAPSHOT_ENTRY_FLAG_HAS_EXPIRY != 0 {
                    Some(expires_at_ms)
                } else {
                    None
                }
            } else {
                None
            };

            let key = take_slice(bytes, &mut offset, key_len, "snapshot key")?.to_vec();
            validate_key(&key).map_err(corruption_from_engine_error)?;

            let value = take_slice(bytes, &mut offset, value_len, "snapshot value")?.to_vec();
            validate_value(&value).map_err(corruption_from_engine_error)?;

            if !seen_keys.insert(key.clone()) {
                return Err(corruption(format!(
                    "duplicate snapshot key in store {name}"
                )));
            }
            entries.push(SnapshotEntry {
                key,
                value,
                expires_at_ms,
            });
        }

        stores.push(SnapshotStore {
            name,
            flags,
            entries,
        });
    }

    if offset != bytes.len() {
        return Err(corruption(format!(
            "snapshot trailing bytes: {}",
            bytes.len().saturating_sub(offset)
        )));
    }

    Ok(SnapshotContents {
        source_last_committed_txid,
        schema_version,
        stores,
    })
}

fn encode_store(dst: &mut Vec<u8>, store: &SnapshotStore) -> Result<()> {
    validate_user_store_name(&store.name)?;
    let name_bytes = store.name.as_bytes();
    let name_len = u16::try_from(name_bytes.len()).map_err(|_| {
        EngineError::Serialization(format!(
            "snapshot store name too long: {}",
            name_bytes.len()
        ))
    })?;

    dst.extend_from_slice(&name_len.to_le_bytes());
    dst.extend_from_slice(&0u16.to_le_bytes());
    dst.extend_from_slice(&store.flags.to_le_bytes());
    dst.extend_from_slice(
        &u64::try_from(store.entries.len())
            .map_err(|_| EngineError::Serialization("snapshot entry count overflow".into()))?
            .to_le_bytes(),
    );
    dst.extend_from_slice(name_bytes);

    let mut seen_keys = BTreeSet::new();
    for entry in &store.entries {
        validate_key(&entry.key)?;
        validate_value(&entry.value)?;
        if !seen_keys.insert(entry.key.clone()) {
            return Err(EngineError::Serialization(format!(
                "duplicate key while encoding snapshot store {}",
                store.name
            )));
        }

        let key_len = u16::try_from(entry.key.len()).map_err(|_| {
            EngineError::Serialization(format!("snapshot key too long: {}", entry.key.len()))
        })?;
        let value_len = u32::try_from(entry.value.len()).map_err(|_| {
            EngineError::Serialization(format!(
                "snapshot value too large to encode: {}",
                entry.value.len()
            ))
        })?;
        let entry_flags = if entry.expires_at_ms.is_some() {
            SNAPSHOT_ENTRY_FLAG_HAS_EXPIRY
        } else {
            0
        };

        dst.extend_from_slice(&key_len.to_le_bytes());
        dst.extend_from_slice(&entry_flags.to_le_bytes());
        dst.extend_from_slice(&value_len.to_le_bytes());
        dst.extend_from_slice(&entry.expires_at_ms.unwrap_or(0).to_le_bytes());
        dst.extend_from_slice(&entry.key);
        dst.extend_from_slice(&entry.value);
    }

    Ok(())
}

fn validate_snapshot_header(bytes: &[u8]) -> Result<u32> {
    if bytes.len() < SNAPSHOT_HEADER_SIZE {
        return Err(corruption(format!(
            "snapshot too short: expected at least {SNAPSHOT_HEADER_SIZE} bytes, got {}",
            bytes.len()
        )));
    }
    if bytes[..8] != SNAPSHOT_MAGIC {
        return Err(corruption("snapshot magic mismatch"));
    }

    let expected = checksum_with_zeroed_region(bytes, SNAPSHOT_CHECKSUM_OFFSET, 4);
    let got = read_u32_le(bytes, SNAPSHOT_CHECKSUM_OFFSET).map_err(corruption_from_engine_error)?;
    if expected != got {
        return Err(corruption(format!(
            "snapshot checksum mismatch: expected {expected:#010x}, got {got:#010x}"
        )));
    }

    let version = read_u32_le(bytes, 8).map_err(corruption_from_engine_error)?;
    if version != 1 && version != 2 && version != SNAPSHOT_VERSION {
        return Err(corruption(format!(
            "unsupported snapshot version {version}"
        )));
    }

    Ok(version)
}

fn take_slice<'a>(bytes: &'a [u8], offset: &mut usize, len: usize, what: &str) -> Result<&'a [u8]> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| corruption(format!("{what} length overflow")))?;
    let slice = bytes
        .get(*offset..end)
        .ok_or_else(|| corruption(format!("{what} out of bounds")))?;
    *offset = end;
    Ok(slice)
}

fn usize_from_u32(value: u32, _what: &str) -> Result<usize> {
    usize::try_from(value).map_err(|_| corruption("snapshot integer conversion overflow"))
}

fn usize_from_u64(value: u64, what: &str) -> Result<usize> {
    usize::try_from(value).map_err(|_| corruption(format!("{what} too large")))
}

fn corruption(message: impl Into<String>) -> EngineError {
    EngineError::Corruption(message.into())
}

fn corruption_from_engine_error(err: EngineError) -> EngineError {
    match err {
        EngineError::Corruption(_) => err,
        other => EngineError::Corruption(other.to_string()),
    }
}
