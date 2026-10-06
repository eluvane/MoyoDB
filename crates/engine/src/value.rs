use crate::bytes::{read_u32_le, read_u64_le, MAX_VALUE_BYTES};
use crate::error::{EngineError, Result};
use serde::{Deserialize, Serialize};

#[cfg(test)]
#[path = "value_revision_tests.rs"]
mod revision_tests;

pub const STORE_FLAG_VALUE_ENVELOPE_V1: u64 = 1 << 0;
pub const STORE_FLAG_SYSTEM_RAW_VALUES: u64 = 1 << 1;
pub const STORE_FLAG_VALUE_REVISION: u64 = 1 << 4;
pub const STORE_FLAG_COMPRESSION_SHIFT: u64 = 2;
pub const STORE_FLAG_COMPRESSION_MASK: u64 = 0b11 << STORE_FLAG_COMPRESSION_SHIFT;
pub const VALUE_ENVELOPE_MAGIC: [u8; 8] = *b"BDTTL001";
pub const VALUE_ENVELOPE_HEADER_SIZE: usize = 16;
pub const VALUE_REVISION_ENVELOPE_MAGIC: [u8; 8] = *b"BDREV001";
pub const VALUE_REVISION_ENVELOPE_HEADER_SIZE: usize = 32;
pub const COMPRESSION_VALUE_MAGIC: [u8; 8] = *b"BDBZVAL1";
pub const COMPRESSION_VALUE_HEADER_SIZE: usize = 18;

#[repr(u8)]
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StoreCompression {
    #[default]
    None = 0,
    Gzip = 1,
    Deflate = 2,
    Snappy = 3,
}

impl StoreCompression {
    pub fn from_bits(bits: u64) -> Result<Self> {
        match bits {
            0 => Ok(Self::None),
            1 => Ok(Self::Gzip),
            2 => Ok(Self::Deflate),
            3 => Ok(Self::Snappy),
            other => Err(EngineError::Corruption(format!(
                "invalid store compression bits: {other}"
            ))),
        }
    }

    pub fn bits(self) -> u64 {
        self as u64
    }
}

#[cfg(test)]
mod compression_tests {
    use super::{store_compression_from_flags, store_flags_for_user_store, StoreCompression};

