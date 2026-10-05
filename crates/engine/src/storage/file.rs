use crate::bytes::encode_db_name;
use crate::error::{EngineError, Result};
use crate::storage::backend::{FileBackend, FileSet};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

const CONTROL_FILE_NAME: &str = "root-manifest.bin";
const CONTROL_SLOT_SIZE: usize = 4096;
const LOCK_DIRECTORY: &str = ".moyodb.lock";
static NEXT_LEASE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
/// The storage path and its parent directories must remain unchanged while open.
pub struct NativeFileBackend {
    path: PathBuf,
    file: Mutex<Option<File>>,
    control: Option<Arc<Self>>,
    lease: Option<Arc<StorageLease>>,
}

impl NativeFileBackend {
    /// The parent directory must exist. The directory lease excludes Node sessions.
    pub fn open(path: impl AsRef<Path>, create_if_missing: bool) -> Result<Self> {
        let path = absolute_path(path.as_ref())?;
        let parent = path.parent().ok_or_else(|| {
            EngineError::Storage("native file path has no parent directory".into())
        })?;
        ensure_directory(parent, false)?;
        validate_file(&path, create_if_missing)?;
        let directory = if parent
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(valid_generation_name)
        {
            parent.parent().unwrap_or(parent)
        } else {
            parent
        };
        let lease = Arc::new(StorageLease::acquire(directory)?);
        let mut file = Self::open_file(path, create_if_missing)?;
        file.lease = Some(lease);
        Ok(file)
    }

