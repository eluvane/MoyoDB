#![cfg(not(target_arch = "wasm32"))]

use moyodb_engine::bytes::encode_db_name;
use moyodb_engine::{Engine, EngineError, NativeFileBackend, OpenConfig};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory {
    path: PathBuf,
    parent: PathBuf,
}

impl TestDirectory {
    fn new() -> Self {
        let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = parent.join(format!(
            "moyodb-native-security-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
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
            .starts_with("moyodb-native-security-"));
        fs::remove_dir_all(resolved).unwrap();
    }
}

#[test]
fn standalone_file_rejects_hard_link_without_changing_target() {
    let directory = TestDirectory::new();
    let target = directory.path().join("target.bin");
    let linked = directory.path().join("linked.bin");
    fs::write(&target, b"external contents").unwrap();
    fs::hard_link(&target, &linked).unwrap();
    assert!(matches!(
        NativeFileBackend::open(&linked, false),
        Err(EngineError::Storage(_))
    ));
    assert_eq!(fs::read(target).unwrap(), b"external contents");
}

#[test]
fn database_files_reject_hard_links_without_changing_target() {
    for name in ["manifest.bin", "main.bin", "wal.bin", "root-manifest.bin"] {
        let directory = TestDirectory::new();
        let target = directory.path().join("target.bin");
        let database = directory.db_path("linked");
        fs::create_dir_all(&database).unwrap();
        fs::write(&target, b"external contents").unwrap();
        fs::hard_link(&target, database.join(name)).unwrap();
        assert!(
            matches!(
                NativeFileBackend::open_db(directory.path(), "linked", true),
                Err(EngineError::Storage(_))
            ),
            "accepted hard-linked {name}"
        );
        assert_eq!(fs::read(target).unwrap(), b"external contents");
    }
}

#[test]
fn wal_hard_link_cannot_truncate_external_file_during_recovery() {
    let directory = TestDirectory::new();
    let target = directory.path().join("target.bin");
    let database = directory.db_path("linked-wal");
    let files = NativeFileBackend::open_db(directory.path(), "linked-wal", true).unwrap();
    let mut engine = Engine::open("linked-wal", files, OpenConfig::default()).unwrap();
    engine.close().unwrap();
    fs::remove_file(database.join("wal.bin")).unwrap();
    fs::write(&target, b"external contents").unwrap();
    fs::hard_link(&target, database.join("wal.bin")).unwrap();
    let opened = NativeFileBackend::open_db(directory.path(), "linked-wal", false)
        .and_then(|files| Engine::open("linked-wal", files, OpenConfig::default()));
    let rejected = matches!(opened, Err(EngineError::Storage(_)));
    drop(opened);
    assert_eq!(fs::read(target).unwrap(), b"external contents");
    assert!(rejected);
}

#[cfg(unix)]
#[test]
fn new_native_storage_has_private_modes_under_permissive_umask() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    if std::env::var_os("MOYODB_NATIVE_PERMISSION_CHILD").is_none() {
        let output = Command::new("sh")
            .args(["-c", "umask 022; exec \"$@\"", "native-permission-test"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "new_native_storage_has_private_modes_under_permissive_umask",
                "--nocapture",
            ])
            .env("MOYODB_NATIVE_PERMISSION_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let directory = TestDirectory::new();
    let root = directory.path().join("new-root");
    let files = NativeFileBackend::open_db(&root, "private", true).unwrap();
    let database = root.join("stackdb").join(encode_db_name("private"));
    for path in [&root, &root.join("stackdb"), &database] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    for path in [files.manifest.path(), files.main.path(), files.wal.path()] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    drop(files);
    let standalone = directory.path().join("standalone.bin");
    let file = NativeFileBackend::open(&standalone, true).unwrap();
    assert_eq!(
        fs::metadata(&standalone).unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(file);
}

#[cfg(unix)]
#[test]
fn opening_existing_storage_preserves_its_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let directory = TestDirectory::new();
    let path = directory.path().join("existing.bin");
    fs::write(&path, b"existing contents").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let file = NativeFileBackend::open(&path, false).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o644
    );
    assert_eq!(
        fs::metadata(directory.path()).unwrap().permissions().mode() & 0o777,
        0o755
    );
    drop(file);
    assert_eq!(fs::read(path).unwrap(), b"existing contents");
}
