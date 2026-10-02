use moyodb_engine::error::{EngineError, Result};
use moyodb_engine::layout::{
    encode_superblock_slot, page_offset, wal_record_total_len, SuperblockState, ValueKind,
    PAGE_SIZE, SUPERBLOCK_SLOT_SIZE, WAL_PAGE_IMAGE_BODY_HEADER_SIZE, WAL_RECORD_HEADER_SIZE,
};
use moyodb_engine::page::{encode_leaf_page, LeafCell};
use moyodb_engine::pager::Pager;
use moyodb_engine::recovery::{recover_if_needed, select_superblock};
use moyodb_engine::storage::backend::FileBackend;
use moyodb_engine::storage::memory::MemoryBackend;
use moyodb_engine::wal::{
    append_transaction, replay_wal_index, replay_wal_transactions, scan_wal_index, CommitRecord,
    PageImageRecord, ReplayTransaction,
};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Default)]
struct IoWork {
    reads: Vec<(u64, usize)>,
    writes: Vec<(u64, usize)>,
    flushes: usize,
    truncates: Vec<u64>,
}

#[derive(Clone, Copy, Debug)]
enum Fault {
    ReadError(usize),
    ShortRead(usize),
    CorruptRead(usize),
    PartialWrite(usize),
    FlushError(usize),
    TruncateError(usize),
}

#[derive(Clone, Copy)]
enum Operation {
    Read,
    Write,
    Flush,
    Truncate,
}

#[derive(Clone, Default)]
struct RecordingBackend {
    inner: MemoryBackend,
    work: Arc<Mutex<IoWork>>,
    fault: Arc<Mutex<Option<Fault>>>,
    triggered: Arc<Mutex<bool>>,
}

impl RecordingBackend {
    fn from_durable(bytes: Vec<u8>) -> Self {
        Self {
            inner: MemoryBackend::from_durable(bytes),
            ..Self::default()
        }
    }

    fn work(&self) -> IoWork {
        self.work.lock().expect("work lock").clone()
    }

    fn reset(&self) {
        *self.work.lock().expect("work lock") = IoWork::default();
        *self.triggered.lock().expect("trigger lock") = false;
    }

    fn arm(&self, fault: Fault) {
        self.reset();
        *self.fault.lock().expect("fault lock") = Some(fault);
    }

    fn trigger(&self, operation: Operation, call: usize) -> Option<Fault> {
        let fault = *self.fault.lock().expect("fault lock");
        let target = match (fault, operation) {
            (
                Some(
                    Fault::ReadError(target)
                    | Fault::ShortRead(target)
                    | Fault::CorruptRead(target),
                ),
                Operation::Read,
            )
            | (Some(Fault::PartialWrite(target)), Operation::Write)
            | (Some(Fault::FlushError(target)), Operation::Flush)
            | (Some(Fault::TruncateError(target)), Operation::Truncate) => target,
            _ => return None,
        };
        if call != target {
            return None;
        }
        *self.triggered.lock().expect("trigger lock") = true;
        fault
    }

    fn snapshot(&self) -> Vec<u8> {
        self.inner.durable_snapshot().expect("durable memory bytes")
    }
}

