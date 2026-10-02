use moyodb_engine::btree::{build_tree, collect_keys_below, lookup, scan, KvPair, RangeSpec};
use moyodb_engine::bytes::{read_u16_le, write_u32_le, write_u64_le};
use moyodb_engine::checksum::checksum_with_zeroed_region;
use moyodb_engine::layout::{
    page_offset, PAGE_HEADER_CHECKSUM_OFFSET, PAGE_HEADER_SIZE, PAGE_SIZE,
};
use moyodb_engine::pager::Pager;
use moyodb_engine::{EngineError, MemoryBackend, Result};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

// This prime length distinguishes payload allocations from the fixture's
// four-byte keys, page images, and collection capacities.
const VALUE_LEN: usize = 251;

#[derive(Clone, Copy, Debug, Default)]
struct AllocationWork {
    requests: usize,
    bytes: usize,
    payload_requests: usize,
    payload_bytes: usize,
    key_requests: usize,
    page_requests: usize,
}

thread_local! {
    static ALLOCATION_WORK: Cell<Option<AllocationWork>> = const { Cell::new(None) };
}

fn count_request(size: usize) {
    ALLOCATION_WORK.with(|counter| {
        if let Some(mut work) = counter.get() {
            work.requests += 1;
            work.bytes += size;
            if size == VALUE_LEN {
                work.payload_requests += 1;
                work.payload_bytes += size;
            }
            if size == 4 {
                work.key_requests += 1;
            }
            if size == PAGE_SIZE {
                work.page_requests += 1;
            }
            counter.set(Some(work));
        }
    });
}

struct WorkAllocator;

// SAFETY: every operation is forwarded unchanged to System. The thread-local
// counters do not read, own, or alter allocated memory.
unsafe impl GlobalAlloc for WorkAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_request(layout.size());
        // SAFETY: GlobalAlloc supplied a valid layout, forwarded to System.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_request(layout.size());
        // SAFETY: GlobalAlloc supplied a valid layout, forwarded to System.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: pointer and layout belong to the unchanged System allocator.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count_request(size);
        // SAFETY: the allocated pointer and layout are forwarded to System.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: WorkAllocator = WorkAllocator;

struct TrackingGuard;

impl Drop for TrackingGuard {
    fn drop(&mut self) {
        ALLOCATION_WORK.with(|counter| counter.set(None));
    }
}

fn measure<T>(operation: impl FnOnce() -> T) -> (T, AllocationWork) {
    ALLOCATION_WORK.with(|counter| {
        assert!(counter.get().is_none(), "nested allocation measurement");
        counter.set(Some(AllocationWork::default()));
    });
    let guard = TrackingGuard;
    let result = operation();
    let work = ALLOCATION_WORK.with(|counter| counter.replace(None).expect("active measurement"));
    drop(guard);
    (result, work)
}

fn entries(count: u32) -> Vec<(Vec<u8>, Vec<u8>)> {
    (0..count)
        .map(|key| (key.to_be_bytes().to_vec(), vec![key as u8; VALUE_LEN]))
        .collect()
}

fn pager_with_entries(
    entries: &[(Vec<u8>, Vec<u8>)],
    cache_pages: usize,
) -> Result<(Pager<MemoryBackend>, u64)> {
    let mut next_page_id = 1;
    let tree = build_tree(entries, &mut next_page_id)?;
    let mut pager = Pager::new(MemoryBackend::new(), cache_pages);
    for (page_id, image) in tree.page_images {
        pager.write_page_image(page_id, &image)?;
    }
    pager.flush()?;
    Ok((pager, tree.root_page_id))
}

fn rows(entries: &[(Vec<u8>, Vec<u8>)]) -> Vec<KvPair> {
    entries
        .iter()
        .map(|(key, value)| KvPair {
            key: key.clone(),
            value: value.clone(),
        })
        .collect()
}

fn assert_bounded_work(reverse: bool) -> Result<()> {
    let source = entries(12);
    for limit in [1, 3, 8] {
        let (mut pager, root) = pager_with_entries(&source, 8)?;
        let range = RangeSpec {
            reverse,
            limit: Some(limit),
            ..RangeSpec::default()
        };
        let (actual, work) = measure(|| scan(&mut pager, root, &range));
        let actual = actual?;
        let mut expected = rows(&source);
        if reverse {
            expected.reverse();
        }
        expected.truncate(limit);
        assert_eq!(actual, expected);
        println!("work bounded_leaf reverse={reverse} limit={limit}: {work:?}");
        assert_eq!(
            work.payload_requests, limit,
            "only returned inline values may be copied"
        );
        assert_eq!(work.payload_bytes, limit * VALUE_LEN);
        assert_eq!(
            work.key_requests, limit,
            "only requested keys may be copied"
        );
        assert_eq!(
            work.page_requests, 0,
            "the single leaf was warmed during setup"
        );
    }
    Ok(())
}

#[test]
fn forward_bounded_leaf_copies_only_requested_inline_values() -> Result<()> {
    assert_bounded_work(false)
}

#[test]
fn reverse_bounded_leaf_copies_only_requested_inline_values() -> Result<()> {
    assert_bounded_work(true)
}

