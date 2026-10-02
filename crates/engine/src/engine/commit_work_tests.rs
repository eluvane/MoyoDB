use super::*;
use crate::layout::{page_offset, PageKind, ValueKind, PAGE_SIZE};
use crate::page::{decode_page, encode_leaf_page, MAX_TREE_LEVEL};
use crate::storage::memory::{MemoryBackend, MemoryBundle};
use std::cell::Cell;

thread_local! {
    static TTL_EXAMINATIONS: Cell<usize> = const { Cell::new(0) };
}

pub(super) fn record_ttl_examination() {
    TTL_EXAMINATIONS.with(|counter| counter.set(counter.get() + 1));
}

fn reset_ttl_examinations() {
    TTL_EXAMINATIONS.with(|counter| counter.set(0));
}

fn ttl_examinations() -> usize {
    TTL_EXAMINATIONS.with(Cell::get)
}

#[test]
fn scans_without_expiring_staged_values_skip_ttl_sweeps() -> Result<()> {
    let mut engine = Engine::open(
        "no-ttl-sweep",
        MemoryBundle::new().files(),
        OpenConfig {
            checkpoint_wal_bytes: u64::MAX,
            checkpoint_dirty_pages: usize::MAX,
            ..OpenConfig::default()
        },
    )?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(tx, "kv")?;
    for key in 0u32..1024 {
        engine.put(tx, "kv", &key.to_be_bytes(), b"v")?;
    }
    reset_ttl_examinations();
    let empty = engine.scan(
        tx,
        "kv",
        &ScanRange {
            limit: Some(0),
            ..ScanRange::default()
        },
    )?;
    let first = engine.scan(
        tx,
        "kv",
        &ScanRange {
            limit: Some(1),
            ..ScanRange::default()
        },
    )?;
    assert!(empty.is_empty());
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].key, 0u32.to_be_bytes());
    assert_eq!(
        ttl_examinations(),
        0,
        "no-TTL scans must not sweep the staged map"
    );
    engine.rollback_tx(tx)
}

#[test]
fn ttl_sweep_preserves_future_expiry_and_never_revives_expired_values() -> Result<()> {
    let stage = StagedStore::created(STORE_FLAG_VALUE_ENVELOPE_V1);
    // Deserialize must treat the TTL state as unknown, including when the
    // public mutation map is subsequently populated outside write helpers.
    let encoded =
        serde_json::to_vec(&stage).map_err(|err| EngineError::Serialization(err.to_string()))?;
    let mut stage: StagedStore = serde_json::from_slice(&encoded)
        .map_err(|err| EngineError::Serialization(err.to_string()))?;
    stage.mutations.insert(
        b"expired".to_vec(),
        MutationValue::Put(StoredValue::with_expiry(b"expired".to_vec(), Some(1))),
    );
    stage.mutations.insert(
        b"future".to_vec(),
        MutationValue::Put(StoredValue::with_expiry(b"future".to_vec(), Some(3))),
    );
    reset_ttl_examinations();
    normalize_expired_stage_mutations(&mut stage, 2);
    assert_eq!(ttl_examinations(), 2);
    assert!(matches!(
        stage.mutations.get(b"expired".as_slice()),
        Some(MutationValue::Delete)
    ));
    assert!(matches!(
        stage.mutations.get(b"future".as_slice()),
        Some(MutationValue::Put(_))
    ));
    normalize_expired_stage_mutations(&mut stage, 3);
    assert!(stage.mutations.values().all(MutationValue::is_delete));
    reset_ttl_examinations();
    normalize_expired_stage_mutations(&mut stage, 0);
    assert!(stage.mutations.values().all(MutationValue::is_delete));
    assert_eq!(
        ttl_examinations(),
        0,
        "a fully normalized stage has no expiring puts left"
    );
    Ok(())
}

