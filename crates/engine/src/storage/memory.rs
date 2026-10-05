use crate::error::{EngineError, Result};
use crate::storage::backend::{FileBackend, FileSet};
use std::collections::BTreeMap;
use std::convert::TryFrom;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Debug, Default)]
struct MemoryFileState {
    working: Vec<u8>,
    durable: Vec<u8>,
    closed: bool,
    dirty_extents: BTreeMap<usize, usize>,
    #[cfg(test)]
    flush_copy_bytes: usize,
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
                dirty_extents: BTreeMap::new(),
                #[cfg(test)]
                flush_copy_bytes: 0,
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

    #[cfg(test)]
    pub fn flush_copy_bytes(&self) -> Result<usize> {
        Ok(self.lock_state()?.flush_copy_bytes)
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

fn resize_zeroed(bytes: &mut Vec<u8>, size: usize, what: &str) -> Result<()> {
    if size > bytes.len() {
        bytes.try_reserve(size - bytes.len()).map_err(|err| {
            EngineError::Storage(format!("{what} allocation failed for {size} bytes: {err}"))
        })?;
    }
    bytes.resize(size, 0);
    Ok(())
}

impl MemoryFileState {
    fn mark_dirty(&mut self, mut start: usize, mut end: usize) {
        if start >= end {
            return;
        }
        if let Some((&previous_start, &previous_end)) =
            self.dirty_extents.range(..=start).next_back()
        {
            if previous_end >= end {
                return;
            }
            if previous_end >= start {
                start = previous_start;
                self.dirty_extents.remove(&previous_start);
            }
        }
        loop {
            let next = self
                .dirty_extents
                .range(start..)
                .next()
                .map(|(&next_start, &next_end)| (next_start, next_end));
            let Some((next_start, next_end)) = next else {
                break;
            };
            if next_start > end {
                break;
            }
            end = end.max(next_end);
            self.dirty_extents.remove(&next_start);
        }
        self.dirty_extents.insert(start, end);
    }

    fn clear_dirty(&mut self) {
        self.dirty_extents.clear();
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
        let mut out = Vec::new();
        resize_zeroed(&mut out, len, "read")?;
        if start >= state.working.len() {
            return Ok(out);
        }
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
        let old_len = state.working.len();
        if end > old_len {
            resize_zeroed(&mut state.working, end, "write")?;
            // The zero-filled gap must replace stale durable bytes after shrink
            // and regrowth, including an empty sparse write.
            if start > old_len {
                state.mark_dirty(old_len, start);
            }
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
        // Clean gaps retain their durable bytes. Sparse writes only require
        // copies of the dirty extents.
        let working_len = state.working.len();
        if state.durable.len() != working_len {
            resize_zeroed(&mut state.durable, working_len, "flush")?;
        }
        #[cfg(test)]
        let mut copied_bytes = 0;
        {
            let MemoryFileState {
                working,
                durable,
                dirty_extents,
                ..
            } = &mut *state;
            for (&start, &end) in dirty_extents.iter() {
                let end = end.min(working_len);
                if start < end {
                    durable[start..end].copy_from_slice(&working[start..end]);
                    #[cfg(test)]
                    {
                        copied_bytes += end - start;
                    }
                }
            }
        }
        #[cfg(test)]
        {
            state.flush_copy_bytes += copied_bytes;
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
        resize_zeroed(&mut state.working, size, "truncate")?;
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
