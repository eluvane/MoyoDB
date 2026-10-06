use super::*;
use crate::btree::lookup;
use crate::bytes::read_u64_le;
use crate::layout::{page_offset, PageKind, ValueKind, PAGE_SIZE};
use crate::page::{decode_page, MAX_TREE_LEVEL};
use crate::payload::{
    decode_payload_descriptor, read_payload, PayloadRef, PAYLOAD_HEADER_SIZE,
    PAYLOAD_READ_CHUNK_BYTES,
};
use crate::storage::memory::MemoryBackend;
use crate::wal::scan_wal;
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::Mutex;

const DB_NAME: &str = "external-payloads";
const BODY_SIZE: usize = 1024 * 1024;
static FLUSH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct Observations {
    protected: Vec<Range<u64>>,
    protected_read_bytes: usize,
    protected_reads: Vec<(u64, usize)>,
    writes: Vec<(u64, usize)>,
    flushes: usize,
    flush_events: Vec<u64>,
    fail_flush: bool,
}

#[derive(Clone)]
struct ObservedBackend {
    inner: MemoryBackend,
    observations: Arc<Mutex<Observations>>,
}

impl ObservedBackend {
    fn new() -> Self {
        Self {
            inner: MemoryBackend::new(),
            observations: Arc::new(Mutex::new(Observations::default())),
        }
    }

    fn crash_copy(&self) -> Self {
        Self {
            inner: MemoryBackend::from_durable(self.inner.durable_snapshot().unwrap()),
            observations: Arc::clone(&self.observations),
        }
    }

    fn protect(&self, references: &[PayloadRef]) -> Result<()> {
        let mut observations = self.observations.lock().unwrap();
        observations.protected = references
            .iter()
            .map(|reference| {
                Ok(page_offset(reference.first_page_id)..page_offset(reference.end_page_id()?))
            })
            .collect::<Result<_>>()?;
        observations.protected_read_bytes = 0;
        observations.protected_reads.clear();
        Ok(())
    }

    fn protected_read_bytes(&self) -> usize {
        self.observations.lock().unwrap().protected_read_bytes
    }

    fn reset_flushes(&self) {
        let mut observations = self.observations.lock().unwrap();
        observations.flushes = 0;
        observations.flush_events.clear();
    }

    fn flushes(&self) -> usize {
        self.observations.lock().unwrap().flushes
    }

    fn flush_events(&self) -> Vec<u64> {
        self.observations.lock().unwrap().flush_events.clone()
    }
}

impl FileBackend for ObservedBackend {
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let mut observations = self.observations.lock().unwrap();
        let end = offset + len as u64;
        for range in observations.protected.clone() {
            if offset < range.end && range.start < end {
                observations.protected_read_bytes +=
                    (end.min(range.end) - offset.max(range.start)) as usize;
                observations.protected_reads.push((offset, len));
            }
        }
        drop(observations);
        self.inner.read_at(offset, len)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        self.observations
            .lock()
            .unwrap()
            .writes
            .push((offset, bytes.len()));
        self.inner.write_at(offset, bytes)
    }

    fn flush(&mut self) -> Result<()> {
        let mut observations = self.observations.lock().unwrap();
        observations.flushes += 1;
        if std::mem::take(&mut observations.fail_flush) {
            return Err(EngineError::Storage("payload main flush failed".into()));
        }
        drop(observations);
        self.inner.flush()?;
        self.observations
            .lock()
            .unwrap()
            .flush_events
            .push(FLUSH_SEQUENCE.fetch_add(1, AtomicOrdering::SeqCst));
        Ok(())
    }

    fn len(&self) -> Result<u64> {
        self.inner.len()
    }

    fn truncate(&mut self, size: u64) -> Result<()> {
        self.inner.truncate(size)
    }

    fn close(&mut self) -> Result<()> {
        self.inner.close()
    }

    fn durable_snapshot(&self) -> Option<Vec<u8>> {
        self.inner.durable_snapshot()
    }
}

struct Fixture {
    engine: Engine<ObservedBackend>,
    manifest: ObservedBackend,
    main: ObservedBackend,
    wal: ObservedBackend,
    seed_txid: u64,
}

fn config() -> OpenConfig {
    OpenConfig {
        cache_pages: 1,
        checkpoint_wal_bytes: u64::MAX,
        checkpoint_dirty_pages: usize::MAX,
        ..OpenConfig::default()
    }
}

