use moyodb_engine::bytes::{MAX_KEY_BYTES, MAX_STORE_NAME_BYTES};
use moyodb_engine::change_feed::{encode_change_log_key, SYSTEM_CHANGELOG_STORE_NAME};
use moyodb_engine::layout::{page_offset, PageKind, PAGE_HEADER_SIZE, PAGE_SIZE};
use moyodb_engine::page::{decode_page, max_overflow_chunk_len};
use moyodb_engine::pager::Pager;
use moyodb_engine::storage::memory::{MemoryBackend, MemoryBundle};
use moyodb_engine::{
    ChangeFeedOptions, ChangeFeedPolicy, ChangeKind, Engine, EngineError, Failpoint, OpenConfig,
    Result, TxMode,
};

fn bytes(len: usize, seed: u32) -> Vec<u8> {
    let mut state = seed;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect()
}

fn config() -> OpenConfig {
    OpenConfig {
        checkpoint_wal_bytes: u64::MAX,
        checkpoint_dirty_pages: usize::MAX,
        ..OpenConfig::default()
    }
}

fn seeded(name: &str, store: &str) -> Result<(MemoryBundle, Engine<MemoryBackend>, u64)> {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open(name, bundle.files(), config())?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(tx, store)?;
    let seed = engine.commit_tx(tx)?;
    engine.checkpoint()?;
    Ok((bundle, engine, seed))
}

#[test]
fn large_user_and_feed_values_recover_at_each_publication_boundary() -> Result<()> {
    let store = "s".repeat(MAX_STORE_NAME_BYTES);
    let entries = [
        (vec![0x11; MAX_KEY_BYTES], bytes(64 * 1024, 0x17_031)),
        (vec![0x22; MAX_KEY_BYTES], bytes(1024 * 1024, 0x27_013)),
    ];
    for failpoint in [
        Failpoint::AfterWalFlush,
        Failpoint::AfterMainFlush,
        Failpoint::BeforeSuperblockFlush,
    ] {
        let (bundle, mut engine, seed) = seeded("paired-value-boundaries", &store)?;
        let tx = engine.begin_tx(TxMode::Readwrite)?;
        for (key, value) in &entries {
            engine.put_with_ttl(tx, &store, key, value, Some(86_400_000))?;
        }
        engine.set_failpoint(Some(failpoint));
        assert!(matches!(
            engine.commit_tx(tx),
            Err(EngineError::InjectedFailure(_))
        ));
        assert!(engine.needs_recovery());
        drop(engine);

        // Reopen only flushed snapshots. Any unflushed memory is discarded.
        let mut recovered = Engine::open(
            "paired-value-boundaries",
            bundle.crash_recovered_files(),
            config(),
        )?;
        let reader = recovered.begin_tx(TxMode::Readonly)?;
        for (key, value) in &entries {
            assert_eq!(
                recovered.get(reader, &store, key)?.as_deref(),
                Some(value.as_slice())
            );
        }
        recovered.rollback_tx(reader)?;
        let feed = recovered.changes_since(seed, ChangeFeedOptions::default())?;
        assert_eq!(feed.changes.len(), entries.len());
        for (record, (key, value)) in feed.changes.iter().zip(&entries) {
            assert_eq!(record.store, store);
            assert_eq!(record.kind, ChangeKind::Put);
            assert_eq!(&record.key, key);
            assert_eq!(record.value.as_deref(), Some(value.as_slice()));
        }
        // Repeated recovery must preserve the latest change-feed transaction.
        let latest = feed.latest_tx_id;
        recovered.recover()?;
        assert_eq!(
            recovered
                .changes_since(seed, ChangeFeedOptions::default())?
                .latest_tx_id,
            latest
        );
    }
    Ok(())
}

fn last_overflow_page(main: &MemoryBackend, root: u64, key: &[u8]) -> Result<(u64, usize)> {
    let mut pager = Pager::new(main.clone(), 32);
    let mut current = root;
    loop {
        let page = decode_page(&pager.read_page(current)?)?;
        match page.header.page_kind {
            PageKind::Internal => {
                let index = page
                    .internal_cells
                    .partition_point(|cell| cell.separator.as_slice() <= key);
                current = page.internal_cells[index.saturating_sub(1)].child_page_id;
            }
            PageKind::Leaf => {
                let cell = page.leaf_cells.iter().find(|cell| cell.key == key).unwrap();
                current = cell.overflow_head_page_id;
                assert_ne!(current, 0);
                break;
            }
            PageKind::Overflow => panic!("tree node unexpectedly contains overflow data"),
        }
    }
    loop {
        let page = decode_page(&pager.read_page(current)?)?;
        let overflow = page.overflow.unwrap();
        if overflow.next_overflow_page_id == 0 {
            return Ok((current, overflow.chunk.len()));
        }
        current = overflow.next_overflow_page_id;
    }
}

