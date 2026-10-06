use super::*;
use crate::layout::{page_offset, PageKind, ValueKind, PAGE_SIZE};
use crate::page::decode_page;
use crate::storage::memory::MemoryBackend;
use std::sync::Mutex;

const VALUE_LEN: usize = 16 * 1024;
const OVERFLOW_PAGES: usize = 5;
const DB_NAME: &str = "checkpoint-retirement";

#[derive(Default)]
struct MainWrites {
    bytes: usize,
    pages: Vec<(u64, PageKind)>,
    fail_next: bool,
}

#[derive(Clone)]
struct RecordingBackend {
    inner: MemoryBackend,
    writes: Arc<Mutex<MainWrites>>,
    record_pages: bool,
}

impl RecordingBackend {
    fn new() -> Self {
        Self {
            inner: MemoryBackend::new(),
            writes: Arc::new(Mutex::new(MainWrites::default())),
            record_pages: false,
        }
    }

    fn reset_writes(&self) {
        *self.writes.lock().unwrap() = MainWrites::default();
    }

    fn crash_copy(&self) -> Self {
        Self {
            inner: MemoryBackend::from_durable(self.inner.durable_snapshot().unwrap()),
            writes: Arc::new(Mutex::new(MainWrites::default())),
            record_pages: self.record_pages,
        }
    }
}

impl FileBackend for RecordingBackend {
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.inner.read_at(offset, len)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        let mut writes = self.writes.lock().unwrap();
        if writes.fail_next {
            writes.fail_next = false;
            return Err(EngineError::Storage("checkpoint main write failed".into()));
        }
        writes.bytes += bytes.len();
        if self.record_pages {
            for (index, image) in bytes.as_chunks::<PAGE_SIZE>().0.iter().enumerate() {
                let page = decode_page(image)?;
                assert_eq!(
                    offset + (index * PAGE_SIZE) as u64,
                    page_offset(page.header.page_id)
                );
                writes
                    .pages
                    .push((page.header.page_id, page.header.page_kind));
            }
        }
        self.inner.write_at(offset, bytes)
    }

    fn flush(&mut self) -> Result<()> {
        self.inner.flush()
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
}

struct Fixture {
    engine: Engine<RecordingBackend>,
    manifest: RecordingBackend,
    main: RecordingBackend,
    wal: RecordingBackend,
}

