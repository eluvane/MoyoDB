// Compile the backend with cfg(test) to access its copy counter.
// The production library excludes this counter.
pub mod error {
    pub use moyodb_engine::error::{EngineError, Result};
}

pub mod storage {
    pub mod backend {
        pub use moyodb_engine::storage::backend::{FileBackend, FileSet};
    }
}

#[path = "../src/storage/memory.rs"]
pub mod memory;

use error::Result;
use memory::MemoryBackend;

#[test]
fn sparse_flush_copies_only_the_written_extents() -> Result<()> {
    let initial = vec![0x55; 65_536];
    let mut expected = initial.clone();
    let mut file = MemoryBackend::from_durable(initial.clone());
    file.write_at(32, b"left")?;
    file.write_at(65_520, b"tail")?;
    expected[32..36].copy_from_slice(b"left");
    expected[65_520..65_524].copy_from_slice(b"tail");
    assert_eq!(file.durable_snapshot(), Some(initial));

    file.flush()?;
    assert_eq!(file.durable_snapshot(), Some(expected));
    let copied = file.flush_copy_bytes()?;
    file.flush()?;
    assert_eq!(
        file.flush_copy_bytes()?,
        copied,
        "a repeated flush copies nothing"
    );
    println!("memory_flush_work: sparse_copied_bytes={copied}");
    assert_eq!(copied, 8, "the clean gap between two writes needs no copy");
    Ok(())
}

#[test]
fn overlapping_and_repeated_writes_are_copied_once() -> Result<()> {
    let initial = vec![0x55; 65_536];
    let mut expected = initial.clone();
    let mut file = MemoryBackend::from_durable(initial.clone());
    file.write_at(10, b"abcdef")?;
    expected[10..16].copy_from_slice(b"abcdef");
    file.write_at(12, b"123456")?;
    expected[12..18].copy_from_slice(b"123456");
    file.write_at(10, b"abcdef")?;
    expected[10..16].copy_from_slice(b"abcdef");
    assert_eq!(file.durable_snapshot(), Some(initial));
    file.flush()?;
    assert_eq!(file.durable_snapshot(), Some(expected));
    assert_eq!(file.flush_copy_bytes()?, 8);
    file.flush()?;
    assert_eq!(file.flush_copy_bytes()?, 8);
    Ok(())
}

#[test]
fn truncate_growth_preserves_zeroes_and_defers_durable_size() -> Result<()> {
    let initial = vec![0x55; 65_536];
    let mut file = MemoryBackend::from_durable(initial.clone());
    file.write_at(4096, b"edit")?;
    file.truncate(8192)?;
    file.truncate(16_384)?;
    file.write_at(16_380, b"tail")?;
    let mut expected = initial[..8192].to_vec();
    expected[4096..4100].copy_from_slice(b"edit");
    expected.resize(16_384, 0);
    expected[16_380..].copy_from_slice(b"tail");
    assert_eq!(file.len()?, 16_384);
    assert_eq!(file.durable_snapshot(), Some(initial));
    file.flush()?;
    assert_eq!(file.durable_snapshot(), Some(expected.clone()));
    let copied = file.flush_copy_bytes()?;
    file.truncate(2048)?;
    file.flush()?;
    assert_eq!(file.durable_snapshot(), Some(expected[..2048].to_vec()));
    assert_eq!(
        file.flush_copy_bytes()?,
        copied,
        "shrinking needs no payload copy"
    );
    println!("memory_flush_work: truncate_growth_copied_bytes={copied}");
    assert_eq!(
        copied, 8196,
        "copy the changed prefix and zeroed retained extent"
    );
    Ok(())
}

#[test]
fn write_regrowth_zeroes_the_retained_durable_gap() -> Result<()> {
    for bytes in [b"tail".as_slice(), b"".as_slice()] {
        let initial = vec![0x55; 65_536];
        let mut file = MemoryBackend::from_durable(initial.clone());
        file.truncate(2048)?;
        file.write_at(4096, bytes)?;
        let mut expected = initial[..2048].to_vec();
        expected.resize(4096, 0);
        expected.extend_from_slice(bytes);
        assert_eq!(file.read_at(0, expected.len())?, expected);
        assert_eq!(file.durable_snapshot(), Some(initial));
        file.flush()?;
        assert_eq!(file.durable_snapshot(), Some(expected));
        assert_eq!(file.flush_copy_bytes()?, 2048 + bytes.len());
    }
    Ok(())
}
