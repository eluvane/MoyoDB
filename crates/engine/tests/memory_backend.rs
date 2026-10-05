use moyodb_engine::storage::backend::FileBackend;
use moyodb_engine::storage::memory::MemoryBackend;

#[test]
fn default_append_offset_observes_changes_through_a_shared_backend() {
    let file = MemoryBackend::new();
    let mut writer = file.clone();
    assert_eq!(file.append_offset().unwrap(), 0);

    writer.write_at(0, b"abcdef").unwrap();
    assert_eq!(file.append_offset().unwrap(), 6);
    writer.truncate(2).unwrap();
    assert_eq!(file.append_offset().unwrap(), 2);
    writer.write_at(8, b"x").unwrap();
    assert_eq!(file.append_offset().unwrap(), 9);
}

#[test]
fn truncate_without_flush_does_not_change_durable_snapshot() {
    let mut file = MemoryBackend::new();
    file.write_at(0, b"abcdef").unwrap();
    file.flush().unwrap();

    file.truncate(0).unwrap();

    let recovered = MemoryBackend::from_durable(file.durable_snapshot().unwrap_or_default());
    assert_eq!(recovered.read_at(0, 6).unwrap(), b"abcdef");
}
