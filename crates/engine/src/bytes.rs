use crate::error::{EngineError, Result};

pub const MAX_STORE_NAME_BYTES: usize = 255;
pub const MAX_KEY_BYTES: usize = 1024;
pub const MAX_VALUE_BYTES: usize = 8 * 1024 * 1024;
/// Upper bound for any value stored in a tree: a user value plus the TTL
/// envelope, or a change-log payload (header, store name, key and value).
pub const MAX_STORED_VALUE_BYTES: usize =
    MAX_VALUE_BYTES + MAX_KEY_BYTES + MAX_STORE_NAME_BYTES + 64;

/// Serialization failures format a `String`. Keep that off the inlined
/// page-read and length-check path.
#[cold]
#[inline(never)]
fn endian_offset_overflow(what: &str, offset: usize) -> EngineError {
    EngineError::Serialization(format!("{what} offset overflow at {offset}"))
}

#[cold]
#[inline(never)]
fn endian_out_of_bounds(what: &str, offset: usize) -> EngineError {
    EngineError::Serialization(format!("{what} out of bounds at {offset}"))
}

#[cold]
#[inline(never)]
fn length_does_not_fit(what: &str, len: usize) -> EngineError {
    EngineError::Serialization(format!("{what}: {len}"))
}

#[inline]
fn checked_range(offset: usize, width: usize, what: &str) -> Result<std::ops::Range<usize>> {
    match offset.checked_add(width) {
        Some(end) => Ok(offset..end),
        None => Err(endian_offset_overflow(what, offset)),
    }
}

#[inline]
pub fn read_u16_le(bytes: &[u8], offset: usize) -> Result<u16> {
    let range = checked_range(offset, 2, "u16")?;
    let Some(slice) = bytes.get(range) else {
        return Err(endian_out_of_bounds("u16", offset));
    };
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

#[inline]
pub fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32> {
    let range = checked_range(offset, 4, "u32")?;
    let Some(slice) = bytes.get(range) else {
        return Err(endian_out_of_bounds("u32", offset));
    };
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

#[inline]
pub fn read_u64_le(bytes: &[u8], offset: usize) -> Result<u64> {
    let range = checked_range(offset, 8, "u64")?;
    let Some(slice) = bytes.get(range) else {
        return Err(endian_out_of_bounds("u64", offset));
    };
    Ok(u64::from_le_bytes([
        slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
    ]))
}

#[inline]
pub fn write_u16_le(dst: &mut [u8], offset: usize, value: u16) -> Result<()> {
    let range = checked_range(offset, 2, "u16")?;
    let Some(slice) = dst.get_mut(range) else {
        return Err(endian_out_of_bounds("u16", offset));
    };
    slice.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

#[inline]
pub fn write_u32_le(dst: &mut [u8], offset: usize, value: u32) -> Result<()> {
    let range = checked_range(offset, 4, "u32")?;
    let Some(slice) = dst.get_mut(range) else {
        return Err(endian_out_of_bounds("u32", offset));
    };
    slice.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

#[inline]
pub fn write_u64_le(dst: &mut [u8], offset: usize, value: u64) -> Result<()> {
    let range = checked_range(offset, 8, "u64")?;
    let Some(slice) = dst.get_mut(range) else {
        return Err(endian_out_of_bounds("u64", offset));
    };
    slice.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

#[inline]
pub(crate) fn try_u16_len(len: usize, what: &str) -> Result<u16> {
    u16::try_from(len).map_err(|_| length_does_not_fit(what, len))
}

#[inline]
pub(crate) fn try_u32_len(len: usize, what: &str) -> Result<u32> {
    u32::try_from(len).map_err(|_| length_does_not_fit(what, len))
}

#[inline]
pub fn compare_keys(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    a.cmp(b)
}

#[inline]
pub fn key_in_range(
    key: &[u8],
    gt: Option<&[u8]>,
    gte: Option<&[u8]>,
    lt: Option<&[u8]>,
    lte: Option<&[u8]>,
) -> bool {
    if let Some(bound) = gt {
        if key <= bound {
            return false;
        }
    }
    if let Some(bound) = gte {
        if key < bound {
            return false;
        }
    }
    if let Some(bound) = lt {
        if key >= bound {
            return false;
        }
    }
    if let Some(bound) = lte {
        if key > bound {
            return false;
        }
    }
    true
}

#[inline]
pub fn validate_store_name(name: &str) -> Result<()> {
    let len = name.len();
    if len <= MAX_STORE_NAME_BYTES {
        Ok(())
    } else {
        Err(store_name_too_long(len))
    }
}

#[inline]
pub fn validate_key(key: &[u8]) -> Result<()> {
    let len = key.len();
    if len <= MAX_KEY_BYTES {
        Ok(())
    } else {
        Err(key_too_large(len))
    }
}

#[inline]
pub fn validate_value(value: &[u8]) -> Result<()> {
    let len = value.len();
    if len <= MAX_VALUE_BYTES {
        Ok(())
    } else {
        Err(value_too_large(len))
    }
}

#[cold]
#[inline(never)]
fn store_name_too_long(len: usize) -> EngineError {
    EngineError::StoreNameTooLong(len)
}

#[cold]
#[inline(never)]
fn key_too_large(len: usize) -> EngineError {
    EngineError::KeyTooLarge(len)
}

#[cold]
#[inline(never)]
fn value_too_large(len: usize) -> EngineError {
    EngineError::ValueTooLarge(len)
}

pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

pub fn encode_db_name(name: &str) -> String {
    hex_encode(name.as_bytes())
}
