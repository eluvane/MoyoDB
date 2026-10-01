use crate::error::{EngineError, Result};
use crate::layout::{page_offset, PAGE_SIZE};
use crate::page::verify_page_image;
use crate::storage::backend::FileBackend;
use std::collections::{HashMap, VecDeque};

#[derive(Debug)]
struct CacheEntry {
    bytes: Vec<u8>,
    generation: u64,
    dirty: bool,
}

// Cached bytes are trusted: images read from the main file are checksummed
// and id-checked once on load, and staged images are produced by this process.
#[derive(Debug)]
pub struct Pager<B: FileBackend> {
    main: B,
    cache_pages: usize,
    cache: HashMap<u64, CacheEntry>,
    // Only clean pages participate in eviction. Dirty pins must not be
    // rotated or compacted on every insertion while a commit grows.
    lru: VecDeque<(u64, u64)>,
    next_generation: u64,
    // Exclusive upper bound for page ids reachable from committed state.
    page_limit: u64,
    dirty_count: usize,
    #[cfg(test)]
    lru_entries_examined: usize,
}

impl<B: FileBackend> Pager<B> {
    pub fn new(main: B, cache_pages: usize) -> Self {
        Self {
            main,
            cache_pages: cache_pages.max(1),
            cache: HashMap::new(),
            lru: VecDeque::new(),
            next_generation: 1,
            page_limit: u64::MAX,
            dirty_count: 0,
            #[cfg(test)]
            lru_entries_examined: 0,
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

    pub(crate) fn set_page_limit(&mut self, next_page_id: u64) {
        self.page_limit = next_page_id.max(1);
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
            if page_id >= self.page_limit {
                return Err(EngineError::Corruption(format!(
                    "page id {page_id} is beyond the allocated range (next page id {})",
                    self.page_limit
                )));
            }
            let bytes = self.main.read_at(page_offset(page_id), PAGE_SIZE)?;
            verify_page_image(&bytes, page_id)?;
            self.insert_cache(page_id, bytes, false);
        } else {
            self.touch(page_id);
        }
        let entry = self
            .cache
            .get(&page_id)
            .ok_or_else(|| EngineError::Internal("pager cache entry disappeared".into()))?;
        f(&entry.bytes)
    }

    pub fn write_page_image(&mut self, page_id: u64, bytes: &[u8]) -> Result<()> {
        if bytes.len() != PAGE_SIZE {
            return Err(EngineError::Serialization(format!(
                "page image wrong size: {}",
                bytes.len()
            )));
        }
        self.main.write_at(page_offset(page_id), bytes)?;
        self.store_cached_page(page_id, bytes.to_vec(), false);
        Ok(())
    }

    // Keeps a committed page visible without touching the main file.
    // Checkpoint writes these back; eviction must not drop them first.
    pub(crate) fn stage_page_image(&mut self, page_id: u64, bytes: Vec<u8>) -> Result<()> {
        if page_id == 0 {
            return Err(EngineError::Corruption("page id 0 is invalid".into()));
        }
        if bytes.len() != PAGE_SIZE {
            return Err(EngineError::Serialization(format!(
                "page image wrong size: {}",
                bytes.len()
            )));
        }
        self.store_cached_page(page_id, bytes, true);
        Ok(())
    }

    pub(crate) fn has_dirty(&self) -> bool {
        self.dirty_count > 0
    }

    pub(crate) fn dirty_page_count(&self) -> usize {
        self.dirty_count
    }

    pub(crate) fn write_back_dirty(&mut self) -> Result<()> {
        if self.dirty_count == 0 {
            return Ok(());
        }
        let mut page_ids: Vec<u64> = self
            .cache
            .iter()
            .filter(|(_, entry)| entry.dirty)
            .map(|(page_id, _)| *page_id)
            .collect();
        page_ids.sort_unstable();
        let Self { main, cache, .. } = self;
        for page_id in page_ids {
            let entry = cache
                .get(&page_id)
                .ok_or_else(|| EngineError::Internal("dirty pager entry disappeared".into()))?;
            main.write_at(page_offset(page_id), &entry.bytes)?;
        }
        Ok(())
    }

    pub(crate) fn mark_dirty_clean(&mut self) {
        if self.dirty_count == 0 {
            return;
        }
        for (page_id, entry) in &mut self.cache {
            if entry.dirty {
                entry.dirty = false;
                entry.generation = self.next_generation;
                self.next_generation = self.next_generation.wrapping_add(1).max(1);
                self.lru.push_back((*page_id, entry.generation));
            }
        }
        self.dirty_count = 0;
        // The caller has durably checkpointed these images. They are now
        // evictable, including when no subsequent cache miss occurs.
        self.evict_if_needed(0);
        self.compact_lru_if_needed();
    }

    /// Forgets every cached page, dirty ones included. Used when in-memory
    /// state can no longer be trusted and must be rebuilt from the files.
    pub(crate) fn discard_cache(&mut self) {
        self.cache.clear();
        self.lru.clear();
        self.dirty_count = 0;
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

    fn store_cached_page(&mut self, page_id: u64, bytes: Vec<u8>, dirty: bool) {
        if let Some(entry) = self.cache.get_mut(&page_id) {
            entry.bytes = bytes;
            match (entry.dirty, dirty) {
                (false, true) => self.dirty_count += 1,
                (true, false) => self.dirty_count -= 1,
                _ => {}
            }
            entry.dirty = dirty;
            if dirty {
                // Invalidate a clean entry's old queue records immediately.
                entry.generation = 0;
            }
            self.touch(page_id);
            return;
        }
        self.insert_cache(page_id, bytes, dirty);
    }

    fn insert_cache(&mut self, page_id: u64, bytes: Vec<u8>, dirty: bool) {
        if dirty {
            self.dirty_count += 1;
        }
        let generation = if dirty { 0 } else { self.bump_generation() };
        self.cache.insert(
            page_id,
            CacheEntry {
                bytes,
                generation,
                dirty,
            },
        );
        if !dirty {
            self.lru.push_back((page_id, generation));
        }
        // Protect a freshly loaded clean page when dirty pins exceed the budget.
        self.evict_if_needed(page_id);
        self.compact_lru_if_needed();
    }

    fn touch(&mut self, page_id: u64) {
        let Some(entry) = self.cache.get_mut(&page_id) else {
            return;
        };
        if entry.dirty || self.lru.back() == Some(&(page_id, entry.generation)) {
            return;
        }
        entry.generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        self.lru.push_back((page_id, entry.generation));
        self.compact_lru_if_needed();
    }

    fn bump_generation(&mut self) -> u64 {
        let generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        generation
    }

    fn evict_if_needed(&mut self, protected_page_id: u64) {
        while self.cache.len() > self.cache_pages {
            let Some((old_page_id, old_generation)) = self.lru.pop_front() else {
                break;
            };
            #[cfg(test)]
            {
                self.lru_entries_examined += 1;
            }
            match self.cache.get(&old_page_id) {
                Some(entry) if !entry.dirty && entry.generation == old_generation => {}
                _ => continue,
            }
            if old_page_id == protected_page_id {
                // The inserted page is the newest clean entry. There are no
                // older eviction candidates left; do not walk the dirty pins.
                self.lru.push_back((old_page_id, old_generation));
                break;
            }
            self.cache.remove(&old_page_id);
        }
    }

    fn compact_lru_if_needed(&mut self) {
        let compact_after = self.cache_pages.saturating_mul(4).max(64);
        if self.lru.len() <= compact_after {
            return;
        }
        let mut compacted = VecDeque::with_capacity(self.cache.len() - self.dirty_count);
        for (page_id, generation) in self.lru.drain(..) {
            #[cfg(test)]
            {
                self.lru_entries_examined += 1;
            }
            let is_current = match self.cache.get(&page_id) {
                Some(entry) => !entry.dirty && entry.generation == generation,
                None => false,
            };
            if is_current {
                compacted.push_back((page_id, generation));
            }
        }
        self.lru = compacted;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::encode_leaf_page;
    use crate::storage::memory::MemoryBackend;

    #[test]
    fn read_survives_when_dirty_pages_pin_the_cache() -> Result<()> {
        let mut pager = Pager::new(MemoryBackend::new(), 1);
        pager.stage_page_image(1, encode_leaf_page(1, 0, 0, &[])?)?;
        pager.stage_page_image(2, encode_leaf_page(2, 0, 0, &[])?)?;
        pager.write_page_image(3, &encode_leaf_page(3, 0, 0, &[])?)?;
        pager.discard_cache();
        pager.stage_page_image(1, encode_leaf_page(1, 0, 0, &[])?)?;
        pager.stage_page_image(2, encode_leaf_page(2, 0, 0, &[])?)?;

        assert_eq!(pager.read_page(3)?.len(), PAGE_SIZE);
        assert_eq!(pager.read_page(1)?.len(), PAGE_SIZE);
        assert_eq!(pager.read_page(2)?.len(), PAGE_SIZE);
        Ok(())
    }

    #[test]
    fn unverifiable_page_is_rejected_on_load() -> Result<()> {
        let mut pager = Pager::new(MemoryBackend::new(), 4);
        let mut image = encode_leaf_page(1, 0, 0, &[])?;
        image[100] ^= 0xff;
        pager.backend_mut().write_at(0, &image)?;
        assert_eq!(
            pager.read_page(1).err().map(|err| err.code()),
            Some("CorruptionError")
        );
        Ok(())
    }

    #[test]
    fn page_ids_beyond_limit_are_rejected() -> Result<()> {
        let mut pager = Pager::new(MemoryBackend::new(), 4);
        pager.write_page_image(1, &encode_leaf_page(1, 0, 0, &[])?)?;
        pager.discard_cache();
        pager.set_page_limit(1);
        assert_eq!(
            pager.read_page(1).err().map(|err| err.code()),
            Some("CorruptionError")
        );
        pager.set_page_limit(2);
        assert!(pager.read_page(1).is_ok());
        Ok(())
    }

    #[test]
    fn staging_and_reading_dirty_pages_does_no_lru_scan() -> Result<()> {
        for pages in [64u64, 1024, 4096] {
            let mut pager = Pager::new(MemoryBackend::new(), 16);
            for page_id in 1..=pages {
                pager.stage_page_image(page_id, encode_leaf_page(page_id, 0, 0, &[])?)?;
            }
            for page_id in 1..=pages {
                pager.with_page(page_id, |_| Ok(()))?;
            }
            assert_eq!(pager.dirty_page_count(), pages as usize);
            assert_eq!(pager.cache.len(), pages as usize);
            assert!(pager.lru.is_empty());
            assert_eq!(pager.lru_entries_examined, 0, "dirty pages={pages}");
        }
        Ok(())
    }

    #[test]
    fn repeated_mru_reads_do_not_append_queue_records() -> Result<()> {
        let mut pager = Pager::new(MemoryBackend::new(), 16);
        pager.write_page_image(1, &encode_leaf_page(1, 0, 0, &[])?)?;
        let next_generation = pager.next_generation;
        for _ in 0..10_000 {
            pager.with_page(1, |_| Ok(()))?;
        }
        assert_eq!(pager.lru.len(), 1);
        assert_eq!(pager.next_generation, next_generation);
        assert_eq!(pager.lru_entries_examined, 0);
        Ok(())
    }

    #[test]
    fn checkpoint_releases_pins_and_restaging_invalidates_clean_records() -> Result<()> {
        let mut pager = Pager::new(MemoryBackend::new(), 2);
        for page_id in 1..=8 {
            pager.stage_page_image(page_id, encode_leaf_page(page_id, 0, 0, &[])?)?;
        }
        pager.write_back_dirty()?;
        pager.flush()?;
        pager.mark_dirty_clean();
        assert!(!pager.has_dirty());
        assert_eq!(pager.cache.len(), 2);
        for page_id in 1..=8 {
            assert_eq!(pager.read_page(page_id)?, encode_leaf_page(page_id, 0, 0, &[])?);
        }
        // Page 8 is clean and MRU. Its stale queue record must not evict it
        // after it is dirtied again, even when every page is pinned.
        for page_id in 8..=16 {
            pager.stage_page_image(page_id, encode_leaf_page(page_id, 0, 0, &[])?)?;
        }
        assert_eq!(pager.dirty_page_count(), 9);
        for page_id in 8..=16 {
            assert!(pager.cache.get(&page_id).is_some_and(|entry| entry.dirty));
        }
        pager.write_back_dirty()?;
        pager.flush()?;
        pager.mark_dirty_clean();
        assert_eq!(pager.cache.len(), 2);
        for page_id in 8..=16 {
            assert_eq!(pager.read_page(page_id)?, encode_leaf_page(page_id, 0, 0, &[])?);
        }
        Ok(())
    }
}
