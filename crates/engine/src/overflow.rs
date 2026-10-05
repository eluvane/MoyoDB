use crate::btree::PageAllocator;
use crate::bytes::MAX_STORED_VALUE_BYTES;
use crate::error::{EngineError, Result};
use crate::page::{
    decode_overflow_body_ref, decode_page_header_verified, encode_overflow_value_chunk,
    max_overflow_chunk_len,
};
use crate::pager::Pager;
use crate::prepared_value::ValueSource;
use crate::storage::backend::FileBackend;
use crate::value::{
    decode_envelope_expiry, store_uses_system_raw_values, store_uses_value_envelope, StoredValue,
    VALUE_ENVELOPE_HEADER_SIZE,
};

#[derive(Debug, Clone)]
pub struct OverflowChain {
    pub head_page_id: u64,
    pub pages: Vec<(u64, Vec<u8>)>,
}

pub fn write_overflow_chain(value: &[u8], alloc: &mut PageAllocator) -> Result<OverflowChain> {
    write_overflow_value(value, alloc)
}

pub(crate) fn write_overflow_value<V: ValueSource + ?Sized>(
    value: &V,
    alloc: &mut PageAllocator,
) -> Result<OverflowChain> {
    let value = value.parts();
    if value.len() == 0 {
        return Err(EngineError::Serialization(
            "overflow chain requires a non-empty value".into(),
        ));
    }
    let chunk_len = max_overflow_chunk_len();
    let chunk_count = value.len().div_ceil(chunk_len);
    let ids: Vec<u64> = (0..chunk_count).map(|_| alloc.allocate()).collect();
    let mut pages = Vec::with_capacity(chunk_count);
    for idx in 0..chunk_count {
        let page_id = ids[idx];
        let next = ids.get(idx + 1).copied().unwrap_or(0);
        pages.push((
            page_id,
            encode_overflow_value_chunk(page_id, next, &value, idx)?,
        ));
    }
    Ok(OverflowChain {
        head_page_id: ids[0],
        pages,
    })
}

fn validate_declared_len(total_len: usize) -> Result<usize> {
    if total_len == 0 || total_len > MAX_STORED_VALUE_BYTES {
        return Err(EngineError::Corruption(format!(
            "overflow value declares invalid length {total_len}"
        )));
    }
    Ok(total_len.div_ceil(max_overflow_chunk_len()))
}

/// Bounds traversal by the declared length to stop corrupt cycles.
/// A false visit result stops without validating the rest of the chain.
fn walk_chain<B: FileBackend>(
    pager: &mut Pager<B>,
    head_page_id: u64,
    total_len: usize,
    mut visit: impl FnMut(u64, &[u8]) -> Result<bool>,
) -> Result<()> {
    let max_pages = validate_declared_len(total_len)?;
    let mut current = head_page_id;
    let mut seen_pages = 0usize;
    let mut seen_bytes = 0usize;
    while current != 0 {
        seen_pages += 1;
        if seen_pages > max_pages {
            return Err(EngineError::Corruption(format!(
                "overflow chain at page {head_page_id} is longer than its declared length {total_len}"
            )));
        }
        let (next, keep_going) = pager.with_page(current, |bytes| {
            let header = decode_page_header_verified(bytes)?;
            if header.page_id != current {
                return Err(EngineError::Corruption(format!(
                    "overflow page header id mismatch: expected {current}, got {}",
                    header.page_id
                )));
            }
            let (next, chunk) = decode_overflow_body_ref(bytes, &header)?;
            seen_bytes = seen_bytes
                .checked_add(chunk.len())
                .filter(|len| *len <= total_len)
                .ok_or_else(|| {
                    EngineError::Corruption(format!(
                        "overflow chain exceeded expected length {total_len}"
                    ))
                })?;
            Ok((next, visit(current, chunk)?))
        })?;
        if !keep_going {
            return Ok(());
        }
        current = next;
    }
    if seen_bytes != total_len {
        return Err(EngineError::Corruption(format!(
            "overflow chain length mismatch: expected {total_len}, got {seen_bytes}"
        )));
    }
    Ok(())
}

