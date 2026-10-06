mod common;

use moyodb_engine::checksum::checksum_with_zeroed_region;
use moyodb_engine::layout::{
    ValueKind, PAGE_SIZE, WAL_COMMIT_BODY_SIZE, WAL_PAGE_IMAGE_BODY_HEADER_SIZE,
    WAL_RECORD_CHECKSUM_OFFSET,
};
use moyodb_engine::page::{encode_leaf_page, LeafCell};
use moyodb_engine::pager::Pager;
use moyodb_engine::recovery::{initialize_empty_db, recover_if_needed};
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

fn seal_wal_record(record: &mut [u8]) {
    let checksum = checksum_with_zeroed_region(record, WAL_RECORD_CHECKSUM_OFFSET, 4);
    record[WAL_RECORD_CHECKSUM_OFFSET..WAL_RECORD_CHECKSUM_OFFSET + 4]
        .copy_from_slice(&checksum.to_le_bytes());
}

fn commit_for(txid: u64) -> CommitRecord {
    CommitRecord {
        txid,
        new_catalog_root_page_id: 1,
        new_next_page_id: 2,
        changed_page_count: 1,
    }
}

#[test]
fn torn_unknown_wal_tag_keeps_the_committed_prefix() {
    let mut wal = MemoryBackend::new();
    let mut offset = 0u64;
    append_transaction(
        &mut wal,
        &mut offset,
        1,
        &[(1, leaf_image(1, 1))],
        &commit_for(1),
    )
    .unwrap();
    let mut torn = vec![0u8; 16];
    torn[..4].copy_from_slice(b"WAL1");
    torn[4] = 99;
    torn[12..16].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
    wal.write_at(offset, &torn).unwrap();

    let txs = scan_wal(&wal).unwrap();
    assert_eq!(txs.len(), 1);
    assert_eq!(txs[0].txid, 1);
}

#[test]
fn checksummed_unknown_wal_records_are_not_truncated() {
    let unknown_tag = {
        let mut record = vec![0u8; 16 + WAL_COMMIT_BODY_SIZE];
        record[..4].copy_from_slice(b"WAL1");
        record[4] = 99;
        record[8..12].copy_from_slice(&(WAL_COMMIT_BODY_SIZE as u32).to_le_bytes());
        seal_wal_record(&mut record);
        record
    };
    let unknown_length = {
        let mut record = vec![0u8; 16];
        record[..4].copy_from_slice(b"WAL1");
        record[4] = 2;
        seal_wal_record(&mut record);
        record
    };
    let header_flags = {
        let mut record = vec![0u8; 16 + WAL_COMMIT_BODY_SIZE];
        record[..4].copy_from_slice(b"WAL1");
        record[4] = 2;
        record[5] = 1;
        record[8..12].copy_from_slice(&(WAL_COMMIT_BODY_SIZE as u32).to_le_bytes());
        seal_wal_record(&mut record);
        record
    };
    let commit_flags = {
        let mut record = vec![0u8; 16 + WAL_COMMIT_BODY_SIZE];
        record[..4].copy_from_slice(b"WAL1");
        record[4] = 2;
        record[8..12].copy_from_slice(&(WAL_COMMIT_BODY_SIZE as u32).to_le_bytes());
        record[16 + 28..16 + 32].copy_from_slice(&1u32.to_le_bytes());
        seal_wal_record(&mut record);
        record
    };
    let page_flags = {
        let payload_len = WAL_PAGE_IMAGE_BODY_HEADER_SIZE + PAGE_SIZE;
        let mut record = vec![0u8; 16 + payload_len];
        record[..4].copy_from_slice(b"WAL1");
        record[4] = 1;
        record[8..12].copy_from_slice(&(payload_len as u32).to_le_bytes());
        record[16 + 20..16 + 24].copy_from_slice(&1u32.to_le_bytes());
        seal_wal_record(&mut record);
        record
    };

    for (record, message) in [
        (unknown_tag, "unsupported WAL record tag 99"),
        (unknown_length, "unsupported WAL payload length 0"),
        (header_flags, "unsupported WAL header flags"),
        (commit_flags, "unsupported WAL commit flags"),
        (page_flags, "unsupported WAL page image flags"),
    ] {
        let mut manifest = MemoryBackend::new();
        let main = MemoryBackend::new();
        let mut pager = Pager::new(main.clone(), 8);
        let mut wal = MemoryBackend::new();
        let base = initialize_empty_db(&mut manifest, &mut pager, &mut wal, 7).unwrap();
        let mut offset = 0u64;
        append_transaction(
            &mut wal,
            &mut offset,
            1,
            &[(1, leaf_image(1, 1))],
            &commit_for(1),
        )
        .unwrap();
        wal.write_at(offset, &record).unwrap();
        offset += record.len() as u64;
        append_transaction(
            &mut wal,
            &mut offset,
            2,
            &[(1, leaf_image(1, 2))],
            &commit_for(2),
        )
        .unwrap();
        wal.flush().unwrap();

        let err = scan_wal(&wal).unwrap_err();
        assert_eq!(err.code(), "CorruptionError", "{message}");
        assert!(err.to_string().contains(message), "{err}");

        let wal_before = wal.read_at(0, wal.len().unwrap() as usize).unwrap();
        let main_before = main.read_at(0, main.len().unwrap() as usize).unwrap();
        let manifest_before = manifest
            .read_at(0, manifest.len().unwrap() as usize)
            .unwrap();
        let err = recover_if_needed(&mut manifest, &mut pager, &mut wal, &base).unwrap_err();
        assert_eq!(err.code(), "CorruptionError", "{message}");
        assert!(err.to_string().contains(message), "{err}");
        assert_eq!(
            wal.read_at(0, wal.len().unwrap() as usize).unwrap(),
            wal_before,
            "{message}"
        );
        assert_eq!(
            main.read_at(0, main.len().unwrap() as usize).unwrap(),
            main_before,
            "{message}"
        );
        assert_eq!(
            manifest
                .read_at(0, manifest.len().unwrap() as usize)
                .unwrap(),
            manifest_before,
            "{message}"
        );
    }
}