impl Fixture {
    fn new(feed: bool) -> Result<Self> {
        let manifest = ObservedBackend::new();
        let main = ObservedBackend::new();
        let wal = ObservedBackend::new();
        let mut engine = Engine::open(
            DB_NAME,
            FileSet::new(manifest.clone(), main.clone(), wal.clone()),
            config(),
        )?;
        let tx = engine.begin_tx(TxMode::Readwrite)?;
        engine.set_change_feed_policy(
            tx,
            ChangeFeedPolicy {
                enabled: feed,
                retain_txids: None,
            },
        )?;
        engine.create_store(tx, "kv")?;
        let seed_txid = engine.commit_tx(tx)?;
        engine.checkpoint()?;
        Ok(Self {
            engine,
            manifest,
            main,
            wal,
            seed_txid,
        })
    }

    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<u64> {
        let tx = self.engine.begin_tx(TxMode::Readwrite)?;
        self.engine.put(tx, "kv", key, value)?;
        self.engine.commit_tx(tx)
    }

    fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let tx = self.engine.begin_tx(TxMode::Readonly)?;
        let value = self.engine.get(tx, "kv", key)?;
        self.engine.rollback_tx(tx)?;
        Ok(value)
    }

    fn reopen(&mut self) -> Result<()> {
        self.manifest = self.manifest.crash_copy();
        self.main = self.main.crash_copy();
        self.wal = self.wal.crash_copy();
        self.engine = Engine::open(
            DB_NAME,
            FileSet::new(self.manifest.clone(), self.main.clone(), self.wal.clone()),
            config(),
        )?;
        Ok(())
    }
}

fn reference<B: FileBackend>(
    engine: &mut Engine<B>,
    store: &str,
    key: &[u8],
) -> Result<PayloadRef> {
    let mut page_id = engine.catalog[store].store_root_page_id;
    for _ in 0..=MAX_TREE_LEVEL {
        let page = decode_page(&engine.pager.read_page(page_id)?)?;
        if page.header.page_kind == PageKind::Leaf {
            let cell = page.leaf_cells.iter().find(|cell| cell.key == key).unwrap();
            assert_eq!(cell.value_kind, ValueKind::External);
            return Ok(decode_payload_descriptor(
                cell.overflow_head_page_id,
                cell.total_value_len,
                &cell.value,
            )?
            .0);
        }
        let index = page
            .internal_cells
            .partition_point(|cell| cell.separator.as_slice() <= key);
        page_id = page.internal_cells[index.saturating_sub(1)].child_page_id;
    }
    Err(EngineError::Internal(
        "payload fixture exceeded tree depth".into(),
    ))
}

fn registry_state<B: FileBackend>(
    engine: &mut Engine<B>,
    reference: PayloadRef,
) -> Result<(u64, u64)> {
    let root = engine.catalog[PAYLOAD_REGISTRY_STORE_NAME].store_root_page_id;
    let bytes = lookup(
        &mut engine.pager,
        root,
        &reference.first_page_id.to_be_bytes(),
    )?
    .unwrap();
    Ok((read_u64_le(&bytes, 16)?, read_u64_le(&bytes, 32)?))
}

fn distinct_bodies() -> Vec<(Vec<u8>, Vec<u8>)> {
    (0..64u32)
        .map(|index| (index.to_be_bytes().to_vec(), vec![index as u8; 65_537]))
        .collect()
}

fn assert_main_before_wal(fixture: &Fixture) {
    assert_eq!(fixture.main.flushes(), 1);
    assert_eq!(fixture.wal.flushes(), 1);
    let main = fixture.main.flush_events();
    let wal = fixture.wal.flush_events();
    assert_eq!(main.len(), 1);
    assert_eq!(wal.len(), 1);
    assert!(main[0] < wal[0]);
}

