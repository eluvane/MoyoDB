use moyodb_engine::snapshot::{encode_snapshot, SnapshotContents, SnapshotEntry, SnapshotStore};
use moyodb_engine::{ChangeFeedPolicy, Engine, MemoryBundle, OpenConfig, TxMode};
use std::fs;
use std::path::Path;

fn legacy_snapshot(version: u32) -> Vec<u8> {
    let mut bytes = vec![0; 32];
    bytes.extend_from_slice(&11u64.to_le_bytes());
    if version == 2 {
        bytes.extend_from_slice(&7u64.to_le_bytes());
    }
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&6u16.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(&1u64.to_le_bytes());
    bytes.extend_from_slice(b"legacy");
    bytes.extend_from_slice(&3u16.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&23u32.to_le_bytes());
    bytes.extend_from_slice(b"raw");
    bytes.extend_from_slice(b"BDTTL001-raw-user-value");
    bytes[..8].copy_from_slice(b"BDSNAP01");
    bytes[8..12].copy_from_slice(&version.to_le_bytes());
    let body_len = (bytes.len() - 32) as u64;
    bytes[16..24].copy_from_slice(&body_len.to_le_bytes());
    let checksum = moyodb_engine::checksum::checksum_with_zeroed_region(&bytes, 24, 4);
    bytes[24..28].copy_from_slice(&checksum.to_le_bytes());
    let decoded = moyodb_engine::snapshot::decode_snapshot(&bytes).unwrap();
    assert_eq!(decoded.schema_version, if version == 2 { 7 } else { 0 });
    assert_eq!(
        decoded.stores[0].entries[0].value,
        b"BDTTL001-raw-user-value"
    );
    bytes
}

fn entry(key: &[u8], value: &[u8], expires_at_ms: Option<u64>) -> SnapshotEntry {
    SnapshotEntry {
        key: key.to_vec(),
        value: value.to_vec(),
        expires_at_ms,
    }
}

fn save_bundle(output: &Path, label: &str, bundle: &MemoryBundle) {
    for (file, bytes) in [
        ("manifest", bundle.manifest.durable_snapshot().unwrap()),
        ("main", bundle.main.durable_snapshot().unwrap()),
        ("wal", bundle.wal.durable_snapshot().unwrap()),
    ] {
        fs::write(output.join(format!("{label}-{file}.bin")), bytes).unwrap();
    }
}

fn main() {
    let output = std::env::args().nth(1).expect("output directory");
    let output = Path::new(&output);
    for version in 1..=2 {
        fs::write(
            output.join(format!("snapshot-v{version}.bin")),
            legacy_snapshot(version),
        )
        .unwrap();
    }
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open(
        "compatibility-release-1.0.1",
        bundle.files(),
        OpenConfig {
            checkpoint_wal_bytes: u64::MAX,
            checkpoint_dirty_pages: usize::MAX,
            ..OpenConfig::default()
        },
    )
    .unwrap();
    let input = SnapshotContents {
        source_last_committed_txid: 0,
        schema_version: 7,
        stores: vec![
            SnapshotStore {
                name: "docs".into(),
                flags: 1,
                entries: vec![
                    entry(&[0, 0xff], &[0, 1, 0xff, 2], None),
                    entry(b"alpha", b"release-1.0.1", None),
                    entry(b"expired", b"must-not-read", Some(1)),
                    entry(b"live-ttl", b"expires-in-2096", Some(4_000_000_000_000)),
                ],
            },
            SnapshotStore {
                name: "empty".into(),
                flags: 1,
                entries: vec![],
            },
            SnapshotStore {
                name: "legacy".into(),
                flags: 0,
                entries: vec![entry(b"raw", b"BDTTL001-raw-user-value", None)],
            },
        ],
    };
    engine
        .import_snapshot(&encode_snapshot(&input).unwrap())
        .unwrap();
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine
        .set_change_feed_policy(
            tx,
            ChangeFeedPolicy {
                enabled: true,
                retain_txids: Some(64),
            },
        )
        .unwrap();
    for index in 0..48 {
        let key = format!("row:{index:03}");
        let value: Vec<u8> = (0..80).map(|byte| ((index + byte) % 251) as u8).collect();
        engine.put(tx, "docs", key.as_bytes(), &value).unwrap();
    }
    let overflow: Vec<u8> = (0..9000).map(|index| (index % 251) as u8).collect();
    engine.put(tx, "docs", b"overflow", &overflow).unwrap();
    engine.commit_tx(tx).unwrap();
    save_bundle(output, "recovery", &bundle);
    fs::write(
        output.join("snapshot-v3.bin"),
        engine.export_snapshot().unwrap(),
    )
    .unwrap();
    engine.checkpoint().unwrap();
    save_bundle(output, "checkpoint", &bundle);
    let stats = engine.stats().unwrap();
    println!(
        "txid={} next_page_id={} main_len={} wal_len={}",
        stats.last_committed_txid, stats.next_page_id, stats.main_len, stats.wal_len
    );
}
