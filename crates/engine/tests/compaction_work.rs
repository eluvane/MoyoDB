use moyodb_engine::catalog::ChangeFeedPolicy;
use moyodb_engine::engine::{Engine, OpenConfig, TxMode};
use moyodb_engine::error::Result;
use moyodb_engine::layout::{page_offset, PAGE_SIZE};
use moyodb_engine::storage::backend::{FileBackend, FileSet};
use moyodb_engine::storage::memory::MemoryBackend;
use moyodb_engine::EngineError;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Work {
    reads: usize,
    writes: Vec<(u64, usize)>,
    flushes: usize,
    fail_write: Option<usize>,
    fail_flush: bool,
}

#[derive(Clone)]
struct TrackedBackend {
    inner: MemoryBackend,
    work: Arc<Mutex<Work>>,
}

impl TrackedBackend {
    fn new() -> Self {
        Self {
            inner: MemoryBackend::new(),
            work: Arc::new(Mutex::new(Work::default())),
        }
    }

    fn reset(&self) {
        *self.work.lock().unwrap() = Work::default();
    }

    fn recovered(&self) -> Self {
        Self {
            inner: MemoryBackend::from_durable(self.inner.durable_snapshot().unwrap()),
            work: Arc::new(Mutex::new(Work::default())),
        }
    }
}

impl FileBackend for TrackedBackend {
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.work.lock().unwrap().reads += 1;
        self.inner.read_at(offset, len)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        let failed = {
            let mut work = self.work.lock().unwrap();
            work.writes.push((offset, bytes.len()));
            work.fail_write == Some(work.writes.len())
        };
        if failed {
            return Err(EngineError::Storage(
                "compaction fixture write fault".into(),
            ));
        }
        self.inner.write_at(offset, bytes)
    }

    fn flush(&mut self) -> Result<()> {
        let failed = {
            let mut work = self.work.lock().unwrap();
            work.flushes += 1;
            work.fail_flush
        };
        if failed {
            return Err(EngineError::Storage(
                "compaction fixture flush fault".into(),
            ));
        }
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

    fn durable_snapshot(&self) -> Option<Vec<u8>> {
        self.inner.durable_snapshot()
    }
}

struct Bundle {
    files: [TrackedBackend; 3],
}

impl Bundle {
    fn open(name: &str) -> (Self, Engine<TrackedBackend>) {
        let files = std::array::from_fn(|_| TrackedBackend::new());
        let bundle = Self { files };
        let engine = Engine::open(name, bundle.file_set(), OpenConfig::default()).unwrap();
        (bundle, engine)
    }

    fn file_set(&self) -> FileSet<TrackedBackend> {
        FileSet::new(
            self.files[0].clone(),
            self.files[1].clone(),
            self.files[2].clone(),
        )
    }

    fn reopen(&self, name: &str) -> Engine<TrackedBackend> {
        Engine::open(
            name,
            FileSet::new(
                self.files[0].recovered(),
                self.files[1].recovered(),
                self.files[2].recovered(),
            ),
            OpenConfig::default(),
        )
        .unwrap()
    }

    fn reset(&self) {
        for file in &self.files {
            file.reset();
        }
    }
}

fn seed(engine: &mut Engine<TrackedBackend>, rows: u32, value_len: usize) {
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine
        .set_change_feed_policy(
            tx,
            ChangeFeedPolicy {
                enabled: false,
                ..ChangeFeedPolicy::default()
            },
        )
        .unwrap();
    engine.create_store(tx, "kv").unwrap();
    for key in 0..rows {
        engine
            .put(tx, "kv", &key.to_be_bytes(), &vec![key as u8; value_len])
            .unwrap();
    }
    engine.commit_tx(tx).unwrap();
    engine.checkpoint().unwrap();
}

