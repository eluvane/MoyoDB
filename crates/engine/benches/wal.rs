use criterion::{black_box, criterion_group, criterion_main, Criterion};
use moyodb_engine::layout::PAGE_SIZE;
use moyodb_engine::storage::memory::MemoryBackend;
use moyodb_engine::wal::{append_commit_record, append_page_image_record, CommitRecord};

fn bench_wal(c: &mut Criterion) {
    c.bench_function("wal_append_100_pages", |b| {
        b.iter(|| {
            let mut wal = MemoryBackend::new();
            let mut offset = 0u64;
            for page_id in 1..=100 {
                let page = vec![page_id as u8; PAGE_SIZE];
                append_page_image_record(&mut wal, &mut offset, 1, page_id, &page).unwrap();
            }
            append_commit_record(
                &mut wal,
                &mut offset,
                CommitRecord {
                    txid: 1,
                    new_catalog_root_page_id: 1,
                    new_next_page_id: 101,
                    changed_page_count: 100,
                },
            )
            .unwrap();
            wal.flush().unwrap();
            black_box(offset);
        });
    });
}

criterion_group!(benches, bench_wal);
criterion_main!(benches);
