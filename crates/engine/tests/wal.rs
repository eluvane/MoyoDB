mod common;

use moyodb_engine::layout::{ValueKind, PAGE_SIZE};
use moyodb_engine::page::{encode_leaf_page, LeafCell};
use moyodb_engine::pager::Pager;
use moyodb_engine::storage::memory::MemoryBackend;
use moyodb_engine::wal::{
    append_commit_record, append_page_image_record, append_transaction, replay_wal_transactions,
    scan_wal, CommitRecord,
};

fn leaf_image(page_id: u64, marker: u8) -> Vec<u8> {
    encode_leaf_page(
        page_id,
        0,
        0,
        &[LeafCell {
            key: vec![marker],
            value: vec![marker],
            value_kind: ValueKind::Inline,
            total_value_len: 1,
            overflow_head_page_id: 0,
        }],
    )
    .unwrap()
}

#[test]
fn wal_append_scan_and_replay() {
    let mut wal = MemoryBackend::new();
    let mut offset = 0u64;
    let page = leaf_image(3, 7);
    append_page_image_record(&mut wal, &mut offset, 1, 3, &page).unwrap();
    append_commit_record(
        &mut wal,
        &mut offset,
        CommitRecord {
            txid: 1,
            new_catalog_root_page_id: 3,
            new_next_page_id: 4,
            changed_page_count: 1,
        },
    )
    .unwrap();
    wal.flush().unwrap();

    let txs = scan_wal(&wal).unwrap();
    assert_eq!(txs.len(), 1);
    assert_eq!(txs[0].page_images[0].page_id, 3);

    let main = MemoryBackend::new();
    let mut pager = Pager::new(main.clone(), 32);
    replay_wal_transactions(&mut pager, &txs).unwrap();
    let read = pager.read_page(3).unwrap();
    assert_eq!(read, page);
}

#[test]
fn wal_batch_append_matches_page_count() {
    let mut wal = MemoryBackend::new();
    let mut offset = 0u64;
    let page = leaf_image(11, 9);
    assert_eq!(page.len(), PAGE_SIZE);
    let err = append_transaction(
        &mut wal,
        &mut offset,
        7,
        &[(11, page)],
        &CommitRecord {
            txid: 7,
            new_catalog_root_page_id: 11,
            new_next_page_id: 12,
            changed_page_count: 2,
        },
    )
    .unwrap_err();
    assert_eq!(err.code(), "SerializationError");
}

#[test]
fn wal_commit_with_mismatched_page_count_is_ignored() {
    let mut wal = MemoryBackend::new();
    let mut offset = 0u64;
    let page = leaf_image(3, 5);
    append_page_image_record(&mut wal, &mut offset, 1, 3, &page).unwrap();
    append_commit_record(
        &mut wal,
        &mut offset,
        CommitRecord {
            txid: 1,
            new_catalog_root_page_id: 3,
            new_next_page_id: 4,
            changed_page_count: 2,
        },
    )
    .unwrap();
    wal.flush().unwrap();

    let txs = scan_wal(&wal).unwrap();
    assert!(txs.is_empty());
}

#[test]
fn checksummed_garbage_page_image_is_corruption() {
    let mut wal = MemoryBackend::new();
    let mut offset = 0u64;
    append_page_image_record(&mut wal, &mut offset, 1, 3, &vec![7u8; PAGE_SIZE]).unwrap();
    append_commit_record(
        &mut wal,
        &mut offset,
        CommitRecord {
            txid: 1,
            new_catalog_root_page_id: 3,
            new_next_page_id: 4,
            changed_page_count: 1,
        },
    )
    .unwrap();

    assert_eq!(scan_wal(&wal).unwrap_err().code(), "CorruptionError");
}

#[test]
fn page_image_beyond_next_page_id_is_corruption() {
    let mut wal = MemoryBackend::new();
    let mut offset = 0u64;
    append_page_image_record(&mut wal, &mut offset, 1, 9, &leaf_image(9, 1)).unwrap();
    append_commit_record(
        &mut wal,
        &mut offset,
        CommitRecord {
            txid: 1,
            new_catalog_root_page_id: 9,
            new_next_page_id: 5,
            changed_page_count: 1,
        },
    )
    .unwrap();

    assert_eq!(scan_wal(&wal).unwrap_err().code(), "CorruptionError");
}

#[test]
fn page_image_with_foreign_page_id_is_corruption() {
    let mut wal = MemoryBackend::new();
    let mut offset = 0u64;
    append_page_image_record(&mut wal, &mut offset, 1, 3, &leaf_image(4, 1)).unwrap();
    append_commit_record(
        &mut wal,
        &mut offset,
        CommitRecord {
            txid: 1,
            new_catalog_root_page_id: 3,
            new_next_page_id: 8,
            changed_page_count: 1,
        },
    )
    .unwrap();

    assert_eq!(scan_wal(&wal).unwrap_err().code(), "CorruptionError");
}

#[test]
fn garbage_in_uncommitted_tail_is_ignored() {
    let mut wal = MemoryBackend::new();
    let mut offset = 0u64;
    append_page_image_record(&mut wal, &mut offset, 1, 3, &vec![0x42; PAGE_SIZE]).unwrap();

    assert!(scan_wal(&wal).unwrap().is_empty());
}

#[test]
fn non_increasing_txids_are_corruption() {
    let mut wal = MemoryBackend::new();
    let mut offset = 0u64;
    for _ in 0..2 {
        append_transaction(
            &mut wal,
            &mut offset,
            5,
            &[(3, leaf_image(3, 1))],
            &CommitRecord {
                txid: 5,
                new_catalog_root_page_id: 3,
                new_next_page_id: 4,
                changed_page_count: 1,
            },
        )
        .unwrap();
    }

    assert_eq!(scan_wal(&wal).unwrap_err().code(), "CorruptionError");
}

#[test]
fn torn_tail_after_valid_commit_is_dropped() {
    let mut wal = MemoryBackend::new();
    let mut offset = 0u64;
    append_transaction(
        &mut wal,
        &mut offset,
        1,
        &[(3, leaf_image(3, 1))],
        &CommitRecord {
            txid: 1,
            new_catalog_root_page_id: 3,
            new_next_page_id: 4,
            changed_page_count: 1,
        },
    )
    .unwrap();
    let mut second = MemoryBackend::new();
    let mut second_offset = 0u64;
    append_transaction(
        &mut second,
        &mut second_offset,
        2,
        &[(3, leaf_image(3, 2))],
        &CommitRecord {
            txid: 2,
            new_catalog_root_page_id: 3,
            new_next_page_id: 4,
            changed_page_count: 1,
        },
    )
    .unwrap();
    let torn = second.read_at(0, second_offset as usize - 7).unwrap();
    wal.write_at(offset, &torn).unwrap();

    let txs = scan_wal(&wal).unwrap();
    assert_eq!(txs.len(), 1);
    assert_eq!(txs[0].txid, 1);
}
