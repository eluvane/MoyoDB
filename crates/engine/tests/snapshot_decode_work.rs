use moyodb_engine::bytes::MAX_KEY_BYTES;
use moyodb_engine::snapshot::{
    decode_snapshot, encode_snapshot, SnapshotContents, SnapshotEntry, SnapshotStore,
    SNAPSHOT_BODY_PREFIX_SIZE, SNAPSHOT_CHECKSUM_OFFSET, SNAPSHOT_ENTRY_HEADER_SIZE,
    SNAPSHOT_HEADER_SIZE, SNAPSHOT_STORE_HEADER_SIZE,
};
use moyodb_engine::EngineError;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ALLOCATED_BYTES: Cell<Option<usize>> = const { Cell::new(None) };
}

struct DecodeAllocator;

fn count_allocation(bytes: usize) {
    ALLOCATED_BYTES.with(|work| {
        if let Some(total) = work.get() {
            work.set(Some(total + bytes));
        }
    });
}

// SAFETY: allocations and deallocations are forwarded unchanged to System.
// The thread-local counter only observes allocation request sizes.
unsafe impl GlobalAlloc for DecodeAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_allocation(layout.size());
        // SAFETY: GlobalAlloc supplies the valid layout forwarded to System.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_allocation(layout.size());
        // SAFETY: GlobalAlloc supplies the valid layout forwarded to System.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: pointer and layout originate from this allocator's System calls.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, bytes: usize) -> *mut u8 {
        count_allocation(bytes);
        // SAFETY: GlobalAlloc supplies the allocated pointer and its layout.
        unsafe { System.realloc(pointer, layout, bytes) }
    }
}

#[global_allocator]
static ALLOCATOR: DecodeAllocator = DecodeAllocator;

fn decode_work(bytes: &[u8]) -> (SnapshotContents, usize) {
    ALLOCATED_BYTES.with(|work| assert!(work.replace(Some(0)).is_none()));
    let decoded = decode_snapshot(bytes);
    let allocated = ALLOCATED_BYTES.with(|work| work.replace(None).unwrap());
    (decoded.unwrap(), allocated)
}

fn key_fixture(key_len: usize) -> SnapshotContents {
    SnapshotContents {
        source_last_committed_txid: 7,
        schema_version: 3,
        stores: vec![SnapshotStore {
            name: "kv".into(),
            flags: 0,
            entries: (0..32u32)
                .map(|index| {
                    let mut key = vec![0x77; key_len];
                    key[..4].copy_from_slice(&index.to_be_bytes());
                    SnapshotEntry {
                        key,
                        value: Vec::new(),
                        expires_at_ms: None,
                    }
                })
                .collect(),
        }],
    }
}

#[test]
fn decoding_copies_each_owned_key_once() {
    let small = key_fixture(32);
    let large = key_fixture(MAX_KEY_BYTES);
    let small_bytes = encode_snapshot(&small).unwrap();
    let large_bytes = encode_snapshot(&large).unwrap();

    let (decoded_small, small_work) = decode_work(&small_bytes);
    let (decoded_large, large_work) = decode_work(&large_bytes);
    assert_eq!(decoded_small, small);
    assert_eq!(decoded_large, large);
    // Container sizes and counts match. Only the 32 owned key lengths differ;
    // uniqueness checking must not allocate a second copy of those key bytes.
    assert_eq!(large_work - small_work, 32 * (MAX_KEY_BYTES - 32));
}

fn name_fixture(name_len: usize) -> SnapshotContents {
    SnapshotContents {
        source_last_committed_txid: 7,
        schema_version: 3,
        stores: (0..32)
            .map(|index| SnapshotStore {
                name: format!("{index:04}{}", "s".repeat(name_len - 4)),
                flags: 0,
                entries: Vec::new(),
            })
            .collect(),
    }
}