impl FileBackend for RecordingBackend {
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let call = {
            let mut work = self.work.lock().expect("work lock");
            work.reads.push((offset, len));
            work.reads.len()
        };
        let fault = self.trigger(Operation::Read, call);
        if matches!(fault, Some(Fault::ReadError(_))) {
            return Err(EngineError::Storage("injected WAL read error".into()));
        }
        let mut bytes = self.inner.read_at(offset, len)?;
        match fault {
            Some(Fault::ShortRead(_)) => {
                bytes.pop();
            }
            Some(Fault::CorruptRead(_)) => bytes[0] ^= 1,
            _ => {}
        }
        Ok(bytes)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        let call = {
            let mut work = self.work.lock().expect("work lock");
            work.writes.push((offset, bytes.len()));
            work.writes.len()
        };
        if matches!(
            self.trigger(Operation::Write, call),
            Some(Fault::PartialWrite(_))
        ) {
            self.inner.write_at(offset, &bytes[..bytes.len() / 2])?;
            return Err(EngineError::Storage("injected partial write".into()));
        }
        self.inner.write_at(offset, bytes)
    }

    fn flush(&mut self) -> Result<()> {
        let call = {
            let mut work = self.work.lock().expect("work lock");
            work.flushes += 1;
            work.flushes
        };
        if matches!(
            self.trigger(Operation::Flush, call),
            Some(Fault::FlushError(_))
        ) {
            return Err(EngineError::Storage("injected flush error".into()));
        }
        self.inner.flush()
    }

    fn len(&self) -> Result<u64> {
        self.inner.len()
    }

    fn truncate(&mut self, size: u64) -> Result<()> {
        let call = {
            let mut work = self.work.lock().expect("work lock");
            work.truncates.push(size);
            work.truncates.len()
        };
        if matches!(
            self.trigger(Operation::Truncate, call),
            Some(Fault::TruncateError(_))
        ) {
            return Err(EngineError::Storage("injected truncate error".into()));
        }
        self.inner.truncate(size)
    }

    fn close(&mut self) -> Result<()> {
        self.inner.close()
    }
}

fn leaf(page_id: u64, marker: u8) -> Result<Vec<u8>> {
    encode_leaf_page(
        page_id,
        0,
        0,
        &[LeafCell {
            key: b"key".to_vec(),
            value: vec![marker],
            value_kind: ValueKind::Inline,
            total_value_len: 1,
            overflow_head_page_id: 0,
        }],
    )
}

struct Fixture {
    manifest: RecordingBackend,
    main: RecordingBackend,
    wal: RecordingBackend,
    expected: SuperblockState,
    oracle_main: Vec<u8>,
    oracle_manifest: Vec<u8>,
    wal_end: u64,
}

impl Fixture {
    fn new(transactions: &[Vec<(u64, Vec<u8>)>], next_page_id: u64) -> Result<Self> {
        Self::with_initial(transactions, next_page_id, &[])
    }

    fn with_initial(
        transactions: &[Vec<(u64, Vec<u8>)>],
        next_page_id: u64,
        extra_initial: &[(u64, Vec<u8>)],
    ) -> Result<Self> {
        let mut manifest = RecordingBackend::default();
        let mut main = RecordingBackend::default();
        let mut wal = RecordingBackend::default();
        let base = SuperblockState {
            generation: 1,
            db_id: 7,
            page_size: PAGE_SIZE as u32,
            catalog_root_page_id: 1,
            next_page_id: extra_initial
                .iter()
                .map(|(id, _)| id + 1)
                .max()
                .unwrap_or(2),
            last_committed_txid: 0,
            last_replayed_wal_offset: 0,
            active_slot: 0,
        };
        manifest.write_at(0, &encode_superblock_slot(&base))?;
        manifest.write_at(SUPERBLOCK_SLOT_SIZE as u64, &[0; SUPERBLOCK_SLOT_SIZE])?;
        manifest.flush()?;
        main.write_at(page_offset(1), &leaf(1, 0)?)?;
        for (page_id, bytes) in extra_initial {
            main.write_at(page_offset(*page_id), bytes)?;
        }
        main.flush()?;
        let mut oracle = MemoryBackend::from_durable(main.snapshot());
        let mut expected = base.clone();
        let mut offset = 0;
        for (index, images) in transactions.iter().enumerate() {
            let txid = index as u64 + 1;
            append_transaction(
                &mut wal,
                &mut offset,
                txid,
                images,
                &CommitRecord {
                    txid,
                    new_catalog_root_page_id: 1,
                    new_next_page_id: next_page_id,
                    changed_page_count: images.len() as u32,
                },
            )?;
            // The oracle applies every input image, including later overwrites.
            for (page_id, bytes) in images {
                oracle.write_at(page_offset(*page_id), bytes)?;
            }
            expected.last_committed_txid = txid;
            expected.next_page_id = next_page_id;
            expected.last_replayed_wal_offset = offset;
        }
        wal.flush()?;
        oracle.flush()?;
        let mut oracle_manifest = manifest.snapshot();
        if !transactions.is_empty() {
            expected.generation += 1;
            expected.active_slot = 1;
            oracle_manifest[SUPERBLOCK_SLOT_SIZE..]
                .copy_from_slice(&encode_superblock_slot(&expected));
        }
        let fixture = Self {
            manifest,
            main,
            wal,
            expected,
            oracle_main: oracle.durable_snapshot().expect("oracle durable bytes"),
            oracle_manifest,
            wal_end: offset,
        };
        fixture.reset();
        Ok(fixture)
    }

