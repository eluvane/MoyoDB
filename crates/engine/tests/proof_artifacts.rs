mod common;

use moyodb_engine::bytes::{MAX_KEY_BYTES, MAX_STORE_NAME_BYTES, MAX_VALUE_BYTES};
use moyodb_engine::engine::{Failpoint, TxMode};
use moyodb_engine::layout::{
    INLINE_VALUE_LIMIT, MAIN_FILE_KIND, MANIFEST_FILE_KIND, PAGE_SIZE, SUPERBLOCK_SLOT_SIZE,
    WAL_FILE_KIND,
};
use moyodb_engine::page::encode_leaf_page;
use moyodb_engine::wal::{append_commit_record, append_page_image_record, CommitRecord};
use serde_json::Value;
use std::collections::BTreeMap;

#[test]
fn derived_constants_match_runtime_values() {
    let value = common::read_artifact("derived_constants.json");
    assert_eq!(value["format_version"].as_u64().unwrap(), 1);
    assert_eq!(value["superblock_magic"].as_str().unwrap(), "STKDB001");
    assert_eq!(value["wal_magic"].as_str().unwrap(), "WAL1");
    assert_eq!(value["page_magic"].as_str().unwrap(), "PAG1");
    assert_eq!(value["page_size"].as_u64().unwrap(), PAGE_SIZE as u64);
    assert_eq!(
        value["superblock_slot_size"].as_u64().unwrap(),
        SUPERBLOCK_SLOT_SIZE as u64
    );
    assert_eq!(
        value["inline_value_limit"].as_u64().unwrap(),
        INLINE_VALUE_LIMIT as u64
    );
    assert_eq!(
        value["file_kinds"]["manifest"].as_u64().unwrap(),
        MANIFEST_FILE_KIND as u64
    );
    assert_eq!(
        value["file_kinds"]["main"].as_u64().unwrap(),
        MAIN_FILE_KIND as u64
    );
    assert_eq!(
        value["file_kinds"]["wal"].as_u64().unwrap(),
        WAL_FILE_KIND as u64
    );
    assert_eq!(
        value["limits"]["store_name_bytes"].as_u64().unwrap(),
        MAX_STORE_NAME_BYTES as u64
    );
    assert_eq!(
        value["limits"]["key_bytes"].as_u64().unwrap(),
        MAX_KEY_BYTES as u64
    );
    assert_eq!(
        value["limits"]["value_bytes"].as_u64().unwrap(),
        MAX_VALUE_BYTES as u64
    );
    let failpoints: Vec<&str> = value["failpoints"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        failpoints,
        vec![
            "after_wal_flush",
            "after_main_flush",
            "before_superblock_flush"
        ]
    );
}

