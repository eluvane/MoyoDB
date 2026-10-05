//! Commit planning borrows immutable payloads and owns only their prefixes
//! and optional chunk checksums.

#[cfg(test)]
mod tests;

use crate::checksum::paired_payload_checksums;
use crate::error::Result;
use crate::page::{max_overflow_chunk_len, should_overflow_value};
use crate::value::StoredValue;

enum Prefix {
    Envelope([u8; 16]),
    Bytes(Vec<u8>),
}

impl Prefix {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Envelope(bytes) => bytes,
            Self::Bytes(bytes) => bytes,
        }
    }
}

pub(crate) struct PreparedValue<'a> {
    prefix: Prefix,
    payload: &'a [u8],
    chunk_checksums: Vec<u32>,
}

impl<'a> PreparedValue<'a> {
    pub(crate) fn stored(value: &'a StoredValue, flags: u64) -> Result<Self> {
        let prefix = match value.encode_prefix(flags)? {
            Some(prefix) => Prefix::Envelope(prefix),
            None => Prefix::Bytes(Vec::new()),
        };
        Ok(Self {
            prefix,
            payload: &value.value,
            chunk_checksums: Vec::new(),
        })
    }

    pub(crate) fn prefixed(prefix: Vec<u8>, payload: &'a [u8]) -> Self {
        Self {
            prefix: Prefix::Bytes(prefix),
            payload,
            chunk_checksums: Vec::new(),
        }
    }

    /// Hashes both prefixed views in one payload pass only when they borrow
    /// the same slice. Checksums remain local to these immutable values.
    pub(crate) fn share_payload_checksums(&mut self, other: &mut Self) {
        #[cfg(test)]
        if work::generic_only() {
            return;
        }
        let first_prefix = self.prefix.as_slice();
        let second_prefix = other.prefix.as_slice();
        let chunk_len = max_overflow_chunk_len();
        if !std::ptr::eq(self.payload, other.payload)
            || !should_overflow_value(first_prefix.len() + self.payload.len())
            || !should_overflow_value(second_prefix.len() + other.payload.len())
            || first_prefix.len() >= chunk_len
            || second_prefix.len() >= chunk_len
        {
            return;
        }
        (self.chunk_checksums, other.chunk_checksums) =
            paired_payload_checksums(first_prefix, second_prefix, self.payload);
    }
}

/// Lets tree algorithms borrow slices or prepared values without copying
/// values into a common representation.
pub(crate) trait ValueSource {
    fn parts(&self) -> ValueParts<'_>;
}

impl ValueSource for [u8] {
    fn parts(&self) -> ValueParts<'_> {
        ValueParts {
            prefix: &[],
            payload: self,
            chunk_checksums: &[],
        }
    }
}

impl ValueSource for PreparedValue<'_> {
    fn parts(&self) -> ValueParts<'_> {
        ValueParts {
            prefix: self.prefix.as_slice(),
            payload: self.payload,
            chunk_checksums: &self.chunk_checksums,
        }
    }
}

pub(crate) struct ValueParts<'a> {
    prefix: &'a [u8],
    payload: &'a [u8],
    chunk_checksums: &'a [u32],
}

impl<'a> ValueParts<'a> {
    pub(crate) fn len(&self) -> usize {
        self.prefix.len() + self.payload.len()
    }

    pub(crate) fn to_vec(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.len());
        bytes.extend_from_slice(self.prefix);
        bytes.extend_from_slice(self.payload);
        bytes
    }

    pub(crate) fn chunk(&self, index: usize) -> (&'a [u8], &'a [u8], Option<u32>) {
        let start = index * max_overflow_chunk_len();
        let end = (start + max_overflow_chunk_len()).min(self.len());
        let prefix = &self.prefix[start.min(self.prefix.len())..end.min(self.prefix.len())];
        let payload = &self.payload
            [start.saturating_sub(self.prefix.len())..end.saturating_sub(self.prefix.len())];
        let checksum = self.chunk_checksums.get(index).copied();
        (prefix, payload, checksum)
    }
}

#[cfg(test)]
pub(crate) mod work {
    use std::cell::Cell;

    thread_local! {
        static GENERIC_ONLY: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) fn generic_only() -> bool {
        GENERIC_ONLY.with(Cell::get)
    }

    pub(crate) fn with_generic_encoding<T>(f: impl FnOnce() -> T) -> T {
        struct Reset(bool);
        impl Drop for Reset {
            fn drop(&mut self) {
                GENERIC_ONLY.with(|flag| flag.set(self.0));
            }
        }
        let previous = GENERIC_ONLY.with(|flag| flag.replace(true));
        let _reset = Reset(previous);
        f()
    }
}
