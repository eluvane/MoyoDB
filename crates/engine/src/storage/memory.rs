use crate::error::{EngineError, Result};
use crate::storage::backend::{FileBackend, FileSet};
use std::convert::TryFrom;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Debug, Default)]
struct MemoryFileState {
    working: Vec<u8>,
    durable: Vec<u8>,
    closed: bool,
    dirty_start: Option<usize>,
    dirty_end: usize,
}

#[derive(Clone, Debug)]
pub struct MemoryBackend {
    inner: Arc<Mutex<MemoryFileState>>,
}

impl MemoryBackend {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(MemoryFileState::default())),
        }
    }

    pub fn from_durable(bytes: Vec<u8>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(MemoryFileState {
                working: bytes.clone(),
                durable: bytes,
                closed: false,
                dirty_start: None,
                dirty_end: 0,
            })),
        }
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, MemoryFileState>> {
        self.inner
            .lock()
            .map_err(|_| EngineError::Storage("memory backend mutex poisoned".into()))
    }

    pub fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        FileBackend::read_at(self, offset, len)
    }

    pub fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        FileBackend::write_at(self, offset, bytes)
    }

    pub fn flush(&mut self) -> Result<()> {
        FileBackend::flush(self)
    }

    pub fn len(&self) -> Result<u64> {
        FileBackend::len(self)
    }

    pub fn is_empty(&self) -> Result<bool> {
        FileBackend::is_empty(self)
    }

    pub fn truncate(&mut self, size: u64) -> Result<()> {
        FileBackend::truncate(self, size)
    }

    pub fn close(&mut self) -> Result<()> {
        FileBackend::close(self)
    }

    pub fn durable_snapshot(&self) -> Option<Vec<u8>> {
        FileBackend::durable_snapshot(self)
    }
}

impl Default for MemoryBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn to_index(offset: u64, what: &str) -> Result<usize> {
    usize::try_from(offset).map_err(|_| {
        EngineError::Storage(format!(
            "{what} offset too large for memory backend: {offset}"
        ))
    })
}

fn checked_end(start: usize, len: usize, what: &str) -> Result<usize> {
    start.checked_add(len).ok_or_else(|| {
        EngineError::Storage(format!("{what} range overflow: start={start} len={len}"))
    })
}

impl MemoryFileState {
    fn mark_dirty(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        self.dirty_start = Some(self.dirty_start.map_or(start, |current| current.min(start)));
        self.dirty_end = self.dirty_end.max(end);
    }

    fn clear_dirty(&mut self) {
        self.dirty_start = None;
        self.dirty_end = 0;
    }
}

impl FileBackend for MemoryBackend {
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let state = self.lock_state()?;
        if state.closed {
            return Err(EngineError::Storage(
                "read from closed memory backend".into(),
            ));
        }
        let start = to_index(offset, "read")?;
        let end = checked_end(start, len, "read")?;
        if start >= state.working.len() {
            return Ok(vec![0u8; len]);
        }
        let mut out = vec![0u8; len];
        let available_end = end.min(state.working.len());
        let copied = available_end.saturating_sub(start);
        out[..copied].copy_from_slice(&state.working[start..available_end]);
        Ok(out)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        let mut state = self.lock_state()?;
        if state.closed {
            return Err(EngineError::Storage(
                "write to closed memory backend".into(),
            ));
        }
        let start = to_index(offset, "write")?;
        let end = checked_end(start, bytes.len(), "write")?;
        if end > state.working.len() {
            state.working.resize(end, 0);
        }
        state.working[start..end].copy_from_slice(bytes);
        state.mark_dirty(start, end);
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        let mut state = self.lock_state()?;
        if state.closed {
            return Err(EngineError::Storage("flush closed memory backend".into()));
        }
        // Preserve durable-snapshot semantics, but copy only the dirty range instead
        // of cloning the whole in-memory file on every flush/commit.
        let working_len = state.working.len();
        if state.durable.len() != working_len {
            state.durable.resize(working_len, 0);
        }
        if let Some(start) = state.dirty_start {
            let end = state.dirty_end.min(working_len);
            if start < end {
                let MemoryFileState {
                    working, durable, ..
                } = &mut *state;
                durable[start..end].copy_from_slice(&working[start..end]);
            }
        }
        state.clear_dirty();
        Ok(())
    }

    fn len(&self) -> Result<u64> {
        let state = self.lock_state()?;
        if state.closed {
            return Err(EngineError::Storage("len on closed memory backend".into()));
        }
        u64::try_from(state.working.len())
            .map_err(|_| EngineError::Storage("memory backend length overflow".into()))
    }

    fn truncate(&mut self, size: u64) -> Result<()> {
        let mut state = self.lock_state()?;
        if state.closed {
            return Err(EngineError::Storage(
                "truncate closed memory backend".into(),
            ));
        }
        let size = to_index(size, "truncate")?;
        let old_len = state.working.len();
        state.working.resize(size, 0);
        if size > old_len {
            state.mark_dirty(old_len, size);
        }
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        let mut state = self.lock_state()?;
        state.closed = true;
        Ok(())
    }

    fn durable_snapshot(&self) -> Option<Vec<u8>> {
        match self.inner.lock() {
            Ok(state) => Some(state.durable.clone()),
            Err(_) => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MemoryBundle {
    pub manifest: MemoryBackend,
    pub main: MemoryBackend,
    pub wal: MemoryBackend,
}

impl MemoryBundle {
    pub fn new() -> Self {
        Self {
            manifest: MemoryBackend::new(),
            main: MemoryBackend::new(),
            wal: MemoryBackend::new(),
        }
    }

    pub fn files(&self) -> FileSet<MemoryBackend> {
        FileSet::new(self.manifest.clone(), self.main.clone(), self.wal.clone())
    }

    pub fn crash_recovered_files(&self) -> FileSet<MemoryBackend> {
        FileSet::new(
            recovered_file(&self.manifest),
            recovered_file(&self.main),
            recovered_file(&self.wal),
        )
    }
}

fn recovered_file(file: &MemoryBackend) -> MemoryBackend {
    MemoryBackend::from_durable(file.durable_snapshot().unwrap_or_default())
}

impl Default for MemoryBundle {
    fn default() -> Self {
        Self::new()
    }
}
