use crc32fast::Hasher;

use crate::layout::{PAGE_HEADER_CHECKSUM_OFFSET, PAGE_HEADER_SIZE, PAGE_SIZE};

const ZEROES: [u8; 16] = [0; 16];

pub fn crc32(bytes: &[u8]) -> u32 {
    #[cfg(test)]
    work::record(bytes.len());
    let mut hasher = Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

/// CRC32 of `bytes` as if `zero_len` bytes starting at `zero_start` were zero.
/// The region is clamped to the slice. No page copy is needed.
pub fn checksum_with_zeroed_region(bytes: &[u8], zero_start: usize, zero_len: usize) -> u32 {
    #[cfg(test)]
    work::record(bytes.len());
    let start = zero_start.min(bytes.len());
    let end = zero_start.saturating_add(zero_len).min(bytes.len());
    let mut hasher = Hasher::new();
    hasher.update(&bytes[..start]);
    let mut remaining = end - start;
    while remaining > 0 {
        let chunk = remaining.min(ZEROES.len());
        hasher.update(&ZEROES[..chunk]);
        remaining -= chunk;
    }
    hasher.update(&bytes[end..]);
    hasher.finalize()
}

// CRC differences propagate linearly through zero bits.
// Fixed transforms avoid generic CRC combination for pages and full overflow chunks.
const CRC32_POLYNOMIAL: u32 = 0xedb8_8320;
type CrcTransform = [[u32; 256]; 4];

static PREFIX_THROUGH_PAGE: CrcTransform = crc_transform(PAGE_SIZE, false);
static EMBEDDED_PAGE_CHECKSUM: CrcTransform =
    crc_transform(PAGE_SIZE - PAGE_HEADER_CHECKSUM_OFFSET, true);
const OVERFLOW_CHUNK_LEN: usize = PAGE_SIZE - PAGE_HEADER_SIZE - 12;
static PREFIX_THROUGH_OVERFLOW: CrcTransform = crc_transform(OVERFLOW_CHUNK_LEN, false);

const fn crc_transform(zero_bytes: usize, include_identity: bool) -> CrcTransform {
    let mut first = 1u32;
    let mut remaining = zero_bytes * 8;
    while remaining != 0 {
        first = (first >> 1) ^ (CRC32_POLYNOMIAL & 0u32.wrapping_sub(first & 1));
        remaining -= 1;
    }

    let mut basis = [0u32; 32];
    let mut bit = 0;
    while bit < 32 {
        basis[bit] = first ^ if include_identity { 1u32 << bit } else { 0 };
        // Invert one reflected CRC bit step to obtain the next input-bit basis.
        let low_bit = first >> 31;
        first = ((first ^ (CRC32_POLYNOMIAL & 0u32.wrapping_sub(low_bit))) << 1) | low_bit;
        bit += 1;
    }

    let mut table = [[0u32; 256]; 4];
    let mut byte = 0;
    while byte < 4 {
        let mut value = 1usize;
        while value < 256 {
            let lowest_bit = value.trailing_zeros() as usize;
            table[byte][value] = table[byte][value & (value - 1)] ^ basis[byte * 8 + lowest_bit];
            value += 1;
        }
        byte += 1;
    }
    table
}

fn transform_crc(table: &CrcTransform, crc: u32) -> u32 {
    table[0][crc as u8 as usize]
        ^ table[1][(crc >> 8) as u8 as usize]
        ^ table[2][(crc >> 16) as u8 as usize]
        ^ table[3][(crc >> 24) as usize]
}

/// CRC32 of `prefix` followed by an immutable, freshly encoded page.
///
/// Requires an unchanged page and its checksum computed with the field zeroed.
/// Correcting for the stored little-endian checksum avoids hashing the page again.
/// Arbitrary input must be hashed directly.
///
/// If `advance(c, n)` propagates a CRC difference through `n` zero bytes, the
/// result is `advance(crc32(prefix), PAGE_SIZE) ^ c ^ advance(c, PAGE_SIZE - k)`,
/// where `c` is the stored page checksum and `k` its field offset. The last
/// advance includes the field itself: the CRC difference of its four
/// little-endian bytes is `advance(c, 4)`. Both advances use fixed tables.
pub(crate) fn crc32_with_generated_page(prefix: &[u8], page_checksum: u32) -> u32 {
    transform_crc(&PREFIX_THROUGH_PAGE, crc32(prefix))
        ^ transform_crc(&EMBEDDED_PAGE_CHECKSUM, page_checksum)
}

/// Propagates a CRC difference through zero bytes. Full overflow chunks use
/// a fixed transform; only the first and last partial chunks need a generic
/// polynomial combine, independent of the number of pages in the value.
fn advance_crc(crc: u32, len: usize) -> u32 {
    if crc == 0 || len == 0 {
        return crc;
    }
    if len == OVERFLOW_CHUNK_LEN {
        return transform_crc(&PREFIX_THROUGH_OVERFLOW, crc);
    }
    let mut hasher = Hasher::new_with_initial(crc);
    hasher.combine(&Hasher::new_with_initial_len(0, len as u64));
    hasher.finalize()
}

/// Page checksum for a freshly encoded overflow page, given the CRC of its
/// immutable chunk. The header's checksum field is zero. Appending n zeroes
/// transforms c into !advance(!c, n), so padding is included without reading it.
pub(crate) fn crc32_with_generated_overflow(
    header: &[u8],
    chunk_checksum: u32,
    chunk_len: usize,
) -> u32 {
    transform_crc(&PREFIX_THROUGH_OVERFLOW, crc32(header))
        ^ !advance_crc(!chunk_checksum, OVERFLOW_CHUNK_LEN - chunk_len)
}

/// Hashes the payload once for two prefixed views with different chunk boundaries.
/// Each prefix must be shorter than an overflow chunk.
/// For cumulative CRCs a and b, CRC(payload[start..end]) = b ^ advance(a, len).
pub(crate) fn paired_payload_checksums(
    first_prefix: &[u8],
    second_prefix: &[u8],
    payload: &[u8],
) -> (Vec<u32>, Vec<u32>) {
    debug_assert!(first_prefix.len() < OVERFLOW_CHUNK_LEN);
    debug_assert!(second_prefix.len() < OVERFLOW_CHUNK_LEN);
    struct Chunks {
        prefix_crc: u32,
        previous_crc: u32,
        previous_end: usize,
        next_end: usize,
        checksums: Vec<u32>,
    }

    let mut chunks = [first_prefix, second_prefix].map(|prefix| Chunks {
        prefix_crc: crc32(prefix),
        previous_crc: 0,
        previous_end: 0,
        next_end: (OVERFLOW_CHUNK_LEN - prefix.len()).min(payload.len()),
        checksums: Vec::with_capacity((prefix.len() + payload.len()).div_ceil(OVERFLOW_CHUNK_LEN)),
    });
    if payload.is_empty() {
        for (chunk, prefix) in chunks.iter_mut().zip([first_prefix, second_prefix]) {
            if !prefix.is_empty() {
                chunk.checksums.push(chunk.prefix_crc);
            }
        }
    }
    let mut hasher = Hasher::new();
    let mut position = 0;
    while position < payload.len() {
        let end = chunks[0].next_end.min(chunks[1].next_end);
        #[cfg(test)]
        work::record(end - position);
        hasher.update(&payload[position..end]);
        let cumulative = hasher.clone().finalize();
        for chunk in &mut chunks {
            if chunk.next_end != end {
                continue;
            }
            let len = end - chunk.previous_end;
            let mut checksum = cumulative ^ advance_crc(chunk.previous_crc, len);
            if chunk.previous_end == 0 {
                checksum ^= advance_crc(chunk.prefix_crc, len);
            }
            chunk.checksums.push(checksum);
            chunk.previous_crc = cumulative;
            chunk.previous_end = end;
            chunk.next_end = (end + OVERFLOW_CHUNK_LEN).min(payload.len());
        }
        position = end;
    }
    let [first, second] = chunks;
    (first.checksums, second.checksums)
}

#[cfg(test)]
pub(crate) mod work {
    use std::cell::Cell;

    thread_local! {
        static HASHED_BYTES: Cell<usize> = const { Cell::new(0) };
    }

    pub(crate) fn reset() {
        HASHED_BYTES.with(|bytes| bytes.set(0));
    }

    pub(crate) fn hashed_bytes() -> usize {
        HASHED_BYTES.with(Cell::get)
    }

    pub(crate) fn record(len: usize) {
        HASHED_BYTES.with(|bytes| bytes.set(bytes.get() + len));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(bytes: &[u8], zero_start: usize, zero_len: usize) -> u32 {
        let mut cloned = bytes.to_vec();
        for byte in cloned.iter_mut().skip(zero_start).take(zero_len) {
            *byte = 0;
        }
        crc32(&cloned)
    }

    #[test]
    fn matches_copying_reference() {
        let bytes: Vec<u8> = (0..4096u32).map(|value| (value * 31 + 7) as u8).collect();
        for (start, len) in [
            (0, 4),
            (4, 4),
            (12, 4),
            (64, 4),
            (4090, 16),
            (5000, 4),
            (0, 0),
        ] {
            assert_eq!(
                checksum_with_zeroed_region(&bytes, start, len),
                reference(&bytes, start, len),
                "start={start} len={len}"
            );
        }
    }

    #[test]
    fn generated_page_crc_matches_hashing_all_bytes() {
        let mut state = 0x731d_a583u32;
        for index in 0..512 {
            let mut page = vec![0u8; PAGE_SIZE];
            for byte in &mut page {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                *byte = state as u8;
            }
            let page_checksum = checksum_with_zeroed_region(&page, PAGE_HEADER_CHECKSUM_OFFSET, 4);
            page[PAGE_HEADER_CHECKSUM_OFFSET..PAGE_HEADER_CHECKSUM_OFFSET + 4]
                .copy_from_slice(&page_checksum.to_le_bytes());
            let prefix = vec![index as u8; [0, 1, 15, 40, 255, 4095, 4096, 4097][index % 8]];
            let mut complete = prefix.clone();
            complete.extend_from_slice(&page);
            assert_eq!(
                crc32_with_generated_page(&prefix, page_checksum),
                crc32(&complete),
                "random page {index}"
            );
        }
    }

    #[test]
    fn fixed_crc_transforms_match_generic_combine() {
        for crc in (0..32)
            .map(|bit| 1u32 << bit)
            .chain([0, u32::MAX, 0x1234_5678])
        {
            for (table, len, identity) in [
                (&PREFIX_THROUGH_PAGE, PAGE_SIZE, false),
                (&PREFIX_THROUGH_OVERFLOW, OVERFLOW_CHUNK_LEN, false),
                (
                    &EMBEDDED_PAGE_CHECKSUM,
                    PAGE_SIZE - PAGE_HEADER_CHECKSUM_OFFSET,
                    true,
                ),
            ] {
                let mut reference = Hasher::new_with_initial(crc);
                reference.combine(&Hasher::new_with_initial_len(0, len as u64));
                let expected = reference.finalize() ^ if identity { crc } else { 0 };
                assert_eq!(transform_crc(table, crc), expected);
            }
        }
    }

    #[test]
    fn paired_checksums_match_complete_chunks_at_both_boundary_sequences() {
        let mut random = 0x739a_1183u32;
        let bytes: Vec<u8> = (0..OVERFLOW_CHUNK_LEN * 5)
            .map(|_| {
                random ^= random << 13;
                random ^= random >> 17;
                random ^= random << 5;
                random as u8
            })
            .collect();
        for first_len in [0, 1, 14, 16, 17, 1293, OVERFLOW_CHUNK_LEN - 1] {
            for second_len in [0, 1, 14, 16, 17, 1293, OVERFLOW_CHUNK_LEN - 1] {
                let first_prefix = &bytes[..first_len];
                let second_prefix = &bytes[OVERFLOW_CHUNK_LEN..OVERFLOW_CHUNK_LEN + second_len];
                let mut lengths = vec![0, 1, 15, 16, 17, 1008, 1009, 1023, 1024, 1025];
                for prefix in [first_len, second_len] {
                    for count in [1, 2, 4] {
                        let boundary = count * OVERFLOW_CHUNK_LEN - prefix;
                        lengths.extend([boundary - 1, boundary, boundary + 1]);
                    }
                }
                lengths.sort_unstable();
                lengths.dedup();
                for len in lengths {
                    let payload = &bytes[..len];
                    let (first, second) =
                        paired_payload_checksums(first_prefix, second_prefix, payload);
                    for (prefix, checksums) in [(first_prefix, first), (second_prefix, second)] {
                        let mut encoded = prefix.to_vec();
                        encoded.extend_from_slice(payload);
                        let expected: Vec<_> =
                            encoded.chunks(OVERFLOW_CHUNK_LEN).map(crc32).collect();
                        assert_eq!(
                            checksums, expected,
                            "prefixes={first_len}/{second_len}, payload={len}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn paired_checksum_work_is_one_payload_pass_plus_both_prefixes() {
        let payload = vec![0x53; 64 * 1024];
        let first = [0x17; 16];
        let second = [0x6b; 1293];
        work::reset();
        let _ = paired_payload_checksums(&first, &second, &payload);
        assert_eq!(
            work::hashed_bytes(),
            payload.len() + first.len() + second.len()
        );
    }
}
