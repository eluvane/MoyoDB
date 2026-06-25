use crate::error::{EngineError, Result};

pub const MAX_STORE_NAME_BYTES: usize = 255;
pub const MAX_KEY_BYTES: usize = 1024;
pub const MAX_VALUE_BYTES: usize = 8 * 1024 * 1024;

fn checked_end(offset: usize, width: usize, what: &str) -> Result<usize> {
    offset
        .checked_add(width)
        .ok_or_else(|| EngineError::Serialization(format!("{what} offset overflow at {offset}")))
}

pub fn read_u16_le(bytes: &[u8], offset: usize) -> Result<u16> {
    let end = checked_end(offset, 2, "u16")?;
    let slice = bytes
        .get(offset..end)
        .ok_or_else(|| EngineError::Serialization(format!("u16 out of bounds at {offset}")))?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

pub fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32> {
    let end = checked_end(offset, 4, "u32")?;
    let slice = bytes
        .get(offset..end)
        .ok_or_else(|| EngineError::Serialization(format!("u32 out of bounds at {offset}")))?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

pub fn read_u64_le(bytes: &[u8], offset: usize) -> Result<u64> {
    let end = checked_end(offset, 8, "u64")?;
    let slice = bytes
        .get(offset..end)
        .ok_or_else(|| EngineError::Serialization(format!("u64 out of bounds at {offset}")))?;
    Ok(u64::from_le_bytes([
        slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
    ]))
}

pub fn write_u16_le(dst: &mut [u8], offset: usize, value: u16) -> Result<()> {
    let end = checked_end(offset, 2, "u16")?;
    let slice = dst
        .get_mut(offset..end)
        .ok_or_else(|| EngineError::Serialization(format!("u16 out of bounds at {offset}")))?;
    slice.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

pub fn write_u32_le(dst: &mut [u8], offset: usize, value: u32) -> Result<()> {
    let end = checked_end(offset, 4, "u32")?;
    let slice = dst
        .get_mut(offset..end)
        .ok_or_else(|| EngineError::Serialization(format!("u32 out of bounds at {offset}")))?;
    slice.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

pub fn write_u64_le(dst: &mut [u8], offset: usize, value: u64) -> Result<()> {
    let end = checked_end(offset, 8, "u64")?;
    let slice = dst
        .get_mut(offset..end)
        .ok_or_else(|| EngineError::Serialization(format!("u64 out of bounds at {offset}")))?;
    slice.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

pub fn compare_keys(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    a.cmp(b)
}

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

pub fn validate_store_name(name: &str) -> Result<()> {
    let len = name.len();
    if len > MAX_STORE_NAME_BYTES {
        return Err(EngineError::StoreNameTooLong(len));
    }
    Ok(())
}

pub fn validate_key(key: &[u8]) -> Result<()> {
    if key.len() > MAX_KEY_BYTES {
        return Err(EngineError::KeyTooLarge(key.len()));
    }
    Ok(())
}

pub fn validate_value(value: &[u8]) -> Result<()> {
    if value.len() > MAX_VALUE_BYTES {
        return Err(EngineError::ValueTooLarge(value.len()));
    }
    Ok(())
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
