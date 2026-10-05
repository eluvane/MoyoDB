use moyodb_engine::btree::{
    apply_mutations, build_tree_from_sorted, lookup, scan, PageAllocator, PageImages, RangeSpec,
    SortedTreeBuilder,
};
use moyodb_engine::error::Result;
use moyodb_engine::layout::PAGE_SIZE;
use moyodb_engine::page::{decode_page, max_overflow_chunk_len};
use moyodb_engine::pager::Pager;
use moyodb_engine::storage::memory::MemoryBackend;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeSet;

thread_local! {
    static METADATA_ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
}

struct WorkAllocator;

fn count_allocation(size: usize) {
    if size != PAGE_SIZE {
        METADATA_ALLOCATIONS.with(|work| {
            if let Some(count) = work.get() {
                work.set(Some(count + 1));
            }
        });
    }
}

// SAFETY: every operation is forwarded unchanged to System. The thread-local
// counter observes sizes without inspecting or changing allocated memory.
unsafe impl GlobalAlloc for WorkAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_allocation(layout.size());
        // SAFETY: GlobalAlloc supplies a valid layout, forwarded to System.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_allocation(layout.size());
        // SAFETY: GlobalAlloc supplies a valid layout, forwarded to System.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: pointer and layout came from this allocator's System calls.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count_allocation(size);
        // SAFETY: GlobalAlloc supplies the allocated pointer and its layout.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: WorkAllocator = WorkAllocator;

#[test]
fn overflow_metadata_allocations_do_not_grow_per_page() -> Result<()> {
    let mut counts = Vec::new();
    for chunks in [8, 512] {
        // Avoid page-sized id/vector allocations so the counter excludes only
        // page images. Payload allocation and reporting are outside the scope.
        let value = vec![0x73; chunks * max_overflow_chunk_len() + 17];
        let mut alloc = PageAllocator::new(1);
        METADATA_ALLOCATIONS.with(|work| work.set(Some(0)));
        let built = build_tree_from_sorted([(b"key".as_slice(), value.as_slice())], &mut alloc);
        let metadata = METADATA_ALLOCATIONS.with(|work| work.replace(None).unwrap());
        let built = built?;
        assert_eq!(built.page_images.len(), chunks + 2);
        println!(
            "overflow_planning chunks={chunks} pages={} metadata_allocations={metadata}",
            built.page_images.len()
        );
        counts.push(metadata);
    }
    assert!(
        counts[1] <= counts[0] + 16,
        "metadata allocation count grew with overflow pages: {counts:?}"
    );
    Ok(())
}

fn install(pager: &mut Pager<MemoryBackend>, images: &PageImages) -> Result<BTreeSet<u64>> {
    assert!(images.windows(2).all(|pair| pair[0].0 < pair[1].0));
    let mut content = crc32fast::Hasher::new();
    for (page_id, bytes) in images {
        // The generic decoder independently verifies every generated checksum.
        assert_eq!(decode_page(bytes)?.header.page_id, *page_id);
        content.update(&page_id.to_le_bytes());
        content.update(bytes);
        pager.write_page_image(*page_id, bytes)?;
    }
    pager.flush()?;
    println!(
        "page_planning images={} ordered_bytes_crc={:08x}",
        images.len(),
        content.finalize()
    );
    Ok(images.iter().map(|(page_id, _)| *page_id).collect())
}

