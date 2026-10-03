use super::*;
use crate::btree::PageAllocator;
use crate::change_feed::{encode_change_record_payload, encode_change_record_prefix, ChangeKind};
use crate::checksum::checksum_with_zeroed_region;
use crate::layout::{
    PageHeader, PageKind, PAGE_HEADER_CHECKSUM_OFFSET, PAGE_HEADER_SIZE, PAGE_MAGIC, PAGE_SIZE,
};
use crate::overflow::{write_overflow_chain, write_overflow_value};
use crate::page::encode_overflow_page;
use crate::value::{
    StoreCompression, STORE_FLAG_COMPRESSION_SHIFT, STORE_FLAG_SYSTEM_RAW_VALUES,
    STORE_FLAG_VALUE_ENVELOPE_V1,
};
use zerocopy::IntoBytes;

/// The prior contiguous encoder, retained only as a disk-byte oracle. It
/// initializes a whole page to zero and hashes every byte of the complete page.
fn reference_page(page_id: u64, next: u64, chunk: &[u8]) -> Vec<u8> {
    let start = PAGE_HEADER_SIZE + 12;
    let header = PageHeader {
        magic: PAGE_MAGIC,
        checksum: 0,
        page_id: page_id.to_le(),
        page_kind: PageKind::Overflow as u8,
        level: 0,
        cell_count: 0,
        lower: (start as u16).to_le(),
        upper: ((start + chunk.len()) as u16).to_le(),
        reserved: 0,
        right_sibling_page_id: 0,
    };
    let mut page = vec![0; PAGE_SIZE];
    page[..PAGE_HEADER_SIZE].copy_from_slice(header.as_bytes());
    page[PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + 8].copy_from_slice(&next.to_le_bytes());
    page[PAGE_HEADER_SIZE + 8..start].copy_from_slice(&(chunk.len() as u32).to_le_bytes());
    page[start..start + chunk.len()].copy_from_slice(chunk);
    let checksum = checksum_with_zeroed_region(&page, PAGE_HEADER_CHECKSUM_OFFSET, 4);
    page[PAGE_HEADER_CHECKSUM_OFFSET..PAGE_HEADER_CHECKSUM_OFFSET + 4]
        .copy_from_slice(&checksum.to_le_bytes());
    page
}

fn assert_chain(value: &PreparedValue<'_>, expected: &[u8]) -> Result<()> {
    assert_eq!(value.parts().to_vec(), expected);
    let mut alloc = PageAllocator::new(u32::MAX as u64 + 1);
    let result = write_overflow_value(value, &mut alloc);
    if expected.is_empty() {
        assert_eq!(
            result.unwrap_err().to_string(),
            write_overflow_chain(expected, &mut PageAllocator::new(1))
                .unwrap_err()
                .to_string(),
        );
        return Ok(());
    }
    let chain = result?;
    let chunks: Vec<_> = expected.chunks(max_overflow_chunk_len()).collect();
    assert_eq!(chain.pages.len(), chunks.len());
    assert_eq!(chain.head_page_id, chain.pages[0].0);
    for (index, ((page_id, page), chunk)) in chain.pages.iter().zip(chunks).enumerate() {
        let next = chain.pages.get(index + 1).map(|page| page.0).unwrap_or(0);
        assert_eq!(*page, reference_page(*page_id, next, chunk));
    }
    Ok(())
}

