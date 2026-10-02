use moyodb_engine::btree::{apply_mutations, lookup, scan, Mutation, PageAllocator, RangeSpec};
use moyodb_engine::pager::Pager;
use moyodb_engine::storage::memory::MemoryBackend;
use moyodb_engine::Result;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fmt::Write as _;

type OwnedMutation = (Vec<u8>, Option<Vec<u8>>);

fn hex(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(result, "{byte:02x}").expect("writing to a String cannot fail");
    }
    result
}

fn optional_hex(bytes: Option<&[u8]>) -> Value {
    bytes.map_or(Value::Null, |bytes| json!(hex(bytes)))
}

fn long_key(index: u8) -> Vec<u8> {
    let mut key = vec![0x61; 900];
    key[0] = 0x80;
    key[1] = index;
    key
}

fn ranges(keys: &BTreeSet<Vec<u8>>) -> Vec<RangeSpec> {
    let mut ranges = vec![
        RangeSpec::default(),
        RangeSpec {
            reverse: true,
            ..RangeSpec::default()
        },
        RangeSpec {
            limit: Some(0),
            ..RangeSpec::default()
        },
        RangeSpec {
            limit: Some(1),
            ..RangeSpec::default()
        },
        RangeSpec {
            reverse: true,
            limit: Some(3),
            ..RangeSpec::default()
        },
    ];
    if let (Some(first), Some(last)) = (keys.first(), keys.last()) {
        ranges.push(RangeSpec {
            gte: Some(first.clone()),
            lte: Some(last.clone()),
            ..RangeSpec::default()
        });
        if first < last {
            ranges.push(RangeSpec {
                gt: Some(first.clone()),
                lt: Some(last.clone()),
                reverse: true,
                limit: Some(3),
                ..RangeSpec::default()
            });
        }
    }
    ranges
}

fn scenario(name: &str, batches: Vec<Vec<OwnedMutation>>, witness: bool) -> Result<Value> {
    let mut pager = Pager::new(MemoryBackend::new(), 128);
    let mut alloc = PageAllocator::new(1);
    let mut root = 0;
    let mut captured_ids = BTreeSet::new();
    let mut known_keys = BTreeSet::new();
    let mut previous_roots = Vec::new();
    let mut frames = Vec::new();
    for (index, mut batch) in batches.into_iter().enumerate() {
        batch.sort_by(|left, right| left.0.cmp(&right.0));
        assert!(batch.windows(2).all(|pair| pair[0].0 < pair[1].0));
        known_keys.extend(batch.iter().map(|entry| entry.0.clone()));
        let borrowed: Vec<Mutation<'_>> = batch
            .iter()
            .map(|(key, value)| (key.as_slice(), value.as_deref()))
            .collect();
        let built = apply_mutations(&mut pager, root, &borrowed, &mut alloc)?;
        root = built.root_page_id;
        for (id, image) in built.page_images {
            pager.write_page_image(id, &image)?;
            captured_ids.insert(id);
        }
        pager.flush()?;
        pager = Pager::new(pager.into_inner(), 128);
        // Read the actual backend images, including all unchanged and retained pages.
        // No Rust decoder supplies Lean's expected contents or structural facts.
        let mut pages = Vec::new();
        for id in &captured_ids {
            let bytes = pager.read_page(*id)?;
            pages.push(json!({ "id": id, "bytes_hex": hex(&bytes) }));
        }
        if name == "long-cow" {
            let height = pager.read_page(root)?[17];
            let wanted = match index {
                0 => 1,
                1 | 2 => 2,
                _ => 0,
            };
            assert_eq!(
                height, wanted,
                "fixture must reach the advertised split/collapse"
            );
        }
        let mut probes = known_keys.clone();
        probes.extend([vec![], vec![0], vec![0xff; 3]]);
        let mut lookups = Vec::new();
        for key in probes {
            let value = lookup(&mut pager, root, &key)?;
            lookups
                .push(json!({ "key_hex": hex(&key), "value_hex": optional_hex(value.as_deref()) }));
        }
        let mut scans = Vec::new();
        for range in ranges(&known_keys) {
            let rows = scan(&mut pager, root, &range)?;
            scans.push(json!({
                "gt": optional_hex(range.gt.as_deref()),
                "gte": optional_hex(range.gte.as_deref()),
                "lt": optional_hex(range.lt.as_deref()),
                "lte": optional_hex(range.lte.as_deref()),
                "reverse": range.reverse,
                "limit": range.limit,
                "rows": rows.iter().map(|row| vec![hex(&row.key), hex(&row.value)]).collect::<Vec<_>>(),
            }));
        }
        if witness {
            // The separator mutant preserves the full scan: the negative witness
            // must concern physical routing, not a compiler failure or lost inputs.
            let actual = scan(&mut pager, root, &RangeSpec::default())?;
            let input: Vec<_> = batch
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value.clone().expect("witness consists only of inserts"),
                    )
                })
                .collect();
            assert_eq!(
                actual
                    .iter()
                    .map(|row| (row.key.clone(), row.value.clone()))
                    .collect::<Vec<_>>(),
                input
            );
        }
        frames.push(json!({
            "id": format!("{name}/{index}"),
            "root": root,
            "pages": pages,
            "mutations": batch.iter().map(|(key, value)| json!({
                "key_hex": hex(key), "value_hex": optional_hex(value.as_deref())
            })).collect::<Vec<_>>(),
            "lookups": lookups,
            "scans": scans,
            "retained_roots": previous_roots.iter().enumerate().map(|(frame, old_root)| {
                json!({ "frame": frame, "root": old_root })
            }).collect::<Vec<_>>(),
        }));
        previous_roots.push(root);
    }
    Ok(json!({ "id": name, "frames": frames }))
}

