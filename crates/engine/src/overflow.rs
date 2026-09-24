use crate::btree::PageAllocator;
use crate::bytes::MAX_STORED_VALUE_BYTES;
use crate::error::{EngineError, Result};
use crate::page::{
    decode_overflow_body_ref, decode_page_header_verified, encode_overflow_page,
    max_overflow_chunk_len,
};
use crate::pager::Pager;
use crate::storage::backend::FileBackend;

#[derive(Debug, Clone)]
pub struct OverflowChain {
    pub head_page_id: u64,
    pub pages: Vec<(u64, Vec<u8>)>,
}

pub fn write_overflow_chain(value: &[u8], alloc: &mut PageAllocator) -> Result<OverflowChain> {
    if value.is_empty() {
        return Err(EngineError::Serialization(
            "overflow chain requires a non-empty value".into(),
        ));
    }
    let chunk_len = max_overflow_chunk_len();
    let chunk_count = value.len().div_ceil(chunk_len);
    let ids: Vec<u64> = (0..chunk_count).map(|_| alloc.allocate()).collect();
    let mut pages = Vec::with_capacity(chunk_count);
    for (idx, chunk) in value.chunks(chunk_len).enumerate() {
        let page_id = ids[idx];
        let next = ids.get(idx + 1).copied().unwrap_or(0);
        pages.push((page_id, encode_overflow_page(page_id, next, chunk)?));
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

/// Walks a chain, handing each chunk to `visit` in order. The walk is bounded by
/// the declared length, so a corrupt chain cannot loop or force a huge allocation.
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

/// Reads at most `prefix_len` bytes from the start of the chain, touching only
/// the pages that hold them.
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
