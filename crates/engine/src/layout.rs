use crate::bytes::{read_u32_le, read_u64_le};
use crate::checksum::checksum_with_zeroed_region;
use crate::error::{EngineError, Result};
use serde::{Deserialize, Serialize};
use std::mem::size_of;
use zerocopy::{AsBytes, FromBytes, FromZeroes};

pub const PAGE_SIZE: usize = 4096;
pub const SUPERBLOCK_SLOT_SIZE: usize = 4096;
pub const FORMAT_VERSION: u32 = 1;
pub const SUPERBLOCK_MAGIC: [u8; 8] = *b"STKDB001";
pub const WAL_MAGIC: [u8; 4] = *b"WAL1";
pub const PAGE_MAGIC: [u8; 4] = *b"PAG1";
pub const MANIFEST_FILE_KIND: u32 = 0;
pub const MAIN_FILE_KIND: u32 = 1;
pub const WAL_FILE_KIND: u32 = 2;
pub const INLINE_VALUE_LIMIT: usize = 1024;

pub const SUPERBLOCK_CHECKSUM_OFFSET: usize = 64;
pub const PAGE_HEADER_CHECKSUM_OFFSET: usize = 4;
pub const WAL_RECORD_CHECKSUM_OFFSET: usize = 12;

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PageKind {
    Leaf = 1,
    Internal = 2,
    Overflow = 3,
}

