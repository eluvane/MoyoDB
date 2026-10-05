use crate::error::{EngineError, Result};
use crate::layout::{page_offset, PAGE_SIZE};
use crate::page::verify_page_image;
use crate::storage::backend::FileBackend;
use std::collections::{HashMap, VecDeque};

// Limit the copy buffer while reducing native and OPFS backend calls.
pub(crate) const PAGE_WRITE_BATCH_PAGES: usize = 64;

// Checkpoint pages are already pinned, so use a larger write buffer.
// Recovery also holds read buffers and uses the smaller batch limit.
const DIRTY_PAGE_WRITE_BATCH_PAGES: usize = 1024;

#[derive(Debug)]
struct CacheEntry {
    bytes: Vec<u8>,
    generation: u64,
    dirty: bool,
}

// Main-file reads are verified once. Image-write callers must supply valid bytes,
// because cached images bypass checksum verification.
#[derive(Debug)]
pub struct Pager<B: FileBackend> {
    main: B,
    cache_pages: usize,
    cache: HashMap<u64, CacheEntry>,
    // Track only clean pages. Growing commits must not scan dirty pins.
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

    // Callers supply verified or freshly encoded images and flush before publication.
    // Transfer their buffers to the cache only after a successful write.
    pub(crate) fn write_page_images(
        &mut self,
        images: &mut Vec<(u64, Vec<u8>)>,
        buffer: &mut Vec<u8>,
    ) -> Result<()> {
        if images.is_empty() {
            return Ok(());
        }
        if images.len() > PAGE_WRITE_BATCH_PAGES
            || images
                .windows(2)
                .any(|pair| pair[0].0.checked_add(1) != Some(pair[1].0))
        {
            return Err(EngineError::Serialization(
                "page image batch must be bounded and consecutive".into(),
            ));
        }
        for (_, bytes) in images.iter() {
            if bytes.len() != PAGE_SIZE {
                return Err(EngineError::Serialization(format!(
                    "page image wrong size: {}",
                    bytes.len()
                )));
            }
        }
        let offset = page_offset(images[0].0);
        if images.len() == 1 {
            self.main.write_at(offset, &images[0].1)?;
        } else {
            buffer.clear();
            buffer.reserve_exact(images.len() * PAGE_SIZE);
            for (_, bytes) in images.iter() {
                buffer.extend_from_slice(bytes);
            }
            self.main.write_at(offset, buffer)?;
        }
        for (page_id, bytes) in images.drain(..) {
            self.store_cached_page(page_id, bytes, false);
        }
        Ok(())
    }

    // Pin committed images until a durable checkpoint makes them evictable.
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
        let mut batch = Vec::new();
        let mut start = 0;
        while start < page_ids.len() {
            let mut end = start + 1;
            while end < page_ids.len()
                && end - start < DIRTY_PAGE_WRITE_BATCH_PAGES
                && page_ids[end - 1].checked_add(1) == Some(page_ids[end])
            {
                end += 1;
            }
            let page_id = page_ids[start];
            if end == start + 1 {
                let entry = cache
                    .get(&page_id)
                    .ok_or_else(|| EngineError::Internal("dirty pager entry disappeared".into()))?;
                main.write_at(page_offset(page_id), &entry.bytes)?;
            } else {
                batch.clear();
                batch.reserve_exact((end - start) * PAGE_SIZE);
                for page_id in &page_ids[start..end] {
                    let entry = cache.get(page_id).ok_or_else(|| {
                        EngineError::Internal("dirty pager entry disappeared".into())
                    })?;
                    batch.extend_from_slice(&entry.bytes);
                }
                // Batch only adjacent dirty pages; writing across gaps would replace other pages.
                main.write_at(page_offset(page_id), &batch)?;
            }
            start = end;
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
        // The caller has completed a durable checkpoint. Evict clean pages now,
        // even if no later cache miss triggers eviction.
        self.evict_if_needed(0);
        self.compact_lru_if_needed();
    }

    /// Drops dirty images as well as clean pages when recovery must rebuild state.
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
                // Old clean queue records must not match the new dirty entry.
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
                // This is the newest clean entry; no older candidates remain.
                // Keep it readable even when dirty pins exceed the budget.
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
        #[cfg(test)]
        {
            self.lru_entries_examined += self.lru.len();
        }
        let cache = &self.cache;
        self.lru
            .retain(|(page_id, generation)| match cache.get(page_id) {
                Some(entry) => !entry.dirty && entry.generation == *generation,
                None => false,
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::encode_leaf_page;
    use crate::storage::memory::MemoryBackend;

    #[derive(Default)]
    struct RecordingBackend {
        inner: MemoryBackend,
        writes: Vec<(u64, usize)>,
        fail_write: Option<usize>,
    }

    impl FileBackend for RecordingBackend {
        fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
            self.inner.read_at(offset, len)
        }

        fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
            self.writes.push((offset, bytes.len()));
            if self.fail_write == Some(self.writes.len()) {
                self.inner.write_at(offset, &bytes[..bytes.len() / 2])?;
                return Err(EngineError::Storage("injected partial page batch".into()));
            }
            self.inner.write_at(offset, bytes)
        }

        fn flush(&mut self) -> Result<()> {
            self.inner.flush()
        }

        fn len(&self) -> Result<u64> {
            self.inner.len()
        }

        fn truncate(&mut self, size: u64) -> Result<()> {
            self.inner.truncate(size)
        }

        fn close(&mut self) -> Result<()> {
            self.inner.close()
        }
    }

