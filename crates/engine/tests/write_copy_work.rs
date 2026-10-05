use moyodb_engine::btree::{apply_mutations, build_tree, lookup, Mutation, PageAllocator};
use moyodb_engine::layout::ValueKind;
use moyodb_engine::page::{decode_page, encode_leaf_page, LeafCell};
use moyodb_engine::pager::Pager;
use moyodb_engine::storage::memory::MemoryBackend;
use moyodb_engine::{EngineError, Result};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

// Only allocations of the old inline payload's distinctive size are counted;
// setup, reference decoding and unrelated test threads stay outside the scope.
thread_local! {
    static OLD_VALUE_ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
}

struct PayloadAllocator;

fn record_allocation(size: usize) {
    if size == 61 {
        OLD_VALUE_ALLOCATIONS.with(|counter| {
            if let Some(count) = counter.get() {
                counter.set(Some(count + 1));
            }
        });
    }
}

// SAFETY: every operation delegates unchanged to System. The thread-local
// counter neither accesses nor owns the allocated memory.
unsafe impl GlobalAlloc for PayloadAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation(layout.size());
        // SAFETY: the allocator receives a valid layout and forwards it unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation(layout.size());
        // SAFETY: the allocator receives a valid layout and forwards it unchanged.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: pointer and layout belong to the delegated System allocation.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record_allocation(size);
        // SAFETY: pointer and layout belong to System; ownership passes through.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: PayloadAllocator = PayloadAllocator;

fn pager_with_images(images: &[(u64, Vec<u8>)]) -> Result<Pager<MemoryBackend>> {
    let mut pager = Pager::new(MemoryBackend::new(), images.len().max(1));
    for (page_id, image) in images {
        pager.write_page_image(*page_id, image)?;
    }
    Ok(pager)
}

