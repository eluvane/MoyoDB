use moyodb_engine::btree::PageAllocator;
use moyodb_engine::layout::{page_offset, PAGE_SIZE};
use moyodb_engine::overflow::{read_overflow_prefix, read_overflow_value, write_overflow_chain};
use moyodb_engine::page::{encode_leaf_page, encode_overflow_page, max_overflow_chunk_len};
use moyodb_engine::pager::Pager;
use moyodb_engine::storage::backend::FileBackend;
use moyodb_engine::{EngineError, MemoryBackend, Result};
use std::sync::{Arc, Mutex};

type ReadTrace = Arc<Mutex<Vec<(u64, usize)>>>;

struct CountingBackend {
    inner: MemoryBackend,
    reads: ReadTrace,
    fail_page: Option<u64>,
}

impl FileBackend for CountingBackend {
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.reads
            .lock()
            .map_err(|_| EngineError::Storage("test read trace poisoned".into()))?
            .push((offset, len));
        if let Some(page) = self.fail_page {
            let failed_offset = page_offset(page);
            if offset <= failed_offset && failed_offset < offset + len as u64 {
                return Err(EngineError::Storage(format!("injected main page {page}")));
            }
        }
        self.inner.read_at(offset, len)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
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

fn pager_with_images(
    images: &[(u64, Vec<u8>)],
    cache_pages: usize,
) -> Result<(Pager<CountingBackend>, ReadTrace)> {
    let mut main = MemoryBackend::new();
    for (page, image) in images {
        main.write_at(page_offset(*page), image)?;
    }
    let reads = Arc::new(Mutex::new(Vec::new()));
    let pager = Pager::new(
        CountingBackend {
            inner: main,
            reads: Arc::clone(&reads),
            fail_page: None,
        },
        cache_pages,
    );
    Ok((pager, reads))
}

fn take_reads(reads: &ReadTrace) -> Vec<(u64, usize)> {
    std::mem::take(&mut *reads.lock().expect("test read trace"))
}

#[test]
fn cold_contiguous_megabyte_chain_uses_bounded_backend_reads() -> Result<()> {
    let value = vec![0x73; 1024 * 1024];
    let chain = write_overflow_chain(&value, &mut PageAllocator::new(1))?;
    assert_eq!(chain.pages.len(), 259);
    let (mut pager, reads) = pager_with_images(&chain.pages, 1)?;
    assert_eq!(
        read_overflow_value(&mut pager, chain.head_page_id, value.len())?,
        value
    );
    let calls = take_reads(&reads);
    println!(
        "overflow cold_1MiB pages={} calls={} bytes={}",
        chain.pages.len(),
        calls.len(),
        calls.iter().map(|(_, len)| len).sum::<usize>()
    );
    assert!(
        calls.len() <= 5,
        "one backend call per bounded page window: {calls:?}"
    );
    assert!(calls.iter().all(|(_, len)| *len <= 64 * PAGE_SIZE));
    Ok(())
}

#[test]
fn one_pass_overflow_body_keeps_hot_tree_page_cached() -> Result<()> {
    let value = vec![0x52; 1024 * 1024];
    let mut chain = write_overflow_chain(&value, &mut PageAllocator::new(2))?;
    chain.pages.push((1, encode_leaf_page(1, 0, 0, &[])?));
    let (mut pager, reads) = pager_with_images(&chain.pages, 1)?;
    let hot = pager.read_page(1)?;
    take_reads(&reads);
    assert_eq!(
        read_overflow_value(&mut pager, chain.head_page_id, value.len())?,
        value
    );
    take_reads(&reads);
    assert_eq!(pager.read_page(1)?, hot);
    assert!(
        take_reads(&reads).is_empty(),
        "overflow pages evicted the hot leaf"
    );
    Ok(())
}

#[test]
fn fragmented_chain_follows_links_and_does_not_validate_neighbors() -> Result<()> {
    let chunk_len = max_overflow_chunk_len();
    let ids: Vec<_> = (0..65u64)
        .map(|index| {
            if index % 2 == 0 {
                index / 2 + 1
            } else {
                index / 2 + 101
            }
        })
        .collect();
    let mut images = Vec::new();
    let mut value = Vec::new();
    for (index, page) in ids.iter().copied().enumerate() {
        let chunk = vec![index as u8; chunk_len];
        value.extend_from_slice(&chunk);
        images.push((
            page,
            encode_overflow_page(page, ids.get(index + 1).copied().unwrap_or(0), &chunk)?,
        ));
    }
    let (mut pager, reads) = pager_with_images(&images, 1)?;
    assert_eq!(read_overflow_value(&mut pager, ids[0], value.len())?, value);
    assert!(take_reads(&reads)
        .iter()
        .all(|(_, len)| *len <= 64 * PAGE_SIZE));
    Ok(())
}

#[test]
fn storage_error_in_unlinked_neighbor_does_not_fail_selected_chain() -> Result<()> {
    let chunk_len = max_overflow_chunk_len();
    let ids: Vec<_> = std::iter::once(1).chain(3..67).collect();
    let value = vec![0x5a; ids.len() * chunk_len];
    let images: Vec<_> = ids
        .iter()
        .copied()
        .enumerate()
        .map(|(index, page)| {
            encode_overflow_page(
                page,
                ids.get(index + 1).copied().unwrap_or(0),
                &value[index * chunk_len..(index + 1) * chunk_len],
            )
            .map(|image| (page, image))
        })
        .collect::<Result<_>>()?;
    let (mut pager, _) = pager_with_images(&images, 1)?;
    pager.backend_mut().fail_page = Some(2);
    assert_eq!(read_overflow_value(&mut pager, 1, value.len())?, value);
    Ok(())
}

#[test]
fn prefix_stops_before_inaccessible_or_corrupt_tail() -> Result<()> {
    let value = vec![0x42; 1024 * 1024];
    let chain = write_overflow_chain(&value, &mut PageAllocator::new(1))?;
    let (mut pager, reads) = pager_with_images(&chain.pages, 1)?;
    pager.backend_mut().fail_page = Some(2);
    assert_eq!(
        read_overflow_prefix(&mut pager, 1, value.len(), 16)?,
        value[..16]
    );
    assert_eq!(take_reads(&reads), vec![(page_offset(1), PAGE_SIZE)]);
    assert!(
        matches!(read_overflow_value(&mut pager, 1, value.len()), Err(EngineError::Storage(message)) if message == "injected main page 2")
    );
    Ok(())
}

#[test]
fn bounded_chain_reads_preserve_corruption_checks() -> Result<()> {
    let chunk_len = max_overflow_chunk_len();
    let value = vec![0x11; 65 * chunk_len];
    let chain = write_overflow_chain(&value, &mut PageAllocator::new(1))?;
    for declared_len in [value.len() - 1, value.len() + 1] {
        let (mut pager, _) = pager_with_images(&chain.pages, 1)?;
        assert!(matches!(
            read_overflow_value(&mut pager, 1, declared_len),
            Err(EngineError::Corruption(_))
        ));
    }
    let mut cycle = chain.pages.clone();
    let last = cycle.last_mut().expect("nonempty chain");
    last.1 = encode_overflow_page(last.0, 1, &value[value.len() - chunk_len..])?;
    let (mut pager, _) = pager_with_images(&cycle, 1)?;
    assert_eq!(
        read_overflow_value(&mut pager, 1, value.len()).unwrap_err(),
        EngineError::Corruption(format!(
            "overflow chain at page 1 is longer than its declared length {}",
            value.len()
        ))
    );
    let mut corrupt = chain.pages;
    corrupt.last_mut().expect("nonempty chain").1[PAGE_SIZE - 1] ^= 0x80;
    let (mut pager, _) = pager_with_images(&corrupt, 1)?;
    assert!(matches!(
        read_overflow_value(&mut pager, 1, value.len()),
        Err(EngineError::Corruption(_))
    ));
    Ok(())
}