fn prefix_batches() -> Vec<Vec<OwnedMutation>> {
    let keys = [
        vec![0],
        vec![0, 0],
        vec![0, 255],
        vec![1],
        vec![255],
        vec![255, 0],
    ];
    vec![
        keys.iter()
            .enumerate()
            .map(|(i, key)| (key.clone(), Some(vec![i as u8; i])))
            .collect(),
        vec![
            (keys[0].clone(), None),
            (keys[1].clone(), Some(vec![9, 0, 255])),
        ],
        vec![(keys[5].clone(), None), (keys[2].clone(), None)],
        vec![
            (keys[0].clone(), Some(vec![])),
            (keys[4].clone(), Some(vec![1])),
        ],
    ]
}

fn cow_batches() -> Vec<Vec<OwnedMutation>> {
    vec![
        (0u8..8).map(|i| (long_key(i), Some(vec![i; 64]))).collect(),
        (8u8..36)
            .map(|i| (long_key(i), Some(vec![i; 64])))
            .collect(),
        vec![
            (long_key(0), Some(vec![0xff; 63])),
            (long_key(4), None),
            (long_key(8), None),
            (long_key(35), Some(vec![0x55; 91])),
        ],
        (0u8..35).map(|i| (long_key(i), None)).collect(),
        vec![(long_key(35), None)],
        vec![],
    ]
}

#[test]
fn export_real_btree_pages_for_lean() -> Result<()> {
    let witness = std::env::var("MOYO_BTREE_CORPUS_MODE").is_ok_and(|mode| mode == "witness");
    let scenarios = if witness {
        vec![scenario(
            "separator-witness",
            vec![(0u8..8).map(|i| (long_key(i), Some(vec![i; 64]))).collect()],
            true,
        )?]
    } else {
        vec![
            scenario("long-cow", cow_batches(), false)?,
            scenario("prefix-keys", prefix_batches(), false)?,
            scenario(
                "empty-root",
                vec![
                    vec![],
                    vec![(vec![0], Some(vec![0])), (vec![1], Some(vec![]))],
                    vec![],
                    vec![(vec![0], None), (vec![1], None)],
                    vec![],
                ],
                false,
            )?,
        ]
    };
    let corpus = json!({
        "schema_version": 1,
        "mode": if witness { "witness" } else { "full" },
        "scenarios": scenarios
    });
    if let Some(path) = std::env::var_os("MOYO_BTREE_CORPUS_OUT") {
        let bytes = serde_json::to_vec(&corpus).expect("fixture JSON must serialize");
        assert!(bytes.len() <= 32 * 1024 * 1024, "bounded Lean corpus");
        std::fs::write(path, bytes).expect("write Lean corpus");
    }
    Ok(())
}
