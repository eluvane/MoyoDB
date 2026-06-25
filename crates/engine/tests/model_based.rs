mod common;

use moyodb_engine::engine::{Failpoint, ScanRange, TxMode};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u32(&mut self) -> u32 {
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.state >> 32) as u32
    }

    fn next_usize(&mut self, max: usize) -> usize {
        if max == 0 {
            0
        } else {
            (self.next_u32() as usize) % max
        }
    }
}

fn key(n: usize) -> Vec<u8> {
    (n as u32).to_be_bytes().to_vec()
}

fn value(op: usize, n: usize) -> Vec<u8> {
    format!("value-{op}-{n}").into_bytes()
}

fn assert_engine_matches_model(
    engine: &mut moyodb_engine::engine::Engine<moyodb_engine::MemoryBackend>,
    model: &BTreeMap<Vec<u8>, Vec<u8>>,
) {
    let ro = engine.begin_tx(TxMode::Readonly).unwrap();
    let rows = common::scan_all(engine, ro, "kv");
    engine.rollback_tx(ro).unwrap();
    let expected: Vec<(Vec<u8>, Vec<u8>)> = model
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    assert_eq!(rows, expected);
}

fn seed_store(
    engine: &mut moyodb_engine::engine::Engine<moyodb_engine::MemoryBackend>,
) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    engine.commit_tx(tx).unwrap();
    BTreeMap::new()
}

#[test]
fn btree_matches_btreemap_random_ops() {
    let (_bundle, mut engine) = common::open_memory_engine("model-random-ops");
    let mut model = seed_store(&mut engine);
    let mut rng = Rng::new(0x5eed_b7ee);

    for op in 0..500usize {
        let k = key(rng.next_usize(160));
        if rng.next_usize(100) < 65 {
            let v = value(op, rng.next_usize(10_000));
            let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
            engine.put(tx, "kv", &k, &v).unwrap();
            engine.commit_tx(tx).unwrap();
            model.insert(k, v);
        } else {
            let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
            let actual = engine.delete(tx, "kv", &k).unwrap();
            engine.commit_tx(tx).unwrap();
            let expected = model.remove(&k).is_some();
            assert_eq!(actual, expected);
        }

        if op % 37 == 0 {
            assert_engine_matches_model(&mut engine, &model);
        }
    }
    assert_engine_matches_model(&mut engine, &model);
}

#[test]
fn scan_matches_btreemap_ranges() {
    let (_bundle, mut engine) = common::open_memory_engine("model-scan-ranges");
    let mut model = seed_store(&mut engine);
    let mut rng = Rng::new(0x51a0_5eed);

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    for i in 0..240usize {
        if rng.next_usize(100) < 80 {
            let k = key(i);
            let v = value(i, i * 17);
            engine.put(tx, "kv", &k, &v).unwrap();
            model.insert(k, v);
        }
    }
    engine.commit_tx(tx).unwrap();

    for _ in 0..100 {
        let start = rng.next_usize(240);
        let end = start + rng.next_usize(240 - start);
        let range = ScanRange {
            gte: Some(key(start)),
            lte: Some(key(end)),
            ..Default::default()
        };
        let ro = engine.begin_tx(TxMode::Readonly).unwrap();
        let rows = engine.scan(ro, "kv", &range).unwrap();
        engine.rollback_tx(ro).unwrap();
        let expected: Vec<(Vec<u8>, Vec<u8>)> = model
            .range(key(start)..=key(end))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let actual: Vec<(Vec<u8>, Vec<u8>)> =
            rows.into_iter().map(|row| (row.key, row.value)).collect();
        assert_eq!(actual, expected);
    }
}

