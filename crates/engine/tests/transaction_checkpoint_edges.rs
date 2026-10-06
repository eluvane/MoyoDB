use moyodb_engine::engine::{Engine, OpenConfig, TxMode};
use moyodb_engine::error::Result;
use moyodb_engine::recovery::select_superblock;
use moyodb_engine::storage::backend::FileBackend;
use moyodb_engine::storage::memory::MemoryBundle;

fn assert_published_txid(manifest: &impl FileBackend, committed: u64) -> Result<()> {
    assert_eq!(
        select_superblock(manifest)?.unwrap().last_committed_txid,
        committed
    );
    Ok(())
}

#[test]
fn explicit_checkpoint_publishes_zero_page_commits() -> Result<()> {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open("empty-checkpoint", bundle.files(), OpenConfig::default())?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    let committed = engine.commit_tx(tx)?;
    assert_eq!(engine.stats()?.dirty_pages, 0);
    assert!(engine.stats()?.wal_len > 0);

    engine.checkpoint()?;
    assert_eq!(engine.stats()?.wal_len, 0);
    let files = bundle.crash_recovered_files();
    assert_published_txid(&files.manifest, committed)?;
    let mut reopened = Engine::open("empty-checkpoint", files, OpenConfig::default())?;
    assert_eq!(reopened.stats()?.last_committed_txid, committed);
    Ok(())
}

#[test]
fn wal_byte_threshold_bounds_zero_page_commits() -> Result<()> {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open(
        "empty-auto-checkpoint",
        bundle.files(),
        OpenConfig {
            checkpoint_wal_bytes: 1,
            ..OpenConfig::default()
        },
    )?;
    let mut committed = 0;
    for _ in 0..4 {
        let tx = engine.begin_tx(TxMode::Readwrite)?;
        committed = engine.commit_tx(tx)?;
        assert_eq!(engine.stats()?.dirty_pages, 0);
        assert_eq!(engine.stats()?.wal_len, 0);
    }
    let files = bundle.crash_recovered_files();
    assert_published_txid(&files.manifest, committed)?;
    Ok(())
}

#[test]
fn close_installs_zero_page_commits() -> Result<()> {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open("empty-close", bundle.files(), OpenConfig::default())?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    let committed = engine.commit_tx(tx)?;
    engine.close()?;

    let files = bundle.crash_recovered_files();
    assert_eq!(files.wal.len()?, 0);
    assert_published_txid(&files.manifest, committed)?;
    Ok(())
}
