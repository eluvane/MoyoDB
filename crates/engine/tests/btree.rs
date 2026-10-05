mod common;

use moyodb_engine::btree::{build_tree, lookup, scan, RangeSpec};
use moyodb_engine::pager::Pager;
use moyodb_engine::storage::memory::MemoryBackend;

#[test]
fn build_lookup_and_scan() {
    let mut next_page_id = 1;
    let entries = vec![
        (b"c".to_vec(), b"3".to_vec()),
        (b"a".to_vec(), b"1".to_vec()),
        (b"b".to_vec(), b"2".to_vec()),
    ];
    let mut sorted = entries.clone();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let tree = build_tree(&sorted, &mut next_page_id).unwrap();
    let main = MemoryBackend::new();
    let mut pager = Pager::new(main, 32);
    for (page_id, bytes) in tree.page_images {
        pager.write_page_image(page_id, &bytes).unwrap();
    }
    pager.flush().unwrap();

    assert_eq!(
        lookup(&mut pager, tree.root_page_id, b"a")
            .unwrap()
            .unwrap(),
        b"1".to_vec()
    );
    assert_eq!(
        lookup(&mut pager, tree.root_page_id, b"b")
            .unwrap()
            .unwrap(),
        b"2".to_vec()
    );

    let rows = scan(
        &mut pager,
        tree.root_page_id,
        &RangeSpec {
            gte: Some(b"a".to_vec()),
            lt: Some(b"c".to_vec()),
            ..Default::default()
        },
    )
    .unwrap();
    let keys: Vec<Vec<u8>> = rows.into_iter().map(|row| row.key).collect();
    assert_eq!(keys, vec![b"a".to_vec(), b"b".to_vec()]);
}

#[test]
fn root_split_scenario_has_many_pages() {
    let mut next_page_id = 1;
    let entries: Vec<(Vec<u8>, Vec<u8>)> = (0u16..300u16)
        .map(|i| (i.to_be_bytes().to_vec(), i.to_be_bytes().to_vec()))
        .collect();
    let tree = build_tree(&entries, &mut next_page_id).unwrap();
    assert!(tree.page_images.len() > 2);
    assert!(tree.root_page_id > 0);
}

#[test]
fn forward_scan_descends_to_lower_bound_and_stops_at_limit() {
    let mut next_page_id = 1;
    let entries: Vec<(Vec<u8>, Vec<u8>)> = (0u8..160u8)
        .map(|i| (vec![i], vec![i.wrapping_add(1)]))
        .collect();
    let tree = build_tree(&entries, &mut next_page_id).unwrap();
    let main = MemoryBackend::new();
    let mut pager = Pager::new(main, 8);
    for (page_id, bytes) in tree.page_images {
        pager.write_page_image(page_id, &bytes).unwrap();
    }
    pager.flush().unwrap();

    let rows = scan(
        &mut pager,
        tree.root_page_id,
        &RangeSpec {
            gte: Some(vec![120]),
            lt: Some(vec![140]),
            limit: Some(5),
            ..Default::default()
        },
    )
    .unwrap();

    let keys: Vec<Vec<u8>> = rows.iter().map(|row| row.key.clone()).collect();
    assert_eq!(
        keys,
        vec![vec![120], vec![121], vec![122], vec![123], vec![124]]
    );
}

#[test]
fn reverse_scan_preserves_upper_bound() {
    let mut next_page_id = 1;
    let entries: Vec<(Vec<u8>, Vec<u8>)> = (0u8..10u8).map(|i| (vec![i], vec![i])).collect();
    let tree = build_tree(&entries, &mut next_page_id).unwrap();
    let main = MemoryBackend::new();
    let mut pager = Pager::new(main, 16);
    for (page_id, bytes) in tree.page_images {
        pager.write_page_image(page_id, &bytes).unwrap();
    }
    pager.flush().unwrap();

    let rows = scan(
        &mut pager,
        tree.root_page_id,
        &RangeSpec {
            lte: Some(vec![4]),
            reverse: true,
            limit: Some(3),
            ..Default::default()
        },
    )
    .unwrap();
    let keys: Vec<Vec<u8>> = rows.into_iter().map(|row| row.key).collect();
    assert_eq!(keys, vec![vec![4], vec![3], vec![2]]);
}

#[test]
fn scans_do_not_depend_on_leaf_sibling_chain() {
    let mut next_page_id = 1;
    let entries: Vec<(Vec<u8>, Vec<u8>)> = (0u32..240u32)
        .map(|i| (i.to_be_bytes().to_vec(), vec![i as u8; 48]))
        .collect();
    let tree = build_tree(&entries, &mut next_page_id).unwrap();

    let mut leaf_pages = 0usize;
    let main = MemoryBackend::new();
    let mut pager = Pager::new(main, 32);
    for (page_id, bytes) in tree.page_images {
        let decoded = moyodb_engine::page::decode_page(&bytes).unwrap();
        if decoded.header.page_kind == moyodb_engine::layout::PageKind::Leaf {
            leaf_pages += 1;
            let rewritten = moyodb_engine::page::encode_leaf_page(
                decoded.header.page_id,
                decoded.header.level,
                0,
                &decoded.leaf_cells,
            )
            .unwrap();
            pager.write_page_image(page_id, &rewritten).unwrap();
        } else {
            pager.write_page_image(page_id, &bytes).unwrap();
        }
    }
    pager.flush().unwrap();
    assert!(leaf_pages > 1, "test requires multiple leaf pages");

    let rows = scan(
        &mut pager,
        tree.root_page_id,
        &RangeSpec {
            gte: Some(32u32.to_be_bytes().to_vec()),
            lt: Some(212u32.to_be_bytes().to_vec()),
            ..Default::default()
        },
    )
    .unwrap();
    let keys: Vec<u32> = rows
        .into_iter()
        .map(|row| u32::from_be_bytes(row.key.as_slice().try_into().unwrap()))
        .collect();
    assert_eq!(keys, (32u32..212u32).collect::<Vec<_>>());
}
