use moyodb_engine::bytes::{MAX_KEY_BYTES, MAX_VALUE_BYTES};
use moyodb_engine::checksum::crc32;
use moyodb_engine::layout::{page_offset, ValueKind, PAGE_SIZE};
use moyodb_engine::page::{decode_page, encode_leaf_page, encode_overflow_page};
use moyodb_engine::payload::{decode_payload_descriptor, PAYLOAD_HEADER_SIZE};
use moyodb_engine::storage::backend::{FileBackend, FileSet};
use moyodb_engine::value::{StoreCompression, COMPRESSION_VALUE_HEADER_SIZE};
use moyodb_engine::{
    Engine, EngineError, MemoryBackend, MemoryBundle, OpenConfig, Result, ScanRange, TxMode,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const LARGE_VALUE: usize = 1024 * 1024;
type ReadTrace = Arc<Mutex<Vec<(u64, usize)>>>;

thread_local! {
    static LARGE_ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
}

struct WorkAllocator;

fn record_allocation(size: usize) {
    if size >= LARGE_VALUE {
        let _ = LARGE_ALLOCATIONS.try_with(|counter| {
            if let Some(count) = counter.get() {
                counter.set(Some(count + 1));
            }
        });
    }
}

// SAFETY: allocation ownership and all arguments pass unchanged to System.
// The thread-local counter does not access the allocated memory.
unsafe impl GlobalAlloc for WorkAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation(layout.size());
        // SAFETY: GlobalAlloc supplies a valid layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record_allocation(layout.size());
        // SAFETY: GlobalAlloc supplies a valid layout.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: the allocation and its layout belong to System.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record_allocation(size);
        // SAFETY: the allocation belongs to System and keeps its original layout.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: WorkAllocator = WorkAllocator;

fn measure_large_allocations<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    LARGE_ALLOCATIONS.with(|counter| counter.set(Some(0)));
    let result = operation();
    let count = LARGE_ALLOCATIONS.with(|counter| counter.replace(None).unwrap_or_default());
    (result, count)
}

fn reference_packet(values: &[Option<Vec<u8>>]) -> Vec<u8> {
    let size = 4 + values.len() * 4 + values.iter().flatten().map(Vec::len).sum::<usize>();
    let mut packet = Vec::with_capacity(size);
    packet.extend_from_slice(&(values.len() as u32).to_le_bytes());
    for value in values {
        packet.extend_from_slice(
            &value
                .as_ref()
                .map_or(u32::MAX, |v| v.len() as u32)
                .to_le_bytes(),
        );
    }
    for value in values.iter().flatten() {
        packet.extend_from_slice(value);
    }
    packet
}

fn unpack_rows(packet: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let count = u32::from_le_bytes(packet[..4].try_into().expect("row count")) as usize;
    let mut offset = 4;
    let mut rows = Vec::new();
    for _ in 0..count {
        let key_len =
            u32::from_le_bytes(packet[offset..offset + 4].try_into().expect("key length")) as usize;
        let value_len = u32::from_le_bytes(
            packet[offset + 4..offset + 8]
                .try_into()
                .expect("value length"),
        ) as usize;
        offset += 8;
        let key = packet[offset..offset + key_len].to_vec();
        offset += key_len;
        let value = packet[offset..offset + value_len].to_vec();
        offset += value_len;
        rows.push((key, value));
    }
    assert_eq!(offset, packet.len());
    rows
}

#[test]
fn packed_get_preserves_wire_and_staged_visibility() -> Result<()> {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open("packed-visibility", bundle.files(), OpenConfig::default())?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(tx, "s")?;
    engine.put(tx, "s", b"b", &[0, 255, 1])?;
    engine.put(tx, "s", b"empty", &[])?;
    engine.put_with_ttl(tx, "s", b"expired", b"gone", Some(0))?;
    engine.commit_tx(tx)?;
    let keys = [b"b".as_slice(), b"missing", b"empty", b"expired", b"b"];
    for mode in [TxMode::Readonly, TxMode::Readwrite] {
        let tx = engine.begin_tx(mode)?;
        if mode == TxMode::Readwrite {
            engine.put(tx, "s", b"b", b"changed")?;
            engine.delete(tx, "s", b"empty")?;
        }
        let expected = reference_packet(&engine.get_many(tx, "s", &keys)?);
        assert_eq!(engine.get_many_packed(tx, "s", &keys)?, expected);
        assert_eq!(
            engine.get_many_packed::<&[u8]>(tx, "s", &[])?,
            0u32.to_le_bytes()
        );
        engine.rollback_tx(tx)?;
    }
    Ok(())
}