#[test]
fn paired_page_checksums_still_cover_headers_payloads_and_padding() -> Result<()> {
    let (bundle, mut engine, seed) = seeded("paired-value-corruption", "docs")?;
    let value = bytes(64 * 1024, 0x79_033);
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.put(tx, "docs", b"key", &value)?;
    let committed = engine.commit_tx(tx)?;
    engine.checkpoint()?;
    let user_root = engine.catalog()["docs"].store_root_page_id;
    let feed_root = engine.catalog()[SYSTEM_CHANGELOG_STORE_NAME].store_root_page_id;
    let feed_key = encode_change_log_key(committed, 0);
    let targets = [
        (false, last_overflow_page(&bundle.main, user_root, b"key")?),
        (
            true,
            last_overflow_page(&bundle.main, feed_root, &feed_key)?,
        ),
    ];
    let original = bundle.main.durable_snapshot().unwrap();
    for (is_feed, (page_id, chunk_len)) in targets {
        assert!(chunk_len > 0 && chunk_len < max_overflow_chunk_len());
        for offset in [
            8,                                     // page id in the page header
            PAGE_HEADER_SIZE,                      // next overflow page id
            PAGE_HEADER_SIZE + 12 + chunk_len / 2, // value payload
            PAGE_SIZE - 1,                         // unused zero padding
        ] {
            let mut corrupted = original.clone();
            corrupted[page_offset(page_id) as usize + offset] ^= 0x80;
            let files = MemoryBundle {
                manifest: MemoryBackend::from_durable(bundle.manifest.durable_snapshot().unwrap()),
                main: MemoryBackend::from_durable(corrupted),
                wal: MemoryBackend::from_durable(bundle.wal.durable_snapshot().unwrap()),
            };
            let mut reopened = Engine::open("paired-value-corruption", files.files(), config())?;
            let error = if is_feed {
                reopened
                    .changes_since(seed, ChangeFeedOptions::default())
                    .unwrap_err()
            } else {
                let reader = reopened.begin_tx(TxMode::Readonly)?;
                reopened.get(reader, "docs", b"key").unwrap_err()
            };
            assert!(matches!(error, EngineError::Corruption(_)), "{error}");
        }
    }
    Ok(())
}

#[test]
fn overwrites_keep_snapshot_values_and_feed_history_independent() -> Result<()> {
    let (bundle, mut engine, seed) = seeded("paired-value-snapshots", "docs")?;
    let key = b"key";
    let old = bytes(2 * max_overflow_chunk_len() - 16, 0x71_225);
    let new = bytes(3 * max_overflow_chunk_len() - 19, 0x45_179);
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.put(tx, "docs", key, &old)?;
    let first = engine.commit_tx(tx)?;
    let snapshot = engine.begin_tx(TxMode::Readonly)?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.put_with_ttl(tx, "docs", key, &new, Some(86_400_000))?;
    let second = engine.commit_tx(tx)?;
    assert_eq!(
        engine.get(snapshot, "docs", key)?.as_deref(),
        Some(old.as_slice())
    );
    let latest = engine.begin_tx(TxMode::Readonly)?;
    assert_eq!(
        engine.get(latest, "docs", key)?.as_deref(),
        Some(new.as_slice())
    );
    engine.rollback_tx(latest)?;
    let feed = engine.changes_since(seed, ChangeFeedOptions::default())?;
    assert_eq!(feed.changes.len(), 2);
    assert_eq!(feed.changes[0].tx_id, first);
    assert_eq!(feed.changes[0].value.as_deref(), Some(old.as_slice()));
    assert_eq!(feed.changes[1].tx_id, second);
    assert_eq!(feed.changes[1].value.as_deref(), Some(new.as_slice()));

    // Deletion and feed pruning must retain the value referenced by the old snapshot.
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.delete(tx, "docs", key)?;
    engine.set_change_feed_policy(
        tx,
        ChangeFeedPolicy {
            enabled: true,
            retain_txids: Some(1),
        },
    )?;
    engine.commit_tx(tx)?;
    assert_eq!(
        engine.get(snapshot, "docs", key)?.as_deref(),
        Some(old.as_slice())
    );
    engine.rollback_tx(snapshot)?;
    engine.checkpoint()?;
    drop(engine);
    let mut reopened = Engine::open(
        "paired-value-snapshots",
        bundle.crash_recovered_files(),
        config(),
    )?;
    let reader = reopened.begin_tx(TxMode::Readonly)?;
    assert_eq!(reopened.get(reader, "docs", key)?, None);
    reopened.rollback_tx(reader)?;
    let feed = reopened.changes_since(second, ChangeFeedOptions::default())?;
    assert_eq!(feed.changes.len(), 1);
    assert_eq!(feed.changes[0].kind, ChangeKind::Delete);
    Ok(())
}
