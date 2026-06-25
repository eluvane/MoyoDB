use moyodb_engine::storage::memory::MemoryBackend;

#[test]
fn truncate_without_flush_does_not_change_durable_snapshot() {
    let mut file = MemoryBackend::new();
    file.write_at(0, b"abcdef").unwrap();
    file.flush().unwrap();

    file.truncate(0).unwrap();

    let recovered = MemoryBackend::from_durable(file.durable_snapshot().unwrap_or_default());
    assert_eq!(recovered.read_at(0, 6).unwrap(), b"abcdef");
}