pub fn read_overflow_value<B: FileBackend>(
    pager: &mut Pager<B>,
    head_page_id: u64,
    total_len: usize,
) -> Result<Vec<u8>> {
    if head_page_id == 0 {
        return Err(EngineError::Corruption(
            "overflow value is missing its head page".into(),
        ));
    }
    validate_declared_len(total_len)?;
    let mut out = Vec::with_capacity(total_len);
    walk_chain(pager, head_page_id, total_len, |_, chunk| {
        out.extend_from_slice(chunk);
        Ok(true)
    })?;
    Ok(out)
}

/// Validates the full chain before decoding the envelope, even for expired values.
/// Keeps the header separate to avoid an extra payload copy.
pub(crate) fn read_overflow_stored_value<B: FileBackend>(
    pager: &mut Pager<B>,
    head_page_id: u64,
    total_len: usize,
    store_flags: u64,
) -> Result<StoredValue> {
    if !store_uses_value_envelope(store_flags) || store_uses_system_raw_values(store_flags) {
        return StoredValue::decode_owned_for_store(
            store_flags,
            read_overflow_value(pager, head_page_id, total_len)?,
        );
    }
    if head_page_id == 0 {
        return Err(EngineError::Corruption(
            "overflow value is missing its head page".into(),
        ));
    }
    validate_declared_len(total_len)?;
    let mut prefix = [0; VALUE_ENVELOPE_HEADER_SIZE];
    let mut prefix_len = 0;
    let mut value = Vec::with_capacity(total_len.saturating_sub(VALUE_ENVELOPE_HEADER_SIZE));
    walk_chain(pager, head_page_id, total_len, |_, chunk| {
        let take = (VALUE_ENVELOPE_HEADER_SIZE - prefix_len).min(chunk.len());
        prefix[prefix_len..prefix_len + take].copy_from_slice(&chunk[..take]);
        prefix_len += take;
        value.extend_from_slice(&chunk[take..]);
        Ok(true)
    })?;
    StoredValue::decode_envelope_parts(&prefix[..prefix_len], value)
}

/// Reads only the TTL header. It may span chunks, and later pages are not checked.
pub(crate) fn read_overflow_expiry<B: FileBackend>(
    pager: &mut Pager<B>,
    head_page_id: u64,
    total_len: usize,
) -> Result<Option<u64>> {
    if head_page_id == 0 {
        return Err(EngineError::Corruption(
            "overflow value is missing its head page".into(),
        ));
    }
    let wanted = VALUE_ENVELOPE_HEADER_SIZE.min(total_len);
    let mut prefix = [0; VALUE_ENVELOPE_HEADER_SIZE];
    let mut prefix_len = 0;
    if wanted > 0 {
        walk_chain(pager, head_page_id, total_len, |_, chunk| {
            let take = (wanted - prefix_len).min(chunk.len());
            prefix[prefix_len..prefix_len + take].copy_from_slice(&chunk[..take]);
            prefix_len += take;
            Ok(prefix_len < wanted)
        })?;
    }
    decode_envelope_expiry(&prefix[..prefix_len])
}

/// Reads at most `prefix_len` leading bytes. Pages after the prefix are not checked.
pub fn read_overflow_prefix<B: FileBackend>(
    pager: &mut Pager<B>,
    head_page_id: u64,
    total_len: usize,
    prefix_len: usize,
) -> Result<Vec<u8>> {
    if head_page_id == 0 {
        return Err(EngineError::Corruption(
            "overflow value is missing its head page".into(),
        ));
    }
    let wanted = prefix_len.min(total_len);
    let mut out = Vec::with_capacity(wanted);
    if wanted == 0 {
        return Ok(out);
    }
    walk_chain(pager, head_page_id, total_len, |_, chunk| {
        let take = (wanted - out.len()).min(chunk.len());
        out.extend_from_slice(&chunk[..take]);
        Ok(out.len() < wanted)
    })?;
    Ok(out)
}

/// Retires every page of a committed chain.
pub fn free_overflow_chain<B: FileBackend>(
    pager: &mut Pager<B>,
    head_page_id: u64,
    total_len: usize,
    alloc: &mut PageAllocator,
) -> Result<()> {
    walk_chain(pager, head_page_id, total_len, |page_id, _| {
        alloc.free(page_id);
        Ok(true)
    })
}