    #[test]
    fn checkpoint_batches_adjacent_pages_without_overwriting_gaps() -> Result<()> {
        let mut pager = Pager::new(RecordingBackend::default(), 16);
        let mut oracle = MemoryBackend::new();
        for page_id in [66, 68] {
            let image = encode_leaf_page(page_id, 0, page_id + 100, &[])?;
            pager.write_page_image(page_id, &image)?;
            oracle.write_at(page_offset(page_id), &image)?;
        }
        pager.main.writes.clear();
        for page_id in (1..=65).chain([67]).chain(69..=72) {
            let image = encode_leaf_page(page_id, 0, 0, &[])?;
            oracle.write_at(page_offset(page_id), &image)?;
            pager.stage_page_image(page_id, image)?;
        }
        pager.write_back_dirty()?;
        assert_eq!(
            pager.main.writes,
            [
                (page_offset(1), 65 * PAGE_SIZE),
                (page_offset(67), PAGE_SIZE),
                (page_offset(69), 4 * PAGE_SIZE),
            ]
        );
        assert_eq!(pager.dirty_page_count(), 70);
        let len = oracle.len()? as usize;
        assert_eq!(pager.main.read_at(0, len)?, oracle.read_at(0, len)?);
        pager.flush()?;
        oracle.flush()?;
        pager.mark_dirty_clean();
        assert!(!pager.has_dirty());
        assert_eq!(
            pager.main.inner.durable_snapshot(),
            oracle.durable_snapshot()
        );
        Ok(())
    }

    #[test]
    fn failed_page_batch_preserves_pins_and_can_be_rewritten() -> Result<()> {
        let mut pager = Pager::new(RecordingBackend::default(), 8);
        let mut oracle = MemoryBackend::new();
        let page_count = (2 * DIRTY_PAGE_WRITE_BATCH_PAGES + 2) as u64;
        for page_id in 1..=page_count {
            let image = encode_leaf_page(page_id, 0, 0, &[])?;
            oracle.write_at(page_offset(page_id), &image)?;
            pager.stage_page_image(page_id, image)?;
        }
        pager.main.fail_write = Some(2);
        assert!(matches!(
            pager.write_back_dirty(),
            Err(EngineError::Storage(_))
        ));
        assert_eq!(pager.dirty_page_count(), page_count as usize);
        assert!(pager.cache.values().all(|entry| entry.dirty));
        assert_eq!(pager.main.writes.len(), 2);
        pager.main.fail_write = None;
        pager.main.writes.clear();
        pager.write_back_dirty()?;
        assert_eq!(pager.main.writes.len(), 3);
        let len = oracle.len()? as usize;
        assert_eq!(pager.main.read_at(0, len)?, oracle.read_at(0, len)?);
        pager.flush()?;
        oracle.flush()?;
        assert_eq!(
            pager.main.inner.durable_snapshot(),
            oracle.durable_snapshot()
        );
        pager.mark_dirty_clean();
        assert_eq!(pager.dirty_page_count(), 0);
        Ok(())
    }

    #[test]
    fn checkpoint_bounds_large_runs_without_changing_page_bytes() -> Result<()> {
        let mut pager = Pager::new(RecordingBackend::default(), 8);
        let mut oracle = MemoryBackend::new();
        for page_id in (1..=1025).rev() {
            let image = encode_leaf_page(page_id, 0, page_id + 1, &[])?;
            oracle.write_at(page_offset(page_id), &image)?;
            pager.stage_page_image(page_id, image)?;
        }
        pager.write_back_dirty()?;
        assert_eq!(
            pager.main.writes,
            [
                (page_offset(1), 4 * 1024 * 1024),
                (page_offset(1025), PAGE_SIZE)
            ]
        );
        assert_eq!(pager.dirty_page_count(), 1025);
        let len = oracle.len()? as usize;
        assert_eq!(pager.main.read_at(0, len)?, oracle.read_at(0, len)?);
        pager.flush()?;
        oracle.flush()?;
        assert_eq!(
            pager.main.inner.durable_snapshot(),
            oracle.durable_snapshot()
        );
        pager.mark_dirty_clean();
        assert!(!pager.has_dirty());
        Ok(())
    }

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
    fn recovery_batch_failure_can_retry_without_caching_partial_images() -> Result<()> {
        let mut pager = Pager::new(RecordingBackend::default(), 8);
        let mut oracle = MemoryBackend::new();
        let mut images = Vec::new();
        let mut buffer = Vec::new();
        for page_id in 1..=3 {
            let image = encode_leaf_page(page_id, 0, page_id + 10, &[])?;
            oracle.write_at(page_offset(page_id), &image)?;
            images.push((page_id, image));
        }
        pager.main.fail_write = Some(1);
        assert!(matches!(
            pager.write_page_images(&mut images, &mut buffer),
            Err(EngineError::Storage(_))
        ));
        assert_eq!(images.len(), 3);
        assert!(pager.cache.is_empty());
        pager.main.fail_write = None;
        pager.main.writes.clear();
        pager.write_page_images(&mut images, &mut buffer)?;
        assert!(images.is_empty());
        assert_eq!(pager.main.writes, [(page_offset(1), 3 * PAGE_SIZE)]);
        assert_eq!(pager.dirty_page_count(), 0);
        let len = oracle.len()? as usize;
        assert_eq!(pager.main.read_at(0, len)?, oracle.read_at(0, len)?);
        pager.flush()?;
        oracle.flush()?;
        assert_eq!(
            pager.main.inner.durable_snapshot(),
            oracle.durable_snapshot()
        );
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
            assert_eq!(
                pager.read_page(page_id)?,
                encode_leaf_page(page_id, 0, 0, &[])?
            );
        }
        // Restaging page 8 must invalidate its clean queue record while all pages are pinned.
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
            assert_eq!(
                pager.read_page(page_id)?,
                encode_leaf_page(page_id, 0, 0, &[])?
            );
        }
        Ok(())
    }
}
