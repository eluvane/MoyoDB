use crate::bytes::{
    compare_keys, read_u16_le, read_u32_le, read_u64_le, write_u16_le, write_u32_le, write_u64_le,
};
use crate::checksum::{checksum_with_zeroed_region, crc32_with_generated_overflow};
use crate::error::{EngineError, Result};
use crate::layout::{
    unsafe_read_struct, PageHeader, PageKind, ValueKind, INLINE_VALUE_LIMIT,
    PAGE_HEADER_CHECKSUM_OFFSET, PAGE_HEADER_SIZE, PAGE_MAGIC, PAGE_SIZE,
};
use crate::payload::decode_payload_descriptor;
use crate::prepared_value::ValueParts;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use zerocopy::IntoBytes;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeafCell {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    pub value_kind: ValueKind,
    pub total_value_len: u32,
    pub overflow_head_page_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InternalCell {
    pub separator: Vec<u8>,
    pub child_page_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OverflowPageBody {
    pub next_overflow_page_id: u64,
    pub chunk: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecodedPage {
    pub header: PageHeaderInfo,
    pub leaf_cells: Vec<LeafCell>,
    pub internal_cells: Vec<InternalCell>,
    pub overflow: Option<OverflowPageBody>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PageHeaderInfo {
    pub page_id: u64,
    pub page_kind: PageKind,
    pub level: u8,
    pub cell_count: u16,
    pub lower: u16,
    pub upper: u16,
    pub right_sibling_page_id: u64,
}

pub(crate) struct LeafCellRef<'a> {
    pub key: &'a [u8],
    pub inline_value: &'a [u8],
    pub value_kind: ValueKind,
    pub total_value_len: u32,
    pub overflow_head_page_id: u64,
}

pub(crate) struct InternalCellRef<'a> {
    pub separator: &'a [u8],
    pub child_page_id: u64,
}

/// Levels must decrease on descent. This limit bounds corrupt-tree traversal.
pub const MAX_TREE_LEVEL: u8 = 48;

/// Checks image size, magic, checksum, header invariants and expected page id.
/// Cells require separate validation.
pub(crate) fn verify_page_image(bytes: &[u8], expected_page_id: u64) -> Result<PageHeaderInfo> {
    let header = decode_page_header(bytes)?;
    if header.page_id != expected_page_id {
        return Err(EngineError::Corruption(format!(
            "page header id mismatch: expected {expected_page_id}, got {}",
            header.page_id
        )));
    }
    Ok(header)
}

pub(crate) fn decode_page_header(bytes: &[u8]) -> Result<PageHeaderInfo> {
    if bytes.len() != PAGE_SIZE {
        return Err(EngineError::Corruption(format!(
            "page size mismatch: expected {}, got {}",
            PAGE_SIZE,
            bytes.len()
        )));
    }
    if bytes[..4] != PAGE_MAGIC {
        return Err(EngineError::Corruption("page magic mismatch".into()));
    }
    let expected = checksum_with_zeroed_region(bytes, PAGE_HEADER_CHECKSUM_OFFSET, 4);
    let checksum = read_u32_le(bytes, PAGE_HEADER_CHECKSUM_OFFSET)?;
    if expected != checksum {
        return Err(EngineError::Corruption("page checksum mismatch".into()));
    }
    decode_page_header_verified(bytes)
}

/// Requires a verified or freshly encoded image. Rechecks header invariants
/// without checking magic or checksum.
pub(crate) fn decode_page_header_verified(bytes: &[u8]) -> Result<PageHeaderInfo> {
    if bytes.len() != PAGE_SIZE {
        return Err(EngineError::Corruption(format!(
            "page size mismatch: expected {}, got {}",
            PAGE_SIZE,
            bytes.len()
        )));
    }
    let header: PageHeader = unsafe_read_struct(&bytes[..PAGE_HEADER_SIZE])?;
    let header_info = PageHeaderInfo {
        page_id: u64::from_le(header.page_id),
        page_kind: PageKind::from_u8(header.page_kind)?,
        level: header.level,
        cell_count: u16::from_le(header.cell_count),
        lower: u16::from_le(header.lower),
        upper: u16::from_le(header.upper),
        right_sibling_page_id: u64::from_le(header.right_sibling_page_id),
    };
    validate_page_bounds(&header_info)?;
    Ok(header_info)
}

pub(crate) fn read_cell_slot(bytes: &[u8], header: &PageHeaderInfo, index: usize) -> Result<usize> {
    if index >= header.cell_count as usize {
        return Err(EngineError::Corruption(format!(
            "cell slot index {index} out of range for {} cells",
            header.cell_count
        )));
    }
    let slot = read_u16_le(bytes, PAGE_HEADER_SIZE + index * 2)? as usize;
    validate_cell_slot(slot, header)?;
    Ok(slot)
}

pub(crate) fn decode_leaf_cell_ref<'a>(bytes: &'a [u8], slot: usize) -> Result<LeafCellRef<'a>> {
    let key_len = read_u16_le(bytes, slot)? as usize;
    let value_kind = ValueKind::from_u8(
        *bytes
            .get(slot + 2)
            .ok_or_else(|| EngineError::Corruption("leaf cell kind".into()))?,
    )?;
    let total_value_len = read_u32_le(bytes, slot + 4)?;
    let overflow_head_page_id = read_u64_le(bytes, slot + 8)?;
    let inline_value_len = read_u32_le(bytes, slot + 16)? as usize;
    let key_start = slot + 20;
    let key_end = key_start
        .checked_add(key_len)
        .ok_or_else(|| EngineError::Corruption("leaf cell key length overflow".into()))?;
    let value_start = key_end;
    let value_end = value_start
        .checked_add(inline_value_len)
        .ok_or_else(|| EngineError::Corruption("leaf cell value out of bounds".into()))?;
    let key = bytes
        .get(key_start..key_end)
        .ok_or_else(|| EngineError::Corruption("leaf cell key out of bounds".into()))?;
    let inline_value = bytes
        .get(value_start..value_end)
        .ok_or_else(|| EngineError::Corruption("leaf cell value out of bounds".into()))?;
    match value_kind {
        ValueKind::Inline => {
            if overflow_head_page_id != 0 {
                return Err(EngineError::Corruption(
                    "inline leaf cell unexpectedly references overflow pages".into(),
                ));
            }
            if inline_value_len != total_value_len as usize {
                return Err(EngineError::Corruption(
                    "inline leaf cell length metadata mismatch".into(),
                ));
            }
        }
        ValueKind::Overflow => {
            if inline_value_len != 0 {
                return Err(EngineError::Corruption(
                    "overflow leaf cell unexpectedly stores inline bytes".into(),
                ));
            }
            if overflow_head_page_id == 0 {
                return Err(EngineError::Corruption(
                    "overflow leaf cell is missing head page id".into(),
                ));
            }
            if total_value_len == 0 {
                return Err(EngineError::Corruption(
                    "overflow leaf cell has zero total length".into(),
                ));
            }
        }
        ValueKind::External => {
            decode_payload_descriptor(overflow_head_page_id, total_value_len, inline_value)?;
        }
    }
    Ok(LeafCellRef {
        key,
        inline_value,
        value_kind,
        total_value_len,
        overflow_head_page_id,
    })
}