    fn open_file(path: impl AsRef<Path>, create_if_missing: bool) -> Result<Self> {
        let path = absolute_path(path.as_ref())?;
        let parent = path.parent().ok_or_else(|| {
            EngineError::Storage("native file path has no parent directory".into())
        })?;
        ensure_directory(parent, false)?;
        validate_file(&path, create_if_missing)?;

        let mut options = OpenOptions::new();
        options.read(true).write(true);
        let (file, created) = match options.open(&path) {
            Ok(file) => (file, false),
            Err(err) if create_if_missing && err.kind() == io::ErrorKind::NotFound => {
                options.create_new(true);
                match options.open(&path) {
                    Ok(file) => (file, true),
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                        validate_file(&path, false)?;
                        options.create_new(false);
                        (
                            options
                                .open(&path)
                                .map_err(|err| io_error("open", &path, err))?,
                            false,
                        )
                    }
                    Err(err) => return Err(io_error("create", &path, err)),
                }
            }
            Err(err) => return Err(io_error("open", &path, err)),
        };
        if !file
            .metadata()
            .map_err(|err| io_error("inspect", &path, err))?
            .is_file()
        {
            return Err(EngineError::Storage(format!(
                "native file is not a regular file: {}",
                path.display()
            )));
        }
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(EngineError::DatabaseBusy(format!(
                    "native file is already open: {}",
                    path.display()
                )));
            }
            Err(TryLockError::Error(err)) => return Err(io_error("lock", &path, err)),
        }
        if created {
            file.sync_all()
                .map_err(|err| io_error("flush", &path, err))?;
            sync_directory(parent)?;
        }
        Ok(Self {
            path,
            file: Mutex::new(Some(file)),
            control: None,
            lease: None,
        })
    }

    /// Stores databases under `root/stackdb/<UTF-8 hex name>`, including OPFS generations.
    pub fn open_db(
        root: impl AsRef<Path>,
        db_name: &str,
        create_if_missing: bool,
    ) -> Result<FileSet<Self>> {
        if db_name.is_empty() || db_name.len() > 127 {
            return Err(EngineError::Storage(
                "native database name must contain 1 to 127 UTF-8 bytes".into(),
            ));
        }
        let root = absolute_path(root.as_ref())?;
        ensure_directory(&root, create_if_missing)?;
        let stackdb = root.join("stackdb");
        ensure_directory(&stackdb, create_if_missing)?;
        let directory = stackdb.join(encode_db_name(db_name));
        ensure_directory(&directory, create_if_missing)?;
        let lease = Arc::new(StorageLease::acquire(&directory)?);
        let control = match fs::symlink_metadata(directory.join(CONTROL_FILE_NAME)) {
            Ok(_) => Some(Arc::new(Self::open_file(
                directory.join(CONTROL_FILE_NAME),
                false,
            )?)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => return Err(io_error("inspect control file", &directory, err)),
        };
        let generation = control
            .as_ref()
            .map(|control| active_generation(control, &directory))
            .transpose()?
            .flatten();
        let active = generation.is_some();
        let data_directory = match generation {
            Some(generation) => {
                let data_directory = directory.join(generation);
                ensure_directory(&data_directory, false).map_err(|err| {
                    EngineError::Corruption(format!(
                        "native active generation is missing or invalid: {err}"
                    ))
                })?;
                data_directory
            }
            None => directory.clone(),
        };
        let mut manifest = Self::open_file(data_directory.join("manifest.bin"), create_if_missing)?;
        let mut main = Self::open_file(data_directory.join("main.bin"), create_if_missing)?;
        let mut wal = Self::open_file(data_directory.join("wal.bin"), create_if_missing)?;
        if active {
            // Stale legacy files must not become authoritative after later control damage.
            for name in ["manifest.bin", "main.bin", "wal.bin"] {
                let path = directory.join(name);
                match fs::symlink_metadata(&path) {
                    Ok(_) => {
                        Self::open_file(&path, false)?.close()?;
                        fs::remove_file(&path)
                            .map_err(|err| io_error("remove stale legacy file", &path, err))?;
                    }
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                    Err(err) => return Err(io_error("inspect legacy file", &path, err)),
                }
            }
            sync_directory(&directory)?;
        }
        manifest.control = control.clone();
        main.control = control.clone();
        wal.control = control;
        manifest.lease = Some(lease.clone());
        main.lease = Some(lease.clone());
        wal.lease = Some(lease);
        Ok(FileSet::new(manifest, main, wal))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock_file(&self, operation: &str) -> Result<MutexGuard<'_, Option<File>>> {
        let file = self
            .file
            .lock()
            .map_err(|_| EngineError::Storage("native file backend mutex poisoned".into()))?;
        if file.is_none() {
            return Err(EngineError::Storage(format!(
                "{operation} on closed native file backend"
            )));
        }
        Ok(file)
    }

    pub fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        FileBackend::read_at(self, offset, len)
    }

    pub fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        FileBackend::write_at(self, offset, bytes)
    }

    pub fn flush(&mut self) -> Result<()> {
        FileBackend::flush(self)
    }

    pub fn len(&self) -> Result<u64> {
        FileBackend::len(self)
    }

    pub fn is_empty(&self) -> Result<bool> {
        FileBackend::is_empty(self)
    }

    pub fn truncate(&mut self, size: u64) -> Result<()> {
        FileBackend::truncate(self, size)
    }

    /// Closing releases the file and lease. Durability requires an earlier flush.
    pub fn close(&mut self) -> Result<()> {
        FileBackend::close(self)
    }
}