#[test]
fn distinct_payloads_and_feed_share_one_main_flush_before_wal() -> Result<()> {
    for full_rewrite in [false, true] {
        let mut fixture = Fixture::new(true)?;
        let entries = distinct_bodies();
        if full_rewrite {
            fixture.put(b"existing", b"retained-inline")?;
        }
        fixture.main.reset_flushes();
        fixture.wal.reset_flushes();
        let tx = fixture.engine.begin_tx(TxMode::Readwrite)?;
        fixture.engine.put_many(tx, "kv", &entries)?;
        if full_rewrite {
            fixture
                .engine
                .txns
                .get_mut(&tx)
                .unwrap()
                .readwrite_mut()?
                .stores
                .get_mut("kv")
                .unwrap()
                .force_full_rewrite = true;
        }
        let committed = fixture.engine.commit_tx(tx)?;
        assert_main_before_wal(&fixture);
        assert_eq!(
            fixture
                .main
                .observations
                .lock()
                .unwrap()
                .writes
                .iter()
                .filter(|(_, length)| *length == 65_537)
                .count(),
            entries.len()
        );
        let references = entries
            .iter()
            .enumerate()
            .map(|(sequence, (key, _))| {
                let primary = reference(&mut fixture.engine, "kv", key)?;
                let feed = reference(
                    &mut fixture.engine,
                    SYSTEM_CHANGELOG_STORE_NAME,
                    &encode_change_log_key(committed, sequence as u32),
                )?;
                assert_eq!(primary, feed);
                assert_eq!(registry_state(&mut fixture.engine, primary)?, (2, 0));
                Ok(primary)
            })
            .collect::<Result<Vec<_>>>()?;
        assert!(references
            .windows(2)
            .all(|pair| pair[0].first_page_id != pair[1].first_page_id));
        for ((_, value), reference) in entries.iter().zip(&references) {
            assert_eq!(read_payload(&fixture.main.crash_copy(), reference)?, *value);
        }
        fixture.reopen()?;
        for (key, value) in &entries {
            assert_eq!(fixture.get(key)?.as_deref(), Some(value.as_slice()));
        }
        if full_rewrite {
            assert_eq!(fixture.get(b"existing")?, Some(b"retained-inline".to_vec()));
        }
    }
    Ok(())
}

#[test]
fn snapshot_payload_batch_flushes_once_before_wal() -> Result<()> {
    let mut fixture = Fixture::new(false)?;
    let entries = distinct_bodies();
    let snapshot = encode_snapshot(&SnapshotContents {
        source_last_committed_txid: fixture.seed_txid,
        schema_version: 0,
        stores: vec![SnapshotStore {
            name: "kv".into(),
            flags: 0,
            entries: entries
                .iter()
                .map(|(key, value)| SnapshotEntry {
                    key: key.clone(),
                    value: value.clone(),
                    expires_at_ms: None,
                })
                .collect(),
        }],
    })?;
    fixture.main.reset_flushes();
    fixture.wal.reset_flushes();
    fixture.engine.import_snapshot(&snapshot)?;
    assert_main_before_wal(&fixture);
    fixture.reopen()?;
    for (key, value) in &entries {
        assert_eq!(fixture.get(key)?.as_deref(), Some(value.as_slice()));
    }
    Ok(())
}

#[test]
fn compaction_payload_batch_uses_final_main_flush_before_manifest() -> Result<()> {
    let mut fixture = Fixture::new(true)?;
    let entries = distinct_bodies();
    let tx = fixture.engine.begin_tx(TxMode::Readwrite)?;
    fixture.engine.put_many(tx, "kv", &entries)?;
    fixture.engine.commit_tx(tx)?;
    let manifest = ObservedBackend::new();
    let main = ObservedBackend::new();
    let wal = ObservedBackend::new();
    let mut compacted = Engine::open(
        "compact-flush-batch",
        FileSet::new(manifest.clone(), main.clone(), wal.clone()),
        config(),
    )?;
    manifest.reset_flushes();
    main.reset_flushes();
    wal.reset_flushes();
    fixture.engine.compact_into(&mut compacted)?;
    assert_eq!(main.flushes(), 1);
    assert_eq!(manifest.flushes(), 1);
    assert_eq!(wal.flushes(), 0);
    assert!(main.flush_events()[0] < manifest.flush_events()[0]);
    let mut reopened = Engine::open(
        "compact-flush-batch",
        FileSet::new(manifest.crash_copy(), main.crash_copy(), wal.crash_copy()),
        config(),
    )?;
    let reader = reopened.begin_tx(TxMode::Readonly)?;
    for (key, value) in &entries {
        assert_eq!(
            reopened.get(reader, "kv", key)?.as_deref(),
            Some(value.as_slice())
        );
    }
    reopened.rollback_tx(reader)?;
    Ok(())
}

