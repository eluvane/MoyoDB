use crc32fast::Hasher;

pub fn crc32(bytes: &[u8]) -> u32 {
    let mut hasher = Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

pub fn checksum_with_zeroed_region(bytes: &[u8], zero_start: usize, zero_len: usize) -> u32 {
    let mut cloned = bytes.to_vec();
    for i in zero_start..zero_start.saturating_add(zero_len).min(cloned.len()) {
        cloned[i] = 0;
    }
    crc32(&cloned)
}