impl Fixture {
    fn new(feed: bool) -> Result<Self> {
        let manifest = RecordingBackend::new();
        let mut main = RecordingBackend::new();
        main.record_pages = true;
        let wal = RecordingBackend::new();
        let mut engine = Engine::open(
            DB_NAME,
            FileSet::new(manifest.clone(), main.clone(), wal.clone()),
            OpenConfig {
                cache_pages: 1,
                checkpoint_wal_bytes: u64::MAX,
                checkpoint_dirty_pages: usize::MAX,
                ..OpenConfig::default()
            },
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
        engine.commit_tx(tx)?;
        engine.checkpoint()?;
        main.reset_writes();
        Ok(Self {
            engine,
            manifest,
            main,
            wal,
        })
    }

    fn put(&mut self, value: &[u8]) -> Result<u64> {
        let tx = self.engine.begin_tx(TxMode::Readwrite)?;
        self.engine.put(tx, "kv", b"key", value)?;
        self.engine.commit_tx(tx)
    }

    fn delete(&mut self) -> Result<u64> {
        let tx = self.engine.begin_tx(TxMode::Readwrite)?;
        assert!(self.engine.delete(tx, "kv", b"key")?);
        self.engine.commit_tx(tx)
    }

    fn reopen(&self) -> Result<Engine<RecordingBackend>> {
        Engine::open(
            DB_NAME,
            FileSet::new(
                self.manifest.crash_copy(),
                self.main.crash_copy(),
                self.wal.crash_copy(),
            ),
            OpenConfig::default(),
        )
    }
}

fn assert_value(engine: &mut Engine<RecordingBackend>, expected: Option<&[u8]>) -> Result<()> {
    let reader = engine.begin_tx(TxMode::Readonly)?;
    assert_eq!(engine.get(reader, "kv", b"key")?.as_deref(), expected);
    engine.rollback_tx(reader)
}

#[test]
fn checkpoint_omits_dead_committed_overflow_images() -> Result<()> {
    let mut fixture = Fixture::new(false)?;
    fixture.put(&vec![0x47; VALUE_LEN])?;
    let root = fixture.engine.catalog["kv"].store_root_page_id;
    let leaf = decode_page(&fixture.engine.pager.read_page(root)?)?;
    assert_eq!(leaf.leaf_cells[0].value_kind, ValueKind::Overflow);
    fixture.delete()?;
    let dirty = fixture.engine.pager.dirty_page_count();
    let wal_len = fixture.wal.len()?;
    assert!(wal_len > VALUE_LEN as u64);
    fixture.engine.checkpoint()?;
    let writes = fixture.main.writes.lock().unwrap();
    let overflow = writes
        .pages
        .iter()
        .filter(|(_, kind)| *kind == PageKind::Overflow)
        .count();
    println!("insert/delete 16KiB: dirty={dirty} main_pages={} main_bytes={} overflow_pages={overflow} wal_before={wal_len}", writes.pages.len(), writes.bytes);
    assert_eq!(overflow, 0);
    assert_eq!(writes.pages.len(), 2);
    assert_eq!(writes.bytes, 2 * PAGE_SIZE);
    drop(writes);
    assert_eq!(fixture.wal.len()?, 0);
    assert_value(&mut fixture.reopen()?, None)
}

#[test]
fn automatic_checkpoint_sees_the_current_commits_retirements() -> Result<()> {
    let mut fixture = Fixture::new(false)?;
    fixture.put(&vec![0x47; VALUE_LEN])?;
    fixture.engine.checkpoint_dirty_pages = 1;
    fixture.delete()?;
    let writes = fixture.main.writes.lock().unwrap();
    assert_eq!(writes.pages.len(), 2);
    assert!(writes
        .pages
        .iter()
        .all(|(_, kind)| *kind != PageKind::Overflow));
    assert_eq!(writes.bytes, 2 * PAGE_SIZE);
    drop(writes);
    assert_eq!(fixture.engine.pager.dirty_page_count(), 0);
    assert_eq!(fixture.wal.len()?, 0);
    assert_value(&mut fixture.reopen()?, None)
}

#[test]
fn checkpoint_preserves_dirty_pages_reachable_by_a_living_snapshot() -> Result<()> {
    let mut fixture = Fixture::new(false)?;
    let value = vec![0x47; VALUE_LEN];
    fixture.put(&value)?;
    let reader = fixture.engine.begin_tx(TxMode::Readonly)?;
    fixture.delete()?;
    assert!(fixture.engine.free_pages.retired_len() > OVERFLOW_PAGES);
    fixture.engine.checkpoint()?;
    let writes = fixture.main.writes.lock().unwrap();
    assert_eq!(
        writes
            .pages
            .iter()
            .filter(|(_, kind)| *kind == PageKind::Overflow)
            .count(),
        OVERFLOW_PAGES
    );
    drop(writes);
    assert_eq!(
        fixture.engine.get(reader, "kv", b"key")?.as_deref(),
        Some(value.as_slice())
    );
    assert_value(&mut fixture.engine, None)?;
    fixture.engine.rollback_tx(reader)?;
    fixture.engine.checkpoint()?;
    assert_eq!(fixture.engine.free_pages.retired_len(), 0);
    assert_value(&mut fixture.reopen()?, None)
}

#[test]
fn checkpoint_releases_retirements_after_the_last_old_snapshot_closes() -> Result<()> {
    let mut fixture = Fixture::new(false)?;
    fixture.put(&vec![0x47; VALUE_LEN])?;
    let reader = fixture.engine.begin_tx(TxMode::Readonly)?;
    fixture.delete()?;
    assert!(fixture.engine.pager.dirty_page_count() > OVERFLOW_PAGES);
    fixture.engine.rollback_tx(reader)?;
    fixture.engine.checkpoint()?;
    let writes = fixture.main.writes.lock().unwrap();
    assert_eq!(writes.pages.len(), 2);
    assert_eq!(writes.bytes, 2 * PAGE_SIZE);
    Ok(())
}

#[test]
fn checkpoint_preserves_reused_page_id_incarnations() -> Result<()> {
    let mut fixture = Fixture::new(false)?;
    fixture.put(&vec![0x47; VALUE_LEN])?;
    let allocated = fixture.engine.superblock.next_page_id;
    fixture.delete()?;
    let value = vec![0x91; VALUE_LEN];
    fixture.put(&value)?;
    assert!(fixture.engine.superblock.next_page_id <= allocated + 1);
    assert_value(&mut fixture.engine, Some(&value))?;
    fixture.engine.checkpoint()?;
    let writes = fixture.main.writes.lock().unwrap();
    assert_eq!(
        writes
            .pages
            .iter()
            .filter(|(_, kind)| *kind == PageKind::Overflow)
            .count(),
        OVERFLOW_PAGES
    );
    drop(writes);
    assert_value(&mut fixture.reopen()?, Some(&value))
}

#[test]
fn checkpoint_keeps_values_retained_in_the_change_feed() -> Result<()> {
    let mut fixture = Fixture::new(true)?;
    let seed_txid = fixture.engine.superblock.last_committed_txid;
    let value = vec![0x47; VALUE_LEN];
    fixture.put(&value)?;
    fixture.delete()?;
    fixture.engine.checkpoint()?;
    let writes = fixture.main.writes.lock().unwrap();
    assert_eq!(
        writes
            .pages
            .iter()
            .filter(|(_, kind)| *kind == PageKind::Overflow)
            .count(),
        OVERFLOW_PAGES
    );
    drop(writes);
    let mut reopened = fixture.reopen()?;
    assert_value(&mut reopened, None)?;
    let feed = reopened.changes_since(seed_txid, ChangeFeedOptions::default())?;
    assert_eq!(feed.changes.len(), 2);
    assert_eq!(feed.changes[0].kind, ChangeKind::Put);
    assert_eq!(feed.changes[0].value.as_deref(), Some(value.as_slice()));
    assert_eq!(feed.changes[1].kind, ChangeKind::Delete);
    Ok(())
}

#[test]
fn discarded_dirty_images_remain_recoverable_at_each_failpoint() -> Result<()> {
    for failpoint in [
        Failpoint::AfterWalFlush,
        Failpoint::AfterMainFlush,
        Failpoint::BeforeSuperblockFlush,
    ] {
        let mut fixture = Fixture::new(false)?;
        fixture.put(&vec![0x47; VALUE_LEN])?;
        let pending_txid = fixture.engine.next_commit_txid;
        fixture.engine.set_failpoint(Some(failpoint));
        assert!(matches!(
            fixture.delete(),
            Err(EngineError::InjectedFailure(_))
        ));
        assert!(fixture.engine.needs_recovery());
        assert!(fixture.wal.len()? > VALUE_LEN as u64);
        let mut recovered = fixture.reopen()?;
        assert_eq!(recovered.superblock.last_committed_txid, pending_txid);
        assert_value(&mut recovered, None)?;
        let report = fixture.engine.recover()?;
        assert_eq!(report.pending_txid, Some(pending_txid));
        assert!(report.pending_committed);
        assert_value(&mut fixture.engine, None)?;
    }
    Ok(())
}

#[test]
fn failed_checkpoint_recovers_the_commit_after_dead_images_are_discarded() -> Result<()> {
    let mut fixture = Fixture::new(false)?;
    fixture.put(&vec![0x47; VALUE_LEN])?;
    let txid = fixture.delete()?;
    assert_eq!(fixture.engine.pager.dirty_page_count(), 2);
    fixture.main.writes.lock().unwrap().fail_next = true;
    assert!(matches!(
        fixture.engine.checkpoint(),
        Err(EngineError::Storage(_))
    ));
    assert!(fixture.engine.needs_recovery());
    assert_eq!(fixture.engine.pager.dirty_page_count(), 2);
    assert!(fixture.wal.len()? > VALUE_LEN as u64);
    let mut reopened = fixture.reopen()?;
    assert_eq!(reopened.superblock.last_committed_txid, txid);
    assert_value(&mut reopened, None)?;
    assert_eq!(fixture.engine.recover()?.last_committed_txid, txid);
    assert_value(&mut fixture.engine, None)
}

#[test]
fn checkpoint_publishes_metadata_when_no_dirty_images_remain() -> Result<()> {
    let mut fixture = Fixture::new(false)?;
    let generation = fixture.engine.superblock.generation;
    let tx = fixture.engine.begin_tx(TxMode::Readwrite)?;
    let txid = fixture.engine.commit_tx(tx)?;
    assert_eq!(fixture.engine.pager.dirty_page_count(), 0);
    assert!(fixture.wal.len()? > 0);
    fixture.engine.checkpoint()?;
    assert!(fixture.main.writes.lock().unwrap().pages.is_empty());
    assert_eq!(fixture.engine.checkpoint_txid, txid);
    assert_eq!(fixture.engine.superblock.generation, generation + 1);
    assert_eq!(fixture.wal.len()?, 0);
    assert_eq!(fixture.reopen()?.superblock.last_committed_txid, txid);
    Ok(())
}
