use criterion::{black_box, criterion_group, criterion_main, Criterion};
use moyodb_engine::btree::{build_tree, lookup, scan, RangeSpec};
use moyodb_engine::pager::Pager;
use moyodb_engine::storage::memory::MemoryBackend;
use std::time::Duration;

const ONE_MILLION: u32 = 1_000_000;
const TEN_MILLION: u32 = 10_000_000;

fn bench_btree(c: &mut Criterion) {
    c.bench_function("btree_insert_build_1000", |b| {
        b.iter(|| {
            let entries: Vec<(Vec<u8>, Vec<u8>)> = (0u32..1000)
                .map(|i| (i.to_be_bytes().to_vec(), vec![42u8; 16]))
                .collect();
            let mut next_page_id = 1;
            black_box(build_tree(&entries, &mut next_page_id).unwrap());
        });
    });

    c.bench_function("btree_point_get_1000", |b| {
        let entries: Vec<(Vec<u8>, Vec<u8>)> = (0u32..1000)
            .map(|i| (i.to_be_bytes().to_vec(), vec![42u8; 16]))
            .collect();
        let mut next_page_id = 1;
        let tree = build_tree(&entries, &mut next_page_id).unwrap();
        let mut pager = Pager::new(MemoryBackend::new(), 64);
        for (page_id, bytes) in tree.page_images {
            pager.write_page_image(page_id, &bytes).unwrap();
        }
        pager.flush().unwrap();
        b.iter(|| {
            let key = black_box(512u32.to_be_bytes().to_vec());
            black_box(lookup(&mut pager, tree.root_page_id, &key).unwrap());
        });
    });

    c.bench_function("btree_range_scan_1000", |b| {
        let entries: Vec<(Vec<u8>, Vec<u8>)> = (0u32..1000)
            .map(|i| (i.to_be_bytes().to_vec(), vec![42u8; 16]))
            .collect();
        let mut next_page_id = 1;
        let tree = build_tree(&entries, &mut next_page_id).unwrap();
        let mut pager = Pager::new(MemoryBackend::new(), 64);
        for (page_id, bytes) in tree.page_images {
            pager.write_page_image(page_id, &bytes).unwrap();
        }
        pager.flush().unwrap();
        b.iter(|| {
            black_box(
                scan(
                    &mut pager,
                    tree.root_page_id,
                    &RangeSpec {
                        gte: Some(100u32.to_be_bytes().to_vec()),
                        lt: Some(200u32.to_be_bytes().to_vec()),
                        ..Default::default()
                    },
                )
                .unwrap(),
            )
        });
    });

    let mut million = c.benchmark_group("btree_1m");
    million.sample_size(10);
    million.measurement_time(Duration::from_secs(20));

    million.bench_function("btree_insert_build_1m", |b| {
        let million_entries = build_entries(ONE_MILLION);
        b.iter(|| {
            let mut next_page_id = 1;
            black_box(build_tree(black_box(&million_entries), &mut next_page_id).unwrap());
        });
    });

    million.bench_function("btree_point_get_1m_random_hot", |b| {
        let million_entries = build_entries(ONE_MILLION);
        let mut next_page_id = 1;
        let tree = build_tree(&million_entries, &mut next_page_id).unwrap();
        let mut pager = Pager::new(MemoryBackend::new(), 16_384);
        for (page_id, bytes) in tree.page_images {
            pager.write_page_image(page_id, &bytes).unwrap();
        }
        pager.flush().unwrap();

        let keys: Vec<[u8; 4]> = (0u32..65_536)
            .scan(0x1db5_0000u32, |state, _| {
                *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                Some((*state % ONE_MILLION).to_be_bytes())
            })
            .collect();
        let mut index = 0usize;

        b.iter(|| {
            let key = black_box(keys[index & (keys.len() - 1)]);
            index = index.wrapping_add(1);
            black_box(lookup(&mut pager, tree.root_page_id, &key).unwrap());
        });
    });

    million.finish();

    let mut ten_million = c.benchmark_group("btree_10m");
    ten_million.sample_size(10);
    ten_million.measurement_time(Duration::from_secs(30));

    ten_million.bench_function("btree_insert_build_10m", |b| {
        let entries = build_entries(TEN_MILLION);
        b.iter(|| {
            let mut next_page_id = 1;
            black_box(build_tree(black_box(&entries), &mut next_page_id).unwrap());
        });
    });

    ten_million.bench_function("btree_point_get_10m_random_hot", |b| {
        let entries = build_entries(TEN_MILLION);
        let mut next_page_id = 1;
        let tree = build_tree(&entries, &mut next_page_id).unwrap();
        let mut pager = Pager::new(MemoryBackend::new(), 262_144);
        for (page_id, bytes) in tree.page_images {
            pager.write_page_image(page_id, &bytes).unwrap();
        }
        pager.flush().unwrap();

        let keys = random_keys(TEN_MILLION);
        let mut index = 0usize;

        b.iter(|| {
            let key = black_box(keys[index & (keys.len() - 1)]);
            index = index.wrapping_add(1);
            black_box(lookup(&mut pager, tree.root_page_id, &key).unwrap());
        });
    });

    ten_million.finish();
}

criterion_group!(benches, bench_btree);
criterion_main!(benches);

fn build_entries(count: u32) -> Vec<(Vec<u8>, Vec<u8>)> {
    (0u32..count)
        .map(|i| (i.to_be_bytes().to_vec(), vec![42u8; 16]))
        .collect()
}

fn random_keys(exclusive_max: u32) -> Vec<[u8; 4]> {
    (0u32..65_536)
        .scan(0x1db5_0000u32, |state, _| {
            *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            Some((*state % exclusive_max).to_be_bytes())
        })
        .collect()
}