#[test]
fn packed_get_removes_materialized_megabyte_values_and_duplicate_clones() -> Result<()> {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open("packed-allocation", bundle.files(), OpenConfig::default())?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(tx, "s")?;
    engine.put(tx, "s", b"k", &vec![0x73; LARGE_VALUE])?;
    engine.commit_tx(tx)?;
    let tx = engine.begin_tx(TxMode::Readonly)?;
    let keys = [b"k".as_slice(), b"k"];
    let (legacy, old_allocations) = measure_large_allocations(|| {
        engine
            .get_many(tx, "s", &keys)
            .map(|values| reference_packet(&values))
    });
    let legacy = legacy?;
    let (packed, new_allocations) =
        measure_large_allocations(|| engine.get_many_packed(tx, "s", &keys));
    assert_eq!(packed?, legacy);
    println!("packed_get 2x1MiB old_large_allocations={old_allocations} new_large_allocations={new_allocations}");
    assert!(old_allocations >= 3);
    assert_eq!(
        new_allocations, 1,
        "only the owned final packet may allocate at least 1 MiB"
    );
    engine.rollback_tx(tx)?;
    Ok(())
}

#[test]
fn bounded_scan_merges_staged_rows_in_both_directions() -> Result<()> {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open("packed-staged-scan", bundle.files(), OpenConfig::default())?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(tx, "s")?;
    for index in 0..20u8 {
        engine.put(tx, "s", &[index], &[index; 7])?;
    }
    engine.commit_tx(tx)?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.delete(tx, "s", &[4])?;
    engine.put(tx, "s", &[6], b"replacement")?;
    engine.put(tx, "s", &[21], b"inserted")?;
    engine.put_with_ttl(tx, "s", &[9], b"expired", Some(0))?;
    for reverse in [false, true] {
        let mut range = ScanRange {
            reverse,
            ..ScanRange::default()
        };
        let expected: Vec<_> = engine
            .scan(tx, "s", &range)?
            .into_iter()
            .map(|row| (row.key, row.value))
            .collect();
        let mut actual = Vec::new();
        loop {
            let page = engine.scan_packed_page(tx, "s", &range, 3, 80, false)?;
            assert!(page.row_count <= 3 && page.packet.len() <= 80);
            actual.extend(unpack_rows(&page.packet));
            if page.exhausted {
                break;
            }
            let key = page.last_key.expect("nonempty continuing page");
            if reverse {
                range.lt = Some(key);
            } else {
                range.gt = Some(key);
            }
        }
        assert_eq!(actual, expected);
    }
    assert_eq!(
        engine
            .scan_packed_page(tx, "s", &ScanRange::default(), 1, 19, false)
            .unwrap_err(),
        EngineError::ValueTooLarge(20)
    );
    engine.rollback_tx(tx)?;
    Ok(())
}

#[test]
fn compressed_scan_budget_uses_expanded_lengths_before_selecting_rows() -> Result<()> {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open(
        "packed-compressed-budget",
        bundle.files(),
        OpenConfig::default(),
    )?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store_with_compression(tx, "s", StoreCompression::Gzip)?;
    let mut encoded = b"BDBZVAL1".to_vec();
    encoded.extend_from_slice(&[1, 1]);
    encoded.extend_from_slice(&1000u32.to_le_bytes());
    encoded.extend_from_slice(&0u32.to_le_bytes());
    encoded.extend_from_slice(&[0x55; 100]);
    for key in [b"a", b"b", b"c"] {
        engine.put(tx, "s", key, &encoded)?;
    }
    for committed in [false, true] {
        let read_tx = if committed {
            engine.commit_tx(tx)?;
            engine.begin_tx(TxMode::Readonly)?
        } else {
            tx
        };
        let page = engine.scan_packed_page(read_tx, "s", &ScanRange::default(), 10, 2022, false)?;
        assert_eq!(page.row_count, 2);
        assert!(!page.exhausted);
        assert_eq!(
            unpack_rows(&page.packet),
            vec![
                (b"a".to_vec(), encoded.clone()),
                (b"b".to_vec(), encoded.clone())
            ]
        );
        assert_eq!(
            engine
                .scan_packed_page(read_tx, "s", &ScanRange::default(), 10, 1012, false)
                .unwrap_err(),
            EngineError::ValueTooLarge(1013)
        );
        if committed {
            engine.rollback_tx(read_tx)?;
        }
    }
    Ok(())
}

