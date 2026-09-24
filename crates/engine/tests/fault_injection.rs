//! Storage fault injection across manifest, main file, and WAL.
//!
//! Every write, flush, and truncate issued by a commit, a checkpoint, a close,
//! or crash recovery is failed in turn. After each fault the process either
//! "crashes" (the database is reopened from the bytes that were durable at that
//! moment) or keeps running and recovers in place once storage works again.
//!
//! Faults are not atomic: a short write stores half its bytes, and a torn flush
//! persists only a prefix or only a suffix of the writes pending on that file.

use moyodb_engine::engine::{Engine, OpenConfig, ScanRange, TxMode};
use moyodb_engine::error::{EngineError, Result};
use moyodb_engine::storage::backend::{FileBackend, FileSet};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

const DB: &str = "fault-injection";
const STORE: &str = "kv";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    /// The Nth write, flush, or truncate fails without effect.
    FailOp,
    /// The Nth write stores the first half of its bytes, then fails.
    ShortWrite,
    /// The Nth flush persists the first half of the pending writes, then fails.
    TornFlushPrefix,
    /// The Nth flush persists only the second half of the pending writes, then fails.
    TornFlushSuffix,
}

const FAULTS: [Fault; 4] = [
    Fault::FailOp,
    Fault::ShortWrite,
    Fault::TornFlushPrefix,
    Fault::TornFlushSuffix,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpKind {
    Write,
    Flush,
    Truncate,
}

impl Fault {
    fn counts(self, op: OpKind) -> bool {
        match self {
            Fault::FailOp => true,
            Fault::ShortWrite => op == OpKind::Write,
            Fault::TornFlushPrefix | Fault::TornFlushSuffix => op == OpKind::Flush,
        }
    }
}

#[derive(Clone, Copy)]
enum Decision {
    Proceed,
    Inject(Fault),
    /// Storage stays broken after the injected fault until it is disarmed.
    Refuse,
}

#[derive(Default)]
struct Controller {
    armed: Option<(Fault, u64)>,
    seen: u64,
    tripped: Option<String>,
}

impl Controller {
    fn decide(&mut self, file: &str, op: OpKind) -> Decision {
        if self.tripped.is_some() {
            return Decision::Refuse;
        }
        let Some((fault, target)) = self.armed else {
            return Decision::Proceed;
        };
        if !fault.counts(op) {
            return Decision::Proceed;
        }
        self.seen += 1;
        if self.seen != target {
            return Decision::Proceed;
        }
        self.tripped = Some(format!("{fault:?} on {file} {op:?}"));
        Decision::Inject(fault)
    }
}

#[derive(Clone)]
enum Pending {
    Write { offset: usize, bytes: Vec<u8> },
    Truncate(usize),
}

impl Pending {
    fn apply(&self, target: &mut Vec<u8>) {
        match self {
            Pending::Write { offset, bytes } => {
                let end = offset + bytes.len();
                if target.len() < end {
                    target.resize(end, 0);
                }
                target[*offset..end].copy_from_slice(bytes);
            }
            Pending::Truncate(size) => target.resize(*size, 0),
        }
    }
}

/// `working` is what reads see; `durable` is what survives a crash. A
/// successful flush makes them equal; a torn flush applies part of `pending`.
#[derive(Default)]
struct FileState {
    working: Vec<u8>,
    durable: Vec<u8>,
    pending: Vec<Pending>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn injected(what: &str) -> EngineError {
    EngineError::Storage(format!("injected storage fault: {what}"))
}

#[derive(Clone)]
struct FaultyFile {
    name: &'static str,
    state: Arc<Mutex<FileState>>,
    controller: Arc<Mutex<Controller>>,
}

impl FaultyFile {
    fn decide(&self, op: OpKind) -> Decision {
        lock(&self.controller).decide(self.name, op)
    }

    fn record(&self, op: Pending) {
        let mut state = lock(&self.state);
        op.apply(&mut state.working);
        state.pending.push(op);
    }
}

impl FileBackend for FaultyFile {
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let state = lock(&self.state);
        let start = offset as usize;
        let mut out = vec![0u8; len];
        if start < state.working.len() {
            let end = (start + len).min(state.working.len());
            out[..end - start].copy_from_slice(&state.working[start..end]);
        }
        Ok(out)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        let stored = match self.decide(OpKind::Write) {
            Decision::Proceed => bytes,
            Decision::Inject(Fault::ShortWrite) => &bytes[..bytes.len() / 2],
            Decision::Inject(_) | Decision::Refuse => return Err(injected("write")),
        };
        let complete = stored.len() == bytes.len();
        self.record(Pending::Write {
            offset: offset as usize,
            bytes: stored.to_vec(),
        });
        if complete {
            Ok(())
        } else {
            Err(injected("short write"))
        }
    }

    fn flush(&mut self) -> Result<()> {
        let decision = self.decide(OpKind::Flush);
        let mut state = lock(&self.state);
        let persisted = match decision {
            Decision::Proceed => {
                state.durable = state.working.clone();
                state.pending.clear();
                return Ok(());
            }
            Decision::Inject(Fault::TornFlushPrefix) => {
                let half = state.pending.len() / 2;
                state.pending[..half].to_vec()
            }
            Decision::Inject(Fault::TornFlushSuffix) => {
                let half = state.pending.len() / 2;
                state.pending[half..].to_vec()
            }
            Decision::Inject(_) | Decision::Refuse => return Err(injected("flush")),
        };
        for op in &persisted {
            op.apply(&mut state.durable);
        }
        Err(injected("torn flush"))
    }

    fn len(&self) -> Result<u64> {
        Ok(lock(&self.state).working.len() as u64)
    }

    fn truncate(&mut self, size: u64) -> Result<()> {
        match self.decide(OpKind::Truncate) {
            Decision::Proceed => {
                self.record(Pending::Truncate(size as usize));
                Ok(())
            }
            Decision::Inject(_) | Decision::Refuse => Err(injected("truncate")),
        }
    }

    fn close(&mut self) -> Result<()> {
        Ok(())
    }

    fn durable_snapshot(&self) -> Option<Vec<u8>> {
        Some(lock(&self.state).durable.clone())
    }
}

struct FaultBundle {
    controller: Arc<Mutex<Controller>>,
    manifest: FaultyFile,
    main: FaultyFile,
    wal: FaultyFile,
}

impl FaultBundle {
    fn new() -> Self {
        Self::with_contents([Vec::new(), Vec::new(), Vec::new()])
    }

    fn with_contents([manifest, main, wal]: [Vec<u8>; 3]) -> Self {
        let controller = Arc::new(Mutex::new(Controller::default()));
        let file = |name: &'static str, bytes: Vec<u8>| FaultyFile {
            name,
            state: Arc::new(Mutex::new(FileState {
                working: bytes.clone(),
                durable: bytes,
                pending: Vec::new(),
            })),
            controller: Arc::clone(&controller),
        };
        Self {
            manifest: file("manifest", manifest),
            main: file("main", main),
            wal: file("wal", wal),
            controller,
        }
    }

    /// The database as a crash at this instant would leave it.
    fn crash_copy(&self) -> Self {
        Self::with_contents([
            lock(&self.manifest.state).durable.clone(),
            lock(&self.main.state).durable.clone(),
            lock(&self.wal.state).durable.clone(),
        ])
    }

    fn files(&self) -> FileSet<FaultyFile> {
        FileSet::new(self.manifest.clone(), self.main.clone(), self.wal.clone())
    }

    fn arm(&self, fault: Fault, target: u64) {
        *lock(&self.controller) = Controller {
            armed: Some((fault, target)),
            ..Controller::default()
        };
    }

    fn disarm(&self) {
        *lock(&self.controller) = Controller::default();
    }

    fn tripped(&self) -> Option<String> {
        lock(&self.controller).tripped.clone()
    }
}

