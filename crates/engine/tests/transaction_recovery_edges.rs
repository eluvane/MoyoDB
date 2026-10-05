use moyodb_engine::engine::{Engine, EngineHealth, OpenConfig, TxMode};
use moyodb_engine::error::{EngineError, Result};
use moyodb_engine::storage::backend::{FileBackend, FileSet};
use moyodb_engine::storage::memory::MemoryBundle;
use moyodb_engine::{ChangeFeedOptions, MemoryBackend};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum File {
    Main,
    Manifest,
    Wal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    Read,
    Write,
    Flush,
    Truncate,
}

type Fault = Arc<Mutex<Option<(File, Operation)>>>;

struct FailingBackend {
    inner: MemoryBackend,
    file: File,
    fault: Fault,
}

impl FailingBackend {
    fn fail_if_armed(&self, operation: Operation) -> Result<()> {
        let mut fault = self.fault.lock().unwrap();
        if *fault == Some((self.file, operation)) {
            *fault = None;
            return Err(EngineError::Storage(format!(
                "injected {:?} {operation:?} failure",
                self.file
            )));
        }
        Ok(())
    }
}

impl FileBackend for FailingBackend {
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.fail_if_armed(Operation::Read)?;
        self.inner.read_at(offset, len)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        self.fail_if_armed(Operation::Write)?;
        self.inner.write_at(offset, bytes)
    }

    fn flush(&mut self) -> Result<()> {
        self.fail_if_armed(Operation::Flush)?;
        self.inner.flush()
    }

    fn len(&self) -> Result<u64> {
        self.inner.len()
    }

    fn truncate(&mut self, size: u64) -> Result<()> {
        self.fail_if_armed(Operation::Truncate)?;
        self.inner.truncate(size)
    }

    fn close(&mut self) -> Result<()> {
        self.inner.close()
    }
}

fn failing_files(bundle: &MemoryBundle, fault: &Fault) -> FileSet<FailingBackend> {
    let files = bundle.files();
    let wrap = |inner, file| FailingBackend {
        inner,
        file,
        fault: Arc::clone(fault),
    };
    FileSet::new(
        wrap(files.manifest, File::Manifest),
        wrap(files.main, File::Main),
        wrap(files.wal, File::Wal),
    )
}