    fn reset(&self) {
        self.manifest.reset();
        self.main.reset();
        self.wal.reset();
    }

    fn recover(&mut self) -> Result<SuperblockState> {
        let base = select_superblock(&self.manifest)?.expect("fixture has a superblock");
        let mut pager = Pager::new(self.main.clone(), 4);
        recover_if_needed(&mut self.manifest, &mut pager, &mut self.wal, &base)
    }

    fn crash_copy(&self) -> Self {
        Self {
            manifest: RecordingBackend::from_durable(self.manifest.snapshot()),
            main: RecordingBackend::from_durable(self.main.snapshot()),
            wal: RecordingBackend::from_durable(self.wal.snapshot()),
            expected: self.expected.clone(),
            oracle_main: self.oracle_main.clone(),
            oracle_manifest: self.oracle_manifest.clone(),
            wal_end: self.wal_end,
        }
    }

    fn assert_oracle(&self, recovered: &SuperblockState) {
        assert_eq!(recovered, &self.expected);
        assert_eq!(self.main.snapshot(), self.oracle_main);
        assert_eq!(self.manifest.snapshot(), self.oracle_manifest);
        assert!(self.wal.snapshot().is_empty());
    }
}

fn adjacent_images() -> Result<Vec<(u64, Vec<u8>)>> {
    (1..=65)
        .map(|page_id| Ok((page_id, leaf(page_id, page_id as u8)?)))
        .collect()
}

#[test]
fn adjacent_wal_source_reads_are_bounded_and_batched() -> Result<()> {
    let fixture = Fixture::new(&[adjacent_images()?], 66)?;
    let txs = scan_wal_index(&fixture.wal)?;
    fixture.reset();
    let mut pager = Pager::new(fixture.main.clone(), 4);
    replay_wal_index(&mut pager, &fixture.wal, &txs)?;
    assert_eq!(fixture.main.snapshot(), fixture.oracle_main);
    let image_header = WAL_RECORD_HEADER_SIZE + WAL_PAGE_IMAGE_BODY_HEADER_SIZE;
    let image_record = wal_record_total_len(WAL_PAGE_IMAGE_BODY_HEADER_SIZE + PAGE_SIZE);
    let reads = fixture.wal.work().reads;
    assert_eq!(
        reads,
        [
            (image_header as u64, 64 * image_record - image_header),
            ((image_header + 64 * image_record) as u64, PAGE_SIZE),
        ],
        "65 adjacent images must use two bounded source reads"
    );
    assert_eq!(
        reads.iter().map(|(_, len)| len).sum::<usize>(),
        65 * PAGE_SIZE + 63 * image_header
    );
    assert_eq!(
        fixture.main.work().writes,
        [
            (page_offset(1), 64 * PAGE_SIZE),
            (page_offset(65), PAGE_SIZE)
        ]
    );
    assert_eq!(fixture.main.work().flushes, 1);
    Ok(())
}

