use super::*;
use crate::layout::PAGE_SIZE;
use crate::payload::{read_payload, write_payload};
use crate::storage::memory::MemoryBackend;

fn register(
    registry: &mut PayloadRegistry,
    pager: &mut Pager<MemoryBackend>,
    alloc: &mut PageAllocator,
    body_len: u32,
    checksum: u32,
    oldest: u64,
) -> Result<PayloadRef> {
    let reservation = registry.reserve(pager, body_len, oldest, alloc)?;
    let reference = PayloadRef {
        first_page_id: reservation.first_page_id,
        body_len,
        checksum,
    };
    registry.register_body(reservation, reference)?;
    Ok(reference)
}

fn persist(
    registry: PayloadRegistry,
    pager: &mut Pager<MemoryBackend>,
    alloc: &mut PageAllocator,
) -> Result<u64> {
    let previous_root = registry.root_page_id;
    let Some(built) = registry.finish(pager, alloc)? else {
        return Ok(previous_root);
    };
    for (page_id, image) in built.page_images {
        pager.write_page_image(page_id, &image)?;
    }
    pager.flush()?;
    pager.discard_cache();
    Ok(built.root_page_id)
}

fn reopen(pager: &Pager<MemoryBackend>) -> Pager<MemoryBackend> {
    Pager::new(
        MemoryBackend::from_durable(pager.backend_ref().durable_snapshot().unwrap()),
        8,
    )
}

fn stored(
    pager: &mut Pager<MemoryBackend>,
    root: u64,
    reference: PayloadRef,
) -> Result<RegistryEntry> {
    decode_entry(
        reference.first_page_id,
        &lookup(pager, root, &reference.first_page_id.to_be_bytes())?.unwrap(),
    )
}

#[test]
fn durable_zero_queue_reuses_whole_capacity_after_snapshots_release() -> Result<()> {
    let mut pager = Pager::new(MemoryBackend::new(), 8);
    let mut alloc = PageAllocator::new(1);
    let mut registry = PayloadRegistry::new(0);
    let old = register(&mut registry, &mut pager, &mut alloc, 16_000, 11, 0)?;
    registry.apply_reference_changes(&[old, old], &[], 1, &mut pager)?;
    let root = persist(registry, &mut pager, &mut alloc)?;
    assert_eq!(stored(&mut pager, root, old)?.references, 2);
    let mut registry = PayloadRegistry::new(root);
    registry.apply_reference_changes(&[], &[old], 2, &mut pager)?;
    let root = persist(registry, &mut pager, &mut alloc)?;
    assert_eq!(stored(&mut pager, root, old)?.references, 1);
    let mut registry = PayloadRegistry::new(root);
    registry.apply_reference_changes(&[], &[old], 3, &mut pager)?;
    let root = persist(registry, &mut pager, &mut alloc)?;
    let retired = stored(&mut pager, root, old)?;
    assert_eq!(retired.references, 0);
    assert_eq!(retired.retired_txid, 3);
    let mut pager = reopen(&pager);
    let next = alloc.next_page_id();
    let mut blocked_alloc = PageAllocator::new(next);
    let mut blocked = PayloadRegistry::new(root);
    assert_ne!(
        blocked
            .reserve(&mut pager, 1_000, 2, &mut blocked_alloc)?
            .first_page_id,
        old.first_page_id
    );
    let mut registry = PayloadRegistry::new(root);
    let reservation = registry.reserve(&mut pager, 1_000, 3, &mut alloc)?;
    assert_eq!(reservation.first_page_id, old.first_page_id);
    assert_eq!(reservation.capacity_pages, retired.capacity_pages);
    let current = PayloadRef {
        first_page_id: old.first_page_id,
        body_len: 1_000,
        checksum: 22,
    };
    registry.register_body(reservation, current)?;
    registry.apply_reference_changes(&[current], &[], 4, &mut pager)?;
    let root = persist(registry, &mut pager, &mut alloc)?;
    let live = stored(&mut pager, root, current)?;
    assert_eq!(live.capacity_pages, retired.capacity_pages);
    assert_eq!(live.generation, retired.generation + 1);
    assert_eq!(live.references, 1);
    assert!(lookup(
        &mut pager,
        root,
        &queue_key(retired.capacity_pages, 3, old.first_page_id)
    )?
    .is_none());
    assert!(alloc
        .freed()
        .iter()
        .all(|page_id| *page_id < old.first_page_id
            || *page_id >= old.first_page_id + retired.capacity_pages));
    let mut guard = PayloadRegistry::new(root);
    assert!(matches!(
        guard.apply_reference_changes(&[], &[old], 5, &mut pager),
        Err(EngineError::Corruption(_))
    ));
    assert!(guard.changes.is_empty());
    assert_eq!(stored(&mut pager, root, current)?.references, 1);
    Ok(())
}

