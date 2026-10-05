use moyodb_engine::engine::{Engine, OpenConfig, TxMode};
use moyodb_engine::error::Result;
use moyodb_engine::layout::{
    encode_superblock_slot, SuperblockState, PAGE_SIZE, SUPERBLOCK_SLOT_SIZE,
};
use moyodb_engine::recovery::select_superblock;
use moyodb_engine::storage::backend::{FileBackend, FileSet};
use moyodb_engine::storage::memory::{MemoryBackend, MemoryBundle};

#[derive(Clone, Copy, Debug)]
enum ReadFault {
    Short,
    Oversized,
}

#[derive(Debug)]
struct ReadLengthBackend {
    inner: MemoryBackend,
    fault: Option<(u64, ReadFault)>,
}

impl ReadLengthBackend {
    fn new(inner: MemoryBackend) -> Self {
        Self { inner, fault: None }
    }
}

impl FileBackend for ReadLengthBackend {
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let mut bytes = self.inner.read_at(offset, len)?;
        match self.fault {
            Some((target, ReadFault::Short)) if target == offset => {
                bytes.pop();
            }
            Some((target, ReadFault::Oversized)) if target == offset => bytes.push(0),
            _ => {}
        }
        Ok(bytes)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
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

fn superblock(generation: u64, active_slot: usize) -> SuperblockState {
    SuperblockState {
        generation,
        db_id: 7,
        page_size: PAGE_SIZE as u32,
        catalog_root_page_id: 1,
        next_page_id: 2,
        last_committed_txid: generation - 1,
        last_replayed_wal_offset: 0,
        active_slot,
    }
}

fn two_slot_manifest() -> MemoryBackend {
    let mut bytes = encode_superblock_slot(&superblock(1, 0)).to_vec();
    bytes.extend_from_slice(&encode_superblock_slot(&superblock(2, 1)));
    MemoryBackend::from_durable(bytes)
}

#[test]
fn manifest_read_length_mismatches_are_storage_errors() {
    for slot in 0..2 {
        for fault in [ReadFault::Short, ReadFault::Oversized] {
            let mut manifest = ReadLengthBackend::new(two_slot_manifest());
            manifest.fault = Some(((slot * SUPERBLOCK_SLOT_SIZE) as u64, fault));
            let err = select_superblock(&manifest)
                .expect_err("a backend read mismatch must not select an older superblock");
            assert_eq!(err.code(), "StorageError", "slot={slot}, fault={fault:?}");
        }
    }
}

#[test]
fn genuinely_truncated_manifest_slot_keeps_valid_previous_slot() {
    for kept in [0, 1, 16, SUPERBLOCK_SLOT_SIZE - 1] {
        let mut manifest = two_slot_manifest();
        manifest
            .truncate((SUPERBLOCK_SLOT_SIZE + kept) as u64)
            .unwrap();
        assert_eq!(
            select_superblock(&manifest).unwrap(),
            Some(superblock(1, 0)),
            "kept={kept}"
        );
    }
}

#[test]
fn checkpointed_open_does_not_fall_back_after_short_manifest_read() {
    let bundle = MemoryBundle::new();
    let name = "wal-recovery-short-manifest-read";
    let mut engine = Engine::open(name, bundle.files(), OpenConfig::default()).unwrap();
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    engine.put(tx, "kv", b"key", b"acknowledged").unwrap();
    engine.commit_tx(tx).unwrap();
    engine.checkpoint().unwrap();
    drop(engine);

    let published = select_superblock(&bundle.manifest).unwrap().unwrap();
    assert_eq!(published.last_committed_txid, 1);
    assert!(bundle.wal.durable_snapshot().unwrap().is_empty());
    let files = bundle.crash_recovered_files();
    let mut manifest = ReadLengthBackend::new(files.manifest);
    manifest.fault = Some((
        (published.active_slot * SUPERBLOCK_SLOT_SIZE) as u64,
        ReadFault::Short,
    ));
    let files = FileSet::new(
        manifest,
        ReadLengthBackend::new(files.main),
        ReadLengthBackend::new(files.wal),
    );
    let err = Engine::open(name, files, OpenConfig::default())
        .expect_err("open must fail instead of exposing the pre-commit catalog");
    assert_eq!(err.code(), "StorageError");
}