#[test]
fn staged_ttl_expiry_hides_live_base_and_survives_clock_reversal_and_commit() -> Result<()> {
    let mut engine = Engine::open(
        "staged-ttl-work",
        MemoryBundle::new().files(),
        OpenConfig::default(),
    )?;
    let seed = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(seed, "kv")?;
    engine.put(seed, "kv", b"key", b"original")?;
    engine.commit_tx(seed)?;
    let reader = engine.begin_tx(TxMode::Readonly)?;
    let writer = engine.begin_tx(TxMode::Readwrite)?;
    let mut tx = engine.take_tx(writer)?;
    let rw = tx.readwrite_mut()?;
    assert!(put_with_staged_at(
        &mut engine.pager,
        rw,
        "kv",
        b"key",
        StoredValue::with_expiry(b"temporary".to_vec(), Some(1)),
        0,
    )?);
    assert!(scan_with_staged(&mut engine.pager, rw, "kv", &ScanRange::default(), 2)?.is_empty());
    assert!(matches!(
        rw.stores["kv"].mutations.get(b"key".as_slice()),
        Some(MutationValue::Delete)
    ));
    assert_eq!(
        get_with_staged(&mut engine.pager, rw, "kv", b"key", 0)?,
        None
    );
    engine.put_tx(tx);
    engine.commit_tx(writer)?;
    assert_eq!(
        engine.get(reader, "kv", b"key")?,
        Some(b"original".to_vec())
    );
    engine.rollback_tx(reader)?;
    let current = engine.begin_tx(TxMode::Readonly)?;
    assert_eq!(engine.get(current, "kv", b"key")?, None);
    engine.rollback_tx(current)
}

struct TtlCleanupFixture {
    engine: Engine<MemoryBackend>,
    bundle: MemoryBundle,
    keys: Vec<Vec<u8>>,
    late_leaf: u64,
    late_image: Vec<u8>,
}

fn leaf_containing_key(engine: &mut Engine<MemoryBackend>, key: &[u8]) -> Result<u64> {
    let mut page_id = engine.catalog()["kv"].store_root_page_id;
    for _ in 0..=MAX_TREE_LEVEL {
        let page = decode_page(&engine.pager.read_page(page_id)?)?;
        if page.header.page_kind == PageKind::Leaf {
            return Ok(page_id);
        }
        assert_eq!(page.header.page_kind, PageKind::Internal);
        let child = page
            .internal_cells
            .iter()
            .rposition(|cell| cell.separator.as_slice() <= key)
            .unwrap_or(0);
        page_id = page.internal_cells[child].child_page_id;
    }
    Err(EngineError::Internal(
        "fixture tree exceeded its depth bound".into(),
    ))
}

impl TtlCleanupFixture {
    fn new() -> Result<Self> {
        let mut bundle = MemoryBundle::new();
        let mut engine = Engine::open(
            "failed-metadata-cleanup",
            bundle.files(),
            OpenConfig {
                cache_pages: 1,
                checkpoint_wal_bytes: u64::MAX,
                checkpoint_dirty_pages: usize::MAX,
                ..OpenConfig::default()
            },
        )?;
        let mut keys = vec![b"a".to_vec()];
        keys.extend((0..16).map(|index| format!("m-{index:02}").into_bytes()));
        keys.push(b"z".to_vec());
        assert!(keys.len() <= 32);
        let seed = engine.begin_tx(TxMode::Readwrite)?;
        engine.set_change_feed_policy(
            seed,
            ChangeFeedPolicy {
                enabled: false,
                retain_txids: None,
            },
        )?;
        engine.create_store(seed, "kv")?;
        for key in &keys {
            engine.put(seed, "kv", key, &[0x47; 768])?;
        }
        engine.commit_tx(seed)?;
        engine.checkpoint()?;

        let first_leaf = leaf_containing_key(&mut engine, b"a")?;
        let late_leaf = leaf_containing_key(&mut engine, b"z")?;
        assert_ne!(
            first_leaf, late_leaf,
            "fixture needs distinct early and late leaves"
        );
        let mut page = decode_page(&engine.pager.read_page(first_leaf)?)?;
        let cell = page
            .leaf_cells
            .iter_mut()
            .find(|cell| cell.key == b"a")
            .ok_or_else(|| EngineError::Internal("fixture missing early base key".into()))?;
        assert_eq!(cell.value_kind, ValueKind::Inline);
        assert_eq!(&cell.value[..8], &crate::value::VALUE_ENVELOPE_MAGIC);
        // Expire an already committed base key at a fixed timestamp. Re-encode
        // the leaf checksum so only the later page is structurally corrupt.
        cell.value[8..16].copy_from_slice(&1u64.to_le_bytes());
        let image = encode_leaf_page(
            first_leaf,
            page.header.level,
            page.header.right_sibling_page_id,
            &page.leaf_cells,
        )?;
        bundle.main.write_at(page_offset(first_leaf), &image)?;
        let late_image = engine.pager.read_page(late_leaf)?;
        let mut corrupt = late_image.clone();
        corrupt[PAGE_SIZE - 1] ^= 0x80;
        bundle.main.write_at(page_offset(late_leaf), &corrupt)?;
        engine.pager.discard_cache();
        Ok(Self {
            engine,
            bundle,
            keys,
            late_leaf,
            late_image,
        })
    }