type Rows = BTreeMap<Vec<u8>, Vec<u8>>;

fn key(index: usize) -> Vec<u8> {
    format!("k{index:03}").into_bytes()
}

fn value(index: usize, round: u8) -> Vec<u8> {
    (0..200)
        .map(|offset| (index + offset) as u8 ^ round)
        .collect()
}

fn big_value() -> Vec<u8> {
    (0..40 * 1024).map(|offset| (offset % 251) as u8).collect()
}

fn seed_rows() -> Rows {
    (0..40).map(|index| (key(index), value(index, 1))).collect()
}

/// Inserts, overwrites, deletes, and writes an overflow value in one commit.
fn mutated_rows() -> Rows {
    let mut rows = seed_rows();
    for index in 40..80 {
        rows.insert(key(index), value(index, 2));
    }
    for index in 0..10 {
        rows.insert(key(index), value(index, 3));
    }
    for index in 10..15 {
        rows.remove(&key(index));
    }
    rows.insert(b"big".to_vec(), big_value());
    rows
}

fn seed(engine: &mut Engine<FaultyFile>) {
    let tx = engine.begin_tx(TxMode::Readwrite).expect("begin seed");
    engine.create_store(tx, STORE).expect("create store");
    for (k, v) in seed_rows() {
        engine.put(tx, STORE, &k, &v).expect("seed put");
    }
    engine.commit_tx(tx).expect("commit seed");
}

