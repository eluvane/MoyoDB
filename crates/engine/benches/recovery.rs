use criterion::{black_box, criterion_group, criterion_main, Criterion};
use moyodb_engine::layout::PAGE_SIZE;
use moyodb_engine::pager::Pager;
use moyodb_engine::storage::memory::MemoryBackend;
use moyodb_engine::wal::{
    append_commit_record, append_page_image_record, replay_wal_transactions, scan_wal, CommitRecord,
};

fn bench_recovery(c: &mut Criterion) {
    c.bench_function("recovery_replay_100_pages", |b| {
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

        b.iter(|| {
            let txs = scan_wal(&wal).unwrap();
            let mut pager = Pager::new(MemoryBackend::new(), 64);
            replay_wal_transactions(&mut pager, &txs).unwrap();
            black_box(pager.read_page(100).unwrap());
        });
    });
}

criterion_group!(benches, bench_recovery);
criterion_main!(benches);