#[test]
fn primary_and_feed_share_one_durable_body_with_small_wal() -> Result<()> {
    let mut fixture = Fixture::new(true)?;
    let main_before = fixture.main.len()?;
    let wal_before = fixture.wal.len()?;
    let value = vec![0x73; BODY_SIZE];
    let txid = fixture.put(b"key", &value)?;
    let primary = reference(&mut fixture.engine, "kv", b"key")?;
    let feed = reference(
        &mut fixture.engine,
        SYSTEM_CHANGELOG_STORE_NAME,
        &encode_change_log_key(txid, 0),
    )?;
    assert_eq!(primary, feed);
    assert_eq!(registry_state(&mut fixture.engine, primary)?, (2, 0));
    assert_eq!(read_payload(&fixture.main.crash_copy(), &primary)?, value);
    let wal_growth = fixture.wal.len()? - wal_before;
    assert!(
        wal_growth < 64 * 1024,
        "descriptor WAL grew by {wal_growth} bytes"
    );
    let transactions = scan_wal(&fixture.wal)?;
    assert!(transactions.last().unwrap().page_images.len() <= 8);
    fixture.engine.checkpoint()?;
    let main_growth = fixture.main.len()? - main_before;
    assert!(
        main_growth <= (BODY_SIZE + 64 * 1024) as u64,
        "shared body main grew by {main_growth} bytes"
    );
    let body_writes = fixture
        .main
        .observations
        .lock()
        .unwrap()
        .writes
        .iter()
        .filter(|(_, len)| *len == BODY_SIZE)
        .count();
    assert_eq!(body_writes, 1);
    fixture.reopen()?;
    fixture.engine.pager.discard_cache();
    fixture.main.protect(&[primary])?;
    checksum::work::reset();
    assert!(fixture.get(b"key")?.as_deref() == Some(value.as_slice()));
    assert_eq!(
        checksum::work::hashed_bytes(),
        BODY_SIZE + PAYLOAD_HEADER_SIZE + PAGE_SIZE
    );
    assert_eq!(
        fixture.main.protected_read_bytes(),
        BODY_SIZE + PAYLOAD_HEADER_SIZE
    );
    assert_eq!(
        fixture
            .main
            .observations
            .lock()
            .unwrap()
            .protected_reads
            .iter()
            .map(|(_, len)| *len)
            .collect::<Vec<_>>(),
        [
            PAYLOAD_HEADER_SIZE,
            PAYLOAD_READ_CHUNK_BYTES,
            PAYLOAD_READ_CHUNK_BYTES,
            PAYLOAD_READ_CHUNK_BYTES,
            PAYLOAD_READ_CHUNK_BYTES
        ]
    );
    assert_eq!(
        fixture
            .engine
            .changes_since(fixture.seed_txid, ChangeFeedOptions::default())?
            .changes[0]
            .value,
        Some(value)
    );
    Ok(())
}

#[test]
fn cold_overwrite_delete_clear_and_drop_do_not_read_old_bodies() -> Result<()> {
    let mut fixture = Fixture::new(true)?;
    fixture.put(b"key", &vec![0x31; BODY_SIZE])?;
    let old = reference(&mut fixture.engine, "kv", b"key")?;
    fixture.engine.checkpoint()?;
    fixture.engine.pager.discard_cache();
    fixture.main.protect(&[old])?;
    fixture.put(b"key", &vec![0x52; BODY_SIZE])?;
    assert_eq!(fixture.main.protected_read_bytes(), 0);
    assert_eq!(registry_state(&mut fixture.engine, old)?, (1, 0));
    let current = reference(&mut fixture.engine, "kv", b"key")?;
    fixture.engine.checkpoint()?;
    fixture.engine.pager.discard_cache();
    fixture.main.protect(&[old, current])?;
    let tx = fixture.engine.begin_tx(TxMode::Readwrite)?;
    assert!(fixture.engine.delete(tx, "kv", b"key")?);
    fixture.engine.commit_tx(tx)?;
    assert_eq!(fixture.main.protected_read_bytes(), 0);
    assert_eq!(registry_state(&mut fixture.engine, current)?, (1, 0));
    fixture.put(b"clear", &vec![0x63; BODY_SIZE])?;
    let cleared = reference(&mut fixture.engine, "kv", b"clear")?;
    fixture.engine.checkpoint()?;
    fixture.engine.pager.discard_cache();
    fixture.main.protect(&[old, current, cleared])?;
    let tx = fixture.engine.begin_tx(TxMode::Readwrite)?;
    fixture.engine.clear_store(tx, "kv")?;
    fixture.engine.commit_tx(tx)?;
    assert_eq!(fixture.main.protected_read_bytes(), 0);
    assert_eq!(registry_state(&mut fixture.engine, cleared)?, (1, 0));
    fixture.put(b"drop", &vec![0x74; BODY_SIZE])?;
    let dropped = reference(&mut fixture.engine, "kv", b"drop")?;
    fixture.engine.checkpoint()?;
    fixture.engine.pager.discard_cache();
    fixture.main.protect(&[old, current, cleared, dropped])?;
    let tx = fixture.engine.begin_tx(TxMode::Readwrite)?;
    fixture.engine.drop_store(tx, "kv")?;
    fixture.engine.set_change_feed_policy(
        tx,
        ChangeFeedPolicy {
            enabled: false,
            retain_txids: None,
        },
    )?;
    let retired = fixture.engine.commit_tx(tx)?;
    assert_eq!(fixture.main.protected_read_bytes(), 0);
    for reference in [old, current, cleared, dropped] {
        assert_eq!(
            registry_state(&mut fixture.engine, reference)?,
            (0, retired)
        );
    }
    Ok(())
}

