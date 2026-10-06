mod common;

use std::cmp::Ordering;

use moyodb_engine::bytes::{
    compare_keys, key_in_range, read_u16_le, read_u32_le, read_u64_le, validate_key,
    validate_store_name, validate_value, write_u16_le, write_u32_le, write_u64_le, MAX_KEY_BYTES,
    MAX_STORE_NAME_BYTES, MAX_VALUE_BYTES,
};
use moyodb_engine::checksum::checksum_with_zeroed_region;
use moyodb_engine::layout::{
    decode_superblock_slot, encode_superblock_slot, page_offset, unsafe_read_struct, PageKind,
    SuperblockState, ValueKind, FORMAT_VERSION, PAGE_HEADER_CHECKSUM_OFFSET, PAGE_HEADER_SIZE,
    PAGE_SIZE, SUPERBLOCK_MAGIC, SUPERBLOCK_SLOT_SIZE,
};
use moyodb_engine::page::{
    decode_page, encode_internal_page, encode_leaf_page, encode_overflow_page, InternalCell,
    LeafCell,
};
use moyodb_engine::EngineError;

fn inline_a_page(page_id: u64) -> Vec<u8> {
    encode_leaf_page(
        page_id,
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
    .unwrap()
}

fn rewrite_page_checksum(page: &mut [u8]) {
    let checksum = checksum_with_zeroed_region(page, PAGE_HEADER_CHECKSUM_OFFSET, 4);
    page[PAGE_HEADER_CHECKSUM_OFFSET..PAGE_HEADER_CHECKSUM_OFFSET + 4]
        .copy_from_slice(&checksum.to_le_bytes());
}

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
    let mut page = inline_a_page(1);
    page[20..22].copy_from_slice(&(PAGE_SIZE as u16).to_le_bytes());
    page[22..24].copy_from_slice(&(0u16).to_le_bytes());
    rewrite_page_checksum(&mut page);

    let err = decode_page(&page).unwrap_err();
    assert_eq!(err.code(), "CorruptionError");
}

#[test]
fn decode_rejects_invalid_inline_leaf_metadata() {
    let mut page = inline_a_page(2);
    let decoded = decode_page(&page).unwrap();
    let slot_offset = PAGE_HEADER_SIZE;
    let cell_offset = u16::from_le_bytes([page[slot_offset], page[slot_offset + 1]]) as usize;
    let total_len_offset = cell_offset + 4;
    page[total_len_offset..total_len_offset + 4].copy_from_slice(&(2u32).to_le_bytes());
    rewrite_page_checksum(&mut page);

    let err = decode_page(&page).unwrap_err();
    assert_eq!(err.code(), "CorruptionError");
    assert_eq!(decoded.leaf_cells[0].value, b"1");
}