fn mutate(engine: &mut Engine<FaultyFile>) -> Result<u64> {
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    let staged = (|| {
        for index in 40..80 {
            engine.put(tx, STORE, &key(index), &value(index, 2))?;
        }
        for index in 0..10 {
            engine.put(tx, STORE, &key(index), &value(index, 3))?;
        }
        for index in 10..15 {
            engine.delete(tx, STORE, &key(index))?;
        }
        engine.put(tx, STORE, b"big", &big_value())
    })();
    if let Err(err) = staged {
        let _ = engine.rollback_tx(tx);
        return Err(err);
    }
    engine.commit_tx(tx)
}

fn dump(engine: &mut Engine<FaultyFile>, ctx: &str) -> Rows {
    let ro = engine
        .begin_tx(TxMode::Readonly)
        .unwrap_or_else(|err| panic!("{ctx}: begin readonly: {err}"));
    let rows = engine
        .scan(ro, STORE, &ScanRange::default())
        .unwrap_or_else(|err| panic!("{ctx}: scan: {err}"))
        .into_iter()
        .map(|pair| (pair.key, pair.value))
        .collect();
    engine
        .rollback_tx(ro)
        .unwrap_or_else(|err| panic!("{ctx}: rollback readonly: {err}"));
    rows
}

fn reopen(bundle: &FaultBundle, config: &OpenConfig, ctx: &str) -> Engine<FaultyFile> {
    Engine::open(DB, bundle.files(), config.clone())
        .unwrap_or_else(|err| panic!("{ctx}: reopen after crash failed: {err}"))
}

fn assert_before_or_after(rows: &Rows, ctx: &str) {
    assert!(
        *rows == seed_rows() || *rows == mutated_rows(),
        "{ctx}: recovered {} rows that match neither the state before nor after the commit",
        rows.len()
    );
}

/// The recovered database accepts a new commit, and that commit survives the next crash.
fn assert_writable(bundle: &FaultBundle, config: &OpenConfig, expected: &Rows, ctx: &str) {
    let mut engine = reopen(bundle, config, ctx);
    let tx = engine
        .begin_tx(TxMode::Readwrite)
        .unwrap_or_else(|err| panic!("{ctx}: begin after recovery: {err}"));
    engine
        .put(tx, STORE, b"later", b"write")
        .unwrap_or_else(|err| panic!("{ctx}: put after recovery: {err}"));
    engine
        .commit_tx(tx)
        .unwrap_or_else(|err| panic!("{ctx}: commit after recovery: {err}"));
    drop(engine);

    let mut expected = expected.clone();
    expected.insert(b"later".to_vec(), b"write".to_vec());
    let mut reopened = reopen(&bundle.crash_copy(), config, ctx);
    assert_eq!(
        dump(&mut reopened, ctx),
        expected,
        "{ctx}: commit after recovery lost"
    );
}

fn deferred_checkpoint() -> OpenConfig {
    OpenConfig::default()
}

/// Every commit checkpoints, so one commit touches WAL, main file, and manifest.
fn eager_checkpoint() -> OpenConfig {
    OpenConfig {
        checkpoint_wal_bytes: 1,
        checkpoint_dirty_pages: 1,
        ..OpenConfig::default()
    }
}