#[test]
fn maximum_compression_fallback_fits_the_full_wire_budget() -> Result<()> {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open(
        "packed-maximum-compression",
        bundle.files(),
        OpenConfig::default(),
    )?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store_with_compression(tx, "s", StoreCompression::Gzip)?;
    let key = vec![0x4b; MAX_KEY_BYTES];
    let payload = vec![0x53; MAX_VALUE_BYTES];
    let mut encoded = b"BDBZVAL1".to_vec();
    encoded.extend_from_slice(&[1, 0]);
    encoded.extend_from_slice(&(MAX_VALUE_BYTES as u32).to_le_bytes());
    encoded.extend_from_slice(&crc32(&payload).to_le_bytes());
    encoded.extend_from_slice(&payload);
    assert_eq!(
        encoded.len(),
        MAX_VALUE_BYTES + COMPRESSION_VALUE_HEADER_SIZE
    );
    engine.put(tx, "s", &key, &encoded)?;
    let byte_budget = MAX_VALUE_BYTES + COMPRESSION_VALUE_HEADER_SIZE + MAX_KEY_BYTES + 12;
    for committed in [false, true] {
        let read_tx = if committed {
            engine.commit_tx(tx)?;
            engine.begin_tx(TxMode::Readonly)?
        } else {
            tx
        };
        let page =
            engine.scan_packed_page(read_tx, "s", &ScanRange::default(), 1, byte_budget, false)?;
        assert_eq!(page.packet.len(), byte_budget);
        assert_eq!(
            unpack_rows(&page.packet),
            vec![(key.clone(), encoded.clone())]
        );
        assert_eq!(
            engine
                .scan_packed_page(
                    read_tx,
                    "s",
                    &ScanRange::default(),
                    1,
                    byte_budget - 1,
                    false
                )
                .unwrap_err(),
            EngineError::ValueTooLarge(byte_budget)
        );
        let packet = engine.get_many_packed(read_tx, "s", &[key.as_slice()])?;
        assert_eq!(packet[8..], encoded);
        if committed {
            engine.rollback_tx(read_tx)?;
        }
    }
    Ok(())
}

struct CountingBackend {
    inner: MemoryBackend,
    reads: Arc<AtomicUsize>,
    fail_offset: Option<u64>,
    trace: Option<ReadTrace>,
}

impl FileBackend for CountingBackend {
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        if let Some(trace) = &self.trace {
            trace
                .lock()
                .map_err(|_| EngineError::Storage("read trace poisoned".into()))?
                .push((offset, len));
        }
        if self
            .fail_offset
            .is_some_and(|failed| offset <= failed && failed < offset + len as u64)
        {
            return Err(EngineError::Storage("injected payload read".into()));
        }
        self.inner.read_at(offset, len)
    }
    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        self.inner.write_at(offset, bytes)
    }
    fn flush(&mut self) -> Result<()> {
        self.inner.flush()
    }
    fn len(&self) -> Result<u64> {
        self.inner.len()
    }
    fn truncate(&mut self, len: u64) -> Result<()> {
        self.inner.truncate(len)
    }
    fn close(&mut self) -> Result<()> {
        self.inner.close()
    }
}

fn counted_engine(
    name: &str,
    bundle: &MemoryBundle,
    fail_offset: Option<u64>,
) -> Result<(Engine<CountingBackend>, Arc<AtomicUsize>)> {
    let reads = Arc::new(AtomicUsize::new(0));
    let files = FileSet::new(
        CountingBackend {
            inner: bundle.manifest.clone(),
            reads: Arc::new(AtomicUsize::new(0)),
            fail_offset: None,
            trace: None,
        },
        CountingBackend {
            inner: bundle.main.clone(),
            reads: Arc::clone(&reads),
            fail_offset,
            trace: None,
        },
        CountingBackend {
            inner: bundle.wal.clone(),
            reads: Arc::new(AtomicUsize::new(0)),
            fail_offset: None,
            trace: None,
        },
    );
    let engine = Engine::open(
        name,
        files,
        OpenConfig {
            cache_pages: 1,
            ..OpenConfig::default()
        },
    )?;
    reads.store(0, Ordering::Relaxed);
    Ok((engine, reads))
}