pub(crate) fn decode_internal_cell_ref<'a>(
    bytes: &'a [u8],
    slot: usize,
) -> Result<InternalCellRef<'a>> {
    let sep_len = read_u16_le(bytes, slot)? as usize;
    let child_page_id = read_u64_le(bytes, slot + 4)?;
    if child_page_id == 0 {
        return Err(EngineError::Corruption(
            "internal cell has child_page_id=0".into(),
        ));
    }
    let sep_start = slot + 12;
    let sep_end = sep_start
        .checked_add(sep_len)
        .ok_or_else(|| EngineError::Corruption("internal separator length overflow".into()))?;
    let separator = bytes
        .get(sep_start..sep_end)
        .ok_or_else(|| EngineError::Corruption("internal cell separator out of bounds".into()))?;
    Ok(InternalCellRef {
        separator,
        child_page_id,
    })
}

pub fn decode_page(bytes: &[u8]) -> Result<DecodedPage> {
    let header_info = decode_page_header(bytes)?;
    let slots = read_cell_slots(bytes, &header_info)?;
    match header_info.page_kind {
        PageKind::Leaf => {
            validate_leaf_page(bytes, &header_info)?;
            let mut leaf_cells = Vec::with_capacity(slots.len());
            for slot in slots {
                leaf_cells.push(decode_leaf_cell(&bytes[slot..])?);
            }
            Ok(DecodedPage {
                header: header_info,
                leaf_cells,
                internal_cells: Vec::new(),
                overflow: None,
            })
        }
        PageKind::Internal => {
            validate_internal_page(bytes, &header_info)?;
            let mut internal_cells = Vec::with_capacity(slots.len());
            for slot in slots {
                internal_cells.push(decode_internal_cell(&bytes[slot..])?);
            }
            Ok(DecodedPage {
                header: header_info,
                leaf_cells: Vec::new(),
                internal_cells,
                overflow: None,
            })
        }
        PageKind::Overflow => {
            let (next_overflow_page_id, chunk) = decode_overflow_body_ref(bytes, &header_info)?;
            let chunk = chunk.to_vec();
            Ok(DecodedPage {
                header: header_info,
                leaf_cells: Vec::new(),
                internal_cells: Vec::new(),
                overflow: Some(OverflowPageBody {
                    next_overflow_page_id,
                    chunk,
                }),
            })
        }
    }
}