/// Fails the `target`th counted operation of one commit plus close. Returns
/// false once `target` is past the last operation, which ends the sweep.
fn commit_case(config: &OpenConfig, fault: Fault, target: u64) -> bool {
    let bundle = FaultBundle::new();
    let mut engine = Engine::open(DB, bundle.files(), config.clone()).expect("open fresh database");
    seed(&mut engine);

    bundle.arm(fault, target);
    let committed = mutate(&mut engine);
    let closed = (committed.is_ok() && bundle.tripped().is_none()).then(|| engine.close());
    let Some(trip) = bundle.tripped() else {
        committed.expect("commit without an injected fault");
        if let Some(closed) = closed {
            closed.expect("close without an injected fault");
        }
        return false;
    };
    let ctx = format!("{fault:?} #{target} ({trip})");

    let crashed = bundle.crash_copy();
    let recovered = dump(&mut reopen(&crashed, config, &ctx), &ctx);
    if committed.is_ok() {
        assert_eq!(recovered, mutated_rows(), "{ctx}: acknowledged commit lost");
    } else {
        assert_before_or_after(&recovered, &ctx);
    }
    assert_writable(&crashed, config, &recovered, &ctx);

    if closed.is_none() {
        recover_in_place(&bundle, &mut engine, config, &ctx);
    }
    true
}

/// Storage comes back and the poisoned engine recovers without a restart.
fn recover_in_place(
    bundle: &FaultBundle,
    engine: &mut Engine<FaultyFile>,
    config: &OpenConfig,
    ctx: &str,
) {
    bundle.disarm();
    if !engine.needs_recovery() {
        assert_eq!(
            dump(engine, ctx),
            seed_rows(),
            "{ctx}: engine stayed healthy after a failed commit but shows its effects"
        );
        return;
    }
    let err = engine
        .begin_tx(TxMode::Readwrite)
        .expect_err("writes must be refused until recovery");
    assert_eq!(err.code(), "RecoveryRequiredError", "{ctx}");

    let report = engine
        .recover()
        .unwrap_or_else(|err| panic!("{ctx}: in-place recovery failed: {err}"));
    let rows = dump(engine, ctx);
    assert_before_or_after(&rows, ctx);
    assert_eq!(
        rows == mutated_rows(),
        report.pending_committed,
        "{ctx}: recovery report disagrees with the recovered rows"
    );

    let mut after_crash = reopen(&bundle.crash_copy(), config, ctx);
    assert_eq!(
        dump(&mut after_crash, ctx),
        rows,
        "{ctx}: in-place recovery reported rows that a crash loses"
    );
}

fn sweep(config: OpenConfig, case: fn(&OpenConfig, Fault, u64) -> bool) {
    for fault in FAULTS {
        let mut target = 1;
        while case(&config, fault, target) {
            target += 1;
            assert!(target < 10_000, "{fault:?}: fault sweep did not terminate");
        }
        assert!(
            target > 1,
            "{fault:?}: scenario issued no storage operation"
        );
    }
}

#[test]
fn every_fault_during_commit_with_deferred_checkpoint_is_atomic() {
    sweep(deferred_checkpoint(), commit_case);
}

#[test]
fn every_fault_during_commit_with_eager_checkpoint_is_atomic() {
    sweep(eager_checkpoint(), commit_case);
}

/// A crash leaves a durable but uninstalled commit in the WAL; every storage
/// operation of the recovery that replays it is then failed in turn. Recovery
/// must stay idempotent: the next clean open still finds the commit.
fn recovery_case(config: &OpenConfig, fault: Fault, target: u64) -> bool {
    let bundle = FaultBundle::new();
    let mut engine = Engine::open(DB, bundle.files(), config.clone()).expect("open fresh database");
    seed(&mut engine);
    mutate(&mut engine).expect("commit before crash");
    drop(engine);

    let crashed = bundle.crash_copy();
    crashed.arm(fault, target);
    let opened = Engine::open(DB, crashed.files(), config.clone());
    let Some(trip) = crashed.tripped() else {
        let mut engine = opened.expect("recovery without an injected fault");
        assert_eq!(dump(&mut engine, "clean recovery"), mutated_rows());
        return false;
    };
    let ctx = format!("recovery {fault:?} #{target} ({trip})");
    drop(opened);

    let recrashed = crashed.crash_copy();
    let rows = dump(&mut reopen(&recrashed, config, &ctx), &ctx);
    assert_eq!(
        rows,
        mutated_rows(),
        "{ctx}: durable commit lost by interrupted recovery"
    );
    assert_writable(&recrashed, config, &rows, &ctx);
    true
}

#[test]
fn every_fault_during_crash_recovery_is_idempotent() {
    sweep(deferred_checkpoint(), recovery_case);
}
