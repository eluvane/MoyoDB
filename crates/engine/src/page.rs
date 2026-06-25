use crate::bytes::{read_u16_le, read_u32_le, read_u64_le, write_u16_le};
use crate::checksum::checksum_with_zeroed_region;
use crate::error::{EngineError, Result};
use crate::layout::{
    unsafe_read_struct, PageHeader, PageKind, ValueKind, INLINE_VALUE_LIMIT,
    PAGE_HEADER_CHECKSUM_OFFSET, PAGE_HEADER_SIZE, PAGE_MAGIC, PAGE_SIZE,
};
use serde::{Deserialize, Serialize};
use zerocopy::AsBytes;

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
        .ok_or_else(|| EngineError::Corruption("leaf cell value length overflow".into()))?;
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
            if header_info.cell_count != 0 {
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
            if header_info.lower as usize != chunk_start {
                return Err(EngineError::Corruption(
                    "overflow page lower bound mismatch".into(),
                ));
            }
            if header_info.upper as usize != chunk_end {
                return Err(EngineError::Corruption(
                    "overflow page upper bound mismatch".into(),
                ));
            }
            let chunk = bytes
                .get(chunk_start..chunk_end)
                .ok_or_else(|| EngineError::Corruption("overflow chunk out of bounds".into()))?
                .to_vec();
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

pub fn encode_leaf_page(
    page_id: u64,
    level: u8,
    right_sibling_page_id: u64,
    cells: &[LeafCell],
) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; PAGE_SIZE];
    let mut upper = PAGE_SIZE;
    let mut slots = Vec::with_capacity(cells.len());
    for cell in cells {
        let encoded = encode_leaf_cell(cell)?;
        if upper < PAGE_HEADER_SIZE + (cells.len() * 2) + encoded.len() {
            return Err(EngineError::Serialization("leaf page overflow".into()));
        }
        upper -= encoded.len();
        buf[upper..upper + encoded.len()].copy_from_slice(&encoded);
        slots.push(upper as u16);
    }
    for (i, slot) in slots.iter().enumerate() {
        write_u16_le(&mut buf, PAGE_HEADER_SIZE + i * 2, *slot)?;
    }
    write_page_header(
        &mut buf,
        PageHeaderInfo {
            page_id,
            page_kind: PageKind::Leaf,
            level,
            cell_count: cells.len() as u16,
            lower: (PAGE_HEADER_SIZE + slots.len() * 2) as u16,
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
    let mut slots = Vec::with_capacity(cells.len());
    for cell in cells {
        let encoded = encode_internal_cell(cell)?;
        if upper < PAGE_HEADER_SIZE + (cells.len() * 2) + encoded.len() {
            return Err(EngineError::Serialization("internal page overflow".into()));
        }
        upper -= encoded.len();
        buf[upper..upper + encoded.len()].copy_from_slice(&encoded);
        slots.push(upper as u16);
    }
    for (i, slot) in slots.iter().enumerate() {
        write_u16_le(&mut buf, PAGE_HEADER_SIZE + i * 2, *slot)?;
    }
    write_page_header(
        &mut buf,
        PageHeaderInfo {
            page_id,
            page_kind: PageKind::Internal,
            level,
            cell_count: cells.len() as u16,
            lower: (PAGE_HEADER_SIZE + slots.len() * 2) as u16,
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
    let max = PAGE_SIZE - PAGE_HEADER_SIZE - 12;
    if chunk.len() > max {
        return Err(EngineError::Serialization(format!(
            "overflow chunk too large: {} > {max}",
            chunk.len()
        )));
    }
    let mut buf = vec![0u8; PAGE_SIZE];
    let chunk_start = PAGE_HEADER_SIZE + 12;
    buf[PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + 8]
        .copy_from_slice(&next_overflow_page_id.to_le_bytes());
    buf[PAGE_HEADER_SIZE + 8..PAGE_HEADER_SIZE + 12]
        .copy_from_slice(&(chunk.len() as u32).to_le_bytes());
    buf[chunk_start..chunk_start + chunk.len()].copy_from_slice(chunk);
    write_page_header(
        &mut buf,
        PageHeaderInfo {
            page_id,
            page_kind: PageKind::Overflow,
            level: 0,
            cell_count: 0,
            lower: chunk_start as u16,
            upper: (chunk_start + chunk.len()) as u16,
            right_sibling_page_id: 0,
        },
    )?;
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

fn encode_leaf_cell(cell: &LeafCell) -> Result<Vec<u8>> {
    let key_len = cell.key.len();
    let use_overflow = cell.value_kind == ValueKind::Overflow;
    let inline_value = if use_overflow {
        &[][..]
    } else {
        cell.value.as_slice()
    };
    let mut out = Vec::with_capacity(leaf_cell_size(key_len, inline_value.len(), use_overflow));
    out.extend_from_slice(&(key_len as u16).to_le_bytes());
    out.push(cell.value_kind as u8);
    out.push(0);
    out.extend_from_slice(&cell.total_value_len.to_le_bytes());
    out.extend_from_slice(&cell.overflow_head_page_id.to_le_bytes());
    out.extend_from_slice(&(inline_value.len() as u32).to_le_bytes());
    out.extend_from_slice(&cell.key);
    out.extend_from_slice(inline_value);
    Ok(out)
}

fn decode_leaf_cell(bytes: &[u8]) -> Result<LeafCell> {
    let key_len = read_u16_le(bytes, 0)? as usize;
    let value_kind = ValueKind::from_u8(
        *bytes
            .get(2)
            .ok_or_else(|| EngineError::Corruption("leaf cell kind".into()))?,
    )?;
    let total_value_len = read_u32_le(bytes, 4)?;
    let overflow_head_page_id = read_u64_le(bytes, 8)?;
    let inline_value_len = read_u32_le(bytes, 16)? as usize;
    let key_start = 20;
    let key_end = key_start + key_len;
    let value_start = key_end;
    let value_end = value_start + inline_value_len;
    let key = bytes
        .get(key_start..key_end)
        .ok_or_else(|| EngineError::Corruption("leaf cell key out of bounds".into()))?
        .to_vec();
    let value = bytes
        .get(value_start..value_end)
        .ok_or_else(|| EngineError::Corruption("leaf cell value out of bounds".into()))?
        .to_vec();
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
    }
    Ok(LeafCell {
        key,
        value,
        value_kind,
        total_value_len,
        overflow_head_page_id,
    })
}

fn encode_internal_cell(cell: &InternalCell) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(internal_cell_size(cell.separator.len()));
    out.extend_from_slice(&(cell.separator.len() as u16).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&cell.child_page_id.to_le_bytes());
    out.extend_from_slice(&cell.separator);
    Ok(out)
}

fn decode_internal_cell(bytes: &[u8]) -> Result<InternalCell> {
    let sep_len = read_u16_le(bytes, 0)? as usize;
    let child_page_id = read_u64_le(bytes, 4)?;
    if child_page_id == 0 {
        return Err(EngineError::Corruption(
            "internal cell has child_page_id=0".into(),
        ));
    }
    let separator = bytes
        .get(12..12 + sep_len)
        .ok_or_else(|| EngineError::Corruption("internal cell separator out of bounds".into()))?
        .to_vec();
    Ok(InternalCell {
        separator,
        child_page_id,
    })
}

fn validate_page_bounds(header: &PageHeaderInfo) -> Result<()> {
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

fn write_page_header(buf: &mut [u8], info: PageHeaderInfo) -> Result<()> {
    let header = PageHeader {
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
    };
    buf[..PAGE_HEADER_SIZE].copy_from_slice(header.as_bytes());
    let checksum = checksum_with_zeroed_region(buf, PAGE_HEADER_CHECKSUM_OFFSET, 4);
    buf[PAGE_HEADER_CHECKSUM_OFFSET..PAGE_HEADER_CHECKSUM_OFFSET + 4]
        .copy_from_slice(&checksum.to_le_bytes());
    Ok(())
}