pub(crate) fn decode_overflow_body_ref<'a>(
    bytes: &'a [u8],
    header: &PageHeaderInfo,
) -> Result<(u64, &'a [u8])> {
    if header.page_kind != PageKind::Overflow {
        return Err(EngineError::Corruption(format!(
            "page {} is not an overflow page",
            header.page_id
        )));
    }
    if header.cell_count != 0 {
        return Err(EngineError::Corruption(
            "overflow page unexpectedly contains cell slots".into(),
        ));
    }
    let next_overflow_page_id = read_u64_le(bytes, PAGE_HEADER_SIZE)?;
    let chunk_len = read_u32_le(bytes, PAGE_HEADER_SIZE + 8)? as usize;
    let chunk_start = PAGE_HEADER_SIZE + 12;
    let chunk_end = chunk_start
        .checked_add(chunk_len)
        .ok_or_else(|| EngineError::Corruption("overflow chunk length overflow".into()))?;
    if header.lower as usize != chunk_start {
        return Err(EngineError::Corruption(
            "overflow page lower bound mismatch".into(),
        ));
    }
    if header.upper as usize != chunk_end {
        return Err(EngineError::Corruption(
            "overflow page upper bound mismatch".into(),
        ));
    }
    if chunk_len == 0 {
        return Err(EngineError::Corruption(
            "overflow page has an empty chunk".into(),
        ));
    }
    let chunk = bytes
        .get(chunk_start..chunk_end)
        .ok_or_else(|| EngineError::Corruption("overflow chunk out of bounds".into()))?;
    Ok((next_overflow_page_id, chunk))
}

pub fn encode_leaf_page(
    page_id: u64,
    level: u8,
    right_sibling_page_id: u64,
    cells: &[LeafCell],
) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; PAGE_SIZE];
    let mut upper = PAGE_SIZE;
    let lower = PAGE_HEADER_SIZE + cells.len() * 2;
    for (index, cell) in cells.iter().enumerate() {
        let use_overflow = cell.value_kind == ValueKind::Overflow;
        let encoded_len = leaf_cell_size(cell.key.len(), cell.value.len(), use_overflow);
        if upper < lower + encoded_len {
            return Err(EngineError::Serialization("leaf page overflow".into()));
        }
        upper -= encoded_len;
        encode_leaf_cell_into(&mut buf[upper..upper + encoded_len], cell)?;
        write_u16_le(&mut buf, PAGE_HEADER_SIZE + index * 2, upper as u16)?;
    }
    write_page_header(
        &mut buf,
        PageHeaderInfo {
            page_id,
            page_kind: PageKind::Leaf,
            level,
            cell_count: cells.len() as u16,
            lower: lower as u16,
            upper: upper as u16,
            right_sibling_page_id,
        },
    )?;
    Ok(buf)
}

