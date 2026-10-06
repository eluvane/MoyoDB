use moyodb_engine::bytes::MAX_KEY_BYTES;
use moyodb_engine::snapshot::{
    decode_snapshot, encode_snapshot, SnapshotContents, SnapshotEntry, SnapshotStore,
    SNAPSHOT_BODY_PREFIX_SIZE, SNAPSHOT_CHECKSUM_OFFSET, SNAPSHOT_ENTRY_HEADER_SIZE,
    SNAPSHOT_HEADER_SIZE, SNAPSHOT_STORE_HEADER_SIZE,
};
use moyodb_engine::EngineError;

fn hex_bytes(value: &str) -> Vec<u8> {
    value
        .split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).unwrap())
        .collect()
}

fn format_fixture() -> SnapshotContents {
    SnapshotContents {
        source_last_committed_txid: 0x0102_0304_0506_0708,
        schema_version: 0x1112_1314_1516_1718,
        stores: vec![
            SnapshotStore {
                name: "z".into(),
                flags: 9,
                entries: vec![
                    SnapshotEntry {
                        key: b"z".to_vec(),
                        value: hex_bytes("42 44 42 5a 56 41 4c 31 01 00 0b 00 00 00 fb 66 b5 07 42 44 54 54 4c 30 30 31 72 61 77"),
                        expires_at_ms: Some(1),
                    },
                    SnapshotEntry {
                        key: b"a".to_vec(),
                        value: hex_bytes("42 44 42 5a 56 41 4c 31 01 00 00 00 00 00 00 00 00 00"),
                        expires_at_ms: None,
                    },
                    SnapshotEntry {
                        key: Vec::new(),
                        value: hex_bytes("42 44 42 5a 56 41 4c 31 01 00 01 00 00 00 7b a5 01 e4 aa"),
                        expires_at_ms: Some(0),
                    },
                ],
            },
            SnapshotStore {
                name: "a".into(),
                flags: 0,
                entries: Vec::new(),
            },
        ],
    }
}

#[test]
fn encoding_keeps_v3_bytes_input_order_and_expiry_metadata() {
    // Fixed wire bytes provide an oracle independent of encode_snapshot.
    let mut expected: Vec<u8> = concat!(
        "42 44 53 4e 41 50 30 31 03 00 00 00 00 00 00 00 ",
        "b6 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 ",
        "08 07 06 05 04 03 02 01 18 17 16 15 14 13 12 11 ",
        "02 00 00 00 00 00 00 00 ",
        // Store z: envelope/compression flags are serialized unchanged.
        "01 00 00 00 09 00 00 00 00 00 00 00 03 00 00 00 00 00 00 00 7a ",
        // Key z, an already expired timestamp, and envelope-looking payload.
        "01 00 01 00 1d 00 00 00 01 00 00 00 00 00 00 00 7a ",
        "42 44 42 5a 56 41 4c 31 01 00 0b 00 00 00 fb 66 b5 07 ",
        "42 44 54 54 4c 30 30 31 72 61 77 ",
        // Key a follows z; the encoder must not sort the supplied entries.
        "01 00 00 00 12 00 00 00 00 00 00 00 00 00 00 00 61 ",
        "42 44 42 5a 56 41 4c 31 01 00 00 00 00 00 00 00 00 00 ",
        // Empty key and Some(0) retain an explicit expiry flag.
        "00 00 01 00 13 00 00 00 00 00 00 00 00 00 00 00 ",
        "42 44 42 5a 56 41 4c 31 01 00 01 00 00 00 7b a5 01 e4 aa ",
        // Empty store a follows z; store order is preserved too.
        "01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 61"
    )
    .split_whitespace()
    .map(|byte| u8::from_str_radix(byte, 16).unwrap())
    .collect();
    let checksum = crc32fast::hash(&expected);
    expected[24..28].copy_from_slice(&checksum.to_le_bytes());

    let contents = format_fixture();
    assert_eq!(encode_snapshot(&contents).unwrap(), expected);
    assert_eq!(decode_snapshot(&expected).unwrap(), contents);
}

#[test]
fn key_boundaries_and_nonadjacent_duplicates_keep_their_errors() {
    let mut contents = format_fixture();
    contents.stores[0].entries[0].key = vec![0xff; MAX_KEY_BYTES];
    let bytes = encode_snapshot(&contents).unwrap();
    assert_eq!(decode_snapshot(&bytes).unwrap(), contents);

    // The repeated keys live in distinct allocations and are not adjacent.
    let repeated_key = contents.stores[0].entries[0].key.clone();
    contents.stores[0].entries.push(SnapshotEntry {
        key: repeated_key,
        value: hex_bytes("42 44 42 5a 56 41 4c 31 01 00 0f 00 00 00 ff d8 17 90 64 69 66 66 65 72 65 6e 74 20 76 61 6c 75 65"),
        expires_at_ms: None,
    });
    let duplicate = encode_snapshot(&contents).unwrap_err();
    assert_eq!(duplicate.code(), "SerializationError");
    assert_eq!(
        duplicate.to_string(),
        "serialization error: duplicate key while encoding snapshot store z"
    );

    contents.stores[0].entries[0].key.push(0xff);
    assert_eq!(
        encode_snapshot(&contents).unwrap_err(),
        EngineError::KeyTooLarge(MAX_KEY_BYTES + 1)
    );
}

