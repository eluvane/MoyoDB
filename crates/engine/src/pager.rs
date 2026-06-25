use crate::error::{EngineError, Result};
use crate::layout::{page_offset, PAGE_SIZE};
use crate::storage::backend::FileBackend;
use std::collections::{HashMap, VecDeque};

#[derive(Debug)]
struct CacheEntry {
    bytes: Vec<u8>,
    generation: u64,
}

#[derive(Debug)]
pub struct Pager<B: FileBackend> {
    main: B,
    cache_pages: usize,
    cache: HashMap<u64, CacheEntry>,
    lru: VecDeque<(u64, u64)>,
    next_generation: u64,
}

impl<B: FileBackend> Pager<B> {
    pub fn new(main: B, cache_pages: usize) -> Self {
        Self {
            main,
            cache_pages: cache_pages.max(1),
            cache: HashMap::new(),
            lru: VecDeque::new(),
            next_generation: 1,
        }
    }

    pub fn into_inner(self) -> B {
        self.main
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.main
    }

    pub fn backend_ref(&self) -> &B {
        &self.main
    }

    pub fn read_page(&mut self, page_id: u64) -> Result<Vec<u8>> {
        self.with_page(page_id, |bytes| Ok(bytes.to_vec()))
    }

    // Lets hot readers inspect cached page bytes without cloning a full page.
    pub(crate) fn with_page<R>(
        &mut self,
        page_id: u64,
        f: impl FnOnce(&[u8]) -> Result<R>,
    ) -> Result<R> {
        if page_id == 0 {
            return Err(EngineError::Corruption("page id 0 is invalid".into()));
        }
        if !self.cache.contains_key(&page_id) {
            let bytes = self.main.read_at(page_offset(page_id), PAGE_SIZE)?;
            self.insert_cache(page_id, bytes);
        } else {
            self.touch(page_id);
        }
        let bytes = self
            .cache
            .get(&page_id)
            .ok_or_else(|| EngineError::Internal("pager cache entry disappeared".into()))?;
        f(&bytes.bytes)
    }

    pub fn write_page_image(&mut self, page_id: u64, bytes: &[u8]) -> Result<()> {
        if bytes.len() != PAGE_SIZE {
            return Err(EngineError::Serialization(format!(
                "page image wrong size: {}",
                bytes.len()
            )));
        }
        self.main.write_at(page_offset(page_id), bytes)?;
        self.insert_cache(page_id, bytes.to_vec());
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        self.main.flush()
    }

    pub fn len(&self) -> Result<u64> {
        self.main.len()
    }

    pub fn is_empty(&self) -> Result<bool> {
        self.main.is_empty()
    }

    pub fn close(&mut self) -> Result<()> {
        self.main.close()
    }

    fn insert_cache(&mut self, page_id: u64, bytes: Vec<u8>) {
        let generation = self.bump_generation();
        self.cache.insert(page_id, CacheEntry { bytes, generation });
        self.lru.push_back((page_id, generation));
        self.evict_if_needed();
        self.compact_lru_if_needed();
    }

    fn touch(&mut self, page_id: u64) {
        let generation = self.bump_generation();
        if let Some(entry) = self.cache.get_mut(&page_id) {
            entry.generation = generation;
            self.lru.push_back((page_id, generation));
        }
        self.compact_lru_if_needed();
    }

    fn bump_generation(&mut self) -> u64 {
        let generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        generation
    }

    fn evict_if_needed(&mut self) {
        while self.cache.len() > self.cache_pages {
            let Some((old_page_id, old_generation)) = self.lru.pop_front() else {
                break;
            };
            let should_remove = match self.cache.get(&old_page_id) {
                Some(entry) => entry.generation == old_generation,
                None => false,
            };
            if should_remove {
                self.cache.remove(&old_page_id);
            }
        }
    }

    fn compact_lru_if_needed(&mut self) {
        let compact_after = self.cache_pages.saturating_mul(4).max(64);
        if self.lru.len() <= compact_after {
            return;
        }
        let mut compacted = VecDeque::with_capacity(self.cache.len());
        for (page_id, generation) in self.lru.drain(..) {
            let is_current = match self.cache.get(&page_id) {
                Some(entry) => entry.generation == generation,
                None => false,
            };
            if is_current {
                compacted.push_back((page_id, generation));
            }
        }
        self.lru = compacted;
    }
}
