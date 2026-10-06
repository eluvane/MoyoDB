use crate::btree::{build_catalog_tree, read_catalog, PageAllocator};
use crate::catalog::{
    decode_change_feed_floor_txid, decode_change_feed_policy, decode_schema_version,
    decode_store_metadata, CatalogState, CATALOG_CHANGE_FEED_FLOOR_TXID_KEY,
    CATALOG_CHANGE_FEED_POLICY_KEY, CATALOG_SCHEMA_VERSION_KEY,
};
use crate::error::{EngineError, Result};
use crate::layout::{
    decode_superblock_slot, encode_superblock_slot, PageKind, SuperblockState, ValueKind,
    PAGE_SIZE, SUPERBLOCK_SLOT_SIZE,
};
use crate::page::{decode_leaf_cell_ref, decode_page_header_verified, read_cell_slot, LeafCellRef};
use crate::pager::Pager;
use crate::storage::backend::{ensure_exact_len, FileBackend};
use crate::wal::{replay_latest_pages, visit_wal_transactions};
use std::collections::BTreeMap;

/// Shares the publication protocol with `root-manifest.bin`.
/// Both use two fixed-size slots. Check magic, checksum, then version.
/// Ignore slots with invalid magic or checksum. A valid checksum with an
/// unknown version is corruption. An empty file is absent. A nonempty file
/// with no valid slot is corruption.
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
    // Short backend reads must not hide a durable newer slot.
    // Only the file length can identify a truncated slot.
    ensure_exact_len(
        manifest.read_at(offset, available)?,
        available,
        "superblock slot read",
    )
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
    let mut recovered = base.clone();
    let mut latest = BTreeMap::new();
    visit_wal_transactions(wal, |commit, pages, end_offset| {
        if commit.txid <= base.last_committed_txid {
            return Ok(());
        }
        if commit.new_next_page_id < recovered.next_page_id {
            return Err(EngineError::Corruption(format!(
                "wal transaction {} shrinks next page id from {} to {}",
                commit.txid, recovered.next_page_id, commit.new_next_page_id
            )));
        }
        recovered.last_committed_txid = commit.txid;
        recovered.catalog_root_page_id = commit.new_catalog_root_page_id;
        recovered.next_page_id = commit.new_next_page_id;
        recovered.last_replayed_wal_offset = end_offset;
        for (page_id, offset) in pages.iter() {
            latest.insert(*page_id, *offset);
        }
        #[cfg(test)]
        crate::wal::scan_work::retain_committed(0, latest.len());
        Ok(())
    })?;
    if recovered.last_committed_txid > base.last_committed_txid {
        // No main-file write happens until every committed record, including
        // overwritten and already-published images, has passed validation.
        replay_latest_pages(pager, wal, latest)?;
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

/// Flushes a slot and verifies it by reading it back.
/// Readback mismatches return a storage error.
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
    // A root leaf is the usual catalog. Its inline cells are decoded from the
    // cached page, without an intermediate copy of every key and value.
    // Internal nodes and overflow values stay on the full scanner so later
    // structural errors still win over metadata errors.
    if let Some(catalog) = try_read_inline_root_leaf(pager, state.catalog_root_page_id)? {
        return Ok(catalog);
    }
    read_catalog(pager, state.catalog_root_page_id)
}

/// `None` means the caller must use `read_catalog`.
fn try_read_inline_root_leaf<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
) -> Result<Option<CatalogState>> {
    if root_page_id == 0 {
        return Ok(None);
    }
    pager.with_page(root_page_id, |bytes| {
        decode_inline_root_leaf(root_page_id, bytes)
    })
}

fn decode_inline_root_leaf(root_page_id: u64, bytes: &[u8]) -> Result<Option<CatalogState>> {
    let header = decode_page_header_verified(bytes)?;
    if header.page_kind != PageKind::Leaf {
        return Ok(None);
    }
    if header.page_id != root_page_id {
        return Err(EngineError::Corruption(format!(
            "page header id mismatch: expected {root_page_id}, got {}",
            header.page_id
        )));
    }
    let count = header.cell_count as usize;
    // Structural cell errors are reported before any metadata is interpreted.
    // An overflow value is materialized by the full scanner, which also sees
    // a later structural error first.
    let mut overflow = false;
    for index in 0..count {
        let slot = read_cell_slot(bytes, &header, index)?;
        let cell = decode_leaf_cell_ref(bytes, slot)?;
        if cell.value_kind != ValueKind::Inline {
            overflow = true;
        }
    }
    if overflow {
        return Ok(None);
    }
    let mut catalog = CatalogState::default();
    for index in 0..count {
        let slot = read_cell_slot(bytes, &header, index)?;
        let cell = decode_leaf_cell_ref(bytes, slot)?;
        apply_inline_catalog_cell(&mut catalog, &cell)?;
    }
    Ok(Some(catalog))
}

