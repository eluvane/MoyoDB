use moyodb_engine::checksum::checksum_with_zeroed_region;
use moyodb_engine::layout::{decode_superblock_slot, PageKind, FORMAT_VERSION, PAGE_SIZE};
use moyodb_engine::page::{
    decode_page, encode_internal_page, encode_leaf_page, encode_overflow_page,
};
use moyodb_engine::snapshot::decode_snapshot;
use moyodb_engine::wal::{append_transaction, scan_wal, scan_wal_index};
use moyodb_engine::{
    ChangeFeedPolicy, Engine, EngineError, MemoryBackend, MemoryBundle, OpenConfig, ScanRange,
    TxMode,
};

const DB_NAME: &str = "compatibility-release-1.0.1";
const CHECKPOINT_MANIFEST: &[u8] = include_bytes!("fixtures/compatibility/checkpoint-manifest.bin");
const CHECKPOINT_MAIN: &[u8] = include_bytes!("fixtures/compatibility/checkpoint-main.bin");
const CHECKPOINT_WAL: &[u8] = include_bytes!("fixtures/compatibility/checkpoint-wal.bin");
const RECOVERY_MANIFEST: &[u8] = include_bytes!("fixtures/compatibility/recovery-manifest.bin");
const RECOVERY_MAIN: &[u8] = include_bytes!("fixtures/compatibility/recovery-main.bin");
const RECOVERY_WAL: &[u8] = include_bytes!("fixtures/compatibility/recovery-wal.bin");
const SNAPSHOT_V1: &[u8] = include_bytes!("fixtures/compatibility/snapshot-v1.bin");
const SNAPSHOT_V2: &[u8] = include_bytes!("fixtures/compatibility/snapshot-v2.bin");
const SNAPSHOT_V3: &[u8] = include_bytes!("fixtures/compatibility/snapshot-v3.bin");

fn fixture_bundle(recovery: bool) -> MemoryBundle {
    let (manifest, main, wal) = if recovery {
        (RECOVERY_MANIFEST, RECOVERY_MAIN, RECOVERY_WAL)
    } else {
        (CHECKPOINT_MANIFEST, CHECKPOINT_MAIN, CHECKPOINT_WAL)
    };
    MemoryBundle {
        manifest: MemoryBackend::from_durable(manifest.to_vec()),
        main: MemoryBackend::from_durable(main.to_vec()),
        wal: MemoryBackend::from_durable(wal.to_vec()),
    }
}

fn current_bundle(recovery: bool) -> MemoryBundle {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open(
        DB_NAME,
        bundle.files(),
        OpenConfig {
            checkpoint_wal_bytes: u64::MAX,
            checkpoint_dirty_pages: usize::MAX,
            ..OpenConfig::default()
        },
    )
    .unwrap();
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine
        .set_change_feed_policy(
            tx,
            ChangeFeedPolicy {
                enabled: true,
                retain_txids: Some(64),
            },
        )
        .unwrap();
    engine.commit_tx(tx).unwrap();
    engine.import_snapshot(SNAPSHOT_V3).unwrap();
    if !recovery {
        engine.checkpoint().unwrap();
    }
    let [manifest, main, wal] = durable_images(&bundle);
    engine.abandon().unwrap();
    MemoryBundle {
        manifest: MemoryBackend::from_durable(manifest),
        main: MemoryBackend::from_durable(main),
        wal: MemoryBackend::from_durable(wal),
    }
}

fn open(bundle: &MemoryBundle) -> Engine<MemoryBackend> {
    Engine::open(
        DB_NAME,
        bundle.files(),
        OpenConfig {
            create_if_missing: false,
            ..OpenConfig::default()
        },
    )
    .unwrap()
}

fn durable_images(bundle: &MemoryBundle) -> [Vec<u8>; 3] {
    [
        bundle.manifest.durable_snapshot().unwrap(),
        bundle.main.durable_snapshot().unwrap(),
        bundle.wal.durable_snapshot().unwrap(),
    ]
}