fn traced_engine(
    name: &str,
    bundle: &MemoryBundle,
    fail_offset: Option<u64>,
) -> Result<(Engine<CountingBackend>, ReadTrace)> {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let files = FileSet::new(
        CountingBackend {
            inner: bundle.manifest.clone(),
            reads: Arc::new(AtomicUsize::new(0)),
            fail_offset: None,
            trace: None,
        },
        CountingBackend {
            inner: bundle.main.clone(),
            reads: Arc::new(AtomicUsize::new(0)),
            fail_offset,
            trace: Some(Arc::clone(&trace)),
        },
        CountingBackend {
            inner: bundle.wal.clone(),
            reads: Arc::new(AtomicUsize::new(0)),
            fail_offset: None,
            trace: None,
        },
    );
    let engine = Engine::open(
        name,
        files,
        OpenConfig {
            cache_pages: 1,
            ..OpenConfig::default()
        },
    )?;
    trace.lock().expect("read trace").clear();
    Ok((engine, trace))
}

fn compression_frame(raw_length: usize, payload_length: usize, tag: u8) -> Vec<u8> {
    let payload = vec![0x53; payload_length];
    let mut encoded = b"BDBZVAL1".to_vec();
    encoded.extend_from_slice(&[1, tag]);
    encoded.extend_from_slice(&(raw_length as u32).to_le_bytes());
    encoded.extend_from_slice(&crc32(&payload).to_le_bytes());
    encoded.extend_from_slice(&payload);
    encoded
}

#[test]
fn value_size_batch_preserves_input_order_staging_ttl_and_snapshots() -> Result<()> {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open(
        "value-size-visibility",
        bundle.files(),
        OpenConfig::default(),
    )?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(tx, "raw")?;
    engine.create_store_with_compression(tx, "compressed", StoreCompression::Gzip)?;
    engine.put(tx, "raw", b"inline", &[0, 255, 1])?;
    engine.put(tx, "raw", b"external", &vec![0x77; MAX_VALUE_BYTES])?;
    engine.put(tx, "raw", b"empty", &[])?;
    engine.put(
        tx,
        "compressed",
        b"inline",
        &compression_frame(1000, 100, 1),
    )?;
    engine.put(
        tx,
        "compressed",
        b"external",
        &compression_frame(MAX_VALUE_BYTES, MAX_VALUE_BYTES, 0),
    )?;
    engine.commit_tx(tx)?;
    let snapshot = engine.begin_tx(TxMode::Readonly)?;
    let writer = engine.begin_tx(TxMode::Readwrite)?;
    engine.put(writer, "raw", b"inline", b"changed")?;
    engine.delete(writer, "raw", b"external")?;
    engine.put_with_ttl(writer, "raw", b"expired", b"gone", Some(0))?;
    engine.put(
        writer,
        "compressed",
        b"inline",
        &compression_frame(2000, 100, 1),
    )?;
    let keys = [
        b"external".as_slice(),
        b"inline",
        b"missing",
        b"empty",
        b"inline",
        b"expired",
    ];
    assert_eq!(
        engine.get_many_value_sizes(snapshot, "raw", &keys)?,
        vec![Some(MAX_VALUE_BYTES), Some(3), None, Some(0), Some(3), None]
    );
    assert_eq!(
        engine.get_many_value_sizes(writer, "raw", &keys)?,
        vec![None, Some(7), None, Some(0), Some(7), None]
    );
    assert_eq!(
        engine.get_many_value_sizes(
            snapshot,
            "compressed",
            &[b"inline".as_slice(), b"external"]
        )?,
        vec![
            Some(1000),
            Some(MAX_VALUE_BYTES + COMPRESSION_VALUE_HEADER_SIZE)
        ]
    );
    assert_eq!(
        engine.get_many_value_sizes(writer, "compressed", &[b"inline".as_slice(), b"external"])?,
        vec![
            Some(2000),
            Some(MAX_VALUE_BYTES + COMPRESSION_VALUE_HEADER_SIZE)
        ]
    );
    assert!(engine
        .get_many_value_sizes::<&[u8]>(writer, "raw", &[])?
        .is_empty());
    engine.commit_tx(writer)?;
    assert_eq!(
        engine.get_many_value_sizes(snapshot, "raw", &keys)?,
        vec![Some(MAX_VALUE_BYTES), Some(3), None, Some(0), Some(3), None]
    );
    let latest = engine.begin_tx(TxMode::Readonly)?;
    assert_eq!(
        engine.get_many_value_sizes(latest, "raw", &keys)?,
        vec![None, Some(7), None, Some(0), Some(7), None]
    );
    engine.rollback_tx(latest)?;
    engine.rollback_tx(snapshot)?;
    Ok(())
}