impl FileBackend for NativeFileBackend {
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        let mut guard = self.lock_file("read")?;
        checked_end(offset, len, "read")?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(len).map_err(|err| {
            EngineError::Storage(format!(
                "native read allocation failed for {len} bytes: {err}"
            ))
        })?;
        bytes.resize(len, 0);
        let file = guard
            .as_mut()
            .ok_or_else(|| EngineError::Storage("read on closed native file backend".into()))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|err| io_error("seek for read", &self.path, err))?;
        let mut read = 0;
        while read < len {
            match file.read(&mut bytes[read..]) {
                Ok(0) => break,
                Ok(count) => read += count,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(io_error("read", &self.path, err)),
            }
        }
        Ok(bytes)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        let mut guard = self.lock_file("write")?;
        checked_end(offset, bytes.len(), "write")?;
        let file = guard
            .as_mut()
            .ok_or_else(|| EngineError::Storage("write on closed native file backend".into()))?;
        if bytes.is_empty() {
            let len = file
                .metadata()
                .map_err(|err| io_error("len", &self.path, err))?
                .len();
            if offset > len {
                file.set_len(offset)
                    .map_err(|err| io_error("extend", &self.path, err))?;
            }
            return Ok(());
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|err| io_error("seek for write", &self.path, err))?;
        file.write_all(bytes)
            .map_err(|err| io_error("write", &self.path, err))
    }

    fn flush(&mut self) -> Result<()> {
        self.lock_file("flush")?
            .as_ref()
            .ok_or_else(|| EngineError::Storage("flush on closed native file backend".into()))?
            .sync_all()
            .map_err(|err| io_error("flush", &self.path, err))
    }

    fn len(&self) -> Result<u64> {
        self.lock_file("len")?
            .as_ref()
            .ok_or_else(|| EngineError::Storage("len on closed native file backend".into()))?
            .metadata()
            .map(|metadata| metadata.len())
            .map_err(|err| io_error("len", &self.path, err))
    }

    fn truncate(&mut self, size: u64) -> Result<()> {
        let guard = self.lock_file("truncate")?;
        checked_end(size, 0, "truncate")?;
        guard
            .as_ref()
            .ok_or_else(|| EngineError::Storage("truncate on closed native file backend".into()))?
            .set_len(size)
            .map_err(|err| io_error("truncate", &self.path, err))
    }

    fn close(&mut self) -> Result<()> {
        let mut file = self
            .file
            .lock()
            .map_err(|_| EngineError::Storage("native file backend mutex poisoned".into()))?;
        file.take();
        self.control.take();
        if let Some(lease) = self.lease.take() {
            if let Ok(mut lease) = Arc::try_unwrap(lease) {
                lease.release()?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct LeaseRecord {
    pid: u32,
    token: String,
}

#[derive(Debug)]
struct StorageLease {
    directory: PathBuf,
    owner: LeaseRecord,
    released: bool,
    published: bool,
}

impl StorageLease {
    fn acquire(directory: &Path) -> Result<Self> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|err| EngineError::Storage(format!("native lease clock error: {err}")))?
            .as_nanos();
        let serial = NEXT_LEASE.fetch_add(1, Ordering::Relaxed);
        let value = timestamp ^ (u128::from(std::process::id()) << 96) ^ (u128::from(serial) << 32);
        let raw = format!("{value:032x}");
        let token = format!(
            "{}-{}-{}-{}-{}",
            &raw[..8],
            &raw[8..12],
            &raw[12..16],
            &raw[16..20],
            &raw[20..]
        );
        let pending = directory.join(format!(".moyodb.pending-{}-{token}", std::process::id()));
        let builder = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = fs::DirBuilder::new();
                builder.mode(0o700);
                builder
            }
            #[cfg(not(unix))]
            {
                fs::DirBuilder::new()
            }
        };
        builder
            .create(&pending)
            .map_err(|err| io_error("create pending lease", &pending, err))?;
        let mut lease = Self {
            directory: pending,
            owner: LeaseRecord {
                pid: std::process::id(),
                token,
            },
            released: false,
            published: false,
        };
        let path = lease.record_path();
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&path)
            .map_err(|err| io_error("create lease record", &path, err))?;
        let bytes = serde_json::to_vec(&lease.owner)
            .map_err(|err| EngineError::Storage(format!("native lease encoding failed: {err}")))?;
        file.write_all(&bytes)
            .map_err(|err| io_error("write lease record", &path, err))?;
        file.sync_all()
            .map_err(|err| io_error("flush lease record", &path, err))?;
        drop(file);
        sync_directory(&lease.directory)?;
        let lock_directory = directory.join(LOCK_DIRECTORY);
        for _ in 0..3 {
            match fs::rename(&lease.directory, &lock_directory) {
                Ok(()) => {
                    lease.directory = lock_directory;
                    lease.published = true;
                    sync_directory(directory)?;
                    return Ok(lease);
                }
                Err(err) => {
                    match fs::symlink_metadata(&lock_directory) {
                        Ok(_) => {}
                        Err(inspection) if inspection.kind() == io::ErrorKind::NotFound => {
                            return Err(io_error("publish lease", &lock_directory, err));
                        }
                        Err(inspection) => {
                            return Err(io_error("inspect lease", &lock_directory, inspection))
                        }
                    }
                    ensure_directory(&lock_directory, false).map_err(|_| {
                        EngineError::DatabaseBusy(
                            "native database lease directory cannot be verified".into(),
                        )
                    })?;
                    if fs::read_dir(&lock_directory)
                        .map_err(|err| io_error("inspect lease entries", &lock_directory, err))?
                        .next()
                        .is_none()
                    {
                        match fs::remove_dir(&lock_directory) {
                            Ok(()) => {}
                            Err(err)
                                if matches!(
                                    err.kind(),
                                    io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::NotFound
                                ) => {}
                            Err(err) => {
                                return Err(io_error("remove empty lease", &lock_directory, err))
                            }
                        }
                        continue;
                    }
                    let owner = read_lease(&lock_directory)?;
                    if process_alive(owner.pid)? {
                        return Err(EngineError::DatabaseBusy(format!(
                            "native database directory is owned by process {}",
                            owner.pid
                        )));
                    }
                    remove_lease(&lock_directory, &owner)?;
                }
            }
        }
        Err(EngineError::DatabaseBusy(
            "native database lease changed while opening".into(),
        ))
    }

    fn record_path(&self) -> PathBuf {
        self.directory.join(format!("{}.json", self.owner.token))
    }

    fn release(&mut self) -> Result<()> {
        if !self.released {
            let removed = if self.published {
                remove_lease(&self.directory, &self.owner)
            } else {
                remove_pending_lease(&self.directory, &self.owner)
            };
            match removed {
                Ok(()) => self.released = true,
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }
}

fn remove_pending_lease(directory: &Path, owner: &LeaseRecord) -> Result<()> {
    ensure_directory(directory, false)?;
    let path = directory.join(format!("{}.json", owner.token));
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            validate_file(&path, false)?;
            fs::remove_file(&path).map_err(|err| io_error("remove pending record", &path, err))?;
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(io_error("inspect pending record", &path, err)),
    }
    fs::remove_dir(directory).map_err(|err| io_error("remove pending lease", directory, err))
}

