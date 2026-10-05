mod common;

use moyodb_engine::btree::{build_tree, load_all_entries, lookup, scan, RangeSpec};
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