#[test]
fn short_source_reads_preserve_page_corruption_error() -> Result<()> {
    for count in [1, 3] {
        let images = (1..=count)
            .map(|page_id| Ok((page_id, leaf(page_id, page_id as u8)?)))
            .collect::<Result<Vec<_>>>()?;
        let fixture = Fixture::new(&[images], count + 1)?;
        let txs = scan_wal_index(&fixture.wal)?;
        fixture.wal.arm(Fault::ShortRead(1));
        let (page_id, offset) = txs[0].pages[0];
        // Before batching, replay passed each raw PAGE_SIZE read straight to
        // verify_page_image. Materialized replay invokes that same validator.
        let old_page = PageImageRecord {
            txid: txs[0].txid,
            page_id,
            bytes: fixture.wal.read_at(offset, PAGE_SIZE)?,
        };
        let old_tx = ReplayTransaction {
            txid: txs[0].txid,
            page_images: vec![old_page],
            commit: CommitRecord {
                changed_page_count: 1,
                ..txs[0].commit.clone()
            },
            end_offset: txs[0].end_offset,
        };
        let mut old_pager = Pager::new(RecordingBackend::default(), 4);
        let old_error = replay_wal_transactions(&mut old_pager, &[old_tx]).unwrap_err();
        assert_eq!(
            old_error,
            EngineError::Corruption("page size mismatch: expected 4096, got 4095".into())
        );

        fixture.wal.arm(Fault::ShortRead(1));
        let mut pager = Pager::new(fixture.main.clone(), 4);
        let error = replay_wal_index(&mut pager, &fixture.wal, &txs).unwrap_err();
        assert_eq!(error, old_error, "source run of {count} images");
        assert!(fixture.main.work().writes.is_empty());
        assert_eq!(fixture.main.work().flushes, 0);
    }
    Ok(())
}

#[test]
fn latest_images_keep_gaps_and_mixed_source_offsets() -> Result<()> {
    let transactions = [
        vec![
            (1, leaf(1, 1)?),
            (2, leaf(2, 1)?),
            (4, leaf(4, 1)?),
            (5, leaf(5, 1)?),
        ],
        vec![(2, leaf(2, 2)?), (4, leaf(4, 2)?)],
    ];
    let mut fixture = Fixture::with_initial(&transactions, 6, &[(3, leaf(3, 99)?)])?;
    let recovered = fixture.recover()?;
    fixture.assert_oracle(&recovered);
    assert_eq!(
        fixture.main.work().writes,
        [
            (page_offset(1), 2 * PAGE_SIZE),
            (page_offset(4), 2 * PAGE_SIZE)
        ]
    );
    Ok(())
}

#[test]
fn repeated_commits_replay_only_final_page_and_keep_collecting_api() -> Result<()> {
    let transactions = (1..=32)
        .map(|marker| Ok(vec![(1, leaf(1, marker)?)]))
        .collect::<Result<Vec<_>>>()?;
    let mut fixture = Fixture::new(&transactions, 2)?;
    let collected = scan_wal_index(&fixture.wal)?;
    assert_eq!(collected.len(), 32);
    assert!(collected.iter().all(|tx| tx.pages.len() == 1));
    assert_eq!(
        collected.last().map(|tx| tx.end_offset),
        Some(fixture.wal_end)
    );
    fixture.reset();
    let recovered = fixture.recover()?;
    fixture.assert_oracle(&recovered);
    let wal_work = fixture.wal.work();
    assert_eq!(
        wal_work.reads.len(),
        (fixture.wal_end as usize).div_ceil(64 * 1024) + 1
    );
    assert_eq!(
        wal_work.reads.iter().map(|(_, len)| len).sum::<usize>(),
        fixture.wal_end as usize + PAGE_SIZE
    );
    assert_eq!(fixture.main.work().writes, [(page_offset(1), PAGE_SIZE)]);
    assert_eq!(fixture.main.work().flushes, 1);
    assert_eq!(fixture.manifest.work().writes.len(), 1);
    assert_eq!(fixture.manifest.work().flushes, 1);
    assert_eq!(wal_work.truncates, [0]);
    assert_eq!(wal_work.flushes, 1);
    Ok(())
}

