use crc32fast::Hasher;

use crate::layout::{PAGE_HEADER_CHECKSUM_OFFSET, PAGE_SIZE};

const ZEROES: [u8; 16] = [0; 16];

pub fn crc32(bytes: &[u8]) -> u32 {
    #[cfg(test)]
    work::record(bytes.len());
    let mut hasher = Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

/// CRC32 of `bytes` as if `zero_len` bytes starting at `zero_start` were zero.
/// Hashes the prefix, a run of zeroes and the suffix, so no page-sized copy is made.
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

// A CRC difference propagates through each following zero bit linearly.
// These tables apply the two fixed transformations needed by a WAL page record.
// They are evaluated at compile time, including on targets without CRC intrinsics.
const CRC32_POLYNOMIAL: u32 = 0xedb8_8320;
type CrcTransform = [[u32; 256]; 4];

static PREFIX_THROUGH_PAGE: CrcTransform = crc_transform(PAGE_SIZE, false);
static EMBEDDED_PAGE_CHECKSUM: CrcTransform =
    crc_transform(PAGE_SIZE - PAGE_HEADER_CHECKSUM_OFFSET, true);

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
/// The page encoder already hashed the page with its checksum field zeroed.
/// Inserting that little-endian checksum changes the CRC by the checksum's
/// linear propagation from the field to the page end. Combining this corrected
/// page CRC with the prefix CRC avoids reading the page payload a second time.
/// This requires the original page checksum; arbitrary input must be hashed.
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

    pub(super) fn record(len: usize) {
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
            // Include empty/short prefixes and both page-aligned boundary sides.
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
}