#[test]
fn keys_only_pruning_does_not_allocate_inline_payloads() -> Result<()> {
    let source = entries(12);
    let upper = 8u32.to_be_bytes();
    for limit in [0, 1, 3, 8, 20] {
        let (mut pager, root) = pager_with_entries(&source, 8)?;
        let (actual, work) = measure(|| collect_keys_below(&mut pager, root, &upper, limit));
        let mut actual = actual?;
        let expected: Vec<_> = source[..8]
            .iter()
            .take(limit)
            .map(|(key, _)| key.clone())
            .collect();
        assert_eq!(actual, expected);
        println!("work keys_only_pruning limit={limit}: {work:?}");
        assert_eq!(work.payload_requests, 0, "pruning does not consume values");
        assert_eq!(work.payload_bytes, 0);
        // One upper bound copy in RangeSpec and one owned iterator bound.
        assert_eq!(work.key_requests, limit.min(8) + 2);
        if let Some(key) = actual.first_mut() {
            key.fill(0xff);
            assert_eq!(
                lookup(&mut pager, root, &source[0].0)?,
                Some(source[0].1.clone())
            );
        }
    }
    Ok(())
}

#[test]
fn bounded_scans_and_pagination_match_an_independent_multileaf_oracle() -> Result<()> {
    let source = entries(32);
    for reverse in [false, true] {
        let mut expected = rows(&source[2..30]);
        if reverse {
            expected.reverse();
        }
        for limit in [Some(0), Some(1), Some(3), Some(40), None] {
            let (mut pager, root) = pager_with_entries(&source, 1)?;
            let range = RangeSpec {
                gte: Some(2u32.to_be_bytes().to_vec()),
                lt: Some(30u32.to_be_bytes().to_vec()),
                reverse,
                limit,
                ..RangeSpec::default()
            };
            let mut wanted = expected.clone();
            wanted.truncate(limit.unwrap_or(usize::MAX));
            assert_eq!(scan(&mut pager, root, &range)?, wanted);
        }
        let (mut pager, root) = pager_with_entries(&source, 1)?;
        let mut range = RangeSpec {
            gte: Some(2u32.to_be_bytes().to_vec()),
            lt: Some(30u32.to_be_bytes().to_vec()),
            reverse,
            limit: Some(3),
            ..RangeSpec::default()
        };
        let mut paginated = Vec::new();
        for _ in 0..source.len() {
            let page = scan(&mut pager, root, &range)?;
            let Some(last) = page.last() else { break };
            if reverse {
                range.lt = Some(last.key.clone());
            } else {
                range.gte = None;
                range.gt = Some(last.key.clone());
            }
            paginated.extend(page);
            if paginated.len() >= expected.len() {
                break;
            }
        }
        assert_eq!(paginated, expected);
    }
    Ok(())
}

#[test]
fn full_scan_keeps_overflow_io_bounded_with_a_one_page_cache() -> Result<()> {
    let source: Vec<_> = (0u32..12)
        .map(|key| {
            let len = if key % 3 == 0 { 1200 } else { VALUE_LEN };
            (key.to_be_bytes().to_vec(), vec![key as u8; len])
        })
        .collect();
    for reverse in [false, true] {
        let (mut pager, root) = pager_with_entries(&source, 1)?;
        let range = RangeSpec {
            reverse,
            ..RangeSpec::default()
        };
        let (actual, work) = measure(|| scan(&mut pager, root, &range));
        let mut expected = rows(&source);
        if reverse {
            expected.reverse();
        }
        assert_eq!(actual?, expected);
        println!("work full_leaf_overflow reverse={reverse}: {work:?}");
        // Four 1200-byte values occupy one overflow page each. The single
        // warmed leaf must not be reread after every overflow materialization.
        assert_eq!(work.page_requests, 4);
        assert_eq!(work.payload_requests, 8);
    }
    Ok(())
}

fn malformed_pager(two_errors: bool) -> Result<(Pager<MemoryBackend>, u64)> {
    let mut next_page_id = 1;
    let mut tree = build_tree(&entries(12), &mut next_page_id)?;
    assert_eq!(
        tree.page_images.len(),
        1,
        "malformed fixture uses one inline leaf"
    );
    let (page_id, mut image) = tree.page_images.pop().expect("single leaf");
    if two_errors {
        let slot = read_u16_le(&image, PAGE_HEADER_SIZE + 2 * 2)? as usize;
        write_u64_le(&mut image, slot + 8, 1)?;
    }
    let slot = read_u16_le(&image, PAGE_HEADER_SIZE + 10 * 2)? as usize;
    write_u32_le(&mut image, slot + 4, (VALUE_LEN + 1) as u32)?;
    let checksum = checksum_with_zeroed_region(&image, PAGE_HEADER_CHECKSUM_OFFSET, 4);
    write_u32_le(&mut image, PAGE_HEADER_CHECKSUM_OFFSET, checksum)?;
    let mut main = MemoryBackend::new();
    main.write_at(page_offset(page_id), &image)?;
    main.flush()?;
    Ok((Pager::new(main, 8), tree.root_page_id))
}

#[test]
fn late_cell_corruption_is_validated_before_the_first_row_in_ascending_order() -> Result<()> {
    for two_errors in [false, true] {
        let message = if two_errors {
            "inline leaf cell unexpectedly references overflow pages"
        } else {
            "inline leaf cell length metadata mismatch"
        };
        for reverse in [false, true] {
            let (mut pager, root) = malformed_pager(two_errors)?;
            let error = scan(
                &mut pager,
                root,
                &RangeSpec {
                    reverse,
                    limit: Some(1),
                    ..RangeSpec::default()
                },
            )
            .unwrap_err();
            assert_eq!(error, EngineError::Corruption(message.into()));
        }
        // The upper-bound binary search probes slots 6, 9, 11, so malformed
        // slots 2 and 10 are encountered by the ascending window validation.
        let (mut pager, root) = malformed_pager(two_errors)?;
        let error = collect_keys_below(&mut pager, root, &12u32.to_be_bytes(), 1).unwrap_err();
        assert_eq!(error, EngineError::Corruption(message.into()));
    }
    Ok(())
}