    fn stage_expired_put(&mut self, tx_id: u64) -> Result<()> {
        // The put's baseline lookup needs the healthy last leaf. Corrupt it
        // only after the staged value exists, before either observation.
        self.repair()?;
        let mut tx = self.engine.take_tx(tx_id)?;
        let result = tx.readwrite_mut().and_then(|rw| {
            put_with_staged_at(
                &mut self.engine.pager,
                rw,
                "kv",
                b"zz-staged",
                StoredValue::with_expiry(b"temporary".to_vec(), Some(1)),
                0,
            )
            .map(|_| ())
        });
        self.engine.put_tx(tx);
        result?;
        let mut corrupt = self.late_image.clone();
        corrupt[PAGE_SIZE - 1] ^= 0x80;
        self.bundle
            .main
            .write_at(page_offset(self.late_leaf), &corrupt)?;
        self.engine.pager.discard_cache();
        Ok(())
    }

    fn mutations(&self, tx_id: u64) -> &BTreeMap<Vec<u8>, MutationValue> {
        let TxInner::Readwrite(rw) = &self.engine.txns[&tx_id].inner else {
            panic!("fixture needs a readwrite transaction");
        };
        &rw.stores["kv"].mutations
    }

    fn repair(&mut self) -> Result<()> {
        self.bundle
            .main
            .write_at(page_offset(self.late_leaf), &self.late_image)?;
        self.engine.pager.discard_cache();
        Ok(())
    }
}

#[test]
fn has_many_failure_matches_full_scan_staged_and_base_ttl_cleanup() -> Result<()> {
    let mut fixture = TtlCleanupFixture::new()?;
    let scan_tx = fixture.engine.begin_tx(TxMode::Readwrite)?;
    fixture.stage_expired_put(scan_tx)?;
    let scan_error = fixture
        .engine
        .scan(scan_tx, "kv", &ScanRange::default())
        .expect_err("the late leaf must fail the full scan");
    assert!(matches!(scan_error, EngineError::Corruption(_)));
    let expected = fixture.mutations(scan_tx).clone();
    assert!(matches!(
        expected.get(b"zz-staged".as_slice()),
        Some(MutationValue::Delete)
    ));
    assert!(
        !expected.contains_key(b"a".as_slice()),
        "failed full scans defer committed-key cleanup"
    );
    fixture.engine.rollback_tx(scan_tx)?;
    fixture.engine.pager.discard_cache();

    let batch_tx = fixture.engine.begin_tx(TxMode::Readwrite)?;
    fixture.stage_expired_put(batch_tx)?;
    let batch_error = fixture
        .engine
        .has_many(batch_tx, "kv", &fixture.keys)
        .expect_err("the late leaf must also fail the metadata batch");
    assert_eq!(batch_error, scan_error);
    assert_eq!(
        fixture.mutations(batch_tx),
        &expected,
        "failed metadata batches normalize staged puts but defer expired base keys"
    );

    fixture.repair()?;
    let visibility = fixture.engine.has_many(batch_tx, "kv", &fixture.keys)?;
    assert_eq!(visibility.len(), fixture.keys.len());
    assert!(!visibility[0]);
    assert!(visibility[1..].iter().all(|visible| *visible));
    assert!(matches!(
        fixture.mutations(batch_tx).get(b"a".as_slice()),
        Some(MutationValue::Delete)
    ));
    assert!(matches!(
        fixture.mutations(batch_tx).get(b"zz-staged".as_slice()),
        Some(MutationValue::Delete)
    ));
    fixture.engine.rollback_tx(batch_tx)
}
