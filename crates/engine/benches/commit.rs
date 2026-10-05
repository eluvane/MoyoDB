use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use moyodb_engine::catalog::ChangeFeedPolicy;
use moyodb_engine::engine::{Engine, OpenConfig, TxMode};
use moyodb_engine::storage::memory::MemoryBundle;

fn seed_engine(entry_count: u32, value_len: usize) -> Engine<moyodb_engine::MemoryBackend> {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open("bench-commit", bundle.files(), OpenConfig::default()).unwrap();
    let value = vec![0x33; value_len];

    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine.create_store(tx, "kv").unwrap();
    for i in 0u32..entry_count {
        engine.put(tx, "kv", &i.to_be_bytes(), &value).unwrap();
    }
    engine.commit_tx(tx).unwrap();
    engine
}

fn bench_commit(c: &mut Criterion) {
    c.bench_function("commit_hot_update_4096_values_256b", |b| {
        b.iter_batched(
            || seed_engine(4096, 256),
            |mut engine| {
                let before = engine.stats().unwrap().next_page_id;
                let mut updated = vec![0x77; 256];
                updated[0] = 0x99;
                let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
                engine
                    .put(tx, "kv", &2048u32.to_be_bytes(), &updated)
                    .unwrap();
                engine.commit_tx(tx).unwrap();
                let after = engine.stats().unwrap().next_page_id;
                black_box(after - before);
            },
            BatchSize::SmallInput,
        );
    });

    c.bench_function("commit_hot_delete_4096_values_256b", |b| {
        b.iter_batched(
            || seed_engine(4096, 256),
            |mut engine| {
                let before = engine.stats().unwrap().next_page_id;
                let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
                black_box(engine.delete(tx, "kv", &2048u32.to_be_bytes()).unwrap());
                engine.commit_tx(tx).unwrap();
                let after = engine.stats().unwrap().next_page_id;
                black_box(after - before);
            },
            BatchSize::SmallInput,
        );
    });
}

fn seed_catalog(store_count: usize, feed: bool) -> Engine<moyodb_engine::MemoryBackend> {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open(
        "bench-catalog",
        bundle.files(),
        OpenConfig {
            checkpoint_wal_bytes: u64::MAX,
            checkpoint_dirty_pages: usize::MAX,
            ..OpenConfig::default()
        },
    )
    .unwrap();
    let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
    engine
        .set_change_feed_policy(
            tx,
            ChangeFeedPolicy {
                enabled: feed,
                ..ChangeFeedPolicy::default()
            },
        )
        .unwrap();
    for index in 0..store_count {
        engine
            .create_store(tx, &format!("store-{index:08}"))
            .unwrap();
    }
    engine.put(tx, "store-00000000", b"key", b"before").unwrap();
    engine.commit_tx(tx).unwrap();
    engine.checkpoint().unwrap();
    engine
}

fn bench_catalog_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("catalog_scaling");
    group.sample_size(20);
    for count in [1, 128, 1024, 8192] {
        let mut reader_engine = seed_catalog(count, false);
        group.bench_function(BenchmarkId::new("readonly_point", count), |b| {
            b.iter(|| {
                let tx = reader_engine.begin_tx(TxMode::Readonly).unwrap();
                black_box(reader_engine.get(tx, "store-00000000", b"key").unwrap());
                reader_engine.rollback_tx(tx).unwrap();
            });
        });
        for (name, feed, reader) in [
            ("one_store_commit", false, false),
            ("one_store_commit_feed", true, false),
            ("one_store_commit_live_reader", false, true),
        ] {
            group.bench_function(BenchmarkId::new(name, count), |b| {
                // Exclude fixture setup and teardown from timing. PerIteration
                // limits fixture memory for the largest catalog.
                b.iter_batched_ref(
                    || {
                        let mut engine = seed_catalog(count, feed);
                        if reader {
                            engine.begin_tx(TxMode::Readonly).unwrap();
                        }
                        engine
                    },
                    |engine| {
                        let tx = engine.begin_tx(TxMode::Readwrite).unwrap();
                        engine.put(tx, "store-00000000", b"key", b"after").unwrap();
                        black_box(engine.commit_tx(tx).unwrap());
                    },
                    BatchSize::PerIteration,
                );
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench_commit, bench_catalog_scaling);
criterion_main!(benches);