#[test]
fn zero_count_extent_is_snapshot_protected_then_reused_after_reopen() -> Result<()> {
    let mut fixture = Fixture::new(true)?;
    let old_value = vec![0x37; BODY_SIZE];
    fixture.put(b"key", &old_value)?;
    let old = reference(&mut fixture.engine, "kv", b"key")?;
    let snapshot = fixture.engine.begin_tx(TxMode::Readonly)?;
    fixture.put(b"key", &vec![0x59; BODY_SIZE])?;
    let tx = fixture.engine.begin_tx(TxMode::Readwrite)?;
    fixture.engine.delete(tx, "kv", b"key")?;
    fixture.engine.set_change_feed_policy(
        tx,
        ChangeFeedPolicy {
            enabled: false,
            retain_txids: None,
        },
    )?;
    let retired = fixture.engine.commit_tx(tx)?;
    assert_eq!(registry_state(&mut fixture.engine, old)?, (0, retired));
    fixture.put(b"new", &vec![0x6a; BODY_SIZE])?;
    let protected_new = reference(&mut fixture.engine, "kv", b"new")?;
    assert_ne!(protected_new.first_page_id, old.first_page_id);
    assert_eq!(fixture.engine.get(snapshot, "kv", b"key")?, Some(old_value));
    fixture.engine.rollback_tx(snapshot)?;
    fixture.engine.checkpoint()?;
    fixture.reopen()?;
    let value = vec![0x7b; BODY_SIZE];
    fixture.put(b"reused", &value)?;
    let reused = reference(&mut fixture.engine, "kv", b"reused")?;
    assert_eq!(reused.first_page_id, old.first_page_id);
    assert_ne!(reused.checksum, old.checksum);
    assert_eq!(registry_state(&mut fixture.engine, reused)?, (1, 0));
    fixture.engine.checkpoint()?;
    fixture.reopen()?;
    assert!(fixture.get(b"reused")?.as_deref() == Some(value.as_slice()));
    Ok(())
}

#[test]
fn feed_pruning_drops_one_reference_and_unchanged_cow_cells_add_none() -> Result<()> {
    let mut fixture = Fixture::new(true)?;
    fixture.put(b"key", &vec![0x24; BODY_SIZE])?;
    let old = reference(&mut fixture.engine, "kv", b"key")?;
    fixture.put(b"side", b"small")?;
    assert_eq!(registry_state(&mut fixture.engine, old)?, (2, 0));
    fixture.engine.checkpoint()?;
    fixture.engine.pager.discard_cache();
    fixture.main.protect(&[old])?;
    let tx = fixture.engine.begin_tx(TxMode::Readwrite)?;
    fixture.engine.put(tx, "kv", b"side", b"changed")?;
    fixture.engine.set_change_feed_policy(
        tx,
        ChangeFeedPolicy {
            enabled: true,
            retain_txids: Some(1),
        },
    )?;
    let pruned = fixture.engine.commit_tx(tx)?;
    assert_eq!(fixture.main.protected_read_bytes(), 0);
    assert_eq!(registry_state(&mut fixture.engine, old)?, (1, 0));
    let overwrite = fixture.put(b"key", &vec![0x35; BODY_SIZE])?;
    assert!(overwrite > pruned);
    assert_eq!(fixture.main.protected_read_bytes(), 0);
    assert_eq!(registry_state(&mut fixture.engine, old)?, (0, overwrite));
    let current = reference(&mut fixture.engine, "kv", b"key")?;
    assert_eq!(registry_state(&mut fixture.engine, current)?, (2, 0));
    fixture.engine.checkpoint()?;
    fixture.reopen()?;
    assert_eq!(registry_state(&mut fixture.engine, old)?, (0, overwrite));
    assert_eq!(registry_state(&mut fixture.engine, current)?, (2, 0));
    Ok(())
}