fn apply_inline_catalog_cell(catalog: &mut CatalogState, cell: &LeafCellRef<'_>) -> Result<()> {
    if cell.key == CATALOG_SCHEMA_VERSION_KEY {
        catalog.schema_version = decode_schema_version(cell.inline_value)?;
        return Ok(());
    }
    if cell.key == CATALOG_CHANGE_FEED_FLOOR_TXID_KEY {
        catalog.change_feed_floor_txid = decode_change_feed_floor_txid(cell.inline_value)?;
        return Ok(());
    }
    if cell.key == CATALOG_CHANGE_FEED_POLICY_KEY {
        catalog.change_feed_policy = decode_change_feed_policy(cell.inline_value)?;
        return Ok(());
    }
    if cell.key.first() == Some(&0xff) {
        return Err(EngineError::Corruption(
            "unknown catalog metadata record".into(),
        ));
    }
    let name = String::from_utf8(cell.key.to_vec())
        .map_err(|err| EngineError::Corruption(format!("catalog key utf8: {err}")))?;
    let meta = decode_store_metadata(cell.inline_value)
        .map_err(|err| EngineError::Corruption(err.to_string()))?;
    catalog.stores.insert(name, meta);
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{page_offset, ValueKind};
    use crate::page::{encode_leaf_page, LeafCell};
    use crate::storage::memory::MemoryBackend;
    use crate::wal::{append_transaction, scan_wal_index, scan_work, CommitRecord};

    #[test]
    fn repeated_commits_recovery_retains_only_latest_offsets() -> Result<()> {
        let mut manifest = MemoryBackend::new();
        let main = MemoryBackend::new();
        let mut pager = Pager::new(main.clone(), 4);
        let mut wal = MemoryBackend::new();
        let base = initialize_empty_db(&mut manifest, &mut pager, &mut wal, 7)?;
        let mut oracle_main = MemoryBackend::from_durable(
            main.durable_snapshot()
                .ok_or_else(|| EngineError::Internal("initial main bytes missing".into()))?,
        );
        let mut oracle_manifest = manifest
            .durable_snapshot()
            .ok_or_else(|| EngineError::Internal("initial manifest bytes missing".into()))?;
        let mut offset = 0;
        let mut final_image = Vec::new();
        for txid in 1..=32 {
            final_image = encode_leaf_page(
                1,
                0,
                0,
                &[LeafCell {
                    key: b"key".to_vec(),
                    value: vec![txid as u8],
                    value_kind: ValueKind::Inline,
                    total_value_len: 1,
                    overflow_head_page_id: 0,
                }],
            )?;
            append_transaction(
                &mut wal,
                &mut offset,
                txid,
                &[(1, final_image.clone())],
                &CommitRecord {
                    txid,
                    new_catalog_root_page_id: 1,
                    new_next_page_id: 2,
                    changed_page_count: 1,
                },
            )?;
            oracle_main.write_at(page_offset(1), &final_image)?;
        }
        wal.flush()?;
        oracle_main.flush()?;
        let mut expected = base.clone();
        expected.generation += 1;
        expected.active_slot = 1;
        expected.last_committed_txid = 32;
        expected.last_replayed_wal_offset = offset;
        oracle_manifest[SUPERBLOCK_SLOT_SIZE..].copy_from_slice(&encode_superblock_slot(&expected));

        scan_work::reset();
        let collecting = {
            let transactions = scan_wal_index(&wal)?;
            assert_eq!(transactions.len(), 32);
            scan_work::snapshot()
        };
        assert_eq!(collecting.retained_transactions_peak, 32);
        assert_eq!(collecting.retained_committed_offsets_peak, 32);
        assert_eq!(collecting.pending_page_growths, 32);

        scan_work::reset();
        let recovered = recover_if_needed(&mut manifest, &mut pager, &mut wal, &base)?;
        let work = scan_work::snapshot();
        assert_eq!(recovered, expected);
        assert_eq!(main.read_at(page_offset(1), PAGE_SIZE)?, final_image);
        assert_eq!(main.durable_snapshot(), oracle_main.durable_snapshot());
        assert_eq!(manifest.durable_snapshot(), Some(oracle_manifest));
        assert_eq!(wal.durable_snapshot(), Some(Vec::new()));
        assert_eq!(
            work.retained_transactions_peak, 0,
            "recovery must not retain historical transaction objects: {work:?}"
        );
        assert_eq!(
            work.retained_committed_offsets_peak, 1,
            "all commits update one distinct page: {work:?}"
        );
        assert_eq!(
            work.pending_page_growths, 1,
            "reuse the pending page buffer between commits: {work:?}"
        );
        println!("recovery scan work: collecting={collecting:?}, streaming={work:?}");
        Ok(())
    }
}