#[test]
fn compaction_batches_consecutive_pages_and_reopens_without_changing_source() {
    let (source_bundle, mut source) = Bundle::open("compact-work-source");
    seed(&mut source, 32, 8192);
    let source_before = source.export_snapshot().unwrap();
    let (target_bundle, mut target) = Bundle::open("compact-work-target");
    let initial_page_id = target.stats().unwrap().next_page_id;
    source_bundle.reset();
    target_bundle.reset();

    let txid = source.compact_into(&mut target).unwrap();
    let target_stats = target.stats().unwrap();
    let image_count = (target_stats.next_page_id - initial_page_id) as usize;
    let writes = target_bundle.files[1].work.lock().unwrap().writes.clone();
    assert!(
        image_count > 64,
        "fixture must cover a full batch and its tail"
    );
    assert_eq!(
        writes.iter().map(|(_, len)| len).sum::<usize>(),
        image_count * PAGE_SIZE
    );
    let mut next_offset = page_offset(initial_page_id);
    for (offset, len) in &writes {
        assert_eq!(*offset, next_offset);
        assert_eq!(len % PAGE_SIZE, 0);
        assert!(*len <= 64 * PAGE_SIZE, "page batching must remain bounded");
        next_offset += *len as u64;
    }
    assert_eq!(source.export_snapshot().unwrap(), source_before);
    let mut reopened = target_bundle.reopen("compact-work-target");
    assert_eq!(reopened.stats().unwrap().last_committed_txid, txid);
    let ro = reopened.begin_tx(TxMode::Readonly).unwrap();
    for key in 0..32u32 {
        assert_eq!(
            reopened.get(ro, "kv", &key.to_be_bytes()).unwrap(),
            Some(vec![key as u8; 8192])
        );
    }
    reopened.rollback_tx(ro).unwrap();

    // Backend write counts measure native/OPFS crossings, independent of timing.
    assert!(
        writes.len() <= image_count.div_ceil(64) + 1,
        "{image_count} images required {} backend writes",
        writes.len()
    );
}

#[test]
fn default_compaction_preserves_internal_stores_and_empty_store_metadata() {
    let (_source_bundle, mut source) = Bundle::open("compact-internal-source");
    let tx = source.begin_tx(TxMode::Readwrite).unwrap();
    source.set_schema_version(tx, 7).unwrap();
    for store in [
        "kv",
        "empty",
        "__browserdb:indexes",
        "__browserdb:index:known",
        "__browserdb:other",
    ] {
        source.create_store(tx, store).unwrap();
    }
    source.put(tx, "kv", b"key", b"value").unwrap();
    for store in [
        "__browserdb:indexes",
        "__browserdb:index:known",
        "__browserdb:other",
    ] {
        source
            .put(tx, store, b"internal", store.as_bytes())
            .unwrap();
    }
    source.commit_tx(tx).unwrap();
    let (target_bundle, mut target) = Bundle::open("compact-internal-target");
    source.compact_into(&mut target).unwrap();

    let mut reopened = target_bundle.reopen("compact-internal-target");
    assert_eq!(reopened.schema_version(), 7);
    let ro = reopened.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(reopened.get(ro, "empty", b"missing").unwrap(), None);
    for store in [
        "__browserdb:indexes",
        "__browserdb:index:known",
        "__browserdb:other",
    ] {
        assert_eq!(
            reopened.get(ro, store, b"internal").unwrap(),
            Some(store.as_bytes().to_vec())
        );
    }
    reopened.rollback_tx(ro).unwrap();
}

