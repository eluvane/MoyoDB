use crate::bytes::{read_u32_le, read_u64_le, MAX_STORED_VALUE_BYTES};
use crate::checksum::{checksum_with_zeroed_region, crc32};
use crate::error::{EngineError, Result};
use crate::layout::PAGE_SIZE;
use crate::storage::backend::FileBackend;
use crc32fast::Hasher;
use serde::{Deserialize, Serialize};

pub const PAYLOAD_HEADER_SIZE: usize = 32;
pub const PAYLOAD_DESCRIPTOR_SIZE: usize = 8;
pub const PAYLOAD_READ_CHUNK_BYTES: usize = 256 * 1024;
const PAYLOAD_MAGIC: [u8; 4] = *b"PAY2";
const HEADER_CHECKSUM_OFFSET: usize = 24;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct PayloadRef {
    pub first_page_id: u64,
    pub body_len: u32,
    pub checksum: u32,
}

impl PayloadRef {
    pub fn page_count(&self) -> Result<u64> {
        payload_page_count(self.body_len)
    }

    pub fn end_page_id(&self) -> Result<u64> {
        self.first_page_id
            .checked_add(self.page_count()?)
            .ok_or_else(|| EngineError::Corruption("external payload page range overflow".into()))
    }

    fn range(&self) -> Result<(u64, u64)> {
        let start = self
            .first_page_id
            .checked_sub(1)
            .and_then(|page| page.checked_mul(PAGE_SIZE as u64))
            .ok_or_else(|| EngineError::Corruption("invalid external payload page id".into()))?;
        let end = self
            .end_page_id()?
            .checked_sub(1)
            .and_then(|page| page.checked_mul(PAGE_SIZE as u64))
            .ok_or_else(|| {
                EngineError::Corruption("external payload byte range overflow".into())
            })?;
        Ok((start, end))
    }
}

pub fn payload_page_count(body_len: u32) -> Result<u64> {
    if body_len == 0 || body_len as usize > MAX_STORED_VALUE_BYTES {
        return Err(EngineError::Corruption(
            "invalid external payload length".into(),
        ));
    }
    Ok((PAYLOAD_HEADER_SIZE as u64 + u64::from(body_len)).div_ceil(PAGE_SIZE as u64))
}

pub fn encode_payload_descriptor(reference: &PayloadRef, prefix: &[u8]) -> Result<Vec<u8>> {
    reference.range()?;
    logical_len(reference, prefix.len())?;
    let capacity = PAYLOAD_DESCRIPTOR_SIZE
        .checked_add(prefix.len())
        .ok_or_else(|| EngineError::Serialization("external descriptor length overflow".into()))?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(capacity).map_err(|error| {
        EngineError::Storage(format!("external descriptor allocation failed: {error}"))
    })?;
    bytes.extend_from_slice(&reference.body_len.to_le_bytes());
    bytes.extend_from_slice(&reference.checksum.to_le_bytes());
    bytes.extend_from_slice(prefix);
    Ok(bytes)
}

pub fn decode_payload_descriptor(
    first_page_id: u64,
    total_value_len: u32,
    bytes: &[u8],
) -> Result<(PayloadRef, &[u8])> {
    if bytes.len() < PAYLOAD_DESCRIPTOR_SIZE {
        return Err(EngineError::Corruption(
            "short external payload descriptor".into(),
        ));
    }
    let reference = PayloadRef {
        first_page_id,
        body_len: read_u32_le(bytes, 0)?,
        checksum: read_u32_le(bytes, 4)?,
    };
    reference.range()?;
    let prefix = &bytes[PAYLOAD_DESCRIPTOR_SIZE..];
    if logical_len(&reference, prefix.len())? != total_value_len {
        return Err(EngineError::Corruption(
            "external payload logical length mismatch".into(),
        ));
    }
    Ok((reference, prefix))
}

fn logical_len(reference: &PayloadRef, prefix_len: usize) -> Result<u32> {
    let total = (reference.body_len as usize)
        .checked_add(prefix_len)
        .filter(|total| *total <= MAX_STORED_VALUE_BYTES)
        .ok_or_else(|| {
            EngineError::Corruption("external payload logical length overflow".into())
        })?;
    u32::try_from(total)
        .map_err(|_| EngineError::Corruption("external payload logical length overflow".into()))
}

/// The caller reserves this contiguous extent and must not overwrite live bodies.
/// The body is durable before the returned reference can enter the WAL.
pub fn write_payload<B: FileBackend>(
    backend: &mut B,
    first_page_id: u64,
    body: &[u8],
) -> Result<PayloadRef> {
    let reference = write_payload_unflushed(backend, first_page_id, body)?;
    backend.flush()?;
    Ok(reference)
}

