use crate::btree::load_all_entries;
use crate::bytes::{
    read_u16_le, read_u32_le, read_u64_le, try_u16_len, try_u32_len, validate_key, write_u32_le,
    write_u64_le,
};
use crate::change_feed::{is_internal_store_name, validate_user_store_name};
use crate::checksum::checksum_with_zeroed_region;
use crate::error::{EngineError, Result};
use crate::pager::Pager;
use crate::storage::backend::FileBackend;
use crate::txn::Snapshot;
use crate::value::{
    validate_store_flags, validate_store_value, StoredValue, STORE_FLAG_VALUE_REVISION,
};
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
            let stored = StoredValue::decode_owned_for_store(meta.flags, pair.value)?;
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
            // Snapshot entries omit revisions. Import assigns new native versions.
            flags: meta.flags & !STORE_FLAG_VALUE_REVISION,
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
    // One buffer: geometric growth would copy every key and value again.
    // Checks run first so a rejected snapshot does not allocate that buffer.
    let encoded_len = validated_snapshot_len(contents)?;
    let mut snapshot = Vec::with_capacity(encoded_len);
    snapshot.resize(SNAPSHOT_HEADER_SIZE, 0);
    snapshot.extend_from_slice(&contents.source_last_committed_txid.to_le_bytes());
    snapshot.extend_from_slice(&contents.schema_version.to_le_bytes());
    snapshot.extend_from_slice(&store_count(contents)?.to_le_bytes());
    snapshot.extend_from_slice(&0u32.to_le_bytes());

    for store in &contents.stores {
        write_store(&mut snapshot, store)?;
    }

    snapshot[..8].copy_from_slice(&SNAPSHOT_MAGIC);
    write_u32_le(&mut snapshot, 8, SNAPSHOT_VERSION)?;
    write_u32_le(&mut snapshot, 12, 0)?;
    let body_len = u64::try_from(snapshot.len() - SNAPSHOT_HEADER_SIZE)
        .map_err(|_| EngineError::Serialization("snapshot body too large".into()))?;
    write_u64_le(&mut snapshot, 16, body_len)?;
    write_u32_le(&mut snapshot, SNAPSHOT_CHECKSUM_OFFSET, 0)?;
    write_u32_le(&mut snapshot, 28, 0)?;

    let checksum = checksum_with_zeroed_region(&snapshot, SNAPSHOT_CHECKSUM_OFFSET, 4);
    write_u32_le(&mut snapshot, SNAPSHOT_CHECKSUM_OFFSET, checksum)?;
    debug_assert_eq!(snapshot.len(), encoded_len);
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
    let source_last_committed_txid = take_u64(bytes, &mut offset)?;
    let schema_version = if version >= 2 {
        take_u64(bytes, &mut offset)?
    } else {
        0
    };
    let store_count = usize_from_u32(take_u32(bytes, &mut offset)?, "snapshot store count")?;
    let directory_flags_at = offset;
    skip_reserved(
        bytes,
        &mut offset,
        4,
        "reserved field after the store count",
    )?;
    let directory_flags =
        read_u32_le(bytes, directory_flags_at).map_err(corruption_from_engine_error)?;
    if directory_flags != 0 {
        return Err(corruption(format!(
            "unsupported snapshot directory flags {directory_flags:#x}"
        )));
    }

    let mut stores = Vec::with_capacity(store_count.min(1024));
    let mut seen_store_names: BTreeSet<&str> = BTreeSet::new();
    for _ in 0..store_count {
        let name_len = usize::from(take_u16(bytes, &mut offset)?);
        skip_reserved(
            bytes,
            &mut offset,
            2,
            "reserved field after the store name length",
        )?;
        let flags = take_u64(bytes, &mut offset)?;
        validate_store_flags(flags).map_err(corruption_from_engine_error)?;
        let entry_count = usize_from_u64(take_u64(bytes, &mut offset)?, "snapshot entry count")?;

        let name_bytes = take_slice(bytes, &mut offset, name_len, "snapshot store name")?;
        let name = std::str::from_utf8(name_bytes)
            .map_err(|err| corruption(format!("snapshot store name utf8: {err}")))?;
        validate_user_store_name(name).map_err(corruption_from_engine_error)?;
        if !seen_store_names.insert(name) {
            return Err(corruption(format!("duplicate snapshot store {name}")));
        }

        let mut entries = Vec::with_capacity(entry_count.min(4096));
        // References into `bytes`: uniqueness must not copy key bytes.
        let mut seen_keys: BTreeSet<&[u8]> = BTreeSet::new();
        for _ in 0..entry_count {
            let key_len = usize::from(take_u16(bytes, &mut offset)?);
            let raw_flags = take_u16(bytes, &mut offset)?;
            if raw_flags & !SNAPSHOT_ENTRY_FLAG_HAS_EXPIRY != 0 || (version < 3 && raw_flags != 0) {
                return Err(corruption(format!(
                    "unsupported snapshot entry flags {raw_flags:#x}"
                )));
            }
            let entry_flags = if version >= 3 { raw_flags } else { 0 };
            let value_len = usize_from_u32(take_u32(bytes, &mut offset)?, "snapshot value length")?;
            let expires_at_ms = if version >= 3 {
                let expires_at_ms = take_u64(bytes, &mut offset)?;
                (entry_flags & SNAPSHOT_ENTRY_FLAG_HAS_EXPIRY != 0).then_some(expires_at_ms)
            } else {
                None
            };

            let key = take_slice(bytes, &mut offset, key_len, "snapshot key")?;
            validate_key(key).map_err(corruption_from_engine_error)?;

            let value = take_slice(bytes, &mut offset, value_len, "snapshot value")?;
            validate_store_value(value, flags).map_err(corruption_from_engine_error)?;
            let value = value.to_vec();

            if !seen_keys.insert(key) {
                return Err(corruption(format!(
                    "duplicate snapshot key in store {name}"
                )));
            }
            entries.push(SnapshotEntry {
                key: key.to_vec(),
                value,
                expires_at_ms,
            });
        }

        stores.push(SnapshotStore {
            name: name.to_owned(),
            flags,
            entries,
        });
    }

    if offset != bytes.len() {
        return Err(corruption(format!(
            "snapshot trailing bytes: {}",
            bytes.len() - offset
        )));
    }

    Ok(SnapshotContents {
        source_last_committed_txid,
        schema_version,
        stores,
    })
}

