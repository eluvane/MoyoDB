use super::*;
use crate::checksum::work as checksum_work;
use crate::layout::{
    wal_record_total_len, PageKind, PAGE_HEADER_SIZE, PAGE_SIZE, WAL_COMMIT_BODY_SIZE,
    WAL_PAGE_IMAGE_BODY_HEADER_SIZE, WAL_RECORD_HEADER_SIZE,
};
use crate::page::decode_page_header_verified;
use crate::prepared_value::work::with_generic_encoding;
use crate::storage::memory::{MemoryBackend, MemoryBundle};
use crate::wal::scan_wal;

fn engine() -> Result<(MemoryBundle, Engine<MemoryBackend>)> {
    let bundle = MemoryBundle::new();
    let engine = Engine::open(
        "paired-crc",
        bundle.files(),
        OpenConfig {
            checkpoint_wal_bytes: u64::MAX,
            checkpoint_dirty_pages: usize::MAX,
            ..OpenConfig::default()
        },
    )?;
    Ok((bundle, engine))
}

#[test]
fn engine_commit_hashes_shared_payload_once_with_full_page_and_wal_coverage() -> Result<()> {
    for (count, len) in [(1, 64 * 1024), (8, 64 * 1024), (8, 1024 * 1024)] {
        let (bundle, mut engine) = engine()?;
        let setup = engine.begin_tx(TxMode::Readwrite)?;
        engine.create_store(setup, "kv")?;
        engine.commit_tx(setup)?;
        let value = vec![0x53; len];
        let tx = engine.begin_tx(TxMode::Readwrite)?;
        for key in 0u32..count {
            engine.put(tx, "kv", &key.to_be_bytes(), &value)?;
        }
        checksum_work::reset();
        let txid = engine.commit_tx(tx)?;
        let hashed = checksum_work::hashed_bytes();
        let transactions = scan_wal(&bundle.wal)?;
        let committed = transactions.last().unwrap();
        assert_eq!(committed.txid, txid);
        let mut overflow = 0;
        for page in &committed.page_images {
            if decode_page_header_verified(&page.bytes)?.page_kind == PageKind::Overflow {
                overflow += 1;
            }
        }
        let pages = committed.page_images.len();
        let page_and_record_headers = overflow * (PAGE_HEADER_SIZE + 12)
            + pages * (WAL_RECORD_HEADER_SIZE + WAL_PAGE_IMAGE_BODY_HEADER_SIZE);
        let expected = count as usize * (len + VALUE_ENVELOPE_HEADER_SIZE + 14 + 2 + 4)
            + page_and_record_headers
            + (pages - overflow) * PAGE_SIZE
            + wal_record_total_len(WAL_COMMIT_BODY_SIZE);
        assert_eq!(hashed, expected, "values={count}, value_len={len}");
        let previous = pages
            * (PAGE_SIZE + WAL_RECORD_HEADER_SIZE + WAL_PAGE_IMAGE_BODY_HEADER_SIZE)
            + wal_record_total_len(WAL_COMMIT_BODY_SIZE);
        assert!(previous - hashed >= count as usize * len);
    }
    Ok(())
}

fn fix_expiry(engine: &mut Engine<MemoryBackend>, tx: u64, store: &str, key: &[u8]) -> Result<()> {
    let stage = engine
        .txns
        .get_mut(&tx)
        .unwrap()
        .readwrite_mut()?
        .stores
        .get_mut(store)
        .unwrap();
    let MutationValue::Put(value) = stage.mutations.get_mut(key).unwrap() else {
        panic!("the staged value must be a put");
    };
    value.expires_at_ms = Some(1 << 62);
    Ok(())
}

