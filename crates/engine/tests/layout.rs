mod common;

use moyodb_engine::checksum::checksum_with_zeroed_region;
use moyodb_engine::layout::{
    decode_superblock_slot, encode_superblock_slot, PageKind, SuperblockState, ValueKind,
    FORMAT_VERSION, PAGE_HEADER_CHECKSUM_OFFSET, PAGE_HEADER_SIZE, PAGE_SIZE, SUPERBLOCK_MAGIC,
    SUPERBLOCK_SLOT_SIZE,
};
use moyodb_engine::page::{
    decode_page, encode_internal_page, encode_leaf_page, encode_overflow_page, InternalCell,
    LeafCell,
};

#[test]
fn leaf_page_roundtrip() {
    let page = encode_leaf_page(
        7,
        0,
        8,
        &[LeafCell {
            key: b"hello".to_vec(),
            value: b"world".to_vec(),
            value_kind: ValueKind::Inline,
            total_value_len: 5,
            overflow_head_page_id: 0,
        }],
    )
    .unwrap();
    let decoded = decode_page(&page).unwrap();
    assert_eq!(decoded.header.page_id, 7);
    assert_eq!(decoded.header.page_kind, PageKind::Leaf);
    assert_eq!(decoded.header.right_sibling_page_id, 8);
    assert_eq!(decoded.leaf_cells[0].key, b"hello");
    assert_eq!(decoded.leaf_cells[0].value, b"world");
}

#[test]
fn internal_page_roundtrip() {
    let page = encode_internal_page(
        11,
        1,
        12,
        &[
            InternalCell {
                separator: b"a".to_vec(),
                child_page_id: 3,
            },
            InternalCell {
                separator: b"z".to_vec(),
                child_page_id: 4,
            },
        ],
    )
    .unwrap();
    let decoded = decode_page(&page).unwrap();
    assert_eq!(decoded.header.page_kind, PageKind::Internal);
    assert_eq!(decoded.internal_cells.len(), 2);
    assert_eq!(decoded.internal_cells[1].separator, b"z");
}

#[test]
fn overflow_page_roundtrip() {
    let page = encode_overflow_page(12, 13, b"abcdefgh").unwrap();
    let decoded = decode_page(&page).unwrap();
    assert_eq!(decoded.header.page_kind, PageKind::Overflow);
    let body = decoded.overflow.unwrap();
    assert_eq!(body.next_overflow_page_id, 13);
    assert_eq!(body.chunk, b"abcdefgh");
}

#[test]
fn superblock_encode_decode_and_selection() {
    let state = SuperblockState {
        generation: 9,
        db_id: 42,
        page_size: PAGE_SIZE as u32,
        catalog_root_page_id: 7,
        next_page_id: 9,
        last_committed_txid: 3,
        last_replayed_wal_offset: 0,
        active_slot: 1,
    };
    let slot = encode_superblock_slot(&state);
    let decoded = decode_superblock_slot(1, &slot).unwrap().unwrap();
    assert_eq!(decoded.generation, 9);
    assert_eq!(decoded.db_id, 42);
    assert_eq!(decoded.catalog_root_page_id, 7);
}

#[test]
fn decode_rejects_overlapping_page_bounds() {
    let mut page = encode_leaf_page(
        1,
        0,
        0,
        &[LeafCell {
            key: b"a".to_vec(),
            value: b"1".to_vec(),
            value_kind: ValueKind::Inline,
            total_value_len: 1,
            overflow_head_page_id: 0,
        }],
    )
    .unwrap();

    page[20..22].copy_from_slice(&(PAGE_SIZE as u16).to_le_bytes());
    page[22..24].copy_from_slice(&(0u16).to_le_bytes());
    let checksum = checksum_with_zeroed_region(&page, PAGE_HEADER_CHECKSUM_OFFSET, 4);
    page[PAGE_HEADER_CHECKSUM_OFFSET..PAGE_HEADER_CHECKSUM_OFFSET + 4]
        .copy_from_slice(&checksum.to_le_bytes());

    let err = decode_page(&page).unwrap_err();
    assert_eq!(err.code(), "CorruptionError");
}

#[test]
fn decode_rejects_invalid_inline_leaf_metadata() {
    let mut page = encode_leaf_page(
        2,
        0,
        0,
        &[LeafCell {
            key: b"a".to_vec(),
            value: b"1".to_vec(),
            value_kind: ValueKind::Inline,
            total_value_len: 1,
            overflow_head_page_id: 0,
        }],
    )
    .unwrap();

    let decoded = decode_page(&page).unwrap();
    let slot_offset = PAGE_HEADER_SIZE;
    let cell_offset = u16::from_le_bytes([page[slot_offset], page[slot_offset + 1]]) as usize;
    let total_len_offset = cell_offset + 4;
    page[total_len_offset..total_len_offset + 4].copy_from_slice(&(2u32).to_le_bytes());
    let checksum = checksum_with_zeroed_region(&page, PAGE_HEADER_CHECKSUM_OFFSET, 4);
    page[PAGE_HEADER_CHECKSUM_OFFSET..PAGE_HEADER_CHECKSUM_OFFSET + 4]
        .copy_from_slice(&checksum.to_le_bytes());

    let err = decode_page(&page).unwrap_err();
    assert_eq!(err.code(), "CorruptionError");
    assert_eq!(decoded.leaf_cells[0].value, b"1");
}

#[test]
fn exported_layout_constants_are_stable() {
    assert_eq!(FORMAT_VERSION, 1);
    assert_eq!(SUPERBLOCK_MAGIC, *b"STKDB001");
    assert_eq!(SUPERBLOCK_SLOT_SIZE, 4096);
}