fn store_count(contents: &SnapshotContents) -> Result<u32> {
    u32::try_from(contents.stores.len())
        .map_err(|_| EngineError::Serialization("snapshot store count overflow".into()))
}

fn entry_count(store: &SnapshotStore) -> Result<u64> {
    u64::try_from(store.entries.len())
        .map_err(|_| EngineError::Serialization("snapshot entry count overflow".into()))
}

fn add_snapshot_len(len: usize, extra: usize) -> Result<usize> {
    len.checked_add(extra)
        .ok_or_else(|| EngineError::Serialization("snapshot body too large".into()))
}

/// Validation order matches the previous single-pass encoder, including
/// duplicate keys and length checks, before any snapshot buffer exists.
fn validated_snapshot_len(contents: &SnapshotContents) -> Result<usize> {
    store_count(contents)?;
    let mut len = add_snapshot_len(SNAPSHOT_HEADER_SIZE, SNAPSHOT_BODY_PREFIX_SIZE)?;
    for store in &contents.stores {
        len = add_snapshot_len(len, validated_store_len(store)?)?;
    }
    Ok(len)
}

fn validated_store_len(store: &SnapshotStore) -> Result<usize> {
    validate_user_store_name(&store.name)?;
    validate_store_flags(store.flags)?;
    let name_bytes = store.name.as_bytes();
    try_u16_len(name_bytes.len(), "snapshot store name too long")?;
    entry_count(store)?;

    let mut len = add_snapshot_len(SNAPSHOT_STORE_HEADER_SIZE, name_bytes.len())?;
    let mut seen_keys: BTreeSet<&[u8]> = BTreeSet::new();
    for entry in &store.entries {
        validate_key(&entry.key)?;
        validate_store_value(&entry.value, store.flags)?;
        if !seen_keys.insert(entry.key.as_slice()) {
            return Err(EngineError::Serialization(format!(
                "duplicate key while encoding snapshot store {}",
                store.name
            )));
        }
        try_u16_len(entry.key.len(), "snapshot key too long")?;
        try_u32_len(entry.value.len(), "snapshot value too large to encode")?;
        len = add_snapshot_len(len, SNAPSHOT_ENTRY_HEADER_SIZE)?;
        len = add_snapshot_len(len, entry.key.len())?;
        len = add_snapshot_len(len, entry.value.len())?;
    }
    Ok(len)
}

fn write_store(dst: &mut Vec<u8>, store: &SnapshotStore) -> Result<()> {
    let name_bytes = store.name.as_bytes();
    let name_len = try_u16_len(name_bytes.len(), "snapshot store name too long")?;
    dst.extend_from_slice(&name_len.to_le_bytes());
    dst.extend_from_slice(&0u16.to_le_bytes());
    dst.extend_from_slice(&store.flags.to_le_bytes());
    dst.extend_from_slice(&entry_count(store)?.to_le_bytes());
    dst.extend_from_slice(name_bytes);

    for entry in &store.entries {
        let key_len = try_u16_len(entry.key.len(), "snapshot key too long")?;
        let value_len = try_u32_len(entry.value.len(), "snapshot value too large to encode")?;
        // Some(0) keeps the flag. A zero timestamp alone means no expiry.
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
    let header_flags = read_u32_le(bytes, 12).map_err(corruption_from_engine_error)?;
    if header_flags != 0 {
        return Err(corruption(format!(
            "unsupported snapshot header flags {header_flags:#x}"
        )));
    }
    let header_tail = read_u32_le(bytes, 28).map_err(corruption_from_engine_error)?;
    if header_tail != 0 {
        return Err(corruption(format!(
            "unsupported snapshot header flags {header_tail:#x}"
        )));
    }

    Ok(version)
}

#[inline]
fn skip_reserved(bytes: &[u8], offset: &mut usize, len: usize, what: &str) -> Result<()> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| corruption(format!("snapshot {what} length overflow")))?;
    if end > bytes.len() {
        return Err(corruption(format!("snapshot {what} is truncated")));
    }
    *offset = end;
    Ok(())
}

fn take_u16(bytes: &[u8], offset: &mut usize) -> Result<u16> {
    let value = read_u16_le(bytes, *offset).map_err(corruption_from_engine_error)?;
    *offset += 2;
    Ok(value)
}

#[inline]
fn take_u32(bytes: &[u8], offset: &mut usize) -> Result<u32> {
    let value = read_u32_le(bytes, *offset).map_err(corruption_from_engine_error)?;
    *offset += 4;
    Ok(value)
}

#[inline]
fn take_u64(bytes: &[u8], offset: &mut usize) -> Result<u64> {
    let value = read_u64_le(bytes, *offset).map_err(corruption_from_engine_error)?;
    *offset += 8;
    Ok(value)
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
