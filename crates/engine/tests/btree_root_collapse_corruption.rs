use moyodb_engine::btree::{apply_mutations, lookup, PageAllocator};
use moyodb_engine::layout::ValueKind;
use moyodb_engine::page::{encode_internal_page, encode_leaf_page, InternalCell, LeafCell};
use moyodb_engine::pager::Pager;
use moyodb_engine::storage::memory::MemoryBackend;
use moyodb_engine::{EngineError, Result};

fn child(separator: &[u8], child_page_id: u64) -> InternalCell {
    InternalCell {
        separator: separator.to_vec(),
        child_page_id,
    }
}

fn leaf(page_id: u64, key: &[u8]) -> Result<Vec<u8>> {
    encode_leaf_page(
        page_id,
        0,
        0,
        &[LeafCell {
            key: key.to_vec(),
            value: b"value".to_vec(),
            value_kind: ValueKind::Inline,
            total_value_len: 5,
            overflow_head_page_id: 0,
        }],
    )
}

#[test]
fn deleting_other_subtree_rejects_repeated_levels_in_retained_root() -> Result<()> {
    for retained_level in [1, 2] {
        let mut pager = Pager::new(MemoryBackend::new(), 8);
        let images = [
            (
                1,
                encode_internal_page(1, 2, 0, &[child(b"a", 2), child(b"z", 5)])?,
            ),
            (
                2,
                encode_internal_page(2, retained_level, 0, &[child(b"a", 3)])?,
            ),
            // The repeated level occurs either at the retained root or its
            // child. Each individual image is well-formed and checksummed.
            (3, encode_internal_page(3, 1, 0, &[child(b"a", 4)])?),
            (4, leaf(4, b"a")?),
            (5, encode_internal_page(5, 1, 0, &[child(b"z", 6)])?),
            (6, leaf(6, b"z")?),
        ];
        for (page_id, bytes) in images {
            pager.write_page_image(page_id, &bytes)?;
        }
        assert!(matches!(
            lookup(&mut pager, 1, b"a"),
            Err(EngineError::Corruption(_))
        ));
        assert_eq!(lookup(&mut pager, 1, b"z")?, Some(b"value".to_vec()));

        let mut alloc = PageAllocator::new(7);
        let result = apply_mutations(&mut pager, 1, &[(b"z".as_slice(), None)], &mut alloc);
        assert!(
            matches!(result, Err(EngineError::Corruption(_))),
            "a retained child's invalid height must not be hidden by root collapse: {result:?}"
        );
    }
    Ok(())
}

#[test]
fn deleting_other_subtree_rejects_cycle_in_retained_root() -> Result<()> {
    let mut pager = Pager::new(MemoryBackend::new(), 4);
    let images = [
        (
            1,
            encode_internal_page(1, 2, 0, &[child(b"a", 2), child(b"z", 3)])?,
        ),
        (2, encode_internal_page(2, 1, 0, &[child(b"a", 2)])?),
        (3, encode_internal_page(3, 1, 0, &[child(b"z", 4)])?),
        (4, leaf(4, b"z")?),
    ];
    for (page_id, bytes) in images {
        pager.write_page_image(page_id, &bytes)?;
    }
    assert!(matches!(
        lookup(&mut pager, 1, b"a"),
        Err(EngineError::Corruption(_))
    ));
    assert_eq!(lookup(&mut pager, 1, b"z")?, Some(b"value".to_vec()));

    let mut alloc = PageAllocator::new(5);
    let result = apply_mutations(&mut pager, 1, &[(b"z".as_slice(), None)], &mut alloc);
    assert!(
        matches!(result, Err(EngineError::Corruption(_))),
        "a cyclic retained root must fail instead of repeatedly retiring itself: {result:?}"
    );
    Ok(())
}
