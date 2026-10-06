#![cfg(not(target_arch = "wasm32"))]

use moyodb_engine::bytes::encode_db_name;
use moyodb_engine::storage::backend::{FileBackend, FileSet};
use moyodb_engine::{
    Engine, EngineError, Failpoint, MemoryBackend, NativeFileBackend, OpenConfig, TxMode,
};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory {
    path: PathBuf,
    parent: PathBuf,
}

struct NodeOwner(Option<Child>);

impl NodeOwner {
    fn child(&mut self) -> &mut Child {
        self.0.as_mut().unwrap()
    }

    fn finish(mut self) -> Output {
        self.0.take().unwrap().wait_with_output().unwrap()
    }
}

impl Drop for NodeOwner {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl TestDirectory {
    fn new() -> Self {
        let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = parent.join(format!(
            "moyodb-native-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::create_dir(path.join("stackdb")).unwrap();
        Self { path, parent }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn db_path(&self, name: &str) -> PathBuf {
        self.path.join("stackdb").join(encode_db_name(name))
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let resolved = fs::canonicalize(&self.path).unwrap();
        assert_eq!(resolved.parent(), Some(self.parent.as_path()));
        assert!(resolved
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("moyodb-native-"));
        fs::remove_dir_all(resolved).unwrap();
    }
}

fn open_engine(directory: &TestDirectory, name: &str, create: bool) -> Engine<NativeFileBackend> {
    let files = NativeFileBackend::open_db(directory.path(), name, create).unwrap();
    Engine::open(
        name,
        files,
        OpenConfig {
            create_if_missing: create,
            ..OpenConfig::default()
        },
    )
    .unwrap()
}

fn commit_row(engine: &mut Engine<NativeFileBackend>, create: bool, key: &[u8], value: &[u8]) {
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    if create {
        engine.create_store(tx, "kv").unwrap();
    }
    engine.put(tx, "kv", key, value).unwrap();
    engine.commit_tx(tx).unwrap();
}

// Frozen output from opfs_shim.js encodeControlSlot for counters 7 and 8.
fn control_slots() -> Vec<u8> {
    let first = [
        66, 68, 66, 82, 79, 79, 84, 49, 1, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 11, 0, 0, 0, 102, 104,
        251, 127, 0, 0, 0, 0, 103, 101, 110, 45, 102, 105, 114, 115, 116, 45, 49,
    ];
    let second = [
        66, 68, 66, 82, 79, 79, 84, 49, 1, 0, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 12, 0, 0, 0, 209, 147,
        12, 250, 0, 0, 0, 0, 103, 101, 110, 45, 115, 101, 99, 111, 110, 100, 45, 50,
    ];
    let mut bytes = vec![0; 8192];
    bytes[..first.len()].copy_from_slice(&first);
    bytes[4096..4096 + second.len()].copy_from_slice(&second);
    bytes
}

fn create_generation(db_path: &Path, name: &str) -> PathBuf {
    let path = db_path.join(name);
    fs::create_dir(&path).unwrap();
    for file in ["manifest.bin", "main.bin", "wal.bin"] {
        fs::write(path.join(file), name.as_bytes()).unwrap();
    }
    path
}

#[test]
fn file_reads_sparse_holes_and_eof_as_zeroes() {
    let directory = TestDirectory::new();
    let path = directory.path().join("data.bin");
    let mut file = NativeFileBackend::open(&path, true).unwrap();
    file.write_at(4, &[1, 2]).unwrap();
    assert_eq!(file.len().unwrap(), 6);
    assert_eq!(file.append_offset().unwrap(), 6);
    assert_eq!(file.read_at(0, 8).unwrap(), [0, 0, 0, 0, 1, 2, 0, 0]);
    assert_eq!(file.read_at(64, 3).unwrap(), [0, 0, 0]);
    file.flush().unwrap();
    file.close().unwrap();

    let file = NativeFileBackend::open(&path, false).unwrap();
    assert_eq!(file.read_at(0, 8).unwrap(), [0, 0, 0, 0, 1, 2, 0, 0]);
    assert_eq!(file.path(), path.as_path());
}

#[test]
fn file_shrink_regrowth_and_empty_sparse_writes_clear_stale_bytes() {
    let directory = TestDirectory::new();
    let path = directory.path().join("data.bin");
    let mut file = NativeFileBackend::open(&path, true).unwrap();
    file.write_at(0, b"abcdef").unwrap();
    file.flush().unwrap();
    file.truncate(2).unwrap();
    assert_eq!(file.append_offset().unwrap(), 2);
    file.write_at(5, b"").unwrap();
    assert_eq!(file.len().unwrap(), 5);
    assert_eq!(file.read_at(0, 6).unwrap(), b"ab\0\0\0\0");
    file.truncate(7).unwrap();
    file.write_at(6, b"x").unwrap();
    file.flush().unwrap();
    file.close().unwrap();
    assert_eq!(fs::read(path).unwrap(), b"ab\0\0\0\0x");
}

#[test]
fn close_is_idempotent_and_rejects_further_operations() {
    let directory = TestDirectory::new();
    let path = directory.path().join("data.bin");
    let mut file = NativeFileBackend::open(&path, true).unwrap();
    file.close().unwrap();
    file.close().unwrap();
    assert!(matches!(file.read_at(0, 0), Err(EngineError::Storage(_))));
    assert!(matches!(
        file.write_at(0, b""),
        Err(EngineError::Storage(_))
    ));
    assert!(matches!(file.flush(), Err(EngineError::Storage(_))));
    assert!(matches!(file.len(), Err(EngineError::Storage(_))));
    assert!(matches!(file.append_offset(), Err(EngineError::Storage(_))));
    assert!(matches!(file.truncate(0), Err(EngineError::Storage(_))));
    NativeFileBackend::open(path, false).unwrap();
}

#[test]
fn exclusive_file_lock_is_released_by_close_and_drop() {
    let directory = TestDirectory::new();
    let path = directory.path().join("data.bin");
    let mut file = NativeFileBackend::open(&path, true).unwrap();
    assert!(matches!(
        NativeFileBackend::open(&path, false),
        Err(EngineError::DatabaseBusy(_))
    ));
    file.close().unwrap();
    let file = NativeFileBackend::open(&path, false).unwrap();
    assert!(matches!(
        NativeFileBackend::open(&path, true),
        Err(EngineError::DatabaseBusy(_))
    ));
    drop(file);
    NativeFileBackend::open(path, false).unwrap();
}

#[test]
fn invalid_ranges_leave_file_open_and_unchanged() {
    let directory = TestDirectory::new();
    let path = directory.path().join("data.bin");
    let mut file = NativeFileBackend::open(path, true).unwrap();
    file.write_at(0, b"stable").unwrap();
    assert!(matches!(
        file.read_at(u64::MAX, 1),
        Err(EngineError::Storage(_))
    ));
    assert!(matches!(
        file.read_at(0, usize::MAX),
        Err(EngineError::Storage(_))
    ));
    assert!(matches!(
        file.write_at(u64::MAX, b"x"),
        Err(EngineError::Storage(_))
    ));
    assert!(matches!(
        file.truncate(u64::MAX),
        Err(EngineError::Storage(_))
    ));
    assert_eq!(file.len().unwrap(), 6);
    assert_eq!(file.read_at(0, 6).unwrap(), b"stable");
    file.flush().unwrap();
}

#[test]
fn open_without_create_does_not_create_files_or_directories() {
    let directory = TestDirectory::new();
    let missing_file = directory.path().join("missing.bin");
    assert!(matches!(
        NativeFileBackend::open(&missing_file, false),
        Err(EngineError::Storage(_))
    ));
    assert!(!missing_file.exists());
    let missing_root = directory.path().join("missing-root");
    assert!(NativeFileBackend::open_db(&missing_root, "missing", false).is_err());
    assert!(!missing_root.exists());
    assert!(NativeFileBackend::open_db(directory.path(), "missing", false).is_err());
    assert!(!directory.db_path("missing").exists());
}

#[test]
fn open_db_encodes_names_and_rejects_invalid_paths() {
    let directory = TestDirectory::new();
    let name = "../native:name/данные";
    let files = NativeFileBackend::open_db(directory.path(), name, true).unwrap();
    assert_eq!(
        files.manifest.path(),
        directory.db_path(name).join("manifest.bin")
    );
    assert_eq!(files.main.path(), directory.db_path(name).join("main.bin"));
    assert_eq!(files.wal.path(), directory.db_path(name).join("wal.bin"));
    drop(files);
    assert!(NativeFileBackend::open_db(directory.path(), "", true).is_err());
    assert!(NativeFileBackend::open_db("", "db", true).is_err());
    let mut invalid = directory.path().as_os_str().to_os_string();
    #[cfg(windows)]
    invalid.push("\\nested\\..\\outside.bin");
    #[cfg(not(windows))]
    invalid.push("/nested/../outside.bin");
    assert!(NativeFileBackend::open(PathBuf::from(invalid), true).is_err());
    assert!(!directory.path().join("nested").exists());
    assert!(NativeFileBackend::open(directory.path(), true).is_err());
    assert!(NativeFileBackend::open_db(directory.path(), &"a".repeat(128), true).is_err());
    assert!(NativeFileBackend::open_db(directory.path(), &"я".repeat(64), true).is_err());
    let longest_name = "a".repeat(127);
    NativeFileBackend::open_db(directory.path(), &longest_name, true).unwrap();
}

#[test]
fn partial_bundle_open_releases_acquired_file_locks() {
    let directory = TestDirectory::new();
    let db_path = directory.db_path("partial");
    fs::create_dir(&db_path).unwrap();
    fs::create_dir(db_path.join("main.bin")).unwrap();
    assert!(NativeFileBackend::open_db(directory.path(), "partial", true).is_err());
    let mut manifest = NativeFileBackend::open(db_path.join("manifest.bin"), false).unwrap();
    manifest.write_at(0, b"lock released").unwrap();
}

#[test]
fn second_database_session_is_busy_until_first_session_closes() {
    let directory = TestDirectory::new();
    let mut engine = open_engine(&directory, "exclusive", true);
    assert!(matches!(
        NativeFileBackend::open_db(directory.path(), "exclusive", false),
        Err(EngineError::DatabaseBusy(_))
    ));
    engine.close().unwrap();
    open_engine(&directory, "exclusive", false).close().unwrap();
}

#[test]
fn committed_wal_survives_drop_and_reopen_without_checkpoint() {
    let directory = TestDirectory::new();
    let mut engine = open_engine(&directory, "wal-reopen", true);
    commit_row(&mut engine, true, b"key", b"value");
    assert!(engine.stats().unwrap().wal_len > 0);
    drop(engine);
    assert!(
        fs::metadata(directory.db_path("wal-reopen").join("wal.bin"))
            .unwrap()
            .len()
            > 0
    );

    let mut reopened = open_engine(&directory, "wal-reopen", false);
    let tx = reopened.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(
        reopened.get(tx, "kv", b"key").unwrap(),
        Some(b"value".to_vec())
    );
    reopened.rollback_tx(tx).unwrap();
    assert_eq!(reopened.stats().unwrap().wal_len, 0);
    reopened.close().unwrap();
}

#[test]
fn recovery_replays_commit_after_wal_flush_failure() {
    let directory = TestDirectory::new();
    let mut engine = open_engine(&directory, "wal-failure", true);
    commit_row(&mut engine, true, b"base", b"stable");
    engine.set_failpoint(Some(Failpoint::AfterWalFlush));
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.put(tx, "kv", b"pending", b"durable").unwrap();
    assert!(matches!(
        engine.commit_tx(tx),
        Err(EngineError::InjectedFailure(_))
    ));
    engine.close().unwrap();

    let mut reopened = open_engine(&directory, "wal-failure", false);
    let tx = reopened.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(
        reopened.get(tx, "kv", b"pending").unwrap(),
        Some(b"durable".to_vec())
    );
    assert_eq!(
        reopened.get(tx, "kv", b"base").unwrap(),
        Some(b"stable".to_vec())
    );
    reopened.rollback_tx(tx).unwrap();
    reopened.close().unwrap();
}

#[test]
fn checkpoint_files_use_existing_memory_backend_format() {
    let directory = TestDirectory::new();
    let mut engine = open_engine(&directory, "format", true);
    commit_row(&mut engine, true, b"key", b"value");
    engine.close().unwrap();
    let db_path = directory.db_path("format");
    assert_eq!(fs::metadata(db_path.join("wal.bin")).unwrap().len(), 0);
    let files = FileSet::new(
        MemoryBackend::from_durable(fs::read(db_path.join("manifest.bin")).unwrap()),
        MemoryBackend::from_durable(fs::read(db_path.join("main.bin")).unwrap()),
        MemoryBackend::from_durable(fs::read(db_path.join("wal.bin")).unwrap()),
    );
    let mut memory = Engine::open(
        "format",
        files,
        OpenConfig {
            create_if_missing: false,
            ..OpenConfig::default()
        },
    )
    .unwrap();
    let tx = memory.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(
        memory.get(tx, "kv", b"key").unwrap(),
        Some(b"value".to_vec())
    );
    memory.rollback_tx(tx).unwrap();
}

#[test]
fn existing_database_is_not_truncated_by_create_open() {
    let directory = TestDirectory::new();
    let mut engine = open_engine(&directory, "existing", true);
    commit_row(&mut engine, true, b"key", b"value");
    engine.close().unwrap();
    let mut reopened = open_engine(&directory, "existing", true);
    let tx = reopened.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(
        reopened.get(tx, "kv", b"key").unwrap(),
        Some(b"value".to_vec())
    );
    reopened.rollback_tx(tx).unwrap();
    reopened.close().unwrap();
}

#[test]
fn opfs_control_slots_select_the_latest_native_generation() {
    let directory = TestDirectory::new();
    let db_path = directory.db_path("generations");
    fs::create_dir(&db_path).unwrap();
    create_generation(&db_path, "gen-first-1");
    let selected = create_generation(&db_path, "gen-second-2");
    fs::write(db_path.join("manifest.bin"), b"stale").unwrap();
    fs::write(db_path.join("main.bin"), b"stale").unwrap();
    fs::write(db_path.join("wal.bin"), b"stale").unwrap();
    fs::write(db_path.join("root-manifest.bin"), control_slots()).unwrap();
    let mut files = NativeFileBackend::open_db(directory.path(), "generations", false).unwrap();
    assert_eq!(files.main.path(), selected.join("main.bin"));
    assert_eq!(files.main.read_at(0, 12).unwrap(), b"gen-second-2");
    assert!(!db_path.join("manifest.bin").exists());
    assert!(!db_path.join("main.bin").exists());
    assert!(!db_path.join("wal.bin").exists());
    files.manifest.close().unwrap();
    files.main.close().unwrap();
    assert!(matches!(
        NativeFileBackend::open(db_path.join("root-manifest.bin"), false),
        Err(EngineError::DatabaseBusy(_))
    ));
    files.wal.close().unwrap();
    NativeFileBackend::open(db_path.join("root-manifest.bin"), false).unwrap();
}

#[test]
fn torn_control_slot_keeps_the_previous_generation_openable() {
    let directory = TestDirectory::new();
    let db_path = directory.db_path("generations");
    fs::create_dir(&db_path).unwrap();
    let selected = create_generation(&db_path, "gen-first-1");
    let mut control = control_slots();
    control[4096 + 32] ^= 1;
    fs::write(db_path.join("root-manifest.bin"), control).unwrap();
    let files = NativeFileBackend::open_db(directory.path(), "generations", false).unwrap();
    assert_eq!(files.main.path(), selected.join("main.bin"));
}

#[test]
fn missing_active_generation_is_rejected_without_creating_an_empty_database() {
    let directory = TestDirectory::new();
    let db_path = directory.db_path("generations");
    fs::create_dir(&db_path).unwrap();
    fs::write(db_path.join("root-manifest.bin"), control_slots()).unwrap();
    assert!(matches!(
        NativeFileBackend::open_db(directory.path(), "generations", true),
        Err(EngineError::Corruption(_))
    ));
    assert!(!db_path.join("gen-second-2").exists());
    assert!(!db_path.join("manifest.bin").exists());
    NativeFileBackend::open(db_path.join("root-manifest.bin"), false).unwrap();
}

#[test]
fn corrupt_control_without_legacy_data_is_rejected() {
    let directory = TestDirectory::new();
    let db_path = directory.db_path("generations");
    fs::create_dir(&db_path).unwrap();
    fs::write(db_path.join("root-manifest.bin"), b"broken").unwrap();
    assert!(matches!(
        NativeFileBackend::open_db(directory.path(), "generations", true),
        Err(EngineError::Corruption(_))
    ));
    assert!(!db_path.join("manifest.bin").exists());
}

#[test]
fn checksummed_unknown_control_version_is_rejected_even_with_a_valid_other_slot() {
    let directory = TestDirectory::new();
    let db_path = directory.db_path("generations");
    fs::create_dir(&db_path).unwrap();
    create_generation(&db_path, "gen-second-2");
    let mut control = control_slots();
    control[8] = 2;
    control[24..28].copy_from_slice(&[169, 25, 232, 67]);
    fs::write(db_path.join("root-manifest.bin"), control).unwrap();
    assert!(matches!(
        NativeFileBackend::open_db(directory.path(), "generations", false),
        Err(EngineError::Corruption(_))
    ));
    assert!(!db_path.join(".moyodb.lock").exists());
}

#[test]
fn corrupt_first_generation_publication_keeps_legacy_data_openable() {
    let directory = TestDirectory::new();
    let db_path = directory.db_path("generations");
    let files = NativeFileBackend::open_db(directory.path(), "generations", true).unwrap();
    drop(files);
    fs::write(db_path.join("root-manifest.bin"), b"broken").unwrap();
    let files = NativeFileBackend::open_db(directory.path(), "generations", false).unwrap();
    assert_eq!(files.main.path(), db_path.join("main.bin"));
}

#[test]
fn malformed_and_live_shared_leases_are_not_removed() {
    let directory = TestDirectory::new();
    let db_path = directory.db_path("lease");
    fs::create_dir(&db_path).unwrap();
    let lock = db_path.join(".moyodb.lock");
    fs::create_dir(&lock).unwrap();
    let token = "11111111-2222-3333-4444-555555555555";
    let record = lock.join(format!("{token}.json"));
    fs::write(&record, b"malformed").unwrap();
    assert!(matches!(
        NativeFileBackend::open_db(directory.path(), "lease", true),
        Err(EngineError::DatabaseBusy(_))
    ));
    assert_eq!(fs::read(&record).unwrap(), b"malformed");
    fs::write(
        &record,
        serde_json::to_vec(&serde_json::json!({"pid": std::process::id(), "token": token}))
            .unwrap(),
    )
    .unwrap();
    assert!(matches!(
        NativeFileBackend::open_db(directory.path(), "lease", true),
        Err(EngineError::DatabaseBusy(_))
    ));
    assert!(record.exists());
    assert!(!db_path.join("manifest.bin").exists());
    assert_eq!(fs::read_dir(&db_path).unwrap().count(), 1);
}

#[cfg(windows)]
#[test]
fn shared_lease_recovers_an_exited_process_while_its_handle_is_open() {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    struct StopChild(Option<Child>);
    impl Drop for StopChild {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    let directory = TestDirectory::new();
    let db_path = directory.db_path("lease");
    fs::create_dir(&db_path).unwrap();
    let mut child = StopChild(Some(
        Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    ));
    let pid = child.0.as_ref().unwrap().id();
    // Keep a process object after exit. OpenProcess still succeeds until this
    // handle is closed, which is not the same as the process still running.
    // SAFETY: The access mask and pid are values. The handle is closed below.
    let held = unsafe { OpenProcess(0x1000, 0, pid) };
    assert!(
        !held.is_null(),
        "probe handle: {}",
        std::io::Error::last_os_error()
    );
    let lock = db_path.join(".moyodb.lock");
    fs::create_dir(&lock).unwrap();
    let token = "11111111-2222-3333-4444-555555555555";
    let record = lock.join(format!("{token}.json"));
    fs::write(
        &record,
        serde_json::to_vec(&serde_json::json!({"pid": pid, "token": token})).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        NativeFileBackend::open_db(directory.path(), "lease", true),
        Err(EngineError::DatabaseBusy(_))
    ));
    assert!(record.exists());
    let mut exited = child.0.take().unwrap();
    exited.kill().unwrap();
    exited.wait().unwrap();
    let files = NativeFileBackend::open_db(directory.path(), "lease", true).unwrap();
    assert!(!record.exists());
    drop(files);
    // SAFETY: `held` came from OpenProcess and is closed exactly once.
    assert_ne!(unsafe { CloseHandle(held) }, 0);
}

#[test]
fn shared_lease_recovers_a_proven_dead_owner_and_an_interrupted_empty_release() {
    let directory = TestDirectory::new();
    let db_path = directory.db_path("lease");
    fs::create_dir(&db_path).unwrap();
    let lock = db_path.join(".moyodb.lock");
    fs::create_dir(&lock).unwrap();
    let token = "11111111-2222-3333-4444-555555555555";
    let record = lock.join(format!("{token}.json"));
    fs::write(
        &record,
        serde_json::to_vec(&serde_json::json!({"pid": i32::MAX, "token": token})).unwrap(),
    )
    .unwrap();
    let files = NativeFileBackend::open_db(directory.path(), "lease", true).unwrap();
    assert!(!record.exists());
    assert_eq!(fs::read_dir(&lock).unwrap().count(), 1);
    drop(files);
    assert!(!lock.exists());
    fs::create_dir(&lock).unwrap();
    let files = NativeFileBackend::open_db(directory.path(), "lease", false).unwrap();
    drop(files);
    assert!(!lock.exists());
}

#[test]
fn lease_remains_held_until_every_data_file_closes() {
    let directory = TestDirectory::new();
    let db_path = directory.db_path("lease");
    let mut files = NativeFileBackend::open_db(directory.path(), "lease", true).unwrap();
    files.manifest.close().unwrap();
    files.main.close().unwrap();
    assert!(db_path.join(".moyodb.lock").exists());
    assert!(matches!(
        NativeFileBackend::open_db(directory.path(), "lease", false),
        Err(EngineError::DatabaseBusy(_))
    ));
    files.wal.close().unwrap();
    assert!(!db_path.join(".moyodb.lock").exists());
}

fn node_storage_script() -> Option<PathBuf> {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/sdk/scripts/test-node-storage.mjs");
    if !script.is_file() || Command::new("node").arg("--version").output().is_err() {
        eprintln!(
            "Node interoperability check skipped: Node or its storage test script is unavailable"
        );
        return None;
    }
    Some(script)
}

#[test]
fn node_adapter_refuses_a_directory_owned_by_native_backend() {
    let Some(script) = node_storage_script() else {
        return;
    };
    let directory = TestDirectory::new();
    let mut engine = open_engine(&directory, "node-lease", true);
    let db_path = directory.db_path("node-lease");
    let blocked = Command::new("node")
        .arg(&script)
        .arg("--interop-probe")
        .arg(&db_path)
        .output()
        .unwrap();
    assert_eq!(
        blocked.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&blocked.stderr)
    );
    engine.close().unwrap();
    let released = Command::new("node")
        .arg(script)
        .arg("--interop-probe")
        .arg(db_path)
        .output()
        .unwrap();
    assert!(
        released.status.success(),
        "{}",
        String::from_utf8_lossy(&released.stderr)
    );
}

#[test]
fn native_backend_refuses_a_directory_owned_by_node_adapter() {
    let Some(script) = node_storage_script() else {
        return;
    };
    let directory = TestDirectory::new();
    open_engine(&directory, "node-lease", true).close().unwrap();
    let db_path = directory.db_path("node-lease");
    let mut owner = NodeOwner(Some(
        Command::new("node")
            .arg(script)
            .arg("--interop-owner")
            .arg(&db_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    let stdout = owner.child().stdout.take().unwrap();
    let (ready, received) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line);
        let _ = ready.send((result, line));
    });
    let message = received.recv_timeout(Duration::from_secs(15));
    if message.is_err() {
        owner.child().kill().unwrap();
        let output = owner.finish();
        reader.join().unwrap();
        panic!(
            "Node storage owner did not become ready: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let (result, line) = message.unwrap();
    result.unwrap();
    if line.trim() != "READY" {
        owner.child().kill().unwrap();
        let output = owner.finish();
        reader.join().unwrap();
        panic!(
            "Node storage owner failed: {line} {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(matches!(
        NativeFileBackend::open_db(directory.path(), "node-lease", false),
        Err(EngineError::DatabaseBusy(_))
    ));
    assert!(matches!(
        NativeFileBackend::open(db_path.join("main.bin"), false),
        Err(EngineError::DatabaseBusy(_))
    ));
    drop(owner.child().stdin.take());
    let output = owner.finish();
    reader.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    open_engine(&directory, "node-lease", false)
        .close()
        .unwrap();
}

#[test]
#[ignore]
fn native_interop_helper() {
    let root = std::env::var_os("MOYODB_INTEROP_ROOT").expect("MOYODB_INTEROP_ROOT");
    let name = std::env::var("MOYODB_INTEROP_DB").expect("MOYODB_INTEROP_DB");
    let mode = std::env::var("MOYODB_INTEROP_MODE").expect("MOYODB_INTEROP_MODE");
    let create = mode == "create";
    let files = NativeFileBackend::open_db(PathBuf::from(root), &name, create).unwrap();
    let mut engine = Engine::open(
        &name,
        files,
        OpenConfig {
            create_if_missing: create,
            ..OpenConfig::default()
        },
    )
    .unwrap();
    match mode.as_str() {
        "create" => commit_row(&mut engine, true, b"interop", b"shared-file-format"),
        "read" | "append" => {
            let tx = engine.begin_tx(TxMode::Readonly).unwrap();
            assert_eq!(
                engine.get(tx, "kv", b"interop").unwrap(),
                Some(b"shared-file-format".to_vec())
            );
            engine.rollback_tx(tx).unwrap();
            if mode == "append" {
                commit_row(&mut engine, false, b"native-append", b"shared-file-format");
            }
        }
        _ => panic!("invalid MOYODB_INTEROP_MODE"),
    }
    engine.close().unwrap();
}

#[test]
#[ignore = "requires the built Node SDK and WASM package"]
fn node_native_file_format_roundtrip_through_active_generation() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/sdk/scripts/test-node-runtime.mjs");
    let directory = TestDirectory::new();
    let run = |mode: &str, name: &str| {
        let output = Command::new("node")
            .arg(&script)
            .arg(mode)
            .arg(directory.path())
            .arg(name)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    let name = "node-native-generation";
    run("interop-create-compacted", name);
    assert!(directory.db_path(name).join("root-manifest.bin").is_file());
    let mut native = open_engine(&directory, name, false);
    let tx = native.begin_tx(TxMode::Readonly).unwrap();
    assert_eq!(
        native.get(tx, "kv", b"interop").unwrap(),
        Some(b"shared-file-format".to_vec())
    );
    native.rollback_tx(tx).unwrap();
    commit_row(&mut native, false, b"native-append", b"shared-file-format");
    assert!(native.stats().unwrap().wal_len > 0);
    native.close().unwrap();
    run("interop-read-native-append", name);

    let name = "native-node-created";
    let mut native = open_engine(&directory, name, true);
    commit_row(&mut native, true, b"interop", b"shared-file-format");
    native.close().unwrap();
    run("interop-read", name);
}

#[cfg(unix)]
#[test]
fn symlink_files_and_directories_are_rejected() {
    use std::os::unix::fs::symlink;

    let directory = TestDirectory::new();
    let target = directory.path().join("target.bin");
    fs::write(&target, b"unchanged").unwrap();
    let linked_file = directory.path().join("linked.bin");
    symlink(&target, &linked_file).unwrap();
    assert!(NativeFileBackend::open(linked_file, true).is_err());
    let linked_directory = directory.path().join("linked-directory");
    symlink(directory.path(), &linked_directory).unwrap();
    assert!(NativeFileBackend::open_db(linked_directory, "db", true).is_err());
    assert_eq!(fs::read(target).unwrap(), b"unchanged");
}