#[test]
fn rebuild_compaction_skips_only_explicit_known_internal_stores() {
    let (_source_bundle, mut source) = Bundle::open("compact-skip-source");
    let tx = source.begin_tx(TxMode::Readwrite).unwrap();
    source.set_schema_version(tx, 7).unwrap();
    for store in [
        "kv",
        "empty",
        "__browserdb:indexes",
        "__browserdb:index:known",
        "__browserdb:other",
    ] {
        source.create_store(tx, store).unwrap();
    }
    source.put(tx, "kv", b"key", b"value").unwrap();
    for store in [
        "__browserdb:indexes",
        "__browserdb:index:known",
        "__browserdb:other",
    ] {
        source
            .put(tx, store, b"internal", store.as_bytes())
            .unwrap();
    }
    source.commit_tx(tx).unwrap();
    let (target_bundle, mut target) = Bundle::open("compact-skip-target");
    source
        .compact_into_skipping_stores(
            &mut target,
            &[
                "__browserdb:indexes".into(),
                "__browserdb:index:known".into(),
                "__browserdb:index:known".into(),
                "__browserdb:index:missing".into(),
            ],
        )
        .unwrap();

    let mut reopened = target_bundle.reopen("compact-skip-target");
    assert_eq!(reopened.schema_version(), 7);
    let ro = reopened.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(
        reopened.get(ro, "kv", b"key").unwrap(),
        Some(b"value".to_vec())
    );
    assert_eq!(reopened.get(ro, "empty", b"missing").unwrap(), None);
    assert_eq!(
        reopened.get(ro, "__browserdb:other", b"internal").unwrap(),
        Some(b"__browserdb:other".to_vec())
    );
    for store in ["__browserdb:indexes", "__browserdb:index:known"] {
        assert_eq!(
            reopened.get(ro, store, b"internal").unwrap_err(),
            EngineError::StoreNotFound(store.into())
        );
    }
    reopened.rollback_tx(ro).unwrap();
    let ro = source.begin_tx(TxMode::Readonly).unwrap();
    for store in ["__browserdb:indexes", "__browserdb:index:known"] {
        assert_eq!(
            source.get(ro, store, b"internal").unwrap(),
            Some(store.as_bytes().to_vec())
        );
    }
    source.rollback_tx(ro).unwrap();
}

#[test]
fn rebuild_compaction_rejects_skipping_user_stores_before_target_writes() {
    let (_source_bundle, mut source) = Bundle::open("compact-skip-invalid-source");
    seed(&mut source, 1, 32);
    let (target_bundle, mut target) = Bundle::open("compact-skip-invalid-target");
    target_bundle.reset();
    let error = source
        .compact_into_skipping_stores(&mut target, &["kv".into()])
        .unwrap_err();
    assert_eq!(error.code(), "InvalidRangeError");
    assert!(target_bundle.files[1]
        .work
        .lock()
        .unwrap()
        .writes
        .is_empty());
    assert_eq!(target.stats().unwrap().last_committed_txid, 0);
    let ro = target.begin_tx(TxMode::Readonly).unwrap();
    target.rollback_tx(ro).unwrap();
    let ro = source.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(
        source.get(ro, "kv", &[0, 0, 0, 0]).unwrap(),
        Some(vec![0; 32])
    );
    source.rollback_tx(ro).unwrap();
}

#[test]
fn compaction_storage_failures_poison_only_target_and_keep_its_old_manifest() {
    for fault in 0..3 {
        let (_source_bundle, mut source) = Bundle::open("compact-fault-source");
        seed(&mut source, 4, 8192);
        let source_before = source.export_snapshot().unwrap();
        let (target_bundle, mut target) = Bundle::open("compact-fault-target");
        let manifest_before = target_bundle.files[0].durable_snapshot().unwrap();
        target_bundle.reset();
        match fault {
            0 => target_bundle.files[1].work.lock().unwrap().fail_write = Some(1),
            1 => target_bundle.files[1].work.lock().unwrap().fail_flush = true,
            _ => target_bundle.files[0].work.lock().unwrap().fail_write = Some(1),
        }

        assert_eq!(
            source.compact_into(&mut target).unwrap_err().code(),
            "StorageError"
        );
        assert_eq!(
            target.begin_tx(TxMode::Readonly).unwrap_err().code(),
            "RecoveryRequiredError"
        );
        assert_eq!(source.export_snapshot().unwrap(), source_before);
        assert_eq!(
            target_bundle.files[0].durable_snapshot().unwrap(),
            manifest_before
        );
        let mut reopened = target_bundle.reopen("compact-fault-target");
        assert!(reopened.store_names().is_empty());
        assert_eq!(reopened.stats().unwrap().last_committed_txid, 0);
    }
}