#[test]
fn collapsed_roots_recycle_only_unpublished_nodes_and_preserve_overflow() -> Result<()> {
    let entries: Vec<_> = (0..128u16)
        .map(|index| {
            let mut key = vec![0x41; 900];
            key.extend_from_slice(&index.to_be_bytes());
            let value = vec![index as u8; max_overflow_chunk_len() * 2 + 17];
            (key, value)
        })
        .collect();
    let mut alloc = PageAllocator::new(1);
    let old = build_tree_from_sorted(
        entries
            .iter()
            .map(|(key, value)| (key.as_slice(), value.as_slice())),
        &mut alloc,
    )?;
    assert!(
        decode_page(&old.page_images.last().unwrap().1)?
            .header
            .level
            >= 2
    );
    let mut pager = Pager::new(MemoryBackend::new(), 8);
    let old_ids = install(&mut pager, &old.page_images)?;

    let survivor = &entries.last().unwrap().0;
    let replacement = vec![0xd3; max_overflow_chunk_len() * 3 + 31];
    let mutations: Vec<_> = entries
        .iter()
        .map(|(key, _)| {
            (
                key.as_slice(),
                (key == survivor).then_some(replacement.as_slice()),
            )
        })
        .collect();
    let rewritten = apply_mutations(&mut pager, old.root_page_id, &mutations, &mut alloc)?;
    let rewritten_ids = install(&mut pager, &rewritten.page_images)?;
    let high_watermark = alloc.next_page_id();

    // Root collapse returns private internal pages to the same allocator. A
    // later tree may use them immediately, including as overflow pages.
    let second_value = vec![0xe5; max_overflow_chunk_len() * 4 + 11];
    let second = build_tree_from_sorted(
        [(b"second".as_slice(), second_value.as_slice())],
        &mut alloc,
    )?;
    let second_ids = install(&mut pager, &second.page_images)?;
    assert!(second_ids.iter().any(|page_id| *page_id < high_watermark));
    assert!(rewritten_ids.is_disjoint(&second_ids));
    assert!(old_ids.is_disjoint(&rewritten_ids));
    assert!(old_ids.is_disjoint(&second_ids));
    let (_, unused, retired) = alloc.into_parts();
    for page_id in unused.iter().chain(&retired) {
        assert!(!rewritten_ids.contains(page_id));
        assert!(!second_ids.contains(page_id));
    }

    // Replaced pages remain readable to the old snapshot; both newly planned
    // trees retain all bytes, including their independent overflow chains.
    let old_rows = scan(&mut pager, old.root_page_id, &RangeSpec::default())?;
    assert_eq!(old_rows.len(), entries.len());
    for (row, (key, value)) in old_rows.iter().zip(&entries) {
        assert_eq!((&row.key, &row.value), (key, value));
    }
    let rows = scan(&mut pager, rewritten.root_page_id, &RangeSpec::default())?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].key, *survivor);
    assert_eq!(rows[0].value, replacement);
    assert_eq!(
        lookup(&mut pager, second.root_page_id, b"second")?,
        Some(second_value)
    );
    Ok(())
}

#[test]
fn streamed_builder_keeps_each_drained_batch_sorted_with_reused_ids() -> Result<()> {
    let mut alloc = PageAllocator::with_reusable(100, vec![8, 2, 91, 19, 5, 63]);
    let mut builder = SortedTreeBuilder::new();
    let mut pager = Pager::new(MemoryBackend::new(), 4);
    let mut published = BTreeSet::new();
    let entries: Vec<_> = (0..24u8)
        .map(|index| (vec![index; 256], vec![index; max_overflow_chunk_len() + 3]))
        .collect();
    for (index, (key, value)) in entries.iter().enumerate() {
        builder.push(key, value, &mut alloc)?;
        if index % 5 == 4 {
            let ids = install(&mut pager, &builder.drain_images())?;
            assert!(published.is_disjoint(&ids));
            published.extend(ids);
        }
    }
    let built = builder.finish(&mut alloc)?;
    let ids = install(&mut pager, &built.page_images)?;
    assert!(published.is_disjoint(&ids));
    let rows = scan(&mut pager, built.root_page_id, &RangeSpec::default())?;
    assert_eq!(rows.len(), entries.len());
    for (row, (key, value)) in rows.iter().zip(&entries) {
        assert_eq!((&row.key, &row.value), (key, value));
    }
    Ok(())
}