#[test]
fn payload_flush_failure_and_rollback_publish_no_descriptor() -> Result<()> {
    let mut fixture = Fixture::new(true)?;
    let old_value = vec![0x1d; BODY_SIZE];
    fixture.put(b"key", &old_value)?;
    let old = reference(&mut fixture.engine, "kv", b"key")?;
    fixture.engine.checkpoint()?;
    let wal_before = fixture.wal.len()?;
    let committed_before = fixture.engine.superblock.last_committed_txid;
    let writes_before = fixture.main.observations.lock().unwrap().writes.len();
    let tx = fixture.engine.begin_tx(TxMode::Readwrite)?;
    fixture
        .engine
        .put(tx, "kv", b"rolled-back", &vec![0x3f; BODY_SIZE])?;
    fixture.engine.rollback_tx(tx)?;
    assert_eq!(
        fixture.main.observations.lock().unwrap().writes.len(),
        writes_before
    );
    fixture.main.reset_flushes();
    fixture.wal.reset_flushes();
    let tx = fixture.engine.begin_tx(TxMode::Readwrite)?;
    fixture
        .engine
        .put(tx, "kv", b"key", &vec![0x2e; BODY_SIZE])?;
    for key in [
        b"unpublished-second".as_slice(),
        b"unpublished-third".as_slice(),
    ] {
        fixture.engine.put(tx, "kv", key, &vec![0x4f; BODY_SIZE])?;
    }
    fixture.main.observations.lock().unwrap().fail_flush = true;
    assert!(matches!(
        fixture.engine.commit_tx(tx),
        Err(EngineError::Storage(_))
    ));
    assert!(!fixture.engine.needs_recovery());
    assert_eq!(fixture.main.flushes(), 1);
    assert!(fixture.main.flush_events().is_empty());
    assert_eq!(fixture.wal.flushes(), 0);
    assert_eq!(
        fixture.main.observations.lock().unwrap().writes[writes_before..]
            .iter()
            .filter(|(_, length)| *length == BODY_SIZE)
            .count(),
        3
    );
    assert_eq!(fixture.wal.len()?, wal_before);
    assert_eq!(
        fixture.engine.superblock.last_committed_txid,
        committed_before
    );
    assert_eq!(reference(&mut fixture.engine, "kv", b"key")?, old);
    assert_eq!(registry_state(&mut fixture.engine, old)?, (2, 0));
    fixture.reopen()?;
    assert_eq!(fixture.get(b"key")?, Some(old_value));
    assert_eq!(fixture.get(b"rolled-back")?, None);
    assert_eq!(fixture.get(b"unpublished-second")?, None);
    assert_eq!(fixture.get(b"unpublished-third")?, None);
    Ok(())
}

#[test]
fn snapshot_import_and_reset_retire_all_old_primary_and_feed_references() -> Result<()> {
    let mut fixture = Fixture::new(true)?;
    let value = vec![0x46; BODY_SIZE];
    fixture.put(b"key", &value)?;
    let old = reference(&mut fixture.engine, "kv", b"key")?;
    let snapshot = fixture.engine.export_snapshot()?;
    assert!(decode_snapshot(&snapshot)?
        .stores
        .iter()
        .all(|store| store.name != PAYLOAD_REGISTRY_STORE_NAME));
    let imported = fixture.engine.import_snapshot(&snapshot)?;
    assert_eq!(registry_state(&mut fixture.engine, old)?, (0, imported));
    let current = reference(&mut fixture.engine, "kv", b"key")?;
    assert_ne!(current.first_page_id, old.first_page_id);
    assert_eq!(registry_state(&mut fixture.engine, current)?, (1, 0));
    assert_eq!(fixture.get(b"key")?, Some(value));
    fixture.engine.checkpoint()?;
    fixture.engine.pager.discard_cache();
    fixture.main.protect(&[old, current])?;
    let reset = fixture.engine.reset()?;
    assert_eq!(fixture.main.protected_read_bytes(), 0);
    assert_eq!(registry_state(&mut fixture.engine, current)?, (0, reset));
    fixture.engine.checkpoint()?;
    fixture.reopen()?;
    fixture.put(b"reuse", &vec![0x57; BODY_SIZE])?;
    let reused = reference(&mut fixture.engine, "kv", b"reuse")?;
    assert_eq!(reused.first_page_id, old.first_page_id);
    Ok(())
}