#[test]
fn rollback_does_not_touch_committed_state() {
    let (_bundle, mut engine) = common::open_memory_engine("model-rollback");
    let mut model = seed_store(&mut engine);
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    for i in 0..50usize {
        let k = key(i);
        let v = value(i, i);
        engine.put(tx, "kv", &k, &v).unwrap();
        model.insert(k, v);
    }
    engine.commit_tx(tx).unwrap();

    let original = model.clone();
    let rw = engine.begin_tx(TxMode::Readwrite).unwrap();
    for i in 0..50usize {
        if i % 2 == 0 {
            engine.delete(rw, "kv", &key(i)).unwrap();
        } else {
            engine.put(rw, "kv", &key(i), b"rolled-back").unwrap();
        }
    }
    engine.rollback_tx(rw).unwrap();

    assert_eq!(model, original);
    assert_engine_matches_model(&mut engine, &model);
}

#[test]
fn commit_matches_model_state() {
    let (_bundle, mut engine) = common::open_memory_engine("model-commit");
    let mut model = seed_store(&mut engine);
    let mut rng = Rng::new(0xc011_1117);

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    for op in 0..300usize {
        let k = key(rng.next_usize(120));
        if rng.next_usize(100) < 75 {
            let v = value(op, rng.next_usize(1_000_000));
            engine.put(tx, "kv", &k, &v).unwrap();
            model.insert(k, v);
        } else {
            engine.delete(tx, "kv", &k).unwrap();
            model.remove(&k);
        }
    }
    engine.commit_tx(tx).unwrap();
    assert_engine_matches_model(&mut engine, &model);
}

#[test]
fn delete_matches_model_state() {
    let (_bundle, mut engine) = common::open_memory_engine("model-delete");
    let mut model = seed_store(&mut engine);
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    for i in 0..100usize {
        let k = key(i);
        let v = value(i, i);
        engine.put(tx, "kv", &k, &v).unwrap();
        model.insert(k, v);
    }
    engine.commit_tx(tx).unwrap();

    let tx2 = engine.begin_tx(TxMode::Readwrite).unwrap();
    for i in (0..100usize).step_by(3) {
        let actual = engine.delete(tx2, "kv", &key(i)).unwrap();
        let expected = model.remove(&key(i)).is_some();
        assert_eq!(actual, expected);
    }
    engine.commit_tx(tx2).unwrap();
    assert_engine_matches_model(&mut engine, &model);
}

#[test]
fn snapshot_roundtrip_random_state() {
    let (_bundle, mut engine) = common::open_memory_engine("model-snapshot-source");
    let mut model = seed_store(&mut engine);
    let mut rng = Rng::new(0x5a9a_5eed);

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    for op in 0..200usize {
        let k = key(rng.next_usize(120));
        let v = value(op, rng.next_usize(1_000_000));
        engine.put(tx, "kv", &k, &v).unwrap();
        model.insert(k, v);
    }
    engine.commit_tx(tx).unwrap();

    let snapshot = engine.export_snapshot().unwrap();
    let (_target_bundle, mut target) = common::open_memory_engine("model-snapshot-target");
    target.import_snapshot(&snapshot).unwrap();
    assert_engine_matches_model(&mut target, &model);
}

#[test]
fn recovery_matches_expected_model() {
    let (bundle, mut engine) = common::open_memory_engine("model-recovery");
    let mut model = seed_store(&mut engine);
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    for i in 0..80usize {
        let k = key(i);
        let v = value(i, i);
        engine.put(tx, "kv", &k, &v).unwrap();
        model.insert(k, v);
    }
    engine.commit_tx(tx).unwrap();

    engine.set_failpoint(Some(Failpoint::AfterWalFlush));
    let tx2 = engine.begin_tx(TxMode::Readwrite).unwrap();
    for i in 20..60usize {
        engine.delete(tx2, "kv", &key(i)).unwrap();
        model.remove(&key(i));
    }
    for i in 100..140usize {
        let k = key(i);
        let v = value(i, i * 3);
        engine.put(tx2, "kv", &k, &v).unwrap();
        model.insert(k, v);
    }
    let err = engine.commit_tx(tx2).unwrap_err();
    assert_eq!(err.code(), "InjectedFailureError");
    drop(engine);

    let mut reopened = common::reopen_memory_engine("model-recovery", &bundle);
    assert_engine_matches_model(&mut reopened, &model);
}
