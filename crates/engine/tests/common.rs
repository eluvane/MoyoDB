#![allow(dead_code)]

use moyodb_engine::engine::{Engine, OpenConfig, ScanRange};
use moyodb_engine::storage::memory::MemoryBundle;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

pub fn open_memory_engine(name: &str) -> (MemoryBundle, Engine<moyodb_engine::MemoryBackend>) {
    let bundle = MemoryBundle::new();
    let engine = Engine::open(name, bundle.files(), OpenConfig::default())
        .unwrap_or_else(|err| panic!("open in-memory engine {name}: {err}"));
    (bundle, engine)
}

pub fn reopen_memory_engine(
    name: &str,
    bundle: &MemoryBundle,
) -> Engine<moyodb_engine::MemoryBackend> {
    Engine::open(name, bundle.crash_recovered_files(), OpenConfig::default())
        .unwrap_or_else(|err| panic!("reopen in-memory engine {name}: {err}"))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn from_hex(hex: &str) -> Vec<u8> {
    assert!(
        hex.len() % 2 == 0,
        "hex string must have even length in proof artifact: {hex}"
    );
    (0..hex.len())
        .step_by(2)
        .map(|idx| {
            u8::from_str_radix(&hex[idx..idx + 2], 16).unwrap_or_else(|err| {
                panic!(
                    "invalid hex byte '{}' at offset {idx}: {err}",
                    &hex[idx..idx + 2]
                )
            })
        })
        .collect()
}

pub fn artifacts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../proofs/artifacts")
}

pub fn read_artifact(name: &str) -> Value {
    let path = artifacts_dir().join(name);
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read proof artifact {}: {err}", path.display()));
    serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("parse proof artifact {}: {err}", path.display()))
}

pub fn scan_all(
    engine: &mut Engine<moyodb_engine::MemoryBackend>,
    tx_id: u64,
    store: &str,
) -> Vec<(Vec<u8>, Vec<u8>)> {
    engine
        .scan(tx_id, store, &ScanRange::default())
        .unwrap_or_else(|err| panic!("scan all rows from {store}: {err}"))
        .into_iter()
        .map(|pair| (pair.key, pair.value))
        .collect()
}
