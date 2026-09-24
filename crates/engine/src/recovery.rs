use crate::btree::{build_catalog_tree, read_catalog, PageAllocator};
use crate::catalog::CatalogState;
use crate::error::{EngineError, Result};
use crate::layout::{
    decode_superblock_slot, encode_superblock_slot, SuperblockState, PAGE_SIZE,
    SUPERBLOCK_SLOT_SIZE,
};
use crate::pager::Pager;
use crate::storage::backend::FileBackend;
use crate::wal::{replay_wal_index, scan_wal_index};

/// Publication protocol shared with the JS control file (`root-manifest.bin`):
/// two fixed-size slots, magic then checksum then version. A slot that fails
/// magic or checksum is torn and ignored; a checksummed slot with an unknown
/// version is corruption. A zero-length file is absent; a non-empty file with
/// no valid slot is corruption, never a fresh database.
pub fn select_superblock<B: FileBackend>(manifest: &B) -> Result<Option<SuperblockState>> {
    let len = manifest.len()?;
    if len == 0 {
        return Ok(None);
    }
    let slot0 = read_slot(manifest, 0, len)?;
    let slot1 = read_slot(manifest, 1, len)?;
    let sb0 = decode_superblock_slot(0, &slot0)?;
    let sb1 = decode_superblock_slot(1, &slot1)?;
    match (sb0, sb1) {
        (Some(a), Some(b)) => Ok(Some(if a.generation >= b.generation { a } else { b })),
        (Some(a), None) => Ok(Some(a)),
        (None, Some(b)) => Ok(Some(b)),
        (None, None) => Err(EngineError::Corruption(
            "manifest has no valid superblock slot".into(),
        )),
    }
}

fn read_slot<B: FileBackend>(manifest: &B, index: usize, len: u64) -> Result<Vec<u8>> {
    let offset = (index * SUPERBLOCK_SLOT_SIZE) as u64;
    if offset >= len {
        return Ok(Vec::new());
    }
    let available = (len - offset).min(SUPERBLOCK_SLOT_SIZE as u64) as usize;
    manifest.read_at(offset, available)
}

pub fn initialize_empty_db<B: FileBackend>(
    manifest: &mut B,
    pager: &mut Pager<B>,
    wal: &mut B,
    db_id: u64,
) -> Result<SuperblockState> {
    let mut alloc = PageAllocator::new(1);
    let built = build_catalog_tree(&CatalogState::default(), &mut alloc)?;
    for (page_id, bytes) in &built.page_images {
        pager.write_page_image(*page_id, bytes)?;
    }
    pager.flush()?;
    wal.truncate(0)?;
    wal.flush()?;
    let state = SuperblockState {
        generation: 1,
        db_id,
        page_size: PAGE_SIZE as u32,
        catalog_root_page_id: built.root_page_id,
        next_page_id: alloc.next_page_id(),
        last_committed_txid: 0,
        last_replayed_wal_offset: 0,
        active_slot: 0,
    };
    let slot = encode_superblock_slot(&state);
    manifest.write_at(0, &slot)?;
    manifest.write_at(SUPERBLOCK_SLOT_SIZE as u64, &[0u8; SUPERBLOCK_SLOT_SIZE])?;
    manifest.flush()?;
    verify_slot(manifest, 0, &slot)?;
    pager.set_page_limit(state.next_page_id);
    Ok(state)
}

pub fn recover_if_needed<B: FileBackend>(
    manifest: &mut B,
    pager: &mut Pager<B>,
    wal: &mut B,
    base: &SuperblockState,
) -> Result<SuperblockState> {
    if base.catalog_root_page_id == 0 || base.catalog_root_page_id >= base.next_page_id {
        return Err(EngineError::Corruption(format!(
            "superblock catalog root {} is outside next page id {}",
            base.catalog_root_page_id, base.next_page_id
        )));
    }
    let txs = scan_wal_index(wal)?;
    let mut recovered = base.clone();
    let mut to_replay = Vec::new();
    for tx in txs {
        if tx.txid <= base.last_committed_txid {
            continue;
        }
        if tx.commit.new_next_page_id < recovered.next_page_id {
            return Err(EngineError::Corruption(format!(
                "wal transaction {} shrinks next page id from {} to {}",
                tx.txid, recovered.next_page_id, tx.commit.new_next_page_id
            )));
        }
        recovered.last_committed_txid = tx.commit.txid;
        recovered.catalog_root_page_id = tx.commit.new_catalog_root_page_id;
        recovered.next_page_id = tx.commit.new_next_page_id;
        recovered.last_replayed_wal_offset = tx.end_offset;
        to_replay.push(tx);
    }
    if !to_replay.is_empty() {
        replay_wal_index(pager, wal, &to_replay)?;
        recovered.generation = recovered
            .generation
            .checked_add(1)
            .ok_or_else(|| EngineError::Corruption("superblock generation overflow".into()))?;
        recovered.active_slot = if base.active_slot == 0 { 1 } else { 0 };
        write_superblock(manifest, &recovered)?;
    }
    if wal.len()? > 0 {
        wal.truncate(0)?;
        wal.flush()?;
    }
    pager.set_page_limit(recovered.next_page_id);
    Ok(recovered)
}

/// Writes one slot, flushes, and reads it back. A short or lost write is a
/// storage error here instead of a silently stale superblock on next open.
pub fn write_superblock<B: FileBackend>(manifest: &mut B, state: &SuperblockState) -> Result<()> {
    let slot = encode_superblock_slot(state);
    let offset = (state.active_slot * SUPERBLOCK_SLOT_SIZE) as u64;
    manifest.write_at(offset, &slot)?;
    manifest.flush()?;
    verify_slot(manifest, state.active_slot, &slot)
}

fn verify_slot<B: FileBackend>(manifest: &B, index: usize, expected: &[u8]) -> Result<()> {
    let offset = (index * SUPERBLOCK_SLOT_SIZE) as u64;
    let written = manifest.read_at(offset, SUPERBLOCK_SLOT_SIZE)?;
    if written != expected {
        return Err(EngineError::Storage(format!(
            "superblock slot {index} did not read back as written"
        )));
    }
    Ok(())
}

pub fn load_catalog_snapshot<B: FileBackend>(
    pager: &mut Pager<B>,
    state: &SuperblockState,
) -> Result<CatalogState> {
    read_catalog(pager, state.catalog_root_page_id)
}

pub fn ensure_openable_or_initialize<B: FileBackend>(
    manifest: &mut B,
    pager: &mut Pager<B>,
    wal: &mut B,
    db_id: u64,
    create_if_missing: bool,
) -> Result<SuperblockState> {
    match select_superblock(manifest)? {
        Some(sb) => {
            pager.set_page_limit(sb.next_page_id);
            Ok(sb)
        }
        None => {
            let main_len = pager.len()?;
            let wal_len = wal.len()?;
            if main_len == 0 && wal_len == 0 {
                if create_if_missing {
                    initialize_empty_db(manifest, pager, wal, db_id)
                } else {
                    Err(EngineError::Storage(
                        "database missing and create_if_missing=false".into(),
                    ))
                }
            } else {
                Err(EngineError::Corruption(
                    "manifest missing valid superblock".into(),
                ))
            }
        }
    }
}