impl PageKind {
    pub fn from_u8(v: u8) -> Result<Self> {
        match v {
            1 => Ok(PageKind::Leaf),
            2 => Ok(PageKind::Internal),
            3 => Ok(PageKind::Overflow),
            _ => Err(EngineError::Corruption(format!("unknown page kind {v}"))),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ValueKind {
    Inline = 1,
    Overflow = 2,
}

impl ValueKind {
    pub fn from_u8(v: u8) -> Result<Self> {
        match v {
            1 => Ok(ValueKind::Inline),
            2 => Ok(ValueKind::Overflow),
            _ => Err(EngineError::Corruption(format!("unknown value kind {v}"))),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum WalTag {
    PageImage = 1,
    Commit = 2,
}

impl WalTag {
    pub fn from_u8(v: u8) -> Result<Self> {
        match v {
            1 => Ok(WalTag::PageImage),
            2 => Ok(WalTag::Commit),
            _ => Err(EngineError::Corruption(format!("unknown wal tag {v}"))),
        }
    }
}

#[repr(C, packed)]
#[derive(AsBytes, FromBytes, FromZeroes, Clone, Copy, Default)]
pub struct SuperblockHeader {
    pub magic: [u8; 8],
    pub format_version: u32,
    pub generation: u64,
    pub db_id: u64,
    pub page_size: u32,
    pub catalog_root_page_id: u64,
    pub next_page_id: u64,
    pub last_committed_txid: u64,
    pub last_replayed_wal_offset: u64,
    pub checksum: u32,
}

#[repr(C, packed)]
#[derive(AsBytes, FromBytes, FromZeroes, Clone, Copy, Default)]
pub struct PageHeader {
    pub magic: [u8; 4],
    pub checksum: u32,
    pub page_id: u64,
    pub page_kind: u8,
    pub level: u8,
    pub cell_count: u16,
    pub lower: u16,
    pub upper: u16,
    pub reserved: u16,
    pub right_sibling_page_id: u64,
}

#[repr(C, packed)]
#[derive(AsBytes, FromBytes, FromZeroes, Clone, Copy, Default)]
pub struct WalRecordHeader {
    pub magic: [u8; 4],
    pub tag: u8,
    pub reserved0: u8,
    pub reserved1: u8,
    pub reserved2: u8,
    pub payload_len: u32,
    pub checksum: u32,
}

#[repr(C, packed)]
#[derive(AsBytes, FromBytes, FromZeroes, Clone, Copy, Default)]
pub struct WalPageImageBodyHeader {
    pub txid: u64,
    pub page_id: u64,
    pub page_len: u32,
    pub reserved: u32,
}

#[repr(C, packed)]
#[derive(AsBytes, FromBytes, FromZeroes, Clone, Copy, Default)]
pub struct WalCommitBody {
    pub txid: u64,
    pub new_catalog_root_page_id: u64,
    pub new_next_page_id: u64,
    pub changed_page_count: u32,
    pub reserved: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SuperblockState {
    pub generation: u64,
    pub db_id: u64,
    pub page_size: u32,
    pub catalog_root_page_id: u64,
    pub next_page_id: u64,
    pub last_committed_txid: u64,
    pub last_replayed_wal_offset: u64,
    pub active_slot: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoreMetadata {
    pub store_root_page_id: u64,
    pub created_txid: u64,
    pub flags: u64,
}

pub const SUPERBLOCK_HEADER_SIZE: usize = size_of::<SuperblockHeader>();
pub const PAGE_HEADER_SIZE: usize = size_of::<PageHeader>();
pub const WAL_RECORD_HEADER_SIZE: usize = size_of::<WalRecordHeader>();
pub const WAL_PAGE_IMAGE_BODY_HEADER_SIZE: usize = size_of::<WalPageImageBodyHeader>();
pub const WAL_COMMIT_BODY_SIZE: usize = size_of::<WalCommitBody>();

pub fn unsafe_read_struct<T: Copy>(bytes: &[u8]) -> Result<T> {
    if bytes.len() < size_of::<T>() {
        return Err(EngineError::Serialization(format!(
            "short struct read: need {}, got {}",
            size_of::<T>(),
            bytes.len()
        )));
    }
    let mut value = std::mem::MaybeUninit::<T>::uninit();
    // SAFETY: the length guard above ensures that `bytes` contains enough bytes
    // to initialize `T`; all call sites use plain #[repr(C)]/integer layout
    // structs that are `Copy`, so this is a byte-level decode boundary.
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            value.as_mut_ptr() as *mut u8,
            size_of::<T>(),
        );
        Ok(value.assume_init())
    }
}

pub fn encode_superblock_slot(state: &SuperblockState) -> [u8; SUPERBLOCK_SLOT_SIZE] {
    let header = SuperblockHeader {
        magic: SUPERBLOCK_MAGIC,
        format_version: FORMAT_VERSION.to_le(),
        generation: state.generation.to_le(),
        db_id: state.db_id.to_le(),
        page_size: state.page_size.to_le(),
        catalog_root_page_id: state.catalog_root_page_id.to_le(),
        next_page_id: state.next_page_id.to_le(),
        last_committed_txid: state.last_committed_txid.to_le(),
        last_replayed_wal_offset: state.last_replayed_wal_offset.to_le(),
        checksum: 0,
    };
    let mut slot = [0u8; SUPERBLOCK_SLOT_SIZE];
    slot[..SUPERBLOCK_HEADER_SIZE].copy_from_slice(header.as_bytes());
    let checksum = checksum_with_zeroed_region(&slot, SUPERBLOCK_CHECKSUM_OFFSET, 4);
    slot[SUPERBLOCK_CHECKSUM_OFFSET..SUPERBLOCK_CHECKSUM_OFFSET + 4]
        .copy_from_slice(&checksum.to_le_bytes());
    slot
}

pub fn decode_superblock_slot(slot_index: usize, bytes: &[u8]) -> Result<Option<SuperblockState>> {
    if bytes.len() < SUPERBLOCK_SLOT_SIZE {
        return Ok(None);
    }
    if bytes[..8] != SUPERBLOCK_MAGIC {
        return Ok(None);
    }
    let expected = checksum_with_zeroed_region(
        &bytes[..SUPERBLOCK_SLOT_SIZE],
        SUPERBLOCK_CHECKSUM_OFFSET,
        4,
    );
    let checksum = read_u32_le(bytes, SUPERBLOCK_CHECKSUM_OFFSET)?;
    if expected != checksum {
        return Ok(None);
    }
    let format_version = read_u32_le(bytes, 8)?;
    if format_version != FORMAT_VERSION {
        return Err(EngineError::Corruption(format!(
            "unsupported format version {format_version}"
        )));
    }
    let page_size = read_u32_le(bytes, 28)?;
    if page_size as usize != PAGE_SIZE {
        return Err(EngineError::Corruption(format!(
            "unexpected page size {page_size}"
        )));
    }
    Ok(Some(SuperblockState {
        generation: read_u64_le(bytes, 12)?,
        db_id: read_u64_le(bytes, 20)?,
        page_size,
        catalog_root_page_id: read_u64_le(bytes, 32)?,
        next_page_id: read_u64_le(bytes, 40)?,
        last_committed_txid: read_u64_le(bytes, 48)?,
        last_replayed_wal_offset: read_u64_le(bytes, 56)?,
        active_slot: slot_index,
    }))
}

pub fn page_offset(page_id: u64) -> u64 {
    page_id.saturating_sub(1).saturating_mul(PAGE_SIZE as u64)
}

pub fn wal_record_total_len(payload_len: usize) -> usize {
    WAL_RECORD_HEADER_SIZE.saturating_add(payload_len)
}
