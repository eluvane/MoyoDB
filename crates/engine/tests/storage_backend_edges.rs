use moyodb_engine::storage::backend::FileBackend;
use moyodb_engine::storage::memory::MemoryBackend;
use moyodb_engine::storage::opfs::OpfsBackend;
use moyodb_engine::EngineError;

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

    assert!(matches!(
        backend.read_at(0, 1).unwrap_err(),
        EngineError::Storage(_)
    ));
    assert!(matches!(
        backend.write_at(0, b"y").unwrap_err(),
        EngineError::Storage(_)
    ));
    assert!(matches!(
        backend.flush().unwrap_err(),
        EngineError::Storage(_)
    ));
    assert!(matches!(
        backend.len().unwrap_err(),
        EngineError::Storage(_)
    ));
    assert!(matches!(
        backend.truncate(0).unwrap_err(),
        EngineError::Storage(_)
    ));
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn native_opfs_backend_reports_unsupported_platform() {
    let mut backend = OpfsBackend::new(7, 1);

    assert!(matches!(
        backend.read_at(0, 1).unwrap_err(),
        EngineError::UnsupportedPlatform(_)
    ));
    assert!(matches!(
        backend.write_at(0, b"x").unwrap_err(),
        EngineError::UnsupportedPlatform(_)
    ));
    assert!(matches!(
        backend.flush().unwrap_err(),
        EngineError::UnsupportedPlatform(_)
    ));
    assert!(matches!(
        backend.len().unwrap_err(),
        EngineError::UnsupportedPlatform(_)
    ));
    assert!(matches!(
        backend.truncate(0).unwrap_err(),
        EngineError::UnsupportedPlatform(_)
    ));
    backend.close().unwrap();
}
