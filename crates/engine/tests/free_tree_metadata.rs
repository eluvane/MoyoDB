use moyodb_engine::btree::{build_tree, free_tree, lookup, PageAllocator};
use moyodb_engine::layout::ValueKind;
use moyodb_engine::page::{
    encode_internal_page, encode_leaf_page, encode_overflow_page, InternalCell, LeafCell,
};
use moyodb_engine::pager::Pager;
use moyodb_engine::storage::memory::MemoryBackend;
use moyodb_engine::{EngineError, Result};

fn inline_cell(key: &[u8]) -> LeafCell {
    LeafCell {
        key: key.to_vec(),
        value: b"v".to_vec(),
        value_kind: ValueKind::Inline,
        total_value_len: 1,
        overflow_head_page_id: 0,
    }
}

fn pager_with_images(images: &[(u64, Vec<u8>)]) -> Result<Pager<MemoryBackend>> {
    let mut pager = Pager::new(MemoryBackend::new(), images.len().max(1));
    for (page_id, bytes) in images {
        pager.write_page_image(*page_id, bytes)?;
    }
    Ok(pager)
}

fn rejected_retirement(images: &[(u64, Vec<u8>)], expected_message: &str) -> Result<Vec<u64>> {
    let mut pager = pager_with_images(images)?;
    let next_page_id = images
        .iter()
        .map(|(page_id, _)| *page_id)
        .max()
        .unwrap_or(0)
        + 1;
    let mut alloc = PageAllocator::new(next_page_id);
    let Err(error) = free_tree(&mut pager, 1, &mut alloc) else {
        panic!("corrupt tree was accepted: {expected_message}");
    };
    match error {
        EngineError::Corruption(message) => assert_eq!(message, expected_message),
        other => panic!("expected corruption, got {other}"),
    }
    Ok(alloc.freed().to_vec())
}

#[test]
fn free_tree_retires_all_mixed_tree_pages_without_changing_source() -> Result<()> {
    let entries: Vec<_> = (0u32..64)
        .map(|index| {
            let value_len = if index % 8 == 0 { 9000 } else { 769 };
            (index.to_be_bytes().to_vec(), vec![index as u8; value_len])
        })
        .collect();
    let mut next_page_id = 1;
    let tree = build_tree(&entries, &mut next_page_id)?;
    let mut pager = pager_with_images(&tree.page_images)?;
    let mut alloc = PageAllocator::new(next_page_id);

    free_tree(&mut pager, tree.root_page_id, &mut alloc)?;

    let mut expected: Vec<_> = tree
        .page_images
        .iter()
        .map(|(page_id, _)| *page_id)
        .collect();
    expected.sort_unstable();
    let mut actual = alloc.freed().to_vec();
    actual.sort_unstable();
    assert_eq!(
        actual, expected,
        "every tree and overflow page is retired once"
    );
    assert_eq!(alloc.next_page_id(), next_page_id);
    for (page_id, bytes) in &tree.page_images {
        assert_eq!(pager.read_page(*page_id)?.as_slice(), bytes.as_slice());
    }
    for (key, value) in &entries {
        assert_eq!(
            lookup(&mut pager, tree.root_page_id, key)?.as_deref(),
            Some(value.as_slice())
        );
    }
    Ok(())
}

#[test]
fn free_tree_checks_the_last_cell_before_retiring_the_node() -> Result<()> {
    let mut invalid_length = inline_cell(b"c");
    invalid_length.total_value_len = 2;
    // The encoders give these malformed cells valid page checksums, so the
    // tree traversal must still validate their metadata and ordering.
    let cases = [
        (
            encode_leaf_page(
                1,
                0,
                0,
                &[inline_cell(b"a"), inline_cell(b"b"), invalid_length],
            )?,
            "inline leaf cell length metadata mismatch",
        ),
        (
            encode_leaf_page(
                1,
                0,
                0,
                &[inline_cell(b"a"), inline_cell(b"c"), inline_cell(b"b")],
            )?,
            "leaf page 1 keys are not strictly increasing",
        ),
        (
            encode_internal_page(
                1,
                1,
                0,
                &[
                    InternalCell {
                        separator: b"a".to_vec(),
                        child_page_id: 2,
                    },
                    InternalCell {
                        separator: b"c".to_vec(),
                        child_page_id: 3,
                    },
                    InternalCell {
                        separator: b"b".to_vec(),
                        child_page_id: 4,
                    },
                ],
            )?,
            "internal page 1 separators are not strictly increasing",
        ),
    ];
    for (image, message) in cases {
        assert!(rejected_retirement(&[(1, image)], message)?.is_empty());
    }
    Ok(())
}

#[test]
fn free_tree_rejects_invalid_and_repeated_child_links() -> Result<()> {
    let invalid_child = encode_internal_page(
        1,
        1,
        0,
        &[InternalCell {
            separator: b"a".to_vec(),
            child_page_id: 0,
        }],
    )?;
    assert!(
        rejected_retirement(&[(1, invalid_child)], "internal cell has child_page_id=0")?.is_empty()
    );

    let self_cycle = encode_internal_page(
        1,
        1,
        0,
        &[InternalCell {
            separator: b"a".to_vec(),
            child_page_id: 1,
        }],
    )?;
    assert_eq!(
        rejected_retirement(&[(1, self_cycle)], "page 1 is reachable twice in one tree")?,
        vec![1]
    );

    let repeated_child = encode_internal_page(
        1,
        1,
        0,
        &[
            InternalCell {
                separator: b"a".to_vec(),
                child_page_id: 2,
            },
            InternalCell {
                separator: b"b".to_vec(),
                child_page_id: 2,
            },
        ],
    )?;
    let leaf = encode_leaf_page(2, 0, 0, &[inline_cell(b"a")])?;
    assert_eq!(
        rejected_retirement(
            &[(1, repeated_child), (2, leaf)],
            "page 2 is reachable twice in one tree",
        )?,
        vec![1, 2]
    );
    Ok(())
}

#[test]
fn free_tree_validates_the_full_overflow_chain() -> Result<()> {
    let leaf = encode_leaf_page(
        1,
        0,
        0,
        &[LeafCell {
            key: b"a".to_vec(),
            value: Vec::new(),
            value_kind: ValueKind::Overflow,
            total_value_len: 4,
            overflow_head_page_id: 2,
        }],
    )?;
    let overflow = encode_overflow_page(2, 0, b"abc")?;
    assert_eq!(
        rejected_retirement(
            &[(1, leaf), (2, overflow)],
            "overflow chain length mismatch: expected 4, got 3",
        )?,
        vec![1, 2]
    );
    Ok(())
}
