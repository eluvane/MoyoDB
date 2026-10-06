use super::{
    decode_internal_cell_ref, decode_leaf_cell_ref, decode_page, encode_internal_page,
    encode_leaf_page, encode_overflow_page, internal_header_at, leaf_header_at, InternalCell,
    LeafCell, PAGE_CELL_COUNT_OFFSET, PAGE_ID_OFFSET, PAGE_KIND_OFFSET, PAGE_LEVEL_OFFSET,
    PAGE_LOWER_OFFSET, PAGE_RIGHT_SIBLING_OFFSET, PAGE_UPPER_OFFSET,
};
use crate::checksum::checksum_with_zeroed_region;
use crate::error::EngineError;
use crate::layout::{
    PageKind, ValueKind, PAGE_HEADER_CHECKSUM_OFFSET, PAGE_HEADER_SIZE, PAGE_MAGIC, PAGE_SIZE,
};

fn put_u16(buf: &mut [u8], offset: usize, value: u16) {
    buf[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(buf: &mut [u8], offset: usize, value: u64) {
    buf[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[allow(clippy::too_many_arguments)]
fn finish_header(
    buf: &mut [u8],
    page_id: u64,
    kind: PageKind,
    level: u8,
    cell_count: u16,
    lower: u16,
    upper: u16,
    right_sibling: u64,
) {
    buf[..4].copy_from_slice(&PAGE_MAGIC);
    put_u64(buf, PAGE_ID_OFFSET, page_id);
    buf[PAGE_KIND_OFFSET] = kind as u8;
    buf[PAGE_LEVEL_OFFSET] = level;
    put_u16(buf, PAGE_CELL_COUNT_OFFSET, cell_count);
    put_u16(buf, PAGE_LOWER_OFFSET, lower);
    put_u16(buf, PAGE_UPPER_OFFSET, upper);
    put_u64(buf, PAGE_RIGHT_SIBLING_OFFSET, right_sibling);
    let checksum = checksum_with_zeroed_region(buf, PAGE_HEADER_CHECKSUM_OFFSET, 4);
    put_u32(buf, PAGE_HEADER_CHECKSUM_OFFSET, checksum);
}

fn reference_leaf(page_id: u64, right_sibling: u64, cells: &[LeafCell]) -> Vec<u8> {
    let mut buf = vec![0u8; PAGE_SIZE];
    let mut upper = PAGE_SIZE;
    let lower = PAGE_HEADER_SIZE + cells.len() * 2;
    for (index, cell) in cells.iter().enumerate() {
        let inline: &[u8] = if cell.value_kind == ValueKind::Overflow {
            &[]
        } else {
            cell.value.as_slice()
        };
        let encoded_len = 20 + cell.key.len() + inline.len();
        assert!(upper >= lower + encoded_len);
        upper -= encoded_len;
        put_u16(&mut buf, upper, cell.key.len() as u16);
        buf[upper + 2] = cell.value_kind as u8;
        put_u32(&mut buf, upper + 4, cell.total_value_len);
        put_u64(&mut buf, upper + 8, cell.overflow_head_page_id);
        put_u32(&mut buf, upper + 16, inline.len() as u32);
        buf[upper + 20..upper + 20 + cell.key.len()].copy_from_slice(&cell.key);
        buf[upper + 20 + cell.key.len()..upper + encoded_len].copy_from_slice(inline);
        put_u16(&mut buf, PAGE_HEADER_SIZE + index * 2, upper as u16);
    }
    finish_header(
        &mut buf,
        page_id,
        PageKind::Leaf,
        0,
        cells.len() as u16,
        lower as u16,
        upper as u16,
        right_sibling,
    );
    buf
}

fn reference_internal(page_id: u64, right_sibling: u64, cells: &[InternalCell]) -> Vec<u8> {
    let mut buf = vec![0u8; PAGE_SIZE];
    let mut upper = PAGE_SIZE;
    let lower = PAGE_HEADER_SIZE + cells.len() * 2;
    for (index, cell) in cells.iter().enumerate() {
        let encoded_len = 12 + cell.separator.len();
        assert!(upper >= lower + encoded_len);
        upper -= encoded_len;
        put_u16(&mut buf, upper, cell.separator.len() as u16);
        put_u64(&mut buf, upper + 4, cell.child_page_id);
        buf[upper + 12..upper + encoded_len].copy_from_slice(&cell.separator);
        put_u16(&mut buf, PAGE_HEADER_SIZE + index * 2, upper as u16);
    }
    finish_header(
        &mut buf,
        page_id,
        PageKind::Internal,
        1,
        cells.len() as u16,
        lower as u16,
        upper as u16,
        right_sibling,
    );
    buf
}

fn reference_overflow(page_id: u64, next: u64, chunk: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; PAGE_SIZE];
    let chunk_start = PAGE_HEADER_SIZE + 12;
    put_u64(&mut buf, PAGE_HEADER_SIZE, next);
    put_u32(&mut buf, PAGE_HEADER_SIZE + 8, chunk.len() as u32);
    buf[chunk_start..chunk_start + chunk.len()].copy_from_slice(chunk);
    finish_header(
        &mut buf,
        page_id,
        PageKind::Overflow,
        0,
        0,
        chunk_start as u16,
        (chunk_start + chunk.len()) as u16,
        0,
    );
    buf
}

#[test]
fn encoded_pages_match_explicit_layout() {
    let leaf_cells = vec![
        LeafCell {
            key: b"alpha".to_vec(),
            value: vec![0x11; 1500],
            value_kind: ValueKind::Inline,
            total_value_len: 1500,
            overflow_head_page_id: 0,
        },
        LeafCell {
            key: b"beta".to_vec(),
            value: Vec::new(),
            value_kind: ValueKind::Overflow,
            total_value_len: 9000,
            overflow_head_page_id: 42,
        },
        LeafCell {
            key: b"c".to_vec(),
            value: b"tail".to_vec(),
            value_kind: ValueKind::Inline,
            total_value_len: 4,
            overflow_head_page_id: 0,
        },
    ];
    let leaf = encode_leaf_page(7, 0, 8, &leaf_cells).unwrap();
    let leaf_reference = reference_leaf(7, 8, &leaf_cells);
    assert_eq!(leaf, leaf_reference);
    assert_eq!(decode_page(&leaf_reference).unwrap().leaf_cells, leaf_cells);
    let slot = u16::from_le_bytes([leaf[PAGE_HEADER_SIZE], leaf[PAGE_HEADER_SIZE + 1]]) as usize;
    assert!(leaf_header_at(&leaf, slot).is_some());

    let internal_cells = vec![
        InternalCell {
            separator: b"m".to_vec(),
            child_page_id: 3,
        },
        InternalCell {
            separator: vec![0x7a; 300],
            child_page_id: 9,
        },
    ];
    let internal = encode_internal_page(11, 1, 12, &internal_cells).unwrap();
    let internal_reference = reference_internal(11, 12, &internal_cells);
    assert_eq!(internal, internal_reference);
    assert_eq!(
        decode_page(&internal_reference).unwrap().internal_cells,
        internal_cells
    );
    let internal_slot =
        u16::from_le_bytes([internal[PAGE_HEADER_SIZE], internal[PAGE_HEADER_SIZE + 1]]) as usize;
    assert!(internal_header_at(&internal, internal_slot).is_some());

    let chunk = vec![0xab; 1000];
    let overflow = encode_overflow_page(12, 13, &chunk).unwrap();
    let overflow_reference = reference_overflow(12, 13, &chunk);
    assert_eq!(overflow, overflow_reference);
    let body = decode_page(&overflow_reference).unwrap().overflow.unwrap();
    assert_eq!(body.next_overflow_page_id, 13);
    assert_eq!(body.chunk, chunk);

    let empty = encode_leaf_page(1, 0, 0, &[]).unwrap();
    assert_eq!(empty, reference_leaf(1, 0, &[]));
    assert!(decode_page(&empty).unwrap().leaf_cells.is_empty());

    let mut tampered = leaf.clone();
    tampered[PAGE_HEADER_SIZE + 80] ^= 0xff;
    assert_eq!(
        decode_page(&tampered).unwrap_err(),
        EngineError::Corruption("page checksum mismatch".into())
    );
}

#[test]
fn rejects_empty_overflow_and_page_id_zero() {
    assert_eq!(
        encode_overflow_page(1, 0, &[]).unwrap_err(),
        EngineError::Serialization("overflow chunk is empty".into())
    );
    let chunk_start = PAGE_HEADER_SIZE + 12;
    let mut empty_chunk = vec![0u8; PAGE_SIZE];
    finish_header(
        &mut empty_chunk,
        4,
        PageKind::Overflow,
        0,
        0,
        chunk_start as u16,
        chunk_start as u16,
        0,
    );
    assert_eq!(
        decode_page(&empty_chunk).unwrap_err(),
        EngineError::Corruption("overflow page has an empty chunk".into())
    );
    let page_zero = encode_leaf_page(0, 0, 0, &[]).unwrap();
    assert_eq!(
        decode_page(&page_zero).unwrap_err(),
        EngineError::Corruption("page header declares page id 0".into())
    );
    let bad_child = encode_internal_page(
        2,
        1,
        0,
        &[InternalCell {
            separator: b"a".to_vec(),
            child_page_id: 0,
        }],
    )
    .unwrap();
    assert_eq!(
        decode_page(&bad_child).unwrap_err(),
        EngineError::Corruption("internal cell has child_page_id=0".into())
    );
    let bad_len = encode_leaf_page(
        3,
        0,
        0,
        &[LeafCell {
            key: b"a".to_vec(),
            value: b"1".to_vec(),
            value_kind: ValueKind::Inline,
            total_value_len: 2,
            overflow_head_page_id: 0,
        }],
    )
    .unwrap();
    assert_eq!(
        decode_page(&bad_len).unwrap_err(),
        EngineError::Corruption("inline leaf cell length metadata mismatch".into())
    );
    let big = LeafCell {
        key: vec![1; 2000],
        value: vec![2; 2000],
        value_kind: ValueKind::Inline,
        total_value_len: 2000,
        overflow_head_page_id: 0,
    };
    assert_eq!(
        encode_leaf_page(1, 0, 0, &[big.clone(), big]).unwrap_err(),
        EngineError::Serialization("leaf page overflow".into())
    );
}

fn leaf_err(bytes: &[u8]) -> EngineError {
    match decode_leaf_cell_ref(bytes, 0) {
        Ok(_) => panic!("truncated leaf cell was accepted"),
        Err(error) => error,
    }
}

fn internal_err(bytes: &[u8]) -> EngineError {
    match decode_internal_cell_ref(bytes, 0) {
        Ok(_) => panic!("truncated internal cell was accepted"),
        Err(error) => error,
    }
}

#[test]
fn truncated_cells_keep_field_errors() {
    assert_eq!(
        leaf_err(&[]),
        EngineError::Serialization("u16 out of bounds at 0".into())
    );
    assert_eq!(
        leaf_err(&[0, 0]),
        EngineError::Corruption("leaf cell kind".into())
    );
    assert_eq!(
        leaf_err(&[0, 0, 1, 0, 0]),
        EngineError::Serialization("u32 out of bounds at 4".into())
    );
    assert_eq!(
        internal_err(&[1, 0]),
        EngineError::Serialization("u64 out of bounds at 4".into())
    );
}