#[test]
fn decode_rejects_hostile_inline_length_without_panicking() {
    let mut page = encode_leaf_page(
        3,
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
    let slot = u16::from_le_bytes([page[PAGE_HEADER_SIZE], page[PAGE_HEADER_SIZE + 1]]) as usize;
    page[slot + 16..slot + 20].copy_from_slice(&u32::MAX.to_le_bytes());
    let checksum = checksum_with_zeroed_region(&page, PAGE_HEADER_CHECKSUM_OFFSET, 4);
    page[PAGE_HEADER_CHECKSUM_OFFSET..PAGE_HEADER_CHECKSUM_OFFSET + 4]
        .copy_from_slice(&checksum.to_le_bytes());

    let err = decode_page(&page).unwrap_err();
    assert_eq!(err.code(), "CorruptionError");
    assert!(
        err.to_string().contains("leaf cell value out of bounds"),
        "{err}"
    );
}

#[test]
fn decode_rejects_overlapping_leaf_cells() {
    let mut page = encode_leaf_page(
        4,
        0,
        0,
        &[
            LeafCell {
                key: b"a".to_vec(),
                value: b"1".to_vec(),
                value_kind: ValueKind::Inline,
                total_value_len: 1,
                overflow_head_page_id: 0,
            },
            LeafCell {
                key: b"m".to_vec(),
                value: b"2".to_vec(),
                value_kind: ValueKind::Inline,
                total_value_len: 1,
                overflow_head_page_id: 0,
            },
        ],
    )
    .unwrap();
    let low_slot =
        u16::from_le_bytes([page[PAGE_HEADER_SIZE + 2], page[PAGE_HEADER_SIZE + 3]]) as usize;
    page[low_slot + 4..low_slot + 8].copy_from_slice(&2u32.to_le_bytes());
    page[low_slot + 16..low_slot + 20].copy_from_slice(&2u32.to_le_bytes());
    let checksum = checksum_with_zeroed_region(&page, PAGE_HEADER_CHECKSUM_OFFSET, 4);
    page[PAGE_HEADER_CHECKSUM_OFFSET..PAGE_HEADER_CHECKSUM_OFFSET + 4]
        .copy_from_slice(&checksum.to_le_bytes());

    let err = decode_page(&page).unwrap_err();
    assert_eq!(err.code(), "CorruptionError");
    assert!(err.to_string().contains("cells overlap"), "{err}");
}

#[test]
fn exported_layout_constants_are_stable() {
    assert_eq!(FORMAT_VERSION, 2);
    assert_eq!(SUPERBLOCK_MAGIC, *b"STKDB001");
    assert_eq!(SUPERBLOCK_SLOT_SIZE, 4096);
}

#[test]
fn endian_key_and_page_offset_contracts() {
    let mut buf = [0u8; 16];
    write_u16_le(&mut buf, 1, 0xAABB).unwrap();
    write_u32_le(&mut buf, 3, 0x1122_3344).unwrap();
    write_u64_le(&mut buf, 8, 0x0102_0304_0506_0708).unwrap();
    assert_eq!(buf[1..3], [0xBB, 0xAA]);
    assert_eq!(buf[3..7], [0x44, 0x33, 0x22, 0x11]);
    assert_eq!(buf[8..16], [0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]);
    assert_eq!(read_u16_le(&buf, 1).unwrap(), 0xAABB);
    assert_eq!(read_u32_le(&buf, 3).unwrap(), 0x1122_3344);
    assert_eq!(read_u64_le(&buf, 8).unwrap(), 0x0102_0304_0506_0708);

    assert_eq!(
        read_u32_le(&buf, usize::MAX - 1).unwrap_err(),
        EngineError::Serialization(format!("u32 offset overflow at {}", usize::MAX - 1))
    );
    assert_eq!(
        read_u64_le(&buf, 9).unwrap_err(),
        EngineError::Serialization("u64 out of bounds at 9".into())
    );
    assert!(write_u16_le(&mut buf, 15, 1).is_err());

    let prefix = [0xff, 0x12, 0x34, 0x56, 0x78, 0xee];
    assert_eq!(
        unsafe_read_struct::<u32>(&prefix[1..]).unwrap(),
        u32::from_ne_bytes([0x12, 0x34, 0x56, 0x78])
    );
    assert_eq!(
        unsafe_read_struct::<u64>(&[0; 7]).unwrap_err(),
        EngineError::Serialization("short struct read: need 8, got 7".into())
    );

    assert_eq!(compare_keys(b"a", b"a"), Ordering::Equal);
    assert_eq!(compare_keys(b"a", b"b"), Ordering::Less);
    assert_eq!(compare_keys(b"ab", b"a"), Ordering::Greater);
    assert_eq!(compare_keys(&[0x80], &[0x7f]), Ordering::Greater);
    assert!(key_in_range(b"m", Some(b"a"), None, Some(b"z"), None));
    assert!(!key_in_range(b"a", Some(b"a"), None, None, None));
    assert!(key_in_range(b"a", None, Some(b"a"), None, Some(b"a")));

    assert!(validate_store_name(&"n".repeat(MAX_STORE_NAME_BYTES)).is_ok());
    assert_eq!(
        validate_store_name(&"n".repeat(MAX_STORE_NAME_BYTES + 1)).unwrap_err(),
        EngineError::StoreNameTooLong(MAX_STORE_NAME_BYTES + 1)
    );
    assert!(validate_key(&vec![0; MAX_KEY_BYTES]).is_ok());
    assert_eq!(
        validate_key(&vec![0; MAX_KEY_BYTES + 1]).unwrap_err(),
        EngineError::KeyTooLarge(MAX_KEY_BYTES + 1)
    );
    assert_eq!(MAX_VALUE_BYTES, 8 * 1024 * 1024);
    assert!(validate_value(&vec![0; MAX_VALUE_BYTES]).is_ok());
    assert_eq!(
        validate_value(&vec![0; MAX_VALUE_BYTES + 1]).unwrap_err(),
        EngineError::ValueTooLarge(MAX_VALUE_BYTES + 1)
    );

    assert_eq!(page_offset(0), 0);
    assert_eq!(page_offset(1), 0);
    assert_eq!(page_offset(2), PAGE_SIZE as u64);
    let max_fit = u64::MAX / PAGE_SIZE as u64;
    assert_eq!(page_offset(max_fit + 1), max_fit * PAGE_SIZE as u64);
    assert_eq!(page_offset(max_fit + 2), u64::MAX);
    assert_eq!(page_offset(u64::MAX), u64::MAX);
}