#[test]
fn torn_last_commit_preserves_committed_prefix_bytes() -> Result<()> {
    let transactions = (1..=32)
        .map(|marker| Ok(vec![(1, leaf(1, marker)?)]))
        .collect::<Result<Vec<_>>>()?;
    let mut fixture = Fixture::new(&transactions, 2)?;
    let prefix = Fixture::new(&transactions[..31], 2)?;
    let bytes = fixture.wal.snapshot();
    fixture.wal = RecordingBackend::from_durable(bytes[..bytes.len() - 1].to_vec());
    fixture.expected = prefix.expected;
    fixture.oracle_main = prefix.oracle_main;
    fixture.oracle_manifest = prefix.oracle_manifest;
    let recovered = fixture.recover()?;
    fixture.assert_oracle(&recovered);
    Ok(())
}

#[test]
fn overwritten_committed_corruption_prevents_every_recovery_write() -> Result<()> {
    let transactions = [
        vec![(1, leaf(1, 1)?)],
        vec![(1, vec![0; PAGE_SIZE])],
        vec![(1, leaf(1, 3)?)],
    ];
    for published_txid in [0, 2] {
        let mut fixture = Fixture::new(&transactions, 2)?;
        let mut base = select_superblock(&fixture.manifest)?.expect("fixture superblock");
        base.last_committed_txid = published_txid;
        fixture
            .manifest
            .write_at(0, &encode_superblock_slot(&base))?;
        fixture.manifest.flush()?;
        fixture.reset();
        let before = (
            fixture.manifest.snapshot(),
            fixture.main.snapshot(),
            fixture.wal.snapshot(),
        );
        assert_eq!(fixture.recover().unwrap_err().code(), "CorruptionError");
        assert!(fixture.main.work().writes.is_empty());
        assert_eq!(fixture.main.work().flushes, 0);
        assert!(fixture.manifest.work().writes.is_empty());
        assert_eq!(fixture.manifest.work().flushes, 0);
        assert!(fixture.wal.work().truncates.is_empty());
        assert_eq!(fixture.wal.work().flushes, 0);
        assert_eq!(
            (
                fixture.manifest.snapshot(),
                fixture.main.snapshot(),
                fixture.wal.snapshot()
            ),
            before
        );
    }
    Ok(())
}

#[test]
fn recovery_faults_retry_to_the_same_durable_bytes() -> Result<()> {
    let images = adjacent_images()?;
    let fixture = Fixture::new(std::slice::from_ref(&images), 66)?;
    let second_replay_read = (fixture.wal_end as usize).div_ceil(64 * 1024) + 2;
    // The scan has completed before these read faults. Both source batches,
    // installation and publication are covered without a large database.
    let faults = [
        (0, Fault::ReadError(second_replay_read)),
        (0, Fault::ShortRead(second_replay_read)),
        (0, Fault::CorruptRead(second_replay_read)),
        (1, Fault::PartialWrite(2)),
        (1, Fault::FlushError(1)),
        (2, Fault::PartialWrite(1)),
        (2, Fault::FlushError(1)),
        (0, Fault::TruncateError(1)),
        (0, Fault::FlushError(1)),
    ];
    for (file, fault) in faults {
        let mut interrupted = Fixture::new(std::slice::from_ref(&images), 66)?;
        let original_wal = interrupted.wal.snapshot();
        let backend = match file {
            0 => interrupted.wal.clone(),
            1 => interrupted.main.clone(),
            _ => interrupted.manifest.clone(),
        };
        backend.arm(fault);
        assert!(
            interrupted.recover().is_err(),
            "{fault:?} must stop recovery"
        );
        assert!(
            *backend.triggered.lock().expect("trigger lock"),
            "unreached {fault:?}"
        );
        assert_eq!(
            interrupted.wal.snapshot(),
            original_wal,
            "{fault:?} must retain durable WAL"
        );
        let mut restarted = interrupted.crash_copy();
        let recovered = restarted.recover()?;
        restarted.assert_oracle(&recovered);
    }
    Ok(())
}
