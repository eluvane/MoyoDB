mod common;

use moyodb_engine::layout::PAGE_SIZE;
use moyodb_engine::pager::Pager;
use moyodb_engine::storage::memory::MemoryBackend;
use moyodb_engine::wal::{
    append_commit_record, append_page_image_record, append_transaction, replay_wal_transactions,
    scan_wal, CommitRecord,
};

#[test]
fn wal_append_scan_and_replay() {
    let mut wal = MemoryBackend::new();
    let mut offset = 0u64;
    let page = vec![7u8; PAGE_SIZE];
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
    let page = vec![9u8; PAGE_SIZE];
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
    let page = vec![5u8; PAGE_SIZE];
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