#[test]
fn failed_healthy_recovery_blocks_work_until_successful_retry() -> Result<()> {
    for failure in [
        (File::Main, Operation::Flush),
        (File::Manifest, Operation::Flush),
        (File::Wal, Operation::Flush),
        (File::Manifest, Operation::Read),
        (File::Wal, Operation::Read),
        (File::Main, Operation::Write),
        (File::Manifest, Operation::Write),
        (File::Wal, Operation::Truncate),
    ] {
        let bundle = MemoryBundle::new();
        let fault = Arc::new(Mutex::new(None));
        let mut engine = Engine::open(
            "failed-healthy-recovery",
            failing_files(&bundle, &fault),
            OpenConfig::default(),
        )?;
        let seed = engine.begin_tx(TxMode::Readwrite)?;
        engine.create_store(seed, "docs")?;
        engine.put(seed, "docs", b"key", b"durable")?;
        let committed = engine.commit_tx(seed)?;
        assert!(engine.stats()?.dirty_pages > 0);
        let snapshot = engine.export_snapshot()?;
        let stale_reader = engine.begin_tx(TxMode::Readonly)?;
        let stale_writer = engine.begin_tx(TxMode::Readwrite)?;
        engine.put(stale_writer, "docs", b"discarded", b"uncommitted")?;

        *fault.lock().unwrap() = Some(failure);
        assert!(matches!(engine.recover(), Err(EngineError::Storage(_))));
        assert_eq!(*fault.lock().unwrap(), None, "unreached {failure:?}");
        assert!(
            matches!(engine.health(), EngineHealth::RecoveryRequired { .. }),
            "{failure:?}: {:?}",
            engine.health()
        );
        assert!(matches!(
            engine.begin_tx(TxMode::Readonly),
            Err(EngineError::RecoveryRequired(_))
        ));
        assert!(matches!(
            engine.begin_tx(TxMode::Readwrite),
            Err(EngineError::RecoveryRequired(_))
        ));
        assert!(matches!(
            engine.get(stale_reader, "docs", b"key"),
            Err(EngineError::RecoveryRequired(_))
        ));
        assert!(matches!(
            engine.commit_tx(stale_writer),
            Err(EngineError::RecoveryRequired(_))
        ));
        assert!(matches!(
            engine.changes_since(0, ChangeFeedOptions::default()),
            Err(EngineError::RecoveryRequired(_))
        ));
        assert!(matches!(
            engine.checkpoint(),
            Err(EngineError::RecoveryRequired(_))
        ));
        assert!(matches!(
            engine.import_snapshot(&snapshot),
            Err(EngineError::RecoveryRequired(_))
        ));

        let report = engine.recover()?;
        assert_eq!(engine.health(), &EngineHealth::Healthy);
        assert_eq!(report.last_committed_txid, committed);
        assert_eq!(report.pending_txid, None);
        assert!(!report.pending_committed);
        assert!(matches!(
            engine.get(stale_reader, "docs", b"key"),
            Err(EngineError::TransactionClosed)
        ));
        assert!(matches!(
            engine.commit_tx(stale_writer),
            Err(EngineError::TransactionClosed)
        ));
        let read = engine.begin_tx(TxMode::Readonly)?;
        assert_eq!(engine.get(read, "docs", b"key")?, Some(b"durable".to_vec()));
        assert_eq!(engine.get(read, "docs", b"discarded")?, None);
        engine.rollback_tx(read)?;

        let mut reopened = Engine::open(
            "failed-healthy-recovery",
            bundle.crash_recovered_files(),
            OpenConfig::default(),
        )?;
        let read = reopened.begin_tx(TxMode::Readonly)?;
        assert_eq!(
            reopened.get(read, "docs", b"key")?,
            Some(b"durable".to_vec())
        );
        assert_eq!(reopened.get(read, "docs", b"discarded")?, None);
        reopened.rollback_tx(read)?;
    }
    Ok(())
}

#[test]
fn failed_zero_page_checkpoint_recovers_the_durable_commit() -> Result<()> {
    for failure in [
        (File::Main, Operation::Flush),
        (File::Manifest, Operation::Write),
        (File::Wal, Operation::Truncate),
    ] {
        let bundle = MemoryBundle::new();
        let fault = Arc::new(Mutex::new(None));
        let mut engine = Engine::open(
            "failed-empty-checkpoint",
            failing_files(&bundle, &fault),
            OpenConfig {
                checkpoint_wal_bytes: 1,
                ..OpenConfig::default()
            },
        )?;
        let tx = engine.begin_tx(TxMode::Readwrite)?;
        *fault.lock().unwrap() = Some(failure);
        assert!(matches!(engine.commit_tx(tx), Err(EngineError::Storage(_))));
        assert_eq!(*fault.lock().unwrap(), None, "unreached {failure:?}");
        assert!(matches!(
            engine.health(),
            EngineHealth::RecoveryRequired {
                pending_txid: Some(1),
                ..
            }
        ));

        let report = engine.recover()?;
        assert_eq!(report.pending_txid, Some(1));
        assert!(report.pending_committed);
        assert_eq!(report.last_committed_txid, 1);
        assert_eq!(engine.stats()?.wal_len, 0);

        let next = engine.begin_tx(TxMode::Readwrite)?;
        assert_eq!(engine.commit_tx(next)?, 2);
        assert_eq!(engine.stats()?.wal_len, 0);
        let mut reopened = Engine::open(
            "failed-empty-checkpoint",
            bundle.crash_recovered_files(),
            OpenConfig::default(),
        )?;
        assert_eq!(reopened.stats()?.last_committed_txid, 2);
    }
    Ok(())
}