#[test]
fn duplicate_detection_keeps_its_store_scope_and_decoder_boundary() {
    let mut contents = format_fixture();
    let other_store_entry = contents.stores[0].entries[0].clone();
    contents.stores[1].entries.push(other_store_entry);
    let bytes = encode_snapshot(&contents).unwrap();
    assert_eq!(decode_snapshot(&bytes).unwrap(), contents);

    // The encoder permits duplicate store names. The decoder must reject them.
    contents.stores[1].name = contents.stores[0].name.clone();
    let bytes = encode_snapshot(&contents).unwrap();
    let duplicate = decode_snapshot(&bytes).unwrap_err();
    assert_eq!(duplicate.code(), "CorruptionError");
    assert_eq!(
        duplicate.to_string(),
        "database corruption: duplicate snapshot store z"
    );
}

fn reseal(bytes: &mut [u8]) {
    bytes[SNAPSHOT_CHECKSUM_OFFSET..SNAPSHOT_CHECKSUM_OFFSET + 4].fill(0);
    let checksum = crc32fast::hash(bytes);
    bytes[SNAPSHOT_CHECKSUM_OFFSET..SNAPSHOT_CHECKSUM_OFFSET + 4]
        .copy_from_slice(&checksum.to_le_bytes());
}

#[test]
fn store_name_reserved_bytes_still_round_trip() {
    let contents = format_fixture();
    let mut bytes = encode_snapshot(&contents).unwrap();
    let mut reserved_at = SNAPSHOT_HEADER_SIZE + SNAPSHOT_BODY_PREFIX_SIZE;
    for store in &contents.stores {
        bytes[reserved_at + 2..reserved_at + 4].fill(0xab);
        reserved_at += SNAPSHOT_STORE_HEADER_SIZE + store.name.len();
        for entry in &store.entries {
            reserved_at += SNAPSHOT_ENTRY_HEADER_SIZE + entry.key.len() + entry.value.len();
        }
    }
    assert_eq!(reserved_at, bytes.len());
    reseal(&mut bytes);
    assert_eq!(decode_snapshot(&bytes).unwrap(), contents);
}

#[test]
fn unknown_header_and_directory_flags_fail_closed() {
    let encoded = encode_snapshot(&format_fixture()).unwrap();
    for (offset, field) in [
        (12, "header"),
        (28, "header"),
        (
            SNAPSHOT_HEADER_SIZE + SNAPSHOT_BODY_PREFIX_SIZE - 4,
            "directory",
        ),
    ] {
        let mut bytes = encoded.clone();
        bytes[offset..offset + 4].fill(0xa5);
        reseal(&mut bytes);
        assert_eq!(
            decode_snapshot(&bytes).unwrap_err(),
            EngineError::Corruption(format!("unsupported snapshot {field} flags 0xa5a5a5a5"))
        );
    }
}

#[test]
fn truncated_reserved_field_does_not_look_like_trailing_bytes() {
    let contents = format_fixture();
    let encoded = encode_snapshot(&contents).unwrap();

    let mut missing_store_count_reserved = encoded.clone();
    let store_count_end = SNAPSHOT_HEADER_SIZE + SNAPSHOT_BODY_PREFIX_SIZE - 4;
    missing_store_count_reserved.truncate(store_count_end);
    let body_len = (store_count_end - SNAPSHOT_HEADER_SIZE) as u64;
    missing_store_count_reserved[16..24].copy_from_slice(&body_len.to_le_bytes());
    reseal(&mut missing_store_count_reserved);
    assert_eq!(
        decode_snapshot(&missing_store_count_reserved).unwrap_err(),
        EngineError::Corruption(
            "snapshot reserved field after the store count is truncated".into()
        )
    );

    let mut missing_name_reserved = encoded;
    let name_len_end = SNAPSHOT_HEADER_SIZE + SNAPSHOT_BODY_PREFIX_SIZE + 2;
    missing_name_reserved.truncate(name_len_end);
    let body_len = (name_len_end - SNAPSHOT_HEADER_SIZE) as u64;
    missing_name_reserved[16..24].copy_from_slice(&body_len.to_le_bytes());
    reseal(&mut missing_name_reserved);
    assert_eq!(
        decode_snapshot(&missing_name_reserved).unwrap_err(),
        EngineError::Corruption(
            "snapshot reserved field after the store name length is truncated".into()
        )
    );
}
