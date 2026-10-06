mod common;

use moyodb_engine::btree::{
    apply_mutations, build_tree, load_all_entries, lookup, scan, PageAllocator, RangeSpec,
};
use moyodb_engine::layout::ValueKind;
use moyodb_engine::page::{encode_internal_page, encode_leaf_page, InternalCell, LeafCell};
use moyodb_engine::pager::Pager;
use moyodb_engine::storage::memory::MemoryBackend;
use moyodb_engine::EngineError;

fn pager_with_tree(entries: &[(Vec<u8>, Vec<u8>)]) -> (Pager<MemoryBackend>, u64) {
    let mut next_page_id = 1;
    let tree = build_tree(entries, &mut next_page_id).unwrap();
    let main = MemoryBackend::new();
    let mut pager = Pager::new(main, 8);
    for (page_id, bytes) in tree.page_images {
        pager.write_page_image(page_id, &bytes).unwrap();
    }
    pager.flush().unwrap();
    (pager, tree.root_page_id)
}

#[test]
fn empty_root_id_behaves_like_empty_tree() {
    let main = MemoryBackend::new();
    let mut pager = Pager::new(main, 8);

    assert_eq!(lookup(&mut pager, 0, b"missing").unwrap(), None);
    assert!(scan(&mut pager, 0, &RangeSpec::default())
        .unwrap()
        .is_empty());
    assert!(load_all_entries(&mut pager, 0).unwrap().is_empty());
}

#[test]
fn built_empty_tree_accepts_lookup_and_scans() {
    let (mut pager, root_page_id) = pager_with_tree(&[]);

    assert!(root_page_id > 0);
    assert_eq!(lookup(&mut pager, root_page_id, b"missing").unwrap(), None);
    assert!(scan(&mut pager, root_page_id, &RangeSpec::default())
        .unwrap()
        .is_empty());
    assert!(load_all_entries(&mut pager, root_page_id)
        .unwrap()
        .is_empty());
}