pub fn encode_internal_page(
    page_id: u64,
    level: u8,
    right_sibling_page_id: u64,
    cells: &[InternalCell],
) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; PAGE_SIZE];
    let mut upper = PAGE_SIZE;
    let lower = PAGE_HEADER_SIZE + cells.len() * 2;
    for (index, cell) in cells.iter().enumerate() {
        let encoded_len = internal_cell_size(cell.separator.len());
        if upper < lower + encoded_len {
            return Err(EngineError::Serialization("internal page overflow".into()));
        }
        upper -= encoded_len;
        encode_internal_cell_into(&mut buf[upper..upper + encoded_len], cell)?;
        write_u16_le(&mut buf, PAGE_HEADER_SIZE + index * 2, upper as u16)?;
    }
    write_page_header(
        &mut buf,
        PageHeaderInfo {
            page_id,
            page_kind: PageKind::Internal,
            level,
            cell_count: cells.len() as u16,
            lower: lower as u16,
            upper: upper as u16,
            right_sibling_page_id,
        },
    )?;
    Ok(buf)
}

pub fn encode_overflow_page(
    page_id: u64,
    next_overflow_page_id: u64,
    chunk: &[u8],
) -> Result<Vec<u8>> {
    encode_overflow_parts(page_id, next_overflow_page_id, chunk, &[], None)
}

pub(crate) fn encode_overflow_value_chunk(
    page_id: u64,
    next_overflow_page_id: u64,
    value: &ValueParts<'_>,
    index: usize,
) -> Result<Vec<u8>> {
    let (prefix, payload, checksum) = value.chunk(index);
    encode_overflow_parts(page_id, next_overflow_page_id, prefix, payload, checksum)
}

fn encode_overflow_parts(
    page_id: u64,
    next_overflow_page_id: u64,
    prefix: &[u8],
    payload: &[u8],
    chunk_checksum: Option<u32>,
) -> Result<Vec<u8>> {
    let max = max_overflow_chunk_len();
    let chunk_len = prefix.len() + payload.len();
    if chunk_len > max {
        return Err(EngineError::Serialization(format!(
            "overflow chunk too large: {} > {max}",
            chunk_len
        )));
    }
    let chunk_start = PAGE_HEADER_SIZE + 12;
    let header = page_header(PageHeaderInfo {
        page_id,
        page_kind: PageKind::Overflow,
        level: 0,
        cell_count: 0,
        lower: chunk_start as u16,
        upper: (chunk_start + chunk_len) as u16,
        right_sibling_page_id: 0,
    });
    // Append initialized bytes so payload bytes are not zeroed before copying.
    let mut buf = Vec::with_capacity(PAGE_SIZE);
    buf.extend_from_slice(header.as_bytes());
    buf.extend_from_slice(&next_overflow_page_id.to_le_bytes());
    buf.extend_from_slice(&(chunk_len as u32).to_le_bytes());
    buf.extend_from_slice(prefix);
    buf.extend_from_slice(payload);
    buf.resize(PAGE_SIZE, 0);
    let checksum = match chunk_checksum {
        Some(checksum) => crc32_with_generated_overflow(&buf[..chunk_start], checksum, chunk_len),
        None => checksum_with_zeroed_region(&buf, PAGE_HEADER_CHECKSUM_OFFSET, 4),
    };
    buf[PAGE_HEADER_CHECKSUM_OFFSET..PAGE_HEADER_CHECKSUM_OFFSET + 4]
        .copy_from_slice(&checksum.to_le_bytes());
    Ok(buf)
}