#[test]
fn external_value_sizes_read_only_headers_and_skip_expired_payloads() -> Result<()> {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open(
        "value-size-body-work",
        bundle.files(),
        OpenConfig::default(),
    )?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(tx, "raw")?;
    engine.create_store_with_compression(tx, "compressed", StoreCompression::Gzip)?;
    engine.put(tx, "raw", b"external", &vec![0x77; MAX_VALUE_BYTES])?;
    engine.put_with_ttl(
        tx,
        "raw",
        b"expired",
        &vec![0x55; LARGE_VALUE],
        Some(60_000),
    )?;
    engine.put(
        tx,
        "compressed",
        b"external",
        &compression_frame(MAX_VALUE_BYTES, MAX_VALUE_BYTES, 0),
    )?;
    engine.commit_tx(tx)?;
    engine.checkpoint()?;
    let mut references = Vec::new();
    for store in ["raw", "compressed"] {
        let root = engine
            .catalog()
            .get(store)
            .expect("store metadata")
            .store_root_page_id;
        let mut page = decode_page(&bundle.main.read_at(page_offset(root), PAGE_SIZE)?)?;
        for cell in &mut page.leaf_cells {
            assert_eq!(cell.value_kind, ValueKind::External);
            let (reference, _) = decode_payload_descriptor(
                cell.overflow_head_page_id,
                cell.total_value_len,
                &cell.value,
            )?;
            references.push((store, cell.key.clone(), reference));
            if cell.key == b"expired" {
                cell.value[16..24].copy_from_slice(&1u64.to_le_bytes());
            }
        }
        bundle.main.clone().write_at(
            page_offset(root),
            &encode_leaf_page(
                root,
                page.header.level,
                page.header.right_sibling_page_id,
                &page.leaf_cells,
            )?,
        )?;
    }
    for (store, key, reference) in references {
        let start = page_offset(reference.first_page_id);
        let body_start = start + PAYLOAD_HEADER_SIZE as u64;
        let fail_offset = if store == "compressed" {
            body_start + COMPRESSION_VALUE_HEADER_SIZE as u64
        } else {
            body_start
        };
        let (mut engine, trace) =
            traced_engine("value-size-body-work", &bundle, Some(fail_offset))?;
        let tx = engine.begin_tx(TxMode::Readonly)?;
        trace.lock().expect("read trace").clear();
        let expected = if key == b"expired" {
            None
        } else {
            Some(
                MAX_VALUE_BYTES
                    + if store == "compressed" {
                        COMPRESSION_VALUE_HEADER_SIZE
                    } else {
                        0
                    },
            )
        };
        assert_eq!(
            engine.get_many_value_sizes(tx, store, &[key.as_slice(), key.as_slice()])?,
            vec![expected, expected]
        );
        trace.lock().expect("read trace").clear();
        let states = engine.get_many_value_states(
            tx,
            store,
            &[key.as_slice(), key.as_slice(), b"missing"],
        )?;
        assert_eq!(states[0], states[1]);
        assert!(states[2].is_none());
        let state = states[0].as_ref().expect("physical value state");
        assert_eq!(state.exists, key != b"expired");
        assert_eq!(state.expired, key == b"expired");
        assert_eq!(state.max_length, expected.unwrap_or(0));
        assert!(state.revision.is_some());
        let body_end = start + reference.page_count()? * PAGE_SIZE as u64;
        let payload_reads: Vec<_> = trace
            .lock()
            .expect("read trace")
            .iter()
            .copied()
            .filter(|(offset, len)| *offset < body_end && start < *offset + *len as u64)
            .collect();
        if store == "compressed" {
            assert_eq!(
                payload_reads,
                vec![
                    (start, PAYLOAD_HEADER_SIZE),
                    (body_start, COMPRESSION_VALUE_HEADER_SIZE)
                ]
            );
            println!(
                "value_sizes compressed_external metadata_bytes={} full_body_bytes=0",
                payload_reads.iter().map(|(_, len)| len).sum::<usize>()
            );
        } else {
            assert!(
                payload_reads.is_empty(),
                "raw/expired metadata accessed payload: {payload_reads:?}"
            );
        }
        engine.rollback_tx(tx)?;
    }
    Ok(())
}