#[test]
fn zero_limit_scan_short_circuits_even_with_matching_rows() {
    let entries = vec![
        (b"a".to_vec(), b"1".to_vec()),
        (b"b".to_vec(), b"2".to_vec()),
    ];
    let (mut pager, root_page_id) = pager_with_tree(&entries);

    let rows = scan(
        &mut pager,
        root_page_id,
        &RangeSpec {
            gte: Some(b"a".to_vec()),
            limit: Some(0),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(rows.is_empty());
}

#[test]
fn collapsed_inclusive_range_returns_exact_key_in_both_directions() {
    let entries: Vec<(Vec<u8>, Vec<u8>)> = (0u8..8u8)
        .map(|i| (vec![i], vec![i.wrapping_add(10)]))
        .collect();
    let (mut pager, root_page_id) = pager_with_tree(&entries);

    let forward = scan(
        &mut pager,
        root_page_id,
        &RangeSpec {
            gte: Some(vec![4]),
            lte: Some(vec![4]),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(forward.len(), 1);
    assert_eq!(forward[0].key, vec![4]);
    assert_eq!(forward[0].value, vec![14]);

    let reverse = scan(
        &mut pager,
        root_page_id,
        &RangeSpec {
            gte: Some(vec![4]),
            lte: Some(vec![4]),
            reverse: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(reverse, forward);
}

#[test]
fn collapsed_exclusive_range_is_rejected() {
    let entries = vec![(b"a".to_vec(), b"1".to_vec())];
    let (mut pager, root_page_id) = pager_with_tree(&entries);

    let err = scan(
        &mut pager,
        root_page_id,
        &RangeSpec {
            gt: Some(b"a".to_vec()),
            lte: Some(b"a".to_vec()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(matches!(err, EngineError::InvalidRange(_)));
}

fn inline_cell(key: &[u8]) -> LeafCell {
    LeafCell {
        key: key.to_vec(),
        value: b"v".to_vec(),
        value_kind: ValueKind::Inline,
        total_value_len: 1,
        overflow_head_page_id: 0,
    }
}

/// Images written through a pager stay cached and skip load-time checks. A new
/// pager over the same backend reads them back the way a reopened file would.
fn reload(pager: Pager<MemoryBackend>) -> Pager<MemoryBackend> {
    Pager::new(pager.into_inner(), 4)
}

fn corruption_message(error: EngineError) -> String {
    match error {
        EngineError::Corruption(message) => message,
        other => panic!("expected corruption, got {other}"),
    }
}

#[test]
fn unsorted_leaf_is_rejected_before_binary_search_can_miss_a_key() {
    let page = encode_leaf_page(
        1,
        0,
        0,
        &[inline_cell(b"a"), inline_cell(b"c"), inline_cell(b"b")],
    )
    .unwrap();
    let mut pager = Pager::new(MemoryBackend::new(), 4);
    pager.write_page_image(1, &page).unwrap();
    let mut pager = reload(pager);

    let lookup_error = lookup(&mut pager, 1, b"b").unwrap_err();
    assert_eq!(
        corruption_message(lookup_error),
        "leaf page 1 keys are not strictly increasing"
    );
    let scan_error = scan(&mut pager, 1, &RangeSpec::default()).unwrap_err();
    assert_eq!(
        corruption_message(scan_error),
        "leaf page 1 keys are not strictly increasing"
    );
}

#[test]
fn repeated_internal_child_is_rejected_before_copy_on_write_retires_it() {
    let root = encode_internal_page(
        1,
        1,
        0,
        &[
            InternalCell {
                separator: b"a".to_vec(),
                child_page_id: 2,
            },
            InternalCell {
                separator: b"m".to_vec(),
                child_page_id: 2,
            },
        ],
    )
    .unwrap();
    let leaf = encode_leaf_page(2, 0, 0, &[inline_cell(b"a"), inline_cell(b"m")]).unwrap();
    let mut pager = Pager::new(MemoryBackend::new(), 4);
    pager.write_page_image(1, &root).unwrap();
    pager.write_page_image(2, &leaf).unwrap();
    let mut pager = reload(pager);

    assert_eq!(
        corruption_message(lookup(&mut pager, 1, b"a").unwrap_err()),
        "internal page 1 repeats child page 2"
    );
    assert_eq!(
        corruption_message(scan(&mut pager, 1, &RangeSpec::default()).unwrap_err()),
        "internal page 1 repeats child page 2"
    );
    let mut alloc = PageAllocator::new(3);
    let error = apply_mutations(
        &mut pager,
        1,
        &[(b"a".as_slice(), Some(b"n".as_slice()))],
        &mut alloc,
    )
    .unwrap_err();
    assert_eq!(
        corruption_message(error),
        "internal page 1 repeats child page 2"
    );
    assert!(
        alloc.freed().is_empty(),
        "the shared child must stay allocated when routing is rejected"
    );
}

#[test]
fn repeated_overflow_head_is_rejected_before_one_cell_can_retire_it() {
    let page = encode_leaf_page(
        1,
        0,
        0,
        &[
            LeafCell {
                key: b"a".to_vec(),
                value: Vec::new(),
                value_kind: ValueKind::Overflow,
                total_value_len: 4,
                overflow_head_page_id: 2,
            },
            LeafCell {
                key: b"b".to_vec(),
                value: Vec::new(),
                value_kind: ValueKind::Overflow,
                total_value_len: 4,
                overflow_head_page_id: 2,
            },
        ],
    )
    .unwrap();
    let mut pager = Pager::new(MemoryBackend::new(), 4);
    pager.write_page_image(1, &page).unwrap();
    let mut pager = reload(pager);

    assert_eq!(
        corruption_message(lookup(&mut pager, 1, b"b").unwrap_err()),
        "leaf page 1 repeats overflow head 2"
    );
    let mut alloc = PageAllocator::new(3);
    let error = apply_mutations(
        &mut pager,
        1,
        &[(b"a".as_slice(), Some(b"n".as_slice()))],
        &mut alloc,
    )
    .unwrap_err();
    assert_eq!(
        corruption_message(error),
        "leaf page 1 repeats overflow head 2"
    );
    assert!(alloc.freed().is_empty());
}