fn assert_release_contents(engine: &mut Engine<MemoryBackend>) {
    assert_eq!(engine.schema_version(), 7);
    assert_eq!(engine.store_names(), ["docs", "empty", "legacy"]);
    assert_eq!(
        engine.change_feed_policy(),
        ChangeFeedPolicy {
            enabled: true,
            retain_txids: Some(64),
        }
    );
    let tx = engine.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(
        engine.get(tx, "docs", b"alpha").unwrap(),
        Some(b"release-1.0.1".to_vec())
    );
    assert_eq!(
        engine.get(tx, "docs", &[0, 0xff]).unwrap(),
        Some(vec![0, 1, 0xff, 2])
    );
    assert_eq!(engine.get(tx, "docs", b"expired").unwrap(), None);
    assert_eq!(
        engine.get(tx, "docs", b"live-ttl").unwrap(),
        Some(b"expires-in-2096".to_vec())
    );
    assert_eq!(
        engine.get(tx, "legacy", b"raw").unwrap(),
        Some(b"BDTTL001-raw-user-value".to_vec())
    );
    let overflow: Vec<u8> = (0..9000).map(|index| (index % 251) as u8).collect();
    assert_eq!(engine.get(tx, "docs", b"overflow").unwrap(), Some(overflow));
    for index in 0..48 {
        let key = format!("row:{index:03}");
        let value: Vec<u8> = (0..80).map(|byte| ((index + byte) % 251) as u8).collect();
        assert_eq!(engine.get(tx, "docs", key.as_bytes()).unwrap(), Some(value));
    }
    let rows = engine.scan(tx, "docs", &ScanRange::default()).unwrap();
    assert_eq!(rows.len(), 52);
    assert!(rows.windows(2).all(|pair| pair[0].key < pair[1].key));
    assert!(engine
        .scan(tx, "empty", &ScanRange::default())
        .unwrap()
        .is_empty());
    engine.rollback_tx(tx).unwrap();
}