fn reference_cells(image: &[u8], mutations: &[Mutation<'_>]) -> Result<Vec<LeafCell>> {
    // The owned decoder keeps this mutation oracle independent of TreeWriter.
    let old = decode_page(image)?;
    let mut cells = Vec::new();
    for mut cell in old.leaf_cells {
        if let Ok(index) = mutations.binary_search_by(|(key, _)| key.cmp(&cell.key.as_slice())) {
            let Some(value) = mutations[index].1 else {
                continue;
            };
            cell.value = value.to_vec();
            cell.value_kind = ValueKind::Inline;
            cell.total_value_len =
                u32::try_from(value.len()).map_err(|_| EngineError::ValueTooLarge(value.len()))?;
            cell.overflow_head_page_id = 0;
        }
        cells.push(cell);
    }
    Ok(cells)
}

#[test]
fn rewrite_copies_only_the_untouched_old_inline_payload() -> Result<()> {
    let entries: Vec<_> = (0u32..32)
        .map(|index| (index.to_be_bytes().to_vec(), vec![index as u8; 61]))
        .collect();
    let mut next_page_id = 1;
    let old = build_tree(&entries, &mut next_page_id)?;
    assert_eq!(old.page_images.len(), 1, "fixture must fit one leaf");
    let replacement = [0x7c; 23];
    let mutations: Vec<Mutation<'_>> = entries
        .iter()
        .take(31)
        .enumerate()
        .map(|(index, (key, _))| {
            (
                key.as_slice(),
                (index % 2 == 0).then_some(replacement.as_slice()),
            )
        })
        .collect();
    let expected_cells = reference_cells(&old.page_images[0].1, &mutations)?;
    let expected = encode_leaf_page(next_page_id, 0, 0, &expected_cells)?;
    let mut pager = pager_with_images(&old.page_images)?;
    let mut alloc = PageAllocator::new(next_page_id);

    OLD_VALUE_ALLOCATIONS.with(|counter| counter.set(Some(0)));
    let result = apply_mutations(&mut pager, old.root_page_id, &mutations, &mut alloc);
    let copies = OLD_VALUE_ALLOCATIONS.with(|counter| counter.replace(None).unwrap_or_default());
    let built = result?;

    assert_eq!(built.root_page_id, next_page_id);
    assert_eq!(built.page_images, vec![(next_page_id, expected)]);
    assert_eq!(alloc.freed(), &[old.root_page_id]);
    for (key, value) in &entries {
        assert_eq!(
            lookup(&mut pager, old.root_page_id, key)?.as_deref(),
            Some(value.as_slice())
        );
    }
    for (page_id, image) in &built.page_images {
        pager.write_page_image(*page_id, image)?;
    }
    for cell in &expected_cells {
        assert_eq!(
            lookup(&mut pager, built.root_page_id, &cell.key)?.as_deref(),
            Some(cell.value.as_slice())
        );
    }
    println!("write_copy_work: old_inline_allocations={copies}");
    assert_eq!(
        copies, 1,
        "only the single untouched 61-byte value needs a copy"
    );
    Ok(())
}

#[test]
fn rewrite_retires_replaced_overflow_and_matches_owned_reader_bytes() -> Result<()> {
    let entries = vec![
        (b"a".to_vec(), vec![0x11; 61]),
        (b"b".to_vec(), vec![0x22; 9000]),
        (b"c".to_vec(), vec![0x33; 61]),
    ];
    let mut next_page_id = 1;
    let old = build_tree(&entries, &mut next_page_id)?;
    let mut pager = pager_with_images(&old.page_images)?;
    let old_leaf = pager.read_page(old.root_page_id)?;
    let replacement = [0x7c; 23];
    let mutations = [(b"b".as_slice(), Some(replacement.as_slice()))];
    let expected = encode_leaf_page(next_page_id, 0, 0, &reference_cells(&old_leaf, &mutations)?)?;
    let mut alloc = PageAllocator::new(next_page_id);
    let built = apply_mutations(&mut pager, old.root_page_id, &mutations, &mut alloc)?;

    assert_eq!(built.page_images, vec![(next_page_id, expected)]);
    let mut retired = alloc.freed().to_vec();
    retired.sort_unstable();
    let mut expected_retired: Vec<_> = old
        .page_images
        .iter()
        .map(|(page_id, _)| *page_id)
        .collect();
    expected_retired.sort_unstable();
    assert_eq!(retired, expected_retired);
    assert_eq!(
        lookup(&mut pager, old.root_page_id, b"b")?.as_deref(),
        Some(entries[1].1.as_slice())
    );
    Ok(())
}

#[test]
fn rewrite_validates_replaced_last_cell_metadata_and_key_order() -> Result<()> {
    let make_cell = |key: &[u8], total_value_len| LeafCell {
        key: key.to_vec(),
        value: vec![0x55; 61],
        value_kind: ValueKind::Inline,
        total_value_len,
        overflow_head_page_id: 0,
    };
    let cases = [
        (
            vec![
                make_cell(b"a", 61),
                make_cell(b"b", 61),
                make_cell(b"c", 60),
            ],
            "inline leaf cell length metadata mismatch",
        ),
        (
            vec![
                make_cell(b"a", 61),
                make_cell(b"c", 61),
                make_cell(b"b", 61),
            ],
            "leaf page 1 keys are not strictly increasing",
        ),
    ];
    let replacement = [0x7c; 23];
    let mutations = [
        (b"a".as_slice(), Some(replacement.as_slice())),
        (b"b".as_slice(), Some(replacement.as_slice())),
        (b"c".as_slice(), Some(replacement.as_slice())),
    ];
    for (cells, expected_message) in cases {
        let image = encode_leaf_page(1, 0, 0, &cells)?;
        let mut pager = pager_with_images(&[(1, image)])?;
        let mut alloc = PageAllocator::new(2);
        let Err(error) = apply_mutations(&mut pager, 1, &mutations, &mut alloc) else {
            panic!("corrupt replaced cell was accepted: {expected_message}");
        };
        match error {
            EngineError::Corruption(message) => assert_eq!(message, expected_message),
            other => panic!("expected corruption, got {other}"),
        }
        assert!(alloc.freed().is_empty());
    }
    Ok(())
}