pub fn leaf_cell_size(key_len: usize, inline_value_len: usize, overflow: bool) -> usize {
    let header_len = 2 + 1 + 1 + 4 + 8 + 4;
    header_len + key_len + if overflow { 0 } else { inline_value_len }
}

pub fn internal_cell_size(separator_len: usize) -> usize {
    2 + 2 + 8 + separator_len
}

pub fn max_overflow_chunk_len() -> usize {
    PAGE_SIZE - PAGE_HEADER_SIZE - 12
}

pub fn should_overflow_value(value_len: usize) -> bool {
    value_len > INLINE_VALUE_LIMIT
}

fn encode_leaf_cell_into(dst: &mut [u8], cell: &LeafCell) -> Result<()> {
    if cell.value_kind == ValueKind::External {
        decode_payload_descriptor(
            cell.overflow_head_page_id,
            cell.total_value_len,
            &cell.value,
        )?;
    }
    let key_len = cell.key.len();
    let use_overflow = cell.value_kind == ValueKind::Overflow;
    let inline_value = if use_overflow {
        &[][..]
    } else {
        cell.value.as_slice()
    };
    write_u16_le(dst, 0, key_len as u16)?;
    dst[2] = cell.value_kind as u8;
    dst[3] = 0;
    write_u32_le(dst, 4, cell.total_value_len)?;
    write_u64_le(dst, 8, cell.overflow_head_page_id)?;
    write_u32_le(dst, 16, inline_value.len() as u32)?;
    dst[20..20 + key_len].copy_from_slice(&cell.key);
    dst[20 + key_len..].copy_from_slice(inline_value);
    Ok(())
}

fn decode_leaf_cell(bytes: &[u8]) -> Result<LeafCell> {
    // The owned decoder used to add the u32 value length with unchecked
    // arithmetic. Delegating keeps hostile lengths on the checked path.
    let cell = decode_leaf_cell_ref(bytes, 0)?;
    Ok(LeafCell {
        key: cell.key.to_vec(),
        value: cell.inline_value.to_vec(),
        value_kind: cell.value_kind,
        total_value_len: cell.total_value_len,
        overflow_head_page_id: cell.overflow_head_page_id,
    })
}

fn encode_internal_cell_into(dst: &mut [u8], cell: &InternalCell) -> Result<()> {
    write_u16_le(dst, 0, cell.separator.len() as u16)?;
    write_u16_le(dst, 2, 0)?;
    write_u64_le(dst, 4, cell.child_page_id)?;
    dst[12..].copy_from_slice(&cell.separator);
    Ok(())
}

fn decode_internal_cell(bytes: &[u8]) -> Result<InternalCell> {
    let cell = decode_internal_cell_ref(bytes, 0)?;
    Ok(InternalCell {
        separator: cell.separator.to_vec(),
        child_page_id: cell.child_page_id,
    })
}

/// Largest leaf cell count that can be stored without overlap: a 20-byte cell
/// plus its 2-byte slot, inside the bytes that follow the page header.
const MAX_PACKED_LEAF_CELLS: usize = (PAGE_SIZE - PAGE_HEADER_SIZE) / 22;
/// Largest internal cell count: a 12-byte cell plus its 2-byte slot.
const MAX_PACKED_INTERNAL_CELLS: usize = (PAGE_SIZE - PAGE_HEADER_SIZE) / 14;

/// Structural checks that a valid checksum does not imply. They cost one pass
/// over the cells, so run them when a page enters the cache, not per traversal.
pub(crate) fn validate_tree_page(bytes: &[u8], header: &PageHeaderInfo) -> Result<()> {
    match header.page_kind {
        PageKind::Leaf => validate_leaf_page(bytes, header),
        PageKind::Internal => validate_internal_page(bytes, header),
        PageKind::Overflow => Ok(()),
    }
}

