use crc32fast::Hasher;

const ZEROES: [u8; 16] = [0; 16];

pub fn crc32(bytes: &[u8]) -> u32 {
    let mut hasher = Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

/// CRC32 of `bytes` as if `zero_len` bytes starting at `zero_start` were zero.
/// Hashes the prefix, a run of zeroes and the suffix, so no page-sized copy is made.
pub fn checksum_with_zeroed_region(bytes: &[u8], zero_start: usize, zero_len: usize) -> u32 {
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
}