    #[test]
    fn compression_tags_keep_old_flags_and_use_the_free_tag() {
        for (compression, bits, name) in [
            (StoreCompression::None, 0, "none"),
            (StoreCompression::Gzip, 1, "gzip"),
            (StoreCompression::Deflate, 2, "deflate"),
            (StoreCompression::Snappy, 3, "snappy"),
        ] {
            assert_eq!(compression.bits(), bits);
            assert!(
                matches!(StoreCompression::from_bits(bits), Ok(actual) if actual == compression)
            );
            let flags = store_flags_for_user_store(compression);
            assert_eq!(flags, 17 | (bits << 2));
            assert!(
                matches!(store_compression_from_flags(flags), Ok(actual) if actual == compression)
            );
            assert!(
                matches!(serde_json::to_string(&compression), Ok(actual) if actual == format!("\"{name}\""))
            );
        }
        assert!(StoreCompression::from_bits(4).is_err());
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValueRevision {
    pub epoch: u64,
    pub ordinal: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValueState {
    pub exists: bool,
    pub expired: bool,
    pub expires_at_ms: Option<u64>,
    pub revision: Option<ValueRevision>,
    pub max_length: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredValue {
    pub value: Vec<u8>,
    pub expires_at_ms: Option<u64>,
    #[serde(default)]
    pub revision: Option<ValueRevision>,
}

impl StoredValue {
    pub fn plain(value: Vec<u8>) -> Self {
        Self {
            value,
            expires_at_ms: None,
            revision: None,
        }
    }

    pub fn with_expiry(value: Vec<u8>, expires_at_ms: Option<u64>) -> Self {
        Self {
            value,
            expires_at_ms,
            revision: None,
        }
    }

    pub(crate) fn with_revision(mut self, revision: ValueRevision) -> Self {
        self.revision = Some(revision);
        self
    }

    pub fn is_expired_at(&self, now_ms: u64) -> bool {
        matches!(self.expires_at_ms, Some(expires_at_ms) if now_ms >= expires_at_ms)
    }

    pub fn encode_for_store(&self, store_flags: u64) -> Result<Vec<u8>> {
        match self.encode_prefix(store_flags)? {
            Some((prefix, length)) => {
                let mut out = Vec::with_capacity(length + self.value.len());
                out.extend_from_slice(&prefix[..length]);
                out.extend_from_slice(&self.value);
                Ok(out)
            }
            None => Ok(self.value.clone()),
        }
    }

    pub(crate) fn encode_prefix(
        &self,
        store_flags: u64,
    ) -> Result<Option<([u8; VALUE_REVISION_ENVELOPE_HEADER_SIZE], usize)>> {
        if store_uses_system_raw_values(store_flags) {
            if self.expires_at_ms.is_some() {
                return Err(EngineError::Internal(
                    "attempted to store ttl value in a system raw-value store".into(),
                ));
            }
            return Ok(None);
        }

        if store_uses_value_envelope(store_flags) {
            // Zero in the header means no expiry. Some(0) is already expired and
            // cannot share that sentinel without becoming permanent on read.
            if self.expires_at_ms == Some(0) {
                return Err(EngineError::Serialization(
                    "expiry at unix epoch cannot be stored in the value envelope".into(),
                ));
            }
            let mut prefix = [0; VALUE_REVISION_ENVELOPE_HEADER_SIZE];
            let length = store_value_prefix_len(store_flags);
            let magic = if store_flags & STORE_FLAG_VALUE_REVISION != 0 {
                VALUE_REVISION_ENVELOPE_MAGIC
            } else {
                VALUE_ENVELOPE_MAGIC
            };
            prefix[..8].copy_from_slice(&magic);
            prefix[8..16].copy_from_slice(&self.expires_at_ms.unwrap_or(0).to_le_bytes());
            if let Some(revision) = self.revision.as_ref().filter(|_| length == 32) {
                if revision.epoch == 0 || revision.ordinal == 0 {
                    return Err(EngineError::Internal("value revision contains zero".into()));
                }
                prefix[16..24].copy_from_slice(&revision.epoch.to_le_bytes());
                prefix[24..32].copy_from_slice(&revision.ordinal.to_le_bytes());
            }
            Ok(Some((prefix, length)))
        } else if self.expires_at_ms.is_some() {
            Err(EngineError::Internal(
                "attempted to store ttl value in a legacy raw-value store".into(),
            ))
        } else {
            Ok(None)
        }
    }

    /// Decodes as [`Self::decode_for_store`] and reuses the input buffer.
    pub fn decode_owned_for_store(store_flags: u64, mut bytes: Vec<u8>) -> Result<Self> {
        if store_uses_system_raw_values(store_flags) {
            return Ok(Self::plain(bytes));
        }
        if !store_uses_value_envelope(store_flags) {
            validate_decoded_value(&bytes, store_flags)?;
            return Ok(Self::plain(bytes));
        }
        let (expires_at_ms, revision) = decode_value_envelope(store_flags, &bytes)?;
        bytes.drain(..store_value_prefix_len(store_flags));
        validate_decoded_value(&bytes, store_flags)?;
        Ok(Self {
            value: bytes,
            expires_at_ms,
            revision,
        })
    }

    pub fn decode_for_store(store_flags: u64, bytes: &[u8]) -> Result<Self> {
        if store_uses_system_raw_values(store_flags) {
            return Ok(Self::plain(bytes.to_vec()));
        }

        if !store_uses_value_envelope(store_flags) {
            validate_decoded_value(bytes, store_flags)?;
            return Ok(Self::plain(bytes.to_vec()));
        }

        let header_size = store_value_prefix_len(store_flags);
        if bytes.len() < header_size {
            return Err(EngineError::Corruption(format!(
                "value envelope too short: expected at least {header_size} bytes, got {}",
                bytes.len()
            )));
        }
        let (expires_at_ms, revision) = decode_value_envelope(store_flags, bytes)?;
        let value = &bytes[header_size..];
        validate_decoded_value(value, store_flags)?;
        Ok(Self {
            value: value.to_vec(),
            expires_at_ms,
            revision,
        })
    }
}

pub fn store_value_prefix_len(flags: u64) -> usize {
    if flags & STORE_FLAG_VALUE_REVISION != 0 {
        VALUE_REVISION_ENVELOPE_HEADER_SIZE
    } else if store_uses_value_envelope(flags) {
        VALUE_ENVELOPE_HEADER_SIZE
    } else {
        0
    }
}

pub fn decode_value_envelope(
    flags: u64,
    prefix: &[u8],
) -> Result<(Option<u64>, Option<ValueRevision>)> {
    if flags & STORE_FLAG_VALUE_REVISION == 0 {
        return decode_envelope_expiry(prefix).map(|expiry| (expiry, None));
    }
    if prefix.len() < VALUE_REVISION_ENVELOPE_HEADER_SIZE
        || prefix[..8] != VALUE_REVISION_ENVELOPE_MAGIC
    {
        return Err(EngineError::Corruption(
            "revision envelope is invalid or truncated".into(),
        ));
    }
    let expiry = read_u64_le(prefix, 8)?;
    let epoch = read_u64_le(prefix, 16)?;
    let ordinal = read_u64_le(prefix, 24)?;
    if (epoch == 0) != (ordinal == 0) {
        return Err(EngineError::Corruption(
            "revision envelope contains a partial revision".into(),
        ));
    }
    Ok((
        (expiry != 0).then_some(expiry),
        (epoch != 0).then_some(ValueRevision { epoch, ordinal }),
    ))
}

/// Reads expiry from the envelope header. Zero means no expiry.
#[inline]
pub fn decode_envelope_expiry(prefix: &[u8]) -> Result<Option<u64>> {
    if prefix.len() < VALUE_ENVELOPE_HEADER_SIZE {
        return Err(EngineError::Corruption(format!(
            "value envelope too short: expected at least {VALUE_ENVELOPE_HEADER_SIZE} bytes, got {}",
            prefix.len()
        )));
    }
    if prefix[..8] != VALUE_ENVELOPE_MAGIC {
        return Err(EngineError::Corruption(
            "value envelope magic mismatch".into(),
        ));
    }
    let expires_at_ms = read_u64_le(prefix, 8)?;
    Ok(if expires_at_ms == 0 {
        None
    } else {
        Some(expires_at_ms)
    })
}

/// Checks expiry from the envelope header. Raw-value stores do not expire.
pub fn stored_value_expired(store_flags: u64, raw: &[u8], now_ms: u64) -> Result<bool> {
    if store_uses_system_raw_values(store_flags) || !store_uses_value_envelope(store_flags) {
        return Ok(false);
    }
    Ok(
        matches!(decode_value_envelope(store_flags, raw)?.0, Some(expires_at_ms) if now_ms >= expires_at_ms),
    )
}

pub fn store_flags_for_user_store(compression: StoreCompression) -> u64 {
    STORE_FLAG_VALUE_ENVELOPE_V1
        | STORE_FLAG_VALUE_REVISION
        | (compression.bits() << STORE_FLAG_COMPRESSION_SHIFT)
}

pub fn store_compression_from_flags(flags: u64) -> Result<StoreCompression> {
    StoreCompression::from_bits(
        (flags & STORE_FLAG_COMPRESSION_MASK) >> STORE_FLAG_COMPRESSION_SHIFT,
    )
}

pub fn validate_store_flags(flags: u64) -> Result<StoreCompression> {
    let known_flags = STORE_FLAG_VALUE_ENVELOPE_V1
        | STORE_FLAG_SYSTEM_RAW_VALUES
        | STORE_FLAG_COMPRESSION_MASK
        | STORE_FLAG_VALUE_REVISION;
    let compression = store_compression_from_flags(flags)?;
    if flags & !known_flags != 0
        || (store_uses_system_raw_values(flags)
            && (store_uses_value_envelope(flags) || compression != StoreCompression::None))
        || (compression != StoreCompression::None && flags & STORE_FLAG_VALUE_ENVELOPE_V1 == 0)
        || (flags & STORE_FLAG_VALUE_REVISION != 0 && store_uses_system_raw_values(flags))
    {
        return Err(EngineError::Corruption(format!(
            "invalid store value flags: {flags:#x}"
        )));
    }
    Ok(compression)
}

pub fn max_store_value_bytes(flags: u64) -> Result<usize> {
    Ok(if validate_store_flags(flags)? == StoreCompression::None {
        MAX_VALUE_BYTES
    } else {
        MAX_VALUE_BYTES + COMPRESSION_VALUE_HEADER_SIZE
    })
}

pub fn validate_store_value_max_size(value: &[u8]) -> Result<()> {
    if value.len() > MAX_VALUE_BYTES + COMPRESSION_VALUE_HEADER_SIZE {
        return Err(EngineError::ValueTooLarge(value.len()));
    }
    Ok(())
}

// The SDK checks payload CRC and decompression. Native reads bound the frame.
pub fn validate_store_value_prefix(prefix: &[u8], value_len: usize, flags: u64) -> Result<()> {
    let compression = validate_store_flags(flags)?;
    let maximum = MAX_VALUE_BYTES
        + if compression == StoreCompression::None {
            0
        } else {
            COMPRESSION_VALUE_HEADER_SIZE
        };
    if value_len > maximum {
        return Err(EngineError::ValueTooLarge(value_len));
    }
    if compression == StoreCompression::None {
        return Ok(());
    }
    if value_len < COMPRESSION_VALUE_HEADER_SIZE || prefix.len() < COMPRESSION_VALUE_HEADER_SIZE {
        return Err(EngineError::Corruption(
            "compression value header is truncated".into(),
        ));
    }
    if prefix[..8] != COMPRESSION_VALUE_MAGIC || prefix[8] != 1 {
        return Err(EngineError::Corruption(
            "compression value header is invalid".into(),
        ));
    }
    let tag = prefix[9];
    if tag != 0 && u64::from(tag) != compression.bits() {
        return Err(EngineError::Corruption(format!(
            "compression value tag does not match its store: {tag}"
        )));
    }
    let raw_len = read_u32_le(prefix, 10)? as usize;
    if raw_len > MAX_VALUE_BYTES {
        return Err(EngineError::ValueTooLarge(raw_len));
    }
    if tag == 0 && value_len - COMPRESSION_VALUE_HEADER_SIZE != raw_len {
        return Err(EngineError::Corruption(
            "uncompressed value length mismatch".into(),
        ));
    }
    Ok(())
}

pub fn validate_store_value(value: &[u8], flags: u64) -> Result<()> {
    validate_store_value_prefix(value, value.len(), flags)
}

fn validate_decoded_value(value: &[u8], flags: u64) -> Result<()> {
    let result = validate_store_value(value, flags);
    // Raw StoredValue callers retain their existing size error.
    if flags & STORE_FLAG_COMPRESSION_MASK == 0 {
        return result;
    }
    result.map_err(|error| match error {
        EngineError::Corruption(_) => error,
        other => EngineError::Corruption(other.to_string()),
    })
}

pub fn store_uses_value_envelope(flags: u64) -> bool {
    flags & (STORE_FLAG_VALUE_ENVELOPE_V1 | STORE_FLAG_VALUE_REVISION) != 0
}

pub fn store_uses_system_raw_values(flags: u64) -> bool {
    flags & STORE_FLAG_SYSTEM_RAW_VALUES != 0
}