fn assert_failed_open_preserves_files(bundle: &MemoryBundle, message: &str) {
    let before = durable_images(bundle);
    let error = match Engine::open(DB_NAME, bundle.files(), OpenConfig::default()) {
        Ok(_) => panic!("unsupported format must fail"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "CorruptionError");
    assert!(error.to_string().contains(message), "{error}");
    assert_eq!(durable_images(bundle), before);
    for (file, bytes) in [&bundle.manifest, &bundle.main, &bundle.wal]
        .into_iter()
        .zip(before)
    {
        assert_eq!(file.len().unwrap(), bytes.len() as u64);
        assert_eq!(file.read_at(0, bytes.len()).unwrap(), bytes);
    }
}

fn rewrite_checksum(bytes: &mut [u8], offset: usize) {
    let checksum = checksum_with_zeroed_region(bytes, offset, 4);
    bytes[offset..offset + 4].copy_from_slice(&checksum.to_le_bytes());
}

fn future_wal_record(payload_len: usize) -> Vec<u8> {
    let mut record = vec![0x5a; 16 + payload_len];
    record[..4].copy_from_slice(b"WAL2");
    record[4] = 99;
    record[8..12].copy_from_slice(&(payload_len as u32).to_le_bytes());
    rewrite_checksum(&mut record, 12);
    record
}

#[test]
fn release_1_0_1_checkpoint_is_rejected_without_changing_storage() {
    let bundle = fixture_bundle(false);
    assert_failed_open_preserves_files(&bundle, "unsupported format version 1");
}

#[test]
fn release_1_0_1_recovery_storage_is_rejected_before_wal_replay() {
    let bundle = fixture_bundle(true);
    assert_failed_open_preserves_files(&bundle, "unsupported format version 1");
}

#[test]
fn page_and_wal_encoders_preserve_release_1_0_1_vectors() {
    let kinds: Vec<PageKind> = CHECKPOINT_MAIN
        .as_chunks::<4096>()
        .0
        .iter()
        .map(|bytes| decode_page(bytes).unwrap().header.page_kind)
        .collect();
    assert!(kinds.contains(&PageKind::Leaf));
    assert!(kinds.contains(&PageKind::Internal));
    assert!(kinds.contains(&PageKind::Overflow));
    for bytes in CHECKPOINT_MAIN.as_chunks::<4096>().0 {
        let page = decode_page(bytes).unwrap();
        let header = page.header;
        let encoded = match header.page_kind {
            PageKind::Leaf => encode_leaf_page(
                header.page_id,
                header.level,
                header.right_sibling_page_id,
                &page.leaf_cells,
            ),
            PageKind::Internal => encode_internal_page(
                header.page_id,
                header.level,
                header.right_sibling_page_id,
                &page.internal_cells,
            ),
            PageKind::Overflow => {
                let overflow = page.overflow.unwrap();
                encode_overflow_page(
                    header.page_id,
                    overflow.next_overflow_page_id,
                    &overflow.chunk,
                )
            }
        }
        .unwrap();
        assert_eq!(encoded, bytes, "page {}", header.page_id);
    }
    let transactions = scan_wal(&MemoryBackend::from_durable(RECOVERY_WAL.to_vec())).unwrap();
    let mut encoded = MemoryBackend::new();
    let mut offset = 0;
    for transaction in transactions {
        let images: Vec<_> = transaction
            .page_images
            .into_iter()
            .map(|image| (image.page_id, image.bytes))
            .collect();
        append_transaction(
            &mut encoded,
            &mut offset,
            transaction.txid,
            &images,
            &transaction.commit,
        )
        .unwrap();
    }
    assert_eq!(encoded.read_at(0, offset as usize).unwrap(), RECOVERY_WAL);
}

#[test]
fn current_storage_accepts_snapshot_contents_and_recovers_new_commits() {
    for recovery in [false, true] {
        let bundle = current_bundle(recovery);
        let mut engine = open(&bundle);
        assert_release_contents(&mut engine);
        let previous_txid = engine.stats().unwrap().last_committed_txid;
        let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
        engine.put(tx, "docs", b"alpha", b"updated").unwrap();
        engine.put(tx, "legacy", b"next", b"raw-write").unwrap();
        engine.delete(tx, "docs", &[0, 0xff]).unwrap();
        engine.create_store(tx, "new-store").unwrap();
        assert_eq!(engine.commit_tx(tx).unwrap(), previous_txid + 1);
        let mut reopened = Engine::open(
            DB_NAME,
            bundle.crash_recovered_files(),
            OpenConfig::default(),
        )
        .unwrap();
        let tx = reopened.begin_tx(TxMode::Readonly).unwrap();
        assert_eq!(
            reopened.get(tx, "docs", b"alpha").unwrap(),
            Some(b"updated".to_vec())
        );
        assert_eq!(
            reopened.get(tx, "legacy", b"next").unwrap(),
            Some(b"raw-write".to_vec())
        );
        assert_eq!(reopened.get(tx, "docs", &[0, 0xff]).unwrap(), None);
        assert_eq!(reopened.schema_version(), 7);
        assert_eq!(
            reopened.catalog()["legacy"].flags,
            moyodb_engine::value::STORE_FLAG_VALUE_REVISION
        );
        reopened.rollback_tx(tx).unwrap();
        let snapshot = decode_snapshot(&reopened.export_snapshot().unwrap()).unwrap();
        assert_eq!(snapshot.source_last_committed_txid, previous_txid + 1);
        assert!(snapshot
            .stores
            .iter()
            .any(|store| store.name == "new-store"));
    }
}

#[test]
fn release_1_0_1_snapshot_versions_import_and_remain_writable() {
    for (bytes, schema_version, source_txid) in [
        (SNAPSHOT_V1, 0, 11),
        (SNAPSHOT_V2, 7, 11),
        (SNAPSHOT_V3, 7, 2),
    ] {
        let bundle = MemoryBundle::new();
        let mut engine = Engine::open(DB_NAME, bundle.files(), OpenConfig::default()).unwrap();
        assert_eq!(engine.import_snapshot(bytes).unwrap(), source_txid + 1);
        assert_eq!(engine.schema_version(), schema_version);
        let tx = engine.begin_tx(TxMode::Readonly).unwrap();
        assert_eq!(
            engine.get(tx, "legacy", b"raw").unwrap(),
            Some(b"BDTTL001-raw-user-value".to_vec())
        );
        engine.rollback_tx(tx).unwrap();
        let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
        engine.put(tx, "legacy", b"next", b"new-value").unwrap();
        engine.commit_tx(tx).unwrap();
        let mut reopened = Engine::open(
            DB_NAME,
            bundle.crash_recovered_files(),
            OpenConfig::default(),
        )
        .unwrap();
        let tx = reopened.begin_tx(TxMode::Readonly).unwrap();
        assert_eq!(
            reopened.get(tx, "legacy", b"next").unwrap(),
            Some(b"new-value".to_vec())
        );
        reopened.rollback_tx(tx).unwrap();
        assert_eq!(
            decode_snapshot(&reopened.export_snapshot().unwrap())
                .unwrap()
                .schema_version,
            schema_version
        );
    }
}

#[test]
fn future_manifest_version_never_falls_back_to_an_older_slot() {
    for slot in 0..2 {
        let mut bundle = current_bundle(false);
        let mut bytes = bundle.manifest.durable_snapshot().unwrap();
        let slot_bytes = &mut bytes[slot * 4096..(slot + 1) * 4096];
        slot_bytes[8..12].copy_from_slice(&(FORMAT_VERSION + 1).to_le_bytes());
        rewrite_checksum(slot_bytes, 64);
        bundle.manifest = MemoryBackend::from_durable(bytes);
        assert_failed_open_preserves_files(
            &bundle,
            &format!("unsupported format version {}", FORMAT_VERSION + 1),
        );
    }
}

#[test]
fn future_page_magic_fails_without_changing_storage() {
    let mut bundle = current_bundle(false);
    let manifest = bundle.manifest.durable_snapshot().unwrap();
    let superblock = decode_superblock_slot(1, &manifest[4096..])
        .unwrap()
        .unwrap();
    let mut bytes = bundle.main.durable_snapshot().unwrap();
    let start = (superblock.catalog_root_page_id as usize - 1) * PAGE_SIZE;
    let page = &mut bytes[start..start + PAGE_SIZE];
    page[..4].copy_from_slice(b"PAG2");
    rewrite_checksum(page, 4);
    bundle.main = MemoryBackend::from_durable(bytes);
    assert_failed_open_preserves_files(&bundle, "page magic mismatch");
}

#[test]
fn future_snapshot_version_rejects_import_without_changing_state() {
    let bundle = current_bundle(false);
    let mut engine = open(&bundle);
    let before = durable_images(&bundle);
    let previous_txid = engine.stats().unwrap().last_committed_txid;
    let mut bytes = SNAPSHOT_V3.to_vec();
    bytes[8..12].copy_from_slice(&4u32.to_le_bytes());
    rewrite_checksum(&mut bytes, 24);
    let error = engine.import_snapshot(&bytes).unwrap_err();
    assert_eq!(error.code(), "CorruptionError");
    assert!(error.to_string().contains("unsupported snapshot version 4"));
    assert_release_contents(&mut engine);
    assert_eq!(engine.stats().unwrap().last_committed_txid, previous_txid);
    assert_eq!(durable_images(&bundle), before);
}

#[test]
fn future_wal_version_rejects_recovery_before_publishing_any_commit() {
    let current = current_bundle(true);
    let current_wal = current.wal.durable_snapshot().unwrap();
    let first_commit_end = scan_wal_index(&current.wal).unwrap()[0].end_offset as usize;
    for offset in [0, first_commit_end] {
        let mut bytes = current_wal.clone();
        let payload_len =
            u32::from_le_bytes(bytes[offset + 8..offset + 12].try_into().unwrap()) as usize;
        let record = &mut bytes[offset..offset + 16 + payload_len];
        record[..4].copy_from_slice(b"WAL2");
        rewrite_checksum(record, 12);
        let mut bundle = current_bundle(true);
        bundle.wal = MemoryBackend::from_durable(bytes);
        assert_failed_open_preserves_files(&bundle, "unsupported WAL format version 2");
    }
}

#[test]
fn future_wal_unknown_tags_and_large_records_are_version_errors() {
    for payload_len in [0, 140_000] {
        let mut bundle = current_bundle(true);
        let mut bytes = bundle.wal.durable_snapshot().unwrap();
        bytes.extend_from_slice(&future_wal_record(payload_len));
        bundle.wal = MemoryBackend::from_durable(bytes);
        assert_failed_open_preserves_files(&bundle, "unsupported WAL format version 2");
    }
}

#[test]
fn damaged_future_or_random_wal_tails_keep_the_current_committed_prefix() {
    let future = future_wal_record(80);
    let mut bad_checksum = future.clone();
    *bad_checksum.last_mut().unwrap() ^= 0xff;
    let mut random_magic = future.clone();
    random_magic[..4].copy_from_slice(b"NOPE");
    rewrite_checksum(&mut random_magic, 12);
    for tail in [
        future[..15].to_vec(),
        future[..future.len() - 1].to_vec(),
        bad_checksum,
        random_magic,
    ] {
        let mut bundle = current_bundle(true);
        let expected = decode_snapshot(SNAPSHOT_V3).unwrap();
        let mut bytes = bundle.wal.durable_snapshot().unwrap();
        bytes.extend_from_slice(&tail);
        bundle.wal = MemoryBackend::from_durable(bytes);
        let mut engine = open(&bundle);
        assert_release_contents(&mut engine);
        let recovered = decode_snapshot(&engine.export_snapshot().unwrap()).unwrap();
        assert_eq!(recovered.schema_version, expected.schema_version);
        assert_eq!(recovered.stores, expected.stores);
    }
}

#[test]
fn current_database_identity_is_preserved() {
    let bundle = current_bundle(false);
    let before = durable_images(&bundle);
    assert!(matches!(
        Engine::open("wrong-name", bundle.files(), OpenConfig::default()),
        Err(EngineError::Corruption(_))
    ));
    assert_eq!(durable_images(&bundle), before);
}
