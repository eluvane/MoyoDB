use crate::bytes::{read_u64_le, validate_value};
use crate::error::{EngineError, Result};
use serde::{Deserialize, Serialize};

pub const STORE_FLAG_VALUE_ENVELOPE_V1: u64 = 1 << 0;
pub const STORE_FLAG_SYSTEM_RAW_VALUES: u64 = 1 << 1;
pub const STORE_FLAG_COMPRESSION_SHIFT: u64 = 2;
pub const STORE_FLAG_COMPRESSION_MASK: u64 = 0b11 << STORE_FLAG_COMPRESSION_SHIFT;
pub const VALUE_ENVELOPE_MAGIC: [u8; 8] = *b"BDTTL001";
pub const VALUE_ENVELOPE_HEADER_SIZE: usize = 16;

#[repr(u8)]
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StoreCompression {
    #[default]
    None = 0,
    Gzip = 1,
    Deflate = 2,
}

impl StoreCompression {
    pub fn from_bits(bits: u64) -> Result<Self> {
        match bits {
            0 => Ok(Self::None),
            1 => Ok(Self::Gzip),
            2 => Ok(Self::Deflate),
            other => Err(EngineError::Corruption(format!(
                "invalid store compression bits: {other}"
            ))),
        }
    }

    pub fn bits(self) -> u64 {
        self as u64
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredValue {
    pub value: Vec<u8>,
    pub expires_at_ms: Option<u64>,
}

impl StoredValue {
    pub fn plain(value: Vec<u8>) -> Self {
        Self {
            value,
            expires_at_ms: None,
        }
    }

    pub fn with_expiry(value: Vec<u8>, expires_at_ms: Option<u64>) -> Self {
        Self {
            value,
            expires_at_ms,
        }
    }

    pub fn is_expired_at(&self, now_ms: u64) -> bool {
        matches!(self.expires_at_ms, Some(expires_at_ms) if now_ms >= expires_at_ms)
    }

    pub fn encode_for_store(&self, store_flags: u64) -> Result<Vec<u8>> {
        if store_uses_system_raw_values(store_flags) {
            if self.expires_at_ms.is_some() {
                return Err(EngineError::Internal(
                    "attempted to store ttl value in a system raw-value store".into(),
                ));
            }
            return Ok(self.value.clone());
        }

        if store_uses_value_envelope(store_flags) {
            let mut out = Vec::with_capacity(VALUE_ENVELOPE_HEADER_SIZE + self.value.len());
            out.extend_from_slice(&VALUE_ENVELOPE_MAGIC);
            out.extend_from_slice(&self.expires_at_ms.unwrap_or(0).to_le_bytes());
            out.extend_from_slice(&self.value);
            Ok(out)
        } else if self.expires_at_ms.is_some() {
            Err(EngineError::Internal(
                "attempted to store ttl value in a legacy raw-value store".into(),
            ))
        } else {
            Ok(self.value.clone())
        }
    }

    pub fn decode_for_store(store_flags: u64, bytes: &[u8]) -> Result<Self> {
        if store_uses_system_raw_values(store_flags) {
            return Ok(Self::plain(bytes.to_vec()));
        }

        if !store_uses_value_envelope(store_flags) {
            validate_value(bytes)?;
            return Ok(Self::plain(bytes.to_vec()));
        }

        if bytes.len() < VALUE_ENVELOPE_HEADER_SIZE {
            return Err(EngineError::Corruption(format!(
                "value envelope too short: expected at least {VALUE_ENVELOPE_HEADER_SIZE} bytes, got {}",
                bytes.len()
            )));
        }
        if bytes[..8] != VALUE_ENVELOPE_MAGIC {
            return Err(EngineError::Corruption(
                "value envelope magic mismatch".into(),
            ));
        }

        let expires_at_ms = read_u64_le(bytes, 8)?;
        let value = bytes[VALUE_ENVELOPE_HEADER_SIZE..].to_vec();
        validate_value(&value)?;
        Ok(Self {
            value,
            expires_at_ms: if expires_at_ms == 0 {
                None
            } else {
                Some(expires_at_ms)
            },
        })
    }
}

pub fn store_flags_for_user_store(compression: StoreCompression) -> u64 {
    STORE_FLAG_VALUE_ENVELOPE_V1 | (compression.bits() << STORE_FLAG_COMPRESSION_SHIFT)
}

pub fn store_compression_from_flags(flags: u64) -> Result<StoreCompression> {
    StoreCompression::from_bits(
        (flags & STORE_FLAG_COMPRESSION_MASK) >> STORE_FLAG_COMPRESSION_SHIFT,
    )
}

pub fn store_uses_value_envelope(flags: u64) -> bool {
    flags & STORE_FLAG_VALUE_ENVELOPE_V1 != 0
}

pub fn store_uses_system_raw_values(flags: u64) -> bool {
    flags & STORE_FLAG_SYSTEM_RAW_VALUES != 0
}