// The caller must flush the main file before publishing any returned reference.
pub(crate) fn write_payload_unflushed<B: FileBackend>(
    backend: &mut B,
    first_page_id: u64,
    body: &[u8],
) -> Result<PayloadRef> {
    if body.is_empty() || body.len() > MAX_STORED_VALUE_BYTES {
        return Err(EngineError::ValueTooLarge(body.len()));
    }
    let reference = PayloadRef {
        first_page_id,
        body_len: body.len() as u32,
        checksum: crc32(body),
    };
    let (start, end) = reference.range()?;
    let mut header = [0u8; PAYLOAD_HEADER_SIZE];
    header[..4].copy_from_slice(&PAYLOAD_MAGIC);
    header[4..8].copy_from_slice(&(PAYLOAD_HEADER_SIZE as u32).to_le_bytes());
    header[8..12].copy_from_slice(&reference.body_len.to_le_bytes());
    header[12..16].copy_from_slice(&reference.checksum.to_le_bytes());
    header[16..24].copy_from_slice(&reference.first_page_id.to_le_bytes());
    let checksum = checksum_with_zeroed_region(&header, HEADER_CHECKSUM_OFFSET, 4);
    header[HEADER_CHECKSUM_OFFSET..HEADER_CHECKSUM_OFFSET + 4]
        .copy_from_slice(&checksum.to_le_bytes());
    backend.write_at(start, &header)?;
    let body_start = start + PAYLOAD_HEADER_SIZE as u64;
    backend.write_at(body_start, body)?;
    let body_end = body_start + u64::from(reference.body_len);
    let padding = (end - body_end) as usize;
    if padding > 0 {
        backend.write_at(body_end, &[0u8; PAGE_SIZE][..padding])?;
    }
    Ok(reference)
}

fn verified_body_start<B: FileBackend>(backend: &B, reference: &PayloadRef) -> Result<u64> {
    let (start, end) = reference.range()?;
    if backend.len()? < end {
        return Err(EngineError::Corruption(
            "external payload extent is truncated".into(),
        ));
    }
    let header = backend.read_at(start, PAYLOAD_HEADER_SIZE)?;
    if header.len() != PAYLOAD_HEADER_SIZE {
        return Err(EngineError::Corruption(
            "external payload header is truncated".into(),
        ));
    }
    if header[..4] != PAYLOAD_MAGIC
        || read_u32_le(&header, 4)? != PAYLOAD_HEADER_SIZE as u32
        || read_u32_le(&header, 28)? != 0
    {
        return Err(EngineError::Corruption(
            "invalid external payload header".into(),
        ));
    }
    let checksum = checksum_with_zeroed_region(&header, HEADER_CHECKSUM_OFFSET, 4);
    if read_u32_le(&header, HEADER_CHECKSUM_OFFSET)? != checksum {
        return Err(EngineError::Corruption(
            "external payload header checksum mismatch".into(),
        ));
    }
    if read_u64_le(&header, 16)? != reference.first_page_id
        || read_u32_le(&header, 8)? != reference.body_len
        || read_u32_le(&header, 12)? != reference.checksum
    {
        return Err(EngineError::Corruption(
            "external payload reference does not match its header".into(),
        ));
    }
    Ok(start + PAYLOAD_HEADER_SIZE as u64)
}

/// The writer may receive bytes before the final checksum check. Discard output on error.
pub fn read_payload_into<B: FileBackend>(
    backend: &B,
    reference: &PayloadRef,
    mut writer: impl FnMut(&[u8]) -> Result<()>,
) -> Result<()> {
    let start = verified_body_start(backend, reference)?;
    let mut offset = 0usize;
    let mut hasher = Hasher::new();
    while offset < reference.body_len as usize {
        let count = PAYLOAD_READ_CHUNK_BYTES.min(reference.body_len as usize - offset);
        let bytes = backend.read_at(start + offset as u64, count)?;
        if bytes.len() != count {
            return Err(EngineError::Corruption(
                "external payload body is truncated".into(),
            ));
        }
        hasher.update(&bytes);
        #[cfg(test)]
        crate::checksum::work::record(bytes.len());
        writer(&bytes)?;
        offset += count;
    }
    if hasher.finalize() != reference.checksum {
        return Err(EngineError::Corruption(
            "external payload body checksum mismatch".into(),
        ));
    }
    Ok(())
}

pub fn read_payload<B: FileBackend>(backend: &B, reference: &PayloadRef) -> Result<Vec<u8>> {
    reference.range()?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(reference.body_len as usize)
        .map_err(|error| {
            EngineError::Storage(format!("external payload allocation failed: {error}"))
        })?;
    read_payload_into(backend, reference, |chunk| {
        bytes.extend_from_slice(chunk);
        Ok(())
    })?;
    Ok(bytes)
}

/// Partial reads validate the header and extent bounds. Full reads also validate the body.
pub fn read_payload_prefix<B: FileBackend>(
    backend: &B,
    reference: &PayloadRef,
    wanted: usize,
) -> Result<Vec<u8>> {
    if wanted >= reference.body_len as usize {
        return read_payload(backend, reference);
    }
    read_payload_metadata_prefix(backend, reference, wanted)
}

/// Metadata reads validate the header and bounds without hashing the body.
pub fn read_payload_metadata_prefix<B: FileBackend>(
    backend: &B,
    reference: &PayloadRef,
    wanted: usize,
) -> Result<Vec<u8>> {
    let wanted = wanted.min(reference.body_len as usize);
    let start = verified_body_start(backend, reference)?;
    let mut prefix = Vec::new();
    prefix.try_reserve_exact(wanted).map_err(|error| {
        EngineError::Storage(format!("external prefix allocation failed: {error}"))
    })?;
    while prefix.len() < wanted {
        let count = PAYLOAD_READ_CHUNK_BYTES.min(wanted - prefix.len());
        let bytes = backend.read_at(start + prefix.len() as u64, count)?;
        if bytes.len() != count {
            return Err(EngineError::Corruption(
                "external payload prefix is truncated".into(),
            ));
        }
        prefix.extend_from_slice(&bytes);
    }
    Ok(prefix)
}

#[cfg(test)]
#[path = "payload_tests.rs"]
mod tests;
