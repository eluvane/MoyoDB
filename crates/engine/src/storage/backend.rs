use crate::error::{EngineError, Result};

pub trait FileBackend: Send {
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>>;
    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()>;
    fn flush(&mut self) -> Result<()>;
    fn len(&self) -> Result<u64>;
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

pub fn ensure_exact_len(bytes: Vec<u8>, expected_len: usize, what: &str) -> Result<Vec<u8>> {
    if bytes.len() != expected_len {
        return Err(EngineError::Storage(format!(
            "{what} length mismatch: expected {expected_len}, got {}",
            bytes.len()
        )));
    }
    Ok(bytes)
}
