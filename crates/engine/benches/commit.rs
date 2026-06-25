use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
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

criterion_group!(benches, bench_commit);
criterion_main!(benches);