#[test]
fn prepared_pages_match_previous_encoder_for_flags_prefixes_and_boundaries() -> Result<()> {
    let chunk_len = max_overflow_chunk_len();
    let envelope = STORE_FLAG_VALUE_ENVELOPE_V1;
    let mut random = 0x6acf_0529u32;
    let bytes: Vec<_> = (0..64 * 1024)
        .map(|_| {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            random as u8
        })
        .collect();
    for flags in [
        0,
        envelope,
        STORE_FLAG_SYSTEM_RAW_VALUES,
        STORE_FLAG_SYSTEM_RAW_VALUES | envelope,
        envelope | (StoreCompression::Gzip.bits() << STORE_FLAG_COMPRESSION_SHIFT),
        envelope | (StoreCompression::Deflate.bits() << STORE_FLAG_COMPRESSION_SHIFT),
    ] {
        for expiry in [None, Some(1 << 62)] {
            for (store, key) in [
                (String::new(), Vec::new()),
                ("kv".into(), vec![0x61; 4]),
                ("s".repeat(255), vec![0x7a; 1024]),
            ] {
                let change_prefix_len = 14 + store.len() + key.len();
                let mut lengths = vec![0, 1, 1008, 1009, 1023, 1024, 1025, bytes.len()];
                for prefix in [0, 16, change_prefix_len] {
                    for count in [1, 2, 4] {
                        let boundary = chunk_len * count - prefix;
                        lengths.extend([boundary - 1, boundary, boundary + 1]);
                    }
                }
                lengths.sort_unstable();
                lengths.dedup();
                for len in lengths {
                    let stored = StoredValue::with_expiry(bytes[..len].to_vec(), expiry);
                    let reference = stored.encode_for_store(flags);
                    let prepared = PreparedValue::stored(&stored, flags);
                    let (mut value, reference) = match (prepared, reference) {
                        (Ok(value), Ok(reference)) => (value, reference),
                        (Err(error), Err(expected)) => {
                            assert_eq!(error.to_string(), expected.to_string());
                            continue;
                        }
                        _ => panic!("prepared and contiguous encoders disagree for flags {flags}"),
                    };
                    let change_bytes = encode_change_record_payload(
                        &store,
                        &key,
                        ChangeKind::Put,
                        Some(&stored.value),
                    )?;
                    let prefix = encode_change_record_prefix(
                        &store,
                        &key,
                        ChangeKind::Put,
                        Some(&stored.value),
                    )?;
                    let mut change = PreparedValue::prefixed(prefix, &stored.value);
                    value.share_payload_checksums(&mut change);
                    assert_chain(&value, &reference)?;
                    assert_chain(&change, &change_bytes)?;
                }
            }
        }
    }
    Ok(())
}

#[test]
fn generic_overflow_encoder_retains_every_disk_byte_and_length_error() -> Result<()> {
    for len in [
        0,
        1,
        15,
        16,
        17,
        1024,
        max_overflow_chunk_len() - 1,
        max_overflow_chunk_len(),
    ] {
        let bytes: Vec<_> = (0..len).map(|value| (value * 131 + 7) as u8).collect();
        assert_eq!(
            encode_overflow_page(17, u32::MAX as u64 + 13, &bytes)?,
            reference_page(17, u32::MAX as u64 + 13, &bytes),
        );
    }
    let bytes = vec![0; max_overflow_chunk_len() + 1];
    assert_eq!(
        encode_overflow_page(1, 0, &bytes).unwrap_err().to_string(),
        format!(
            "serialization error: overflow chunk too large: {} > {}",
            bytes.len(),
            max_overflow_chunk_len(),
        ),
    );
    Ok(())
}

#[test]
fn equal_content_in_different_allocations_keeps_generic_checksumming() {
    let first = StoredValue::plain(vec![0x31; 64 * 1024]);
    let second = first.clone();
    let mut value = PreparedValue::stored(&first, STORE_FLAG_VALUE_ENVELOPE_V1).unwrap();
    let mut other = PreparedValue::stored(&second, STORE_FLAG_VALUE_ENVELOPE_V1).unwrap();
    value.share_payload_checksums(&mut other);
    assert!(value.chunk_checksums.is_empty());
    assert!(other.chunk_checksums.is_empty());
}

#[test]
fn change_prefix_validation_matches_contiguous_encoding() {
    for (kind, value, key) in [
        (ChangeKind::Put, None, &b"key"[..]),
        (ChangeKind::Delete, Some(&b"value"[..]), &b"key"[..]),
        (ChangeKind::Clear, None, &b"key"[..]),
        (ChangeKind::Drop, Some(&b"value"[..]), &b""[..]),
    ] {
        assert_eq!(
            encode_change_record_prefix("kv", key, kind, value)
                .unwrap_err()
                .to_string(),
            encode_change_record_payload("kv", key, kind, value)
                .unwrap_err()
                .to_string(),
        );
    }
}