fn complete_commit_bytes() -> Result<Vec<Vec<u8>>> {
    let (bundle, mut engine) = engine()?;
    let names = [
        "raw".to_string(),
        "ttl".to_string(),
        "gzip".to_string(),
        "s".repeat(255),
    ];
    let flags = [
        0,
        store_flags_for_user_store(StoreCompression::None),
        store_flags_for_user_store(StoreCompression::Gzip),
        store_flags_for_user_store(StoreCompression::Deflate),
    ];
    let setup = engine.begin_tx(TxMode::Readwrite)?;
    engine.set_change_feed_policy(
        setup,
        ChangeFeedPolicy {
            enabled: true,
            retain_txids: Some(2),
        },
    )?;
    for (name, flags) in names.iter().zip(flags) {
        engine.create_store(setup, name)?;
        // Use legacy raw values as well as the supported compression formats.
        engine
            .txns
            .get_mut(&setup)
            .unwrap()
            .readwrite_mut()?
            .stores
            .get_mut(name)
            .unwrap()
            .flags = flags;
    }
    engine.commit_tx(setup)?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    let chunk = crate::page::max_overflow_chunk_len();
    let lengths = [
        0,
        1,
        1008,
        1009,
        1024,
        1025,
        chunk - 17,
        chunk - 16,
        chunk - 15,
        2 * chunk - 16,
        2 * chunk - 15,
        64 * 1024,
    ];
    let keys: Vec<Vec<u8>> = (0..lengths.len())
        .map(|index| {
            vec![
                index as u8;
                if index == 0 {
                    0
                } else if index % 3 == 0 {
                    1024
                } else {
                    index + 1
                }
            ]
        })
        .collect();
    for (name_index, name) in names.iter().enumerate() {
        for (index, (&len, key)) in lengths.iter().zip(&keys).enumerate() {
            let value: Vec<_> = (0..len)
                .map(|offset| (offset * 197 + index * 29 + name_index) as u8)
                .collect();
            engine.put_with_ttl(tx, name, key, &value, (name_index == 1).then_some(60_000))?;
            if name_index == 1 {
                fix_expiry(&mut engine, tx, name, key)?;
            }
        }
    }
    engine.commit_tx(tx)?;
    let mut images = vec![bundle.wal.durable_snapshot().unwrap()];

    let tx = engine.begin_tx(TxMode::Readwrite)?;
    // A TTL write to a legacy raw store requires a full rewrite.
    engine.put_with_ttl(tx, &names[0], &keys[4], &vec![0x91; 8192], Some(60_000))?;
    fix_expiry(&mut engine, tx, &names[0], &keys[4])?;
    for name in &names[..2] {
        engine.delete(tx, name, &keys[0])?;
        engine.delete(tx, name, b"absent")?;
        engine.put(tx, name, &keys[11], &vec![0x72; 16 * 1024])?;
    }
    engine.clear_store(tx, &names[2])?;
    engine.put(tx, &names[2], &keys[3], &vec![0x31; 8192])?;
    engine.commit_tx(tx)?;
    images.push(bundle.wal.durable_snapshot().unwrap());

    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.drop_store(tx, &names[3])?;
    engine.put(tx, &names[1], b"last", &vec![0x42; 64 * 1024])?;
    engine.commit_tx(tx)?;
    images.push(bundle.wal.durable_snapshot().unwrap());
    engine.checkpoint()?;
    images.push(bundle.main.durable_snapshot().unwrap());
    images.push(bundle.manifest.durable_snapshot().unwrap());
    Ok(images)
}

#[test]
fn full_commit_wal_and_checkpoint_are_byte_identical_to_generic_checksumming() -> Result<()> {
    let generic = with_generic_encoding(complete_commit_bytes)?;
    let prepared = complete_commit_bytes()?;
    assert_eq!(prepared, generic);
    Ok(())
}

fn stock_hash_work(count: usize, value_len: usize, batch_size: usize) -> Result<usize> {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open("stock-hash-work", bundle.files(), OpenConfig::default())?;
    let setup = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(setup, "kv")?;
    engine.commit_tx(setup)?;
    // Match bench/workloads.ts keyBytes/valueBytes and transaction sizes.
    // Measure checksum work only after data generation and database setup.
    let entries: Vec<_> = (0..count)
        .map(|index| {
            let key = format!("k{index:012x}___").into_bytes();
            let mut state = (index + 1) as u32;
            let value: Vec<_> = (0..value_len)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    state as u8
                })
                .collect();
            (key, value)
        })
        .collect();
    checksum_work::reset();
    for batch in entries.chunks(batch_size) {
        let tx = engine.begin_tx(TxMode::Readwrite)?;
        engine.put_many(tx, "kv", batch)?;
        engine.commit_tx(tx)?;
    }
    Ok(checksum_work::hashed_bytes())
}

#[test]
#[ignore = "64 MiB stock workload work accounting; run explicitly for performance evidence"]
fn stock_large_write_hash_work_accounting() -> Result<()> {
    for (count, value_len, batch_size) in [(1000, 64 * 1024, 100), (64, 1024 * 1024, 8)] {
        let before = with_generic_encoding(|| stock_hash_work(count, value_len, batch_size))?;
        let after = stock_hash_work(count, value_len, batch_size)?;
        assert!(before - after >= count * value_len);
        eprintln!(
            "stock_hash_work values={count} value_bytes={value_len} batch={batch_size} \
             hashed_before={before} hashed_after={after} removed={}",
            before - after,
        );
    }
    Ok(())
}
