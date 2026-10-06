use moyodb_engine::storage::backend::FileBackend;
use moyodb_engine::storage::memory::MemoryBackend;
use moyodb_engine::storage::opfs::OpfsBackend;
use moyodb_engine::EngineError;

fn assert_storage_error<T>(result: std::result::Result<T, EngineError>) {
    assert!(matches!(result, Err(EngineError::Storage(_))));
}

fn assert_unsupported<T>(result: std::result::Result<T, EngineError>) {
    assert!(matches!(result, Err(EngineError::UnsupportedPlatform(_))));
}

fn pending_stable_backend() -> (MemoryBackend, u64) {
    let mut backend = MemoryBackend::from_durable(b"stable".to_vec());
    backend.write_at(0, b"S").unwrap();
    (backend, isize::MAX as u64 + 1)
}

fn assert_oversized_reject_leaves_stable_bytes(backend: &mut MemoryBackend) {
    assert_eq!(backend.len().unwrap(), 6);
    assert_eq!(backend.read_at(0, 6).unwrap(), b"Stable");
    assert_eq!(backend.durable_snapshot().unwrap(), b"stable");
    backend.flush().unwrap();
    assert_eq!(backend.durable_snapshot().unwrap(), b"Stable");
}

#[test]
fn memory_backend_reads_sparse_holes_as_zeroes() {
    let mut backend = MemoryBackend::new();
    backend.write_at(4, &[1, 2]).unwrap();

    assert_eq!(backend.len().unwrap(), 6);
    assert_eq!(backend.read_at(0, 8).unwrap(), vec![0, 0, 0, 0, 1, 2, 0, 0]);
    assert_eq!(backend.read_at(64, 3).unwrap(), vec![0, 0, 0]);
}

#[test]
fn memory_backend_durable_snapshot_changes_only_after_flush() {
    let mut backend = MemoryBackend::new();
    backend.write_at(0, b"working").unwrap();
    assert_eq!(backend.durable_snapshot().unwrap(), Vec::<u8>::new());

    backend.flush().unwrap();
    assert_eq!(backend.durable_snapshot().unwrap(), b"working".to_vec());

    backend.write_at(0, b"pending").unwrap();
    assert_eq!(backend.durable_snapshot().unwrap(), b"working".to_vec());
}

#[test]
fn memory_backend_rejects_operations_after_close() {
    let mut backend = MemoryBackend::new();
    backend.write_at(0, b"x").unwrap();
    backend.close().unwrap();

    assert_storage_error(backend.read_at(0, 1));
    let mut buf = [0xAB];
    assert_storage_error(backend.read_at_into(0, &mut buf));
    assert_eq!(buf, [0xAB]);
    let mut owned = b"keep".to_vec();
    assert_storage_error(backend.read_at_into_vec(0, 1, &mut owned));
    assert_eq!(owned, b"keep");
    assert_storage_error(backend.write_at(0, b"y"));
    assert_storage_error(backend.flush());
    assert_storage_error(backend.len());
    assert_storage_error(backend.truncate(0));
}

#[test]
fn memory_backend_rejects_unrepresentable_read_capacity_without_poisoning_state() {
    let backend = MemoryBackend::from_durable(b"stable".to_vec());
    let oversized = isize::MAX as usize + 1;

    assert!(matches!(
        backend.read_at(0, oversized).unwrap_err(),
        EngineError::Storage(_)
    ));
    assert_eq!(backend.read_at(0, 6).unwrap(), b"stable");
}

#[test]
fn memory_backend_rejects_unrepresentable_write_capacity_without_poisoning_state() {
    let (mut backend, oversized) = pending_stable_backend();

    assert_storage_error(backend.write_at(oversized - 1, b"x"));
    assert_oversized_reject_leaves_stable_bytes(&mut backend);
}

#[test]
fn memory_backend_rejects_unrepresentable_truncate_capacity_without_poisoning_state() {
    let (mut backend, oversized) = pending_stable_backend();

    assert_storage_error(backend.truncate(oversized));
    assert_oversized_reject_leaves_stable_bytes(&mut backend);
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn native_opfs_backend_reports_unsupported_platform() {
    let mut backend = OpfsBackend::new(7, 1);

    assert_unsupported(backend.read_at(0, 1));
    assert_unsupported(backend.write_at(0, b"x"));
    assert_unsupported(backend.flush());
    assert_unsupported(backend.len());
    assert_unsupported(backend.truncate(0));
    backend.close().unwrap();
}