impl Drop for StorageLease {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

fn read_lease(directory: &Path) -> Result<LeaseRecord> {
    let invalid =
        || EngineError::DatabaseBusy("native database lease owner cannot be verified".into());
    ensure_directory(directory, false).map_err(|_| invalid())?;
    let records = fs::read_dir(directory)
        .map_err(|_| invalid())?
        .take(2)
        .map(|record| record.map(|record| record.file_name()))
        .collect::<io::Result<Vec<_>>>()
        .map_err(|_| invalid())?;
    if records.len() != 1 {
        return Err(invalid());
    }
    let filename = records[0].to_str().ok_or_else(invalid)?;
    let token = filename.strip_suffix(".json").ok_or_else(invalid)?;
    if !valid_token(token) {
        return Err(invalid());
    }
    let path = directory.join(filename);
    validate_file(&path, false).map_err(|_| invalid())?;
    let file = File::open(&path).map_err(|_| invalid())?;
    let mut bytes = Vec::new();
    file.take(1025)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.len() > 1024 {
        return Err(invalid());
    }
    let owner: LeaseRecord = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if owner.pid == 0 || owner.token != token {
        return Err(invalid());
    }
    Ok(owner)
}

fn valid_token(token: &str) -> bool {
    token.len() == 36
        && token.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn remove_lease(directory: &Path, owner: &LeaseRecord) -> Result<()> {
    let current = read_lease(directory)?;
    if current.pid != owner.pid || current.token != owner.token {
        return Err(EngineError::DatabaseBusy(
            "native database lease changed before release".into(),
        ));
    }
    let path = directory.join(format!("{}.json", owner.token));
    fs::remove_file(&path).map_err(|err| io_error("remove lease record", &path, err))?;
    match fs::remove_dir(directory) {
        Ok(()) => {}
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::NotFound
            ) => {}
        Err(err) => return Err(io_error("remove lease directory", directory, err)),
    }
    if let Some(parent) = directory.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

#[cfg(windows)]
fn process_alive(pid: u32) -> Result<bool> {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    // SAFETY: OpenProcess takes value parameters. Its handle remains local.
    let handle = unsafe { OpenProcess(0x1000, 0, pid) };
    if handle.is_null() {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(87) {
            return Ok(false);
        }
        return Err(EngineError::DatabaseBusy(format!(
            "native lease process cannot be inspected: {err}"
        )));
    }
    // SAFETY: This handle came from OpenProcess and is closed exactly once.
    let closed = unsafe { CloseHandle(handle) };
    if closed == 0 {
        return Err(EngineError::DatabaseBusy(format!(
            "native lease process handle cannot be closed: {}",
            io::Error::last_os_error()
        )));
    }
    Ok(true)
}

#[cfg(unix)]
fn process_alive(pid: u32) -> Result<bool> {
    use std::ffi::c_int;
    unsafe extern "C" {
        fn kill(pid: c_int, signal: c_int) -> c_int;
    }
    let pid = c_int::try_from(pid).map_err(|_| {
        EngineError::DatabaseBusy("native lease process ID cannot be inspected".into())
    })?;
    // SAFETY: A positive PID and signal zero only query process existence.
    if unsafe { kill(pid, 0) } == 0 {
        return Ok(true);
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(3) {
        return Ok(false);
    }
    Err(EngineError::DatabaseBusy(format!(
        "native lease process cannot be inspected: {err}"
    )))
}

#[cfg(not(any(windows, unix)))]
fn process_alive(_pid: u32) -> Result<bool> {
    Err(EngineError::DatabaseBusy(
        "native lease process probe is unavailable".into(),
    ))
}

fn active_generation(control: &NativeFileBackend, directory: &Path) -> Result<Option<String>> {
    if control.is_empty()? {
        return Ok(None);
    }
    let bytes = control.read_at(0, CONTROL_SLOT_SIZE * 2)?;
    let first = decode_control_slot(&bytes[..CONTROL_SLOT_SIZE])?;
    let second = decode_control_slot(&bytes[CONTROL_SLOT_SIZE..])?;
    let selected = match (first, second) {
        (Some(first), Some(second)) => Some(if first.0 >= second.0 { first } else { second }),
        (Some(first), None) => Some(first),
        (None, Some(second)) => Some(second),
        (None, None) => None,
    };
    if let Some((_, generation)) = selected {
        return Ok(Some(generation));
    }
    for name in ["manifest.bin", "main.bin", "wal.bin"] {
        match fs::symlink_metadata(directory.join(name)) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                return Ok(None)
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(io_error("inspect legacy file", directory, err)),
        }
    }
    Err(EngineError::Corruption(
        "native control file has no valid slot".into(),
    ))
}

fn decode_control_slot(bytes: &[u8]) -> Result<Option<(u64, String)>> {
    if bytes.len() < CONTROL_SLOT_SIZE {
        return Ok(None);
    }
    let bytes = &bytes[..CONTROL_SLOT_SIZE];
    if &bytes[..8] != b"BDBROOT1" {
        return Ok(None);
    }
    let mut checksum = 0x811c_9dc5u32;
    for (index, byte) in bytes.iter().enumerate() {
        let value = if (24..28).contains(&index) { 0 } else { *byte };
        checksum = (checksum ^ u32::from(value)).wrapping_mul(0x0100_0193);
    }
    let stored_checksum = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
    if checksum != stored_checksum {
        return Ok(None);
    }
    let version = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    if version != 1 {
        return Err(EngineError::Corruption(format!(
            "unsupported control file version {version}"
        )));
    }
    let name_len = usize::from(u16::from_le_bytes([bytes[20], bytes[21]]));
    if name_len > CONTROL_SLOT_SIZE - 32 {
        return Err(EngineError::Corruption(format!(
            "control slot name length out of bounds: {name_len}"
        )));
    }
    let name = std::str::from_utf8(&bytes[32..32 + name_len])
        .map_err(|_| EngineError::Corruption("invalid generation in control slot".into()))?;
    if !valid_generation_name(name) {
        return Err(EngineError::Corruption(format!(
            "invalid generation in control slot: {name}"
        )));
    }
    let counter = u64::from_le_bytes([
        bytes[12], bytes[13], bytes[14], bytes[15], bytes[16], bytes[17], bytes[18], bytes[19],
    ]);
    Ok(Some((counter, name.into())))
}

fn valid_generation_name(name: &str) -> bool {
    let mut parts = name.split('-');
    let prefix = parts.next().unwrap_or_default();
    let first = parts.next().unwrap_or_default();
    let second = parts.next().unwrap_or_default();
    prefix.eq_ignore_ascii_case("gen")
        && !first.is_empty()
        && !second.is_empty()
        && first.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && second.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && parts.next().is_none()
}

fn checked_end(offset: u64, len: usize, operation: &str) -> Result<u64> {
    let len = u64::try_from(len)
        .map_err(|_| EngineError::Storage(format!("native {operation} length overflow")))?;
    let end = offset.checked_add(len).ok_or_else(|| {
        EngineError::Storage(format!(
            "native {operation} range overflow: offset={offset} len={len}"
        ))
    })?;
    if end > i64::MAX as u64 {
        return Err(EngineError::Storage(format!(
            "native {operation} exceeds maximum file offset: {end}"
        )));
    }
    Ok(end)
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() || path.components().any(|part| part == Component::ParentDir) {
        return Err(EngineError::Storage("invalid native storage path".into()));
    }
    #[cfg(windows)]
    for part in path.components() {
        if let Component::Normal(name) = part {
            let name = name.to_string_lossy();
            let base = name
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase();
            if name.contains(':')
                || name.ends_with(['.', ' '])
                || matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || (base.len() == 4
                    && (base.starts_with("COM") || base.starts_with("LPT"))
                    && matches!(base.as_bytes()[3], b'1'..=b'9'))
            {
                return Err(EngineError::Storage(
                    "invalid native storage path component".into(),
                ));
            }
        }
    }
    std::path::absolute(path).map_err(|err| io_error("resolve path", path, err))
}

fn ensure_directory(path: &Path, create_if_missing: bool) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if matches!(component, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(EngineError::Storage(format!(
                    "native storage directory is not a regular directory: {}",
                    current.display()
                )))
            }
            Err(err) if create_if_missing && err.kind() == io::ErrorKind::NotFound => {
                match fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(err) => return Err(io_error("create directory", &current, err)),
                }
                let metadata = fs::symlink_metadata(&current)
                    .map_err(|err| io_error("inspect directory", &current, err))?;
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(EngineError::Storage(format!(
                        "native storage directory is not a regular directory: {}",
                        current.display()
                    )));
                }
                if let Some(parent) = current.parent() {
                    sync_directory(parent)?;
                }
            }
            Err(err) => return Err(io_error("inspect directory", &current, err)),
        }
    }
    Ok(())
}

fn validate_file(path: &Path, create_if_missing: bool) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(EngineError::Storage(format!(
            "native storage path is not a regular file: {}",
            path.display()
        ))),
        Err(err) if create_if_missing && err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(io_error("inspect file", path, err)),
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|err| io_error("flush directory", path, err))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    // The standard library does not expose directory flushing on these platforms.
    Ok(())
}

fn io_error(operation: &str, path: &Path, err: io::Error) -> EngineError {
    EngineError::Storage(format!("native {operation} {}: {err}", path.display()))
}
