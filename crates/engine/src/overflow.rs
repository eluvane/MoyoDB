use crate::error::{EngineError, Result};
use crate::page::{decode_page, encode_overflow_page, max_overflow_chunk_len};
use crate::pager::Pager;
use crate::storage::backend::FileBackend;
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct OverflowChain {
    pub head_page_id: u64,
    pub pages: Vec<(u64, Vec<u8>)>,
}

pub fn write_overflow_chain(value: &[u8], next_page_id: &mut u64) -> Result<OverflowChain> {
    let chunk_len = max_overflow_chunk_len();
    let chunks: Vec<Vec<u8>> = value
        .chunks(chunk_len)
        .map(|chunk| chunk.to_vec())
        .collect();
    let mut pages = Vec::with_capacity(chunks.len());
    let mut head = 0;
    let mut ids = Vec::with_capacity(chunks.len());
    for _ in 0..chunks.len() {
        ids.push(*next_page_id);
        *next_page_id += 1;
    }
    for (idx, chunk) in chunks.into_iter().enumerate() {
        let page_id = ids[idx];
        let next = ids.get(idx + 1).copied().unwrap_or(0);
        let image = encode_overflow_page(page_id, next, &chunk)?;
        if idx == 0 {
            head = page_id;
        }
        pages.push((page_id, image));
    }
    Ok(OverflowChain {
        head_page_id: head,
        pages,
    })
}

pub fn read_overflow_value<B: FileBackend>(
    pager: &mut Pager<B>,
    head_page_id: u64,
    total_len: usize,
) -> Result<Vec<u8>> {
    if head_page_id == 0 {
        return Ok(Vec::new());
    }
    let mut current = head_page_id;
    let mut out = Vec::with_capacity(total_len);
    let mut visited = HashSet::new();
    while current != 0 {
        if !visited.insert(current) {
            return Err(EngineError::Corruption(format!(
                "overflow chain contains a cycle at page {current}"
            )));
        }
        let image = pager.read_page(current)?;
        let page = decode_page(&image)?;
        if page.header.page_id != current {
            return Err(EngineError::Corruption(format!(
                "overflow page header id mismatch: expected {current}, got {}",
                page.header.page_id
            )));
        }
        let body = page
            .overflow
            .ok_or_else(|| EngineError::Corruption("expected overflow page".into()))?;
        out.extend_from_slice(&body.chunk);
        if out.len() > total_len {
            return Err(EngineError::Corruption(format!(
                "overflow chain exceeded expected length {total_len}"
            )));
        }
        current = body.next_overflow_page_id;
    }
    if out.len() != total_len {
        return Err(EngineError::Corruption(format!(
            "overflow chain length mismatch: expected {total_len}, got {}",
            out.len()
        )));
    }
    Ok(out)
}