#[test]
fn btree_scenarios_conform() {
    let value = common::read_artifact("btree_traces.json");
    let scenarios = value["scenarios"].as_array().unwrap();
    assert!(scenarios.len() >= 10);

    for scenario in scenarios {
        let (_bundle, mut engine) = common::open_memory_engine(scenario["name"].as_str().unwrap());
        let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
        for op in scenario["operations"].as_array().unwrap() {
            match op["op"].as_str().unwrap() {
                "createStore" => engine
                    .create_store(tx, op["store"].as_str().unwrap())
                    .unwrap(),
                "put" => engine
                    .put(
                        tx,
                        op["store"].as_str().unwrap(),
                        &common::from_hex(op["key_hex"].as_str().unwrap()),
                        &common::from_hex(op["value_hex"].as_str().unwrap()),
                    )
                    .unwrap(),
                "delete" => {
                    let _ = engine
                        .delete(
                            tx,
                            op["store"].as_str().unwrap(),
                            &common::from_hex(op["key_hex"].as_str().unwrap()),
                        )
                        .unwrap();
                }
                other => panic!("unknown op {other}"),
            }
        }
        engine.commit_tx(tx).unwrap();

        let ro = engine.begin_tx(TxMode::Readonly).unwrap();
        let rows = if scenario.get("scan").is_some() {
            let scan = scenario["scan"].as_object().unwrap();
            engine
                .scan(
                    ro,
                    scan["store"].as_str().unwrap(),
                    &moyodb_engine::engine::ScanRange {
                        gte: scan
                            .get("gte_hex")
                            .and_then(|v| v.as_str())
                            .map(common::from_hex),
                        gt: scan
                            .get("gt_hex")
                            .and_then(|v| v.as_str())
                            .map(common::from_hex),
                        lt: scan
                            .get("lt_hex")
                            .and_then(|v| v.as_str())
                            .map(common::from_hex),
                        lte: scan
                            .get("lte_hex")
                            .and_then(|v| v.as_str())
                            .map(common::from_hex),
                        reverse: scan
                            .get("reverse")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                        limit: scan
                            .get("limit")
                            .and_then(|v| v.as_u64())
                            .map(|v| v as usize),
                    },
                )
                .unwrap()
        } else {
            engine
                .scan(ro, "kv", &moyodb_engine::engine::ScanRange::default())
                .unwrap()
        };
        let got_keys: Vec<String> = rows.iter().map(|kv| common::hex(&kv.key)).collect();
        let expected_keys: Vec<String> = scenario["expected"]["keys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(got_keys, expected_keys, "scenario {}", scenario["name"]);

        if let Some(expected_values) = scenario["expected"].get("values") {
            let got_values: BTreeMap<String, String> = rows
                .iter()
                .map(|kv| (common::hex(&kv.key), common::hex(&kv.value)))
                .collect();
            assert_eq!(
                got_values,
                json_hex_map(expected_values),
                "scenario {}",
                scenario["name"]
            );
        }
        engine.rollback_tx(ro).unwrap();
    }
}

#[test]
fn wal_recovery_scenarios_conform() {
    let value = common::read_artifact("wal_recovery_traces.json");
    let scenarios = value["scenarios"].as_array().unwrap();
    assert!(scenarios.len() >= 6);

    for scenario in scenarios {
        let name = scenario["name"].as_str().unwrap();
        let expect_after = scenario["expect_after"].as_bool().unwrap();

        match scenario["failpoint"].as_str() {
            Some("manual_incomplete_tail") => {
                let (bundle, mut engine) = common::open_memory_engine(name);
                let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
                engine.create_store(tx, "kv").unwrap();
                engine.put(tx, "kv", b"before", b"ok").unwrap();
                engine.commit_tx(tx).unwrap();
                drop(engine);

                let mut wal = bundle.wal.clone();
                let offset = wal.len().unwrap();
                wal.write_at(offset, &[1, 2, 3, 4, 5, 6]).unwrap();
                wal.flush().unwrap();

                let mut reopened = common::reopen_memory_engine(name, &bundle);
                let ro = reopened.begin_tx(TxMode::Readonly).unwrap();
                let after = reopened.get(ro, "kv", b"after").unwrap();
                reopened.rollback_tx(ro).unwrap();
                assert_eq!(after.is_some(), expect_after, "scenario {name}");
            }
            Some("manual_page_count_mismatch") => {
                let (bundle, mut engine) = common::open_memory_engine(name);
                let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
                engine.create_store(tx, "kv").unwrap();
                engine.put(tx, "kv", b"before", b"ok").unwrap();
                engine.commit_tx(tx).unwrap();
                drop(engine);

                let mut wal = bundle.wal.clone();
                let mut offset = wal.len().unwrap();
                let page = encode_leaf_page(99, 0, 0, &[]).unwrap();
                append_page_image_record(&mut wal, &mut offset, 77, 99, &page).unwrap();
                append_commit_record(
                    &mut wal,
                    &mut offset,
                    CommitRecord {
                        txid: 77,
                        new_catalog_root_page_id: 99,
                        new_next_page_id: 100,
                        changed_page_count: 2,
                    },
                )
                .unwrap();
                wal.flush().unwrap();

                let mut reopened = common::reopen_memory_engine(name, &bundle);
                let ro = reopened.begin_tx(TxMode::Readonly).unwrap();
                let after = reopened.get(ro, "kv", b"after").unwrap();
                reopened.rollback_tx(ro).unwrap();
                assert_eq!(after.is_some(), expect_after, "scenario {name}");
            }
            failpoint => {
                let (bundle, mut engine) = common::open_memory_engine(name);
                let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
                engine.create_store(tx, "kv").unwrap();
                engine.put(tx, "kv", b"before", b"ok").unwrap();
                engine.commit_tx(tx).unwrap();

                let tx2 = engine.begin_tx(TxMode::Readwrite).unwrap();
                engine.put(tx2, "kv", b"after", b"yes").unwrap();
                match failpoint {
                    Some("after_wal_flush") => engine.set_failpoint(Some(Failpoint::AfterWalFlush)),
                    Some("after_main_flush") => {
                        engine.set_failpoint(Some(Failpoint::AfterMainFlush))
                    }
                    Some("before_superblock_flush") => {
                        engine.set_failpoint(Some(Failpoint::BeforeSuperblockFlush))
                    }
                    None => {
                        if name == "crash_before_wal_flush" {
                            drop(engine);

                            let mut reopened = common::reopen_memory_engine(name, &bundle);
                            let ro = reopened.begin_tx(TxMode::Readonly).unwrap();
                            let after = reopened.get(ro, "kv", b"after").unwrap();
                            reopened.rollback_tx(ro).unwrap();
                            assert_eq!(after.is_some(), expect_after, "scenario {name}");
                            continue;
                        }
                    }
                    other => panic!("unexpected failpoint {other:?}"),
                }
                let _ = engine.commit_tx(tx2);
                drop(engine);

                let mut reopened = common::reopen_memory_engine(name, &bundle);
                let ro = reopened.begin_tx(TxMode::Readonly).unwrap();
                let after = reopened.get(ro, "kv", b"after").unwrap();
                reopened.rollback_tx(ro).unwrap();
                assert_eq!(after.is_some(), expect_after, "scenario {name}");
            }
        }
    }
}

#[test]
fn txn_serialization_scenarios_conform() {
    let value = common::read_artifact("txn_serialization_traces.json");
    let scenarios = value["scenarios"].as_array().unwrap();
    assert!(scenarios.len() >= 3);

    let scenario_by_name: BTreeMap<String, &Value> = scenarios
        .iter()
        .map(|scenario| (scenario["name"].as_str().unwrap().to_string(), scenario))
        .collect();

    {
        let (_bundle, mut engine) = common::open_memory_engine("txn-serialization-sequential");
        let init = engine.begin_tx(TxMode::Readwrite).unwrap();
        engine.create_store(init, "kv").unwrap();
        engine.commit_tx(init).unwrap();

        let tx1 = engine.begin_tx(TxMode::Readwrite).unwrap();
        engine.put(tx1, "kv", b"a", b"1").unwrap();
        engine.commit_tx(tx1).unwrap();

        let tx2 = engine.begin_tx(TxMode::Readwrite).unwrap();
        engine.put(tx2, "kv", b"b", b"2").unwrap();
        engine.commit_tx(tx2).unwrap();

        let ro = engine.begin_tx(TxMode::Readonly).unwrap();
        let state = scan_hex_map(&mut engine, ro, "kv");
        engine.rollback_tx(ro).unwrap();
        assert_eq!(
            state,
            json_hex_map(&scenario_by_name["two_sequential_write_txs"]["expected"]),
        );
    }

    {
        let (_bundle, mut engine) = common::open_memory_engine("txn-serialization-snapshot");
        let init = engine.begin_tx(TxMode::Readwrite).unwrap();
        engine.create_store(init, "kv").unwrap();
        engine.commit_tx(init).unwrap();

        let tx1 = engine.begin_tx(TxMode::Readwrite).unwrap();
        engine.put(tx1, "kv", b"a", b"1").unwrap();
        engine.commit_tx(tx1).unwrap();

        let snapshot = engine.begin_tx(TxMode::Readonly).unwrap();
        let tx2 = engine.begin_tx(TxMode::Readwrite).unwrap();
        engine.put(tx2, "kv", b"b", b"2").unwrap();
        engine.commit_tx(tx2).unwrap();

        let snapshot_state = scan_hex_map(&mut engine, snapshot, "kv");
        engine.rollback_tx(snapshot).unwrap();
        let ro = engine.begin_tx(TxMode::Readonly).unwrap();
        let final_state = scan_hex_map(&mut engine, ro, "kv");
        engine.rollback_tx(ro).unwrap();

        let expected = &scenario_by_name["readonly_snapshot_before_second_commit"]["expected"];
        assert_eq!(
            snapshot_state,
            json_hex_map(&expected["snapshot_before_second"]),
        );
        assert_eq!(final_state, json_hex_map(&expected["final"]));
    }

    {
        let (_bundle, mut engine) = common::open_memory_engine("txn-serialization-overwrite");
        let init = engine.begin_tx(TxMode::Readwrite).unwrap();
        engine.create_store(init, "kv").unwrap();
        engine.commit_tx(init).unwrap();

        let tx1 = engine.begin_tx(TxMode::Readwrite).unwrap();
        engine.put(tx1, "kv", b"a", b"1").unwrap();
        engine.commit_tx(tx1).unwrap();

        let ro_before = engine.begin_tx(TxMode::Readonly).unwrap();
        let first_state = scan_hex_map(&mut engine, ro_before, "kv");
        engine.rollback_tx(ro_before).unwrap();

        let tx2 = engine.begin_tx(TxMode::Readwrite).unwrap();
        engine.put(tx2, "kv", b"a", b"2").unwrap();
        engine.commit_tx(tx2).unwrap();

        let ro_final = engine.begin_tx(TxMode::Readonly).unwrap();
        let final_state = scan_hex_map(&mut engine, ro_final, "kv");
        engine.rollback_tx(ro_final).unwrap();

        let expected = &scenario_by_name["later_write_overwrites_key"]["expected"];
        assert_eq!(first_state, json_hex_map(&expected["first"]));
        assert_eq!(final_state, json_hex_map(&expected["final"]));
    }
}

fn json_hex_map(value: &Value) -> BTreeMap<String, String> {
    value
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
        .collect()
}

fn scan_hex_map(
    engine: &mut moyodb_engine::engine::Engine<moyodb_engine::MemoryBackend>,
    tx_id: u64,
    store: &str,
) -> BTreeMap<String, String> {
    engine
        .scan(tx_id, store, &moyodb_engine::engine::ScanRange::default())
        .unwrap()
        .into_iter()
        .map(|pair| (common::hex(&pair.key), common::hex(&pair.value)))
        .collect()
}