#[test]
fn decoding_copies_each_owned_store_name_once() {
    let small = name_fixture(32);
    let large = name_fixture(240);
    let small_bytes = encode_snapshot(&small).unwrap();
    let large_bytes = encode_snapshot(&large).unwrap();

    let (decoded_small, small_work) = decode_work(&small_bytes);
    let (decoded_large, large_work) = decode_work(&large_bytes);
    assert_eq!(decoded_small, small);
    assert_eq!(decoded_large, large);
    assert_eq!(large_work - small_work, 32 * (240 - 32));
}

fn reseal(bytes: &mut [u8]) {
    bytes[SNAPSHOT_CHECKSUM_OFFSET..SNAPSHOT_CHECKSUM_OFFSET + 4].fill(0);
    let checksum = crc32fast::hash(bytes);
    bytes[SNAPSHOT_CHECKSUM_OFFSET..SNAPSHOT_CHECKSUM_OFFSET + 4]
        .copy_from_slice(&checksum.to_le_bytes());
}

#[test]
fn valid_checksum_duplicates_keep_value_validation_and_checksum_order() {
    let contents = SnapshotContents {
        source_last_committed_txid: 1,
        schema_version: 0,
        stores: vec![SnapshotStore {
            name: "kv".into(),
            flags: 0,
            entries: (*b"ab")
                .into_iter()
                .map(|key| SnapshotEntry {
                    key: vec![key],
                    value: Vec::new(),
                    expires_at_ms: None,
                })
                .collect(),
        }],
    };
    let mut bytes = encode_snapshot(&contents).unwrap();
    let first_entry = SNAPSHOT_HEADER_SIZE
        + SNAPSHOT_BODY_PREFIX_SIZE
        + SNAPSHOT_STORE_HEADER_SIZE
        + contents.stores[0].name.len();
    let second_entry = first_entry + SNAPSHOT_ENTRY_HEADER_SIZE + 1;
    bytes[second_entry + SNAPSHOT_ENTRY_HEADER_SIZE] = b'a';
    reseal(&mut bytes);
    let duplicate = decode_snapshot(&bytes).unwrap_err();
    assert_eq!(duplicate.code(), "CorruptionError");
    assert_eq!(
        duplicate,
        EngineError::Corruption("duplicate snapshot key in store kv".into())
    );

    // The second key is duplicate, but its value cannot be read. Existing
    // value validation runs before inserting the key into the uniqueness set.
    bytes[second_entry + 4..second_entry + 8].copy_from_slice(&1u32.to_le_bytes());
    reseal(&mut bytes);
    assert_eq!(
        decode_snapshot(&bytes).unwrap_err(),
        EngineError::Corruption("snapshot value out of bounds".into())
    );

    bytes[SNAPSHOT_CHECKSUM_OFFSET] ^= 1;
    let checksum_error = decode_snapshot(&bytes).unwrap_err();
    assert_eq!(checksum_error.code(), "CorruptionError");
    assert!(checksum_error
        .to_string()
        .contains("snapshot checksum mismatch"));
}

#[test]
fn valid_checksum_duplicate_store_names_keep_the_original_error() {
    let mut contents = name_fixture(32);
    contents.stores.truncate(2);
    let mut bytes = encode_snapshot(&contents).unwrap();
    let first_name = SNAPSHOT_HEADER_SIZE + SNAPSHOT_BODY_PREFIX_SIZE + SNAPSHOT_STORE_HEADER_SIZE;
    let second_name = first_name + contents.stores[0].name.len() + SNAPSHOT_STORE_HEADER_SIZE;
    let name_len = contents.stores[0].name.len();
    let repeated_name = bytes[first_name..first_name + name_len].to_vec();
    bytes[second_name..second_name + name_len].copy_from_slice(&repeated_name);
    reseal(&mut bytes);
    let duplicate = decode_snapshot(&bytes).unwrap_err();
    assert_eq!(duplicate.code(), "CorruptionError");
    assert_eq!(
        duplicate,
        EngineError::Corruption(format!(
            "duplicate snapshot store {}",
            contents.stores[0].name
        ))
    );
}