#[test]
fn overwritten_zero_extent_can_be_reserved_again_after_unpublished_registry() -> Result<()> {
    let mut pager = Pager::new(MemoryBackend::new(), 8);
    let mut alloc = PageAllocator::new(1);
    let mut registry = PayloadRegistry::new(0);
    let reservation = registry.reserve(&mut pager, 16_000, 0, &mut alloc)?;
    let old = write_payload(
        pager.backend_mut(),
        reservation.first_page_id,
        &vec![1; 16_000],
    )?;
    registry.register_body(reservation, old)?;
    registry.apply_reference_changes(&[old], &[], 1, &mut pager)?;
    let root = persist(registry, &mut pager, &mut alloc)?;
    let mut registry = PayloadRegistry::new(root);
    registry.apply_reference_changes(&[], &[old], 2, &mut pager)?;
    let root = persist(registry, &mut pager, &mut alloc)?;
    let mut unpublished = PayloadRegistry::new(root);
    let reservation = unpublished.reserve(&mut pager, 1_000, 2, &mut alloc)?;
    write_payload(
        pager.backend_mut(),
        reservation.first_page_id,
        &vec![2; 1_000],
    )?;
    let mut pager = reopen(&pager);
    let mut registry = PayloadRegistry::new(root);
    let reservation = registry.reserve(&mut pager, 2_000, 2, &mut alloc)?;
    assert_eq!(reservation.first_page_id, old.first_page_id);
    let current = write_payload(
        pager.backend_mut(),
        reservation.first_page_id,
        &vec![3; 2_000],
    )?;
    registry.register_body(reservation, current)?;
    registry.apply_reference_changes(&[current], &[], 3, &mut pager)?;
    let root = persist(registry, &mut pager, &mut alloc)?;
    let mut pager = reopen(&pager);
    assert_eq!(read_payload(pager.backend_ref(), &current)?, vec![3; 2_000]);
    assert_eq!(
        stored(&mut pager, root, current)?.capacity_pages,
        old.page_count()?
    );
    Ok(())
}

#[test]
fn stale_queue_cannot_reuse_a_live_incarnation() -> Result<()> {
    let mut pager = Pager::new(MemoryBackend::new(), 8);
    let mut alloc = PageAllocator::new(1);
    let mut registry = PayloadRegistry::new(0);
    let current = register(&mut registry, &mut pager, &mut alloc, 1_000, 22, 0)?;
    registry.apply_reference_changes(&[current], &[], 1, &mut pager)?;
    let root = persist(registry, &mut pager, &mut alloc)?;
    let stale = RegistryEntry {
        reference: PayloadRef {
            checksum: 11,
            ..current
        },
        references: 0,
        capacity_pages: 1,
        retired_txid: 1,
        generation: 1,
    };
    let stale_key = queue_key(1, 1, current.first_page_id);
    let stale_value = encode_queue_value(&stale);
    let built = apply_mutations(
        &mut pager,
        root,
        &[(&stale_key, Some(&stale_value))],
        &mut alloc,
    )?;
    for (page_id, image) in built.page_images {
        pager.write_page_image(page_id, &image)?;
    }
    let mut registry = PayloadRegistry::new(built.root_page_id);
    let new = register(&mut registry, &mut pager, &mut alloc, 1_000, 33, 1)?;
    assert_ne!(new.first_page_id, current.first_page_id);
    registry.apply_reference_changes(&[new], &[], 2, &mut pager)?;
    let root = persist(registry, &mut pager, &mut alloc)?;
    assert_eq!(stored(&mut pager, root, current)?.references, 1);
    assert!(lookup(&mut pager, root, &stale_key)?.is_none());
    assert!(!alloc.freed().contains(&current.first_page_id));
    Ok(())
}