#[test]
fn value_states_keep_staged_expired_revision_and_isolate_snapshots() -> Result<()> {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open(
        "value-state-visibility",
        bundle.files(),
        OpenConfig::default(),
    )?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    let index_store = "__browserdb:index:test";
    let index_key = b"i\0\0k\0\0";
    let rejected_key = b"b\0\0k\0\0";
    engine.create_store(tx, "s")?;
    engine.create_store(tx, index_store)?;
    engine.put(tx, "s", b"k", b"old")?;
    let old = engine.get_many_value_states(tx, "s", &[b"k".as_slice()])?[0]
        .clone()
        .expect("staged value");
    assert!(old.exists && !old.expired);
    assert_eq!(old.max_length, 3);
    assert!(old.revision.is_some());
    engine.commit_tx(tx)?;
    let snapshot = engine.begin_tx(TxMode::Readonly)?;
    let writer = engine.begin_tx(TxMode::Readwrite)?;
    engine.put_with_ttl(writer, "s", b"k", b"expired", Some(0))?;
    let expired = engine.get_many_value_states(writer, "s", &[b"k".as_slice()])?[0]
        .clone()
        .expect("expired physical value");
    assert!(!expired.exists && expired.expired);
    assert_eq!(expired.max_length, 0);
    assert!(expired.expires_at_ms.is_some());
    assert_ne!(expired.revision, old.revision);
    let revision = expired.revision.as_ref().expect("expired revision");
    engine.put_index_entry_checked(
        writer,
        "s",
        index_store,
        b"k",
        index_key,
        revision.epoch,
        revision.ordinal,
    )?;
    let stamped_index =
        engine.get_many_value_states(writer, index_store, &[index_key.as_slice()])?[0]
            .clone()
            .expect("index value");
    assert_eq!(stamped_index.revision, expired.revision);
    assert_eq!(stamped_index.max_length, 0);
    let old_revision = old.revision.as_ref().expect("old revision");
    assert!(matches!(
        engine.put_index_entry_checked(
            writer,
            "s",
            index_store,
            b"k",
            rejected_key,
            old_revision.epoch,
            old_revision.ordinal
        ),
        Err(EngineError::ValueRevisionConflict)
    ));
    assert!(
        engine.get_many_value_states(writer, index_store, &[rejected_key.as_slice()])?[0].is_none()
    );
    engine.commit_tx(writer)?;
    assert_eq!(
        engine.get_many_value_states(snapshot, "s", &[b"k".as_slice()])?[0],
        Some(old)
    );
    let latest = engine.begin_tx(TxMode::Readonly)?;
    assert_eq!(
        engine.get_many_value_states(latest, "s", &[b"k".as_slice()])?[0],
        None
    );
    engine.rollback_tx(latest)?;
    engine.rollback_tx(snapshot)?;
    Ok(())
}