/// Leaf order is a search invariant. Overlapping spans and a repeated overflow
/// head are rejected here so a later copy cannot retire a chain that another
/// cell on the page still references.
pub(crate) fn validate_leaf_page(bytes: &[u8], header: &PageHeaderInfo) -> Result<()> {
    debug_assert_eq!(
        MAX_PACKED_LEAF_CELLS,
        (PAGE_SIZE - PAGE_HEADER_SIZE) / (leaf_cell_size(0, 0, true) + 2)
    );
    let count = header.cell_count as usize;
    let mut spans = [(0usize, 0usize); MAX_PACKED_LEAF_CELLS];
    let mut heads = [0u64; MAX_PACKED_LEAF_CELLS];
    let mut head_count = 0usize;
    for index in 0..count {
        let slot = read_cell_slot(bytes, header, index)?;
        let cell = decode_leaf_cell_ref(bytes, slot)?;
        if index > 0 {
            let previous_slot = read_cell_slot(bytes, header, index - 1)?;
            let previous = decode_leaf_cell_ref(bytes, previous_slot)?;
            if compare_keys(previous.key, cell.key) != Ordering::Less {
                return Err(EngineError::Corruption(format!(
                    "leaf page {} keys are not strictly increasing",
                    header.page_id
                )));
            }
        }
        if cell.value_kind == ValueKind::Overflow {
            if heads[..head_count].contains(&cell.overflow_head_page_id) {
                return Err(EngineError::Corruption(format!(
                    "leaf page {} repeats overflow head {}",
                    header.page_id, cell.overflow_head_page_id
                )));
            }
            if head_count < heads.len() {
                heads[head_count] = cell.overflow_head_page_id;
                head_count += 1;
            }
        }
        if index < spans.len() {
            spans[index] = (slot, slot + 20 + cell.key.len() + cell.inline_value.len());
        }
    }
    if count > MAX_PACKED_LEAF_CELLS {
        return Err(EngineError::Corruption(format!(
            "leaf page {} cells overlap",
            header.page_id
        )));
    }
    reject_overlapping_spans(&spans[..count], header.page_id, "leaf")
}

/// Internal binary search and copy-on-write both assume strictly increasing
/// separators and a private child page per slot.
pub(crate) fn validate_internal_page(bytes: &[u8], header: &PageHeaderInfo) -> Result<()> {
    debug_assert_eq!(
        MAX_PACKED_INTERNAL_CELLS,
        (PAGE_SIZE - PAGE_HEADER_SIZE) / (internal_cell_size(0) + 2)
    );
    let count = header.cell_count as usize;
    let mut spans = [(0usize, 0usize); MAX_PACKED_INTERNAL_CELLS];
    let mut children = [0u64; MAX_PACKED_INTERNAL_CELLS];
    for index in 0..count {
        let slot = read_cell_slot(bytes, header, index)?;
        let cell = decode_internal_cell_ref(bytes, slot)?;
        if index > 0 {
            let previous_slot = read_cell_slot(bytes, header, index - 1)?;
            let previous = decode_internal_cell_ref(bytes, previous_slot)?;
            if compare_keys(previous.separator, cell.separator) != Ordering::Less {
                return Err(EngineError::Corruption(format!(
                    "internal page {} separators are not strictly increasing",
                    header.page_id
                )));
            }
        }
        if index < children.len() && children[..index].contains(&cell.child_page_id) {
            return Err(EngineError::Corruption(format!(
                "internal page {} repeats child page {}",
                header.page_id, cell.child_page_id
            )));
        }
        if index < spans.len() {
            children[index] = cell.child_page_id;
            spans[index] = (slot, slot + 12 + cell.separator.len());
        }
    }
    if count > MAX_PACKED_INTERNAL_CELLS {
        return Err(EngineError::Corruption(format!(
            "internal page {} cells overlap",
            header.page_id
        )));
    }
    reject_overlapping_spans(&spans[..count], header.page_id, "internal")
}

fn reject_overlapping_spans(spans: &[(usize, usize)], page_id: u64, kind: &str) -> Result<()> {
    let mut order = [0usize; MAX_PACKED_INTERNAL_CELLS];
    let count = spans.len();
    if count > order.len() {
        return Err(EngineError::Corruption(format!(
            "{kind} page {page_id} cells overlap"
        )));
    }
    for (index, slot) in order.iter_mut().enumerate().take(count) {
        *slot = index;
    }
    order[..count].sort_unstable_by_key(|&index| spans[index].0);
    for pair in order[..count].windows(2) {
        let left = spans[pair[0]];
        let right = spans[pair[1]];
        if left.1 > right.0 {
            return Err(EngineError::Corruption(format!(
                "{kind} page {page_id} cells overlap"
            )));
        }
    }
    Ok(())
}

