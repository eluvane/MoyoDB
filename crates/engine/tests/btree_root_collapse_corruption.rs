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

fn install_then_reject_deleting_other_key(
    pager: &mut Pager<MemoryBackend>,
    images: &[(u64, Vec<u8>)],
    next_page_id: u64,
    context: &str,
) -> Result<()> {
    for (page_id, bytes) in images {
        pager.write_page_image(*page_id, bytes)?;
    }
    assert!(matches!(
        lookup(pager, 1, b"a"),
        Err(EngineError::Corruption(_))
    ));
    assert_eq!(lookup(pager, 1, b"z")?, Some(b"value".to_vec()));

    let mut alloc = PageAllocator::new(next_page_id);
    let result = apply_mutations(pager, 1, &[(b"z".as_slice(), None)], &mut alloc);
    assert!(
        matches!(result, Err(EngineError::Corruption(_))),
        "{context}: {result:?}"
    );
    Ok(())
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
        install_then_reject_deleting_other_key(
            &mut pager,
            &images,
            7,
            "a retained child's invalid height must not be hidden by root collapse",
        )?;
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
    install_then_reject_deleting_other_key(
        &mut pager,
        &images,
        5,
        "a cyclic retained root must fail instead of repeatedly retiring itself",
    )?;
    Ok(())
}