#[test]
fn paged_bookmark_reads_are_bounded_by_pages_and_rows() -> Result<()> {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open("packed-page-work", bundle.files(), OpenConfig::default())?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(tx, "s")?;
    for index in 0..1024u32 {
        engine.put(tx, "s", &index.to_be_bytes(), &[0x52; 512])?;
    }
    engine.commit_tx(tx)?;
    engine.checkpoint()?;
    let (mut engine, reads) = counted_engine("packed-page-work", &bundle, None)?;
    let tx = engine.begin_tx(TxMode::Readonly)?;
    let mut range = ScanRange::default();
    let mut total = 0;
    let mut pages = 0;
    loop {
        let before = reads.load(Ordering::Relaxed);
        let page = engine.scan_packed_page(tx, "s", &range, 8, 5000, false)?;
        let work = reads.load(Ordering::Relaxed) - before;
        assert!(
            work <= 20,
            "a page must seek and read only its bounded rows: {work}"
        );
        total += page.row_count;
        pages += 1;
        if page.exhausted {
            break;
        }
        range.gt = page.last_key;
    }
    let calls = reads.load(Ordering::Relaxed);
    println!("packed_scan rows={total} pages={pages} main_read_calls={calls}");
    assert_eq!(total, 1024);
    assert!(calls <= pages * 20);
    engine.rollback_tx(tx)?;
    Ok(())
}

#[test]
fn keys_only_scan_skips_payload_body_and_packed_get_validates_expired_body() -> Result<()> {
    let bundle = MemoryBundle::new();
    let mut engine = Engine::open("packed-body-work", bundle.files(), OpenConfig::default())?;
    let tx = engine.begin_tx(TxMode::Readwrite)?;
    engine.create_store(tx, "s")?;
    engine.put(tx, "s", b"live", &vec![0x44; LARGE_VALUE])?;
    engine.put_with_ttl(tx, "s", b"expired", &vec![0x77; LARGE_VALUE], Some(60_000))?;
    engine.commit_tx(tx)?;
    engine.checkpoint()?;
    let mut bodies = Vec::new();
    for page_id in 1..=bundle.main.len()? / PAGE_SIZE as u64 {
        let bytes = bundle.main.read_at(page_offset(page_id), PAGE_SIZE)?;
        let Ok(mut page) = decode_page(&bytes) else {
            continue;
        };
        let mut rewrite_leaf = false;
        for cell in &mut page.leaf_cells {
            if cell.key == b"live" || cell.key == b"expired" {
                let failed_page =
                    cell.overflow_head_page_id + u64::from(cell.value_kind == ValueKind::Overflow);
                bodies.push((cell.key.clone(), page_offset(failed_page)));
                if cell.key == b"expired" {
                    if cell.value_kind == ValueKind::Overflow {
                        let head = cell.overflow_head_page_id;
                        let mut overflow =
                            decode_page(&bundle.main.read_at(page_offset(head), PAGE_SIZE)?)?
                                .overflow
                                .expect("overflow header");
                        overflow.chunk[8..16].copy_from_slice(&1u64.to_le_bytes());
                        bundle.main.clone().write_at(
                            page_offset(head),
                            &encode_overflow_page(
                                head,
                                overflow.next_overflow_page_id,
                                &overflow.chunk,
                            )?,
                        )?;
                    } else {
                        let prefix_offset = if cell.value_kind == ValueKind::External {
                            8
                        } else {
                            0
                        };
                        cell.value[prefix_offset + 8..prefix_offset + 16]
                            .copy_from_slice(&1u64.to_le_bytes());
                        rewrite_leaf = true;
                    }
                }
            }
        }
        if rewrite_leaf {
            bundle.main.clone().write_at(
                page_offset(page_id),
                &encode_leaf_page(
                    page_id,
                    page.header.level,
                    page.header.right_sibling_page_id,
                    &page.leaf_cells,
                )?,
            )?;
        }
    }
    assert_eq!(bodies.len(), 2);
    for (key, failed) in bodies {
        let (mut engine, _) = counted_engine("packed-body-work", &bundle, Some(failed))?;
        let tx = engine.begin_tx(TxMode::Readonly)?;
        let page = engine.scan_packed_page(tx, "s", &ScanRange::default(), 10, 100, true)?;
        assert_eq!(
            unpack_rows(&page.packet),
            vec![(b"live".to_vec(), Vec::new())]
        );
        assert_eq!(
            engine
                .scan_packed_page(tx, "s", &ScanRange::default(), 10, 100, false)
                .unwrap_err(),
            EngineError::ValueTooLarge(4 + 8 + b"live".len() + LARGE_VALUE)
        );
        assert_eq!(
            engine
                .get_many_packed(tx, "s", &[key.as_slice()])
                .unwrap_err(),
            EngineError::Storage("injected payload read".into())
        );
        engine.rollback_tx(tx)?;
    }
    Ok(())
}
