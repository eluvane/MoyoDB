use crate::btree::read_catalog;
use crate::catalog::{CatalogMap, CatalogState};
use crate::error::{EngineError, Result};
use crate::layout::{
    decode_superblock_slot, encode_superblock_slot, SuperblockState, PAGE_SIZE,
    SUPERBLOCK_SLOT_SIZE,
};
use crate::pager::Pager;
use crate::storage::backend::FileBackend;
use crate::wal::{replay_wal_transactions, scan_wal};

pub fn select_superblock<B: FileBackend>(manifest: &B) -> Result<Option<SuperblockState>> {
    let len = manifest.len()? as usize;
    if len == 0 {
        return Ok(None);
    }
    let slot0 = manifest.read_at(0, SUPERBLOCK_SLOT_SIZE)?;
    let slot1 = manifest.read_at(SUPERBLOCK_SLOT_SIZE as u64, SUPERBLOCK_SLOT_SIZE)?;
    let sb0 = decode_superblock_slot(0, &slot0)?;
    let sb1 = decode_superblock_slot(1, &slot1)?;
    match (sb0, sb1) {
        (Some(a), Some(b)) => Ok(Some(if a.generation >= b.generation { a } else { b })),
        (Some(a), None) => Ok(Some(a)),
        (None, Some(b)) => Ok(Some(b)),
        (None, None) => Ok(None),
    }
}

pub fn initialize_empty_db<B: FileBackend>(
    manifest: &mut B,
    pager: &mut Pager<B>,
    wal: &mut B,
    db_id: u64,
) -> Result<SuperblockState> {
    let mut next_page_id = 1u64;
    let built = crate::btree::build_catalog_tree(&CatalogMap::new(), 0, 0, &mut next_page_id)?;
    let root_page_id = built.root_page_id;
    let built_pages = built.page_images;
    for (page_id, bytes) in built_pages {
        pager.write_page_image(page_id, &bytes)?;
    }
    pager.flush()?;
    wal.truncate(0)?;
    wal.flush()?;
    let state = SuperblockState {
        generation: 1,
        db_id,
        page_size: PAGE_SIZE as u32,
        catalog_root_page_id: root_page_id,
        next_page_id,
        last_committed_txid: 0,
        last_replayed_wal_offset: 0,
        active_slot: 0,
    };
    let slot = encode_superblock_slot(&state);
    manifest.write_at(0, &slot)?;
    manifest.write_at(SUPERBLOCK_SLOT_SIZE as u64, &[0u8; SUPERBLOCK_SLOT_SIZE])?;
    manifest.flush()?;
    Ok(state)
}

pub fn recover_if_needed<B: FileBackend>(
    manifest: &mut B,
    pager: &mut Pager<B>,
    wal: &mut B,
    base: &SuperblockState,
) -> Result<SuperblockState> {
    let txs = scan_wal(wal)?;
    let mut to_replay = Vec::new();
    let mut recovered = base.clone();
    for tx in txs {
        if tx.txid > recovered.last_committed_txid {
            recovered.last_committed_txid = tx.commit.txid;
            recovered.catalog_root_page_id = tx.commit.new_catalog_root_page_id;
            recovered.next_page_id = tx.commit.new_next_page_id;
            recovered.last_replayed_wal_offset = tx.end_offset;
            to_replay.push(tx);
        }
    }
    if !to_replay.is_empty() {
        replay_wal_transactions(pager, &to_replay)?;
        recovered.generation += 1;
        recovered.active_slot = if base.active_slot == 0 { 1 } else { 0 };
        write_superblock(manifest, &recovered)?;
    }
    if wal.len()? > 0 {
        wal.truncate(0)?;
        wal.flush()?;
    }
    Ok(recovered)
}

pub fn write_superblock<B: FileBackend>(manifest: &mut B, state: &SuperblockState) -> Result<()> {
    let slot = encode_superblock_slot(state);
    let offset = (state.active_slot * SUPERBLOCK_SLOT_SIZE) as u64;
    manifest.write_at(offset, &slot)?;
    manifest.flush()?;
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
        Some(sb) => Ok(sb),
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