#[test]
fn durable_cursor_bounds_metadata_and_reaches_larger_eligible_extents() -> Result<()> {
    let mut pager = Pager::new(MemoryBackend::new(), 8);
    let mut alloc = PageAllocator::new(1);
    let mut registry = PayloadRegistry::new(0);
    let mut blocked = Vec::new();
    for checksum in 0..96 {
        blocked.push(register(
            &mut registry,
            &mut pager,
            &mut alloc,
            1_000,
            checksum,
            0,
        )?);
    }
    let eligible = register(&mut registry, &mut pager, &mut alloc, 4_096, 100, 0)?;
    let mut references = blocked.clone();
    references.push(eligible);
    registry.apply_reference_changes(&references, &[], 1, &mut pager)?;
    let root = persist(registry, &mut pager, &mut alloc)?;
    let mut registry = PayloadRegistry::new(root);
    registry.apply_reference_changes(&[], &[eligible], 2, &mut pager)?;
    let root = persist(registry, &mut pager, &mut alloc)?;
    let mut registry = PayloadRegistry::new(root);
    registry.apply_reference_changes(&[], &blocked, 3, &mut pager)?;
    let root = persist(registry, &mut pager, &mut alloc)?;
    let mut pager = reopen(&pager);
    let mut registry = PayloadRegistry::new(root);
    let fresh = register(&mut registry, &mut pager, &mut alloc, 1_000, 200, 2)?;
    let second = register(&mut registry, &mut pager, &mut alloc, 1_000, 201, 2)?;
    assert_eq!(registry.queue_entries_read, REUSE_SCAN_LIMIT);
    assert_eq!(registry.entries.len(), 2);
    assert_ne!(fresh.first_page_id, eligible.first_page_id);
    registry.apply_reference_changes(&[fresh, second], &[], 4, &mut pager)?;
    let root = persist(registry, &mut pager, &mut alloc)?;
    let mut pager = reopen(&pager);
    let mut registry = PayloadRegistry::new(root);
    let reused = registry.reserve(&mut pager, 1_000, 2, &mut alloc)?;
    assert_eq!(registry.queue_entries_read, REUSE_SCAN_LIMIT);
    assert_eq!(registry.entries.len(), 1);
    assert_eq!(reused.first_page_id, eligible.first_page_id);
    assert_eq!(reused.capacity_pages, 2);
    assert_eq!(
        encode_entry(&stored(&mut pager, root, eligible)?).len(),
        RECORD_SIZE
    );
    Ok(())
}

#[test]
fn reference_errors_leave_logical_counts_unchanged() -> Result<()> {
    let mut pager = Pager::new(MemoryBackend::new(), 8);
    let mut alloc = PageAllocator::new(1);
    let mut registry = PayloadRegistry::new(0);
    let reference = register(&mut registry, &mut pager, &mut alloc, 1_000, 11, 0)?;
    registry.apply_reference_changes(&[reference], &[], 1, &mut pager)?;
    let root = persist(registry, &mut pager, &mut alloc)?;
    let mut registry = PayloadRegistry::new(root);
    assert!(matches!(
        registry.apply_reference_changes(&[], &[reference, reference], 2, &mut pager),
        Err(EngineError::Corruption(_))
    ));
    let other = PayloadRef {
        checksum: 22,
        ..reference
    };
    assert!(matches!(
        registry.apply_reference_changes(&[reference], &[other], 2, &mut pager),
        Err(EngineError::Corruption(_))
    ));
    assert!(registry.changes.is_empty());
    assert_eq!(registry.entries[&reference.first_page_id].references, 1);
    assert!(registry.finish(&mut pager, &mut alloc)?.is_none());
    Ok(())
}

#[test]
fn fresh_payload_reservations_do_not_consume_tree_reuse_or_overflow() -> Result<()> {
    let mut alloc = PageAllocator::with_reusable(100, vec![2, 3]);
    assert_eq!(alloc.reserve_fresh_pages(3)?, 100);
    assert_eq!(alloc.next_page_id(), 103);
    assert_eq!(alloc.allocate(), 2);
    assert_eq!(alloc.allocate(), 3);
    assert!(alloc.freed().is_empty());
    assert!(alloc.reserve_fresh_pages(0).is_err());
    assert_eq!(alloc.next_page_id(), 103);
    let mut alloc = PageAllocator::new(u64::MAX - 1);
    assert!(alloc.reserve_fresh_pages(2).is_err());
    assert_eq!(alloc.next_page_id(), u64::MAX - 1);
    let maximum_next = u64::MAX / PAGE_SIZE as u64 + 1;
    let mut alloc = PageAllocator::new(maximum_next);
    assert!(alloc.reserve_fresh_pages(1).is_err());
    assert_eq!(alloc.next_page_id(), maximum_next);
    Ok(())
}

#[test]
fn malformed_queue_keys_return_corruption() -> Result<()> {
    assert_eq!(decode_queue_key(&queue_key(2, 3, 4))?, (2, 3, 4));
    let valid = queue_key(2, 3, 4);
    let mut wrong_prefix = valid.clone();
    wrong_prefix[0] = 0xfe;
    for key in [
        Vec::new(),
        valid[..24].to_vec(),
        wrong_prefix,
        queue_key(0, 3, 4),
        queue_key(2, 0, 4),
        queue_key(2, 3, 0),
    ] {
        assert!(matches!(
            decode_queue_key(&key),
            Err(EngineError::Corruption(_))
        ));
    }
    Ok(())
}
