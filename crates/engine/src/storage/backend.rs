use crate::error::{EngineError, Result};

pub trait FileBackend: Send {
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>>;

    /// Fills `buf` with the same bytes as `read_at(offset, buf.len())`.
    ///
    /// Holes and bytes past the end of the file are zeroes. A short or long
    /// owned read is a storage error and leaves `buf` unchanged. The default
    /// copies that owned read. Override it when the bytes are already in hand
    /// so the caller does not pay for a second buffer.
    fn read_at_into(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let bytes = ensure_exact_len(self.read_at(offset, buf.len())?, buf.len(), "read")?;
        buf.copy_from_slice(&bytes);
        Ok(())
    }

    /// Replaces `buf` with `len` bytes at `offset`, keeping spare capacity.
    ///
    /// The bytes and errors match `read_at`. On error the previous contents
    /// stay. The default copies the owned read after `buf` can hold `len`
    /// bytes. Override it to copy straight into `buf` when the caller already
    /// reserved that space.
    fn read_at_into_vec(&self, offset: u64, len: usize, buf: &mut Vec<u8>) -> Result<()> {
        let bytes = ensure_exact_len(self.read_at(offset, len)?, len, "read")?;
        ensure_vec_capacity(buf, len, "read")?;
        buf.clear();
        buf.extend_from_slice(&bytes);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()>;
    fn flush(&mut self) -> Result<()>;
    fn len(&self) -> Result<u64>;
    /// Returns the current end of file for the next append.
    ///
    /// Cached sizes require exclusive file access. Refresh the size after any
    /// operation that leaves it uncertain. This does not reserve space or
    /// synchronize writers.
    fn append_offset(&self) -> Result<u64> {
        self.len()
    }
    fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }
    fn truncate(&mut self, size: u64) -> Result<()>;
    fn close(&mut self) -> Result<()>;
    fn durable_snapshot(&self) -> Option<Vec<u8>> {
        None
    }
}

pub struct FileSet<B: FileBackend> {
    pub manifest: B,
    pub main: B,
    pub wal: B,
}

impl<B: FileBackend> FileSet<B> {
    pub fn new(manifest: B, main: B, wal: B) -> Self {
        Self {
            manifest,
            main,
            wal,
        }
    }
}

/// Database names are valid Unicode and at most 127 UTF-8 bytes.
/// Native files and OPFS both derive `stackdb/hex(UTF-8 name)` from that rule.
pub(crate) fn validate_database_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 127 {
        return Err(EngineError::Storage(
            "database name must contain 1 to 127 UTF-8 bytes".into(),
        ));
    }
    Ok(())
}

/// Accepts UTF-16 code units from a JavaScript string before any lossy UTF-8
/// conversion. Unpaired surrogates are not valid Unicode; replacing them would
/// make distinct names encode to the same database path.
// Native builds reach this through tests. The WASM bindings are the production caller.
#[cfg_attr(not(any(test, target_arch = "wasm32")), allow(dead_code))]
pub(crate) fn database_name_from_utf16(units: &[u16]) -> Result<String> {
    let name = String::from_utf16(units)
        .map_err(|_| EngineError::Storage("database name must contain valid Unicode".into()))?;
    validate_database_name(&name)?;
    Ok(name)
}

pub fn ensure_exact_len(bytes: Vec<u8>, expected_len: usize, what: &str) -> Result<Vec<u8>> {
    if bytes.len() != expected_len {
        return Err(EngineError::Storage(format!(
            "{what} length mismatch: expected {expected_len}, got {}",
            bytes.len()
        )));
    }
    Ok(bytes)
}

/// Grows `bytes` so `capacity >= needed` without changing its contents.
///
/// `Vec::try_reserve` adds to the length, not the capacity. Requesting
/// `needed - len` makes room for a later `clear` plus an exact fill, and a
/// failure leaves the vector as it was.
fn ensure_vec_capacity(bytes: &mut Vec<u8>, needed: usize, what: &str) -> Result<()> {
    if needed > bytes.capacity() {
        let additional = needed - bytes.len();
        bytes.try_reserve(additional).map_err(|err| {
            EngineError::Storage(format!(
                "{what} allocation failed for {needed} bytes: {err}"
            ))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{database_name_from_utf16, validate_database_name};
    use crate::error::EngineError;

    #[test]
    fn database_names_use_utf8_bytes_and_reject_unpaired_surrogates() {
        assert!(validate_database_name("").is_err());
        assert!(validate_database_name(&"a".repeat(127)).is_ok());
        assert!(validate_database_name(&"a".repeat(128)).is_err());
        assert!(database_name_from_utf16(&['a' as u16; 127]).is_ok());
        assert!(matches!(
            database_name_from_utf16(&['a' as u16; 128]),
            Err(EngineError::Storage(_))
        ));
        // U+044F is я: one UTF-16 unit and two UTF-8 bytes.
        assert!(database_name_from_utf16(&[0x044F; 63]).is_ok());
        assert!(matches!(
            database_name_from_utf16(&[0x044F; 64]),
            Err(EngineError::Storage(_))
        ));
        let mut emoji = Vec::new();
        for _ in 0..31 {
            emoji.extend_from_slice(&[0xD83D, 0xDE00]);
        }
        assert_eq!(database_name_from_utf16(&emoji).unwrap().len(), 124);
        emoji.extend_from_slice(&[0xD83D, 0xDE00]);
        assert!(matches!(
            database_name_from_utf16(&emoji),
            Err(EngineError::Storage(_))
        ));
        // wasm-bindgen's String conversion replaces each unpaired surrogate with
        // U+FFFD, so these distinct JS names would open one database.
        assert_eq!(
            String::from_utf16_lossy(&[0xD800]),
            String::from_utf16_lossy(&[0xD801])
        );
        assert!(validate_database_name(&String::from_utf16_lossy(&[0xD800])).is_ok());
        assert!(matches!(
            database_name_from_utf16(&[0xD800]),
            Err(EngineError::Storage(_))
        ));
        assert!(matches!(
            database_name_from_utf16(&[0xD801, b'a' as u16]),
            Err(EngineError::Storage(_))
        ));
    }
}