#[test]
fn compaction_rewrites_current_bodies_and_registry_without_retained_feed() -> Result<()> {
    let mut fixture = Fixture::new(true)?;
    fixture.put(b"key", &vec![0x18; BODY_SIZE])?;
    let value = vec![0x29; BODY_SIZE];
    fixture.put(b"key", &value)?;
    let source_ref = reference(&mut fixture.engine, "kv", b"key")?;
    assert_eq!(registry_state(&mut fixture.engine, source_ref)?, (2, 0));
    let expected = decode_snapshot(&fixture.engine.export_snapshot()?)?;
    let manifest = ObservedBackend::new();
    let main = ObservedBackend::new();
    let wal = ObservedBackend::new();
    let mut compacted = Engine::open(
        "compacted-external",
        FileSet::new(manifest.clone(), main.clone(), wal.clone()),
        config(),
    )?;
    fixture.engine.compact_into(&mut compacted)?;
    let copied = reference(&mut compacted, "kv", b"key")?;
    assert_eq!(registry_state(&mut compacted, copied)?, (1, 0));
    assert!(!compacted.catalog.contains_key(SYSTEM_CHANGELOG_STORE_NAME));
    let actual = decode_snapshot(&compacted.export_snapshot()?)?;
    assert_eq!(actual.stores, expected.stores);
    assert!(main.len()? <= (BODY_SIZE + 64 * 1024) as u64);
    drop(compacted);
    let mut reopened = Engine::open(
        "compacted-external",
        FileSet::new(manifest.crash_copy(), main.crash_copy(), wal.crash_copy()),
        config(),
    )?;
    let reader = reopened.begin_tx(TxMode::Readonly)?;
    assert_eq!(reopened.get(reader, "kv", b"key")?, Some(value));
    reopened.rollback_tx(reader)?;
    Ok(())
}

#[test]
fn registry_name_is_reserved_and_snapshot_import_rejects_it() -> Result<()> {
    let mut fixture = Fixture::new(true)?;
    let tx = fixture.engine.begin_tx(TxMode::Readwrite)?;
    assert!(matches!(
        fixture.engine.create_store(tx, PAYLOAD_REGISTRY_STORE_NAME),
        Err(EngineError::ReservedStoreName(_))
    ));
    fixture.engine.rollback_tx(tx)?;
    let contents = SnapshotContents {
        source_last_committed_txid: 0,
        schema_version: 0,
        stores: vec![SnapshotStore {
            name: "x".repeat(PAYLOAD_REGISTRY_STORE_NAME.len()),
            flags: 0,
            entries: Vec::new(),
        }],
    };
    let mut encoded = encode_snapshot(&contents)?;
    let offset = crate::snapshot::SNAPSHOT_HEADER_SIZE
        + crate::snapshot::SNAPSHOT_BODY_PREFIX_SIZE
        + crate::snapshot::SNAPSHOT_STORE_HEADER_SIZE;
    encoded[offset..offset + PAYLOAD_REGISTRY_STORE_NAME.len()]
        .copy_from_slice(PAYLOAD_REGISTRY_STORE_NAME.as_bytes());
    let checksum = checksum::checksum_with_zeroed_region(
        &encoded,
        crate::snapshot::SNAPSHOT_CHECKSUM_OFFSET,
        4,
    );
    encoded
        [crate::snapshot::SNAPSHOT_CHECKSUM_OFFSET..crate::snapshot::SNAPSHOT_CHECKSUM_OFFSET + 4]
        .copy_from_slice(&checksum.to_le_bytes());
    assert!(matches!(
        fixture.engine.import_snapshot(&encoded),
        Err(EngineError::Corruption(_))
    ));
    Ok(())
}