fn validate_page_bounds(header: &PageHeaderInfo) -> Result<()> {
    if header.page_id == 0 {
        return Err(EngineError::Corruption(
            "page header declares page id 0".into(),
        ));
    }
    match header.page_kind {
        PageKind::Leaf => {
            if header.level != 0 {
                return Err(EngineError::Corruption(format!(
                    "leaf page {} declares level {}",
                    header.page_id, header.level
                )));
            }
        }
        PageKind::Internal => {
            if header.level == 0 || header.level > MAX_TREE_LEVEL {
                return Err(EngineError::Corruption(format!(
                    "internal page {} declares invalid level {}",
                    header.page_id, header.level
                )));
            }
            if header.cell_count == 0 {
                return Err(EngineError::Corruption(format!(
                    "internal page {} has no children",
                    header.page_id
                )));
            }
        }
        PageKind::Overflow => {
            if header.level != 0 || header.cell_count != 0 || header.right_sibling_page_id != 0 {
                return Err(EngineError::Corruption(format!(
                    "overflow page {} has tree metadata",
                    header.page_id
                )));
            }
        }
    }
    let lower = header.lower as usize;
    let upper = header.upper as usize;
    let slot_table_end = PAGE_HEADER_SIZE + header.cell_count as usize * 2;
    if lower < slot_table_end {
        return Err(EngineError::Corruption(format!(
            "page lower bound {} overlaps slot table ending at {}",
            header.lower, slot_table_end
        )));
    }
    if upper > PAGE_SIZE {
        return Err(EngineError::Corruption(format!(
            "page upper bound {} exceeds page size {}",
            header.upper, PAGE_SIZE
        )));
    }
    if lower > upper {
        return Err(EngineError::Corruption(format!(
            "page lower bound {} exceeds upper bound {}",
            header.lower, header.upper
        )));
    }
    Ok(())
}

fn read_cell_slots(bytes: &[u8], header: &PageHeaderInfo) -> Result<Vec<usize>> {
    let cell_count = header.cell_count as usize;
    let mut slots = Vec::with_capacity(cell_count);
    for i in 0..cell_count {
        slots.push(read_cell_slot(bytes, header, i)?);
    }
    Ok(slots)
}

fn validate_cell_slot(slot: usize, header: &PageHeaderInfo) -> Result<()> {
    let upper = header.upper as usize;
    if slot < upper || slot >= PAGE_SIZE {
        return Err(EngineError::Corruption(format!(
            "cell slot {slot} is outside data region {upper}..{}",
            PAGE_SIZE
        )));
    }
    Ok(())
}

fn page_header(info: PageHeaderInfo) -> PageHeader {
    PageHeader {
        magic: PAGE_MAGIC,
        checksum: 0,
        page_id: info.page_id.to_le(),
        page_kind: info.page_kind as u8,
        level: info.level,
        cell_count: info.cell_count.to_le(),
        lower: info.lower.to_le(),
        upper: info.upper.to_le(),
        reserved: 0,
        right_sibling_page_id: info.right_sibling_page_id.to_le(),
    }
}

fn write_page_header(buf: &mut [u8], info: PageHeaderInfo) -> Result<()> {
    let header = page_header(info);
    buf[..PAGE_HEADER_SIZE].copy_from_slice(header.as_bytes());
    let checksum = checksum_with_zeroed_region(buf, PAGE_HEADER_CHECKSUM_OFFSET, 4);
    buf[PAGE_HEADER_CHECKSUM_OFFSET..PAGE_HEADER_CHECKSUM_OFFSET + 4]
        .copy_from_slice(&checksum.to_le_bytes());
    Ok(())
}
