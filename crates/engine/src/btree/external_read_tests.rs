use super::{
    lookup_prefix, lookup_stored_value, lookup_stored_value_info, lookup_stored_value_into,
    lookup_stored_value_size, lookup_stored_value_state, lookup_value_expiry,
    materialize_pending_stored_value, pending_stored_value_info, TreeIter,
};
use crate::checksum::work as checksum_work;
use crate::error::{EngineError, Result};
use crate::layout::{page_offset, ValueKind};
use crate::page::{decode_page, encode_leaf_page, LeafCell};
use crate::pager::Pager;
use crate::payload::{encode_payload_descriptor, write_payload, PAYLOAD_HEADER_SIZE};
use crate::storage::memory::MemoryBackend;
use crate::value::{
    store_flags_for_user_store, StoreCompression, StoredValue, ValueRevision, ValueState,
    STORE_FLAG_VALUE_ENVELOPE_V1,
};

fn external_pager(value: &[u8], expiry: Option<u64>) -> Result<Pager<MemoryBackend>> {
    external_pager_for_store(value, expiry, None, STORE_FLAG_VALUE_ENVELOPE_V1)
}

fn external_pager_for_store(
    value: &[u8],
    expiry: Option<u64>,
    revision: Option<ValueRevision>,
    flags: u64,
) -> Result<Pager<MemoryBackend>> {
    let mut main = MemoryBackend::new();
    let payload = write_payload(&mut main, 2, value)?;
    let prefix = StoredValue {
        value: Vec::new(),
        expires_at_ms: expiry,
        revision,
    }
    .encode_for_store(flags)?;
    let descriptor = encode_payload_descriptor(&payload, &prefix)?;
    let leaf = encode_leaf_page(
        1,
        0,
        0,
        &[LeafCell {
            key: b"key".to_vec(),
            value: descriptor,
            value_kind: ValueKind::External,
            total_value_len: (prefix.len() + value.len()) as u32,
            overflow_head_page_id: payload.first_page_id,
        }],
    )?;
    main.write_at(page_offset(1), &leaf)?;
    Ok(Pager::new(main, 1))
}

#[test]
fn external_point_and_scan_readers_share_payload_sink() -> Result<()> {
    let value = vec![0x71; 1024 * 1024];
    let mut pager = external_pager(&value, Some(123))?;
    assert_eq!(
        lookup_stored_value_info(&mut pager, 1, b"key", STORE_FLAG_VALUE_ENVELOPE_V1, None)?,
        Some((value.len(), Some(123)))
    );
    let mut output = Vec::with_capacity(value.len());
    assert_eq!(
        lookup_stored_value_into(
            &mut pager,
            1,
            b"key",
            STORE_FLAG_VALUE_ENVELOPE_V1,
            None,
            |chunk| {
                output.extend_from_slice(chunk);
                Ok(())
            },
        )?,
        Some(Some(123))
    );
    assert_eq!(output, value);
    assert_eq!(
        lookup_stored_value(&mut pager, 1, b"key", STORE_FLAG_VALUE_ENVELOPE_V1, None)?,
        Some(StoredValue::with_expiry(value.clone(), Some(123)))
    );
    let mut iter = TreeIter::new(&mut pager, 1, &super::RangeSpec::default())?;
    let pair = iter.next(&mut pager)?.expect("external row");
    assert_eq!(
        pending_stored_value_info(&mut pager, &pair.value, STORE_FLAG_VALUE_ENVELOPE_V1)?,
        (value.len(), Some(123))
    );
    assert_eq!(
        materialize_pending_stored_value(&mut pager, pair.value, STORE_FLAG_VALUE_ENVELOPE_V1)?,
        StoredValue::with_expiry(value, Some(123))
    );
    Ok(())
}

#[test]
fn external_expiry_requires_only_cached_descriptor() -> Result<()> {
    let mut pager = external_pager(&vec![0x51; 1024 * 1024], Some(1))?;
    pager.read_page(1)?;
    pager.backend_mut().close()?;
    assert_eq!(
        lookup_value_expiry(&mut pager, 1, b"key", STORE_FLAG_VALUE_ENVELOPE_V1, None)?,
        Some(Some(1))
    );
    assert!(matches!(
        lookup_stored_value(&mut pager, 1, b"key", STORE_FLAG_VALUE_ENVELOPE_V1, None),
        Err(EngineError::Storage(_))
    ));
    Ok(())
}

#[test]
fn external_get_checks_expired_body_checksum() -> Result<()> {
    let mut pager = external_pager(&vec![0x31; 1024 * 1024], Some(1))?;
    let last = page_offset(2) + PAYLOAD_HEADER_SIZE as u64 + 1024 * 1024 - 1;
    pager.backend_mut().write_at(last, &[0x41])?;
    assert_eq!(
        lookup_value_expiry(&mut pager, 1, b"key", STORE_FLAG_VALUE_ENVELOPE_V1, None)?,
        Some(Some(1))
    );
    assert_eq!(
        lookup_stored_value(&mut pager, 1, b"key", STORE_FLAG_VALUE_ENVELOPE_V1, None).unwrap_err(),
        EngineError::Corruption("external payload body checksum mismatch".into())
    );
    Ok(())
}

#[test]
fn external_short_prefix_does_not_check_later_body() -> Result<()> {
    let value = vec![0x53; 1024 * 1024];
    let mut pager = external_pager(&value, None)?;
    let last = page_offset(2) + PAYLOAD_HEADER_SIZE as u64 + value.len() as u64 - 1;
    pager.backend_mut().write_at(last, &[0x41])?;
    let prefix = StoredValue::plain(Vec::new()).encode_for_store(STORE_FLAG_VALUE_ENVELOPE_V1)?;
    assert_eq!(
        lookup_prefix(&mut pager, 1, b"key", prefix.len())?,
        Some(prefix.clone())
    );
    let mut expected = prefix;
    expected.extend_from_slice(&value[..18]);
    assert_eq!(
        lookup_prefix(&mut pager, 1, b"key", expected.len())?,
        Some(expected)
    );
    assert!(matches!(
        super::lookup(&mut pager, 1, b"key"),
        Err(EngineError::Corruption(_))
    ));
    Ok(())
}

#[test]
fn external_point_metadata_preserves_full_read_error_order() -> Result<()> {
    let mut pager = external_pager(&vec![0x31; 1024 * 1024], Some(1))?;
    let mut leaf = decode_page(&pager.read_page(1)?)?.leaf_cells;
    leaf[0].value[8] ^= 0x80;
    pager.write_page_image(1, &encode_leaf_page(1, 0, 0, &leaf)?)?;
    let last = page_offset(2) + PAYLOAD_HEADER_SIZE as u64 + 1024 * 1024 - 1;
    pager.backend_mut().write_at(last, &[0x41])?;
    let expected = EngineError::Corruption("external payload body checksum mismatch".into());
    assert_eq!(
        lookup_stored_value(&mut pager, 1, b"key", STORE_FLAG_VALUE_ENVELOPE_V1, None).unwrap_err(),
        expected
    );
    assert_eq!(
        lookup_stored_value_info(&mut pager, 1, b"key", STORE_FLAG_VALUE_ENVELOPE_V1, None)
            .unwrap_err(),
        expected
    );
    Ok(())
}

#[test]
fn external_metadata_does_not_hash_body_and_full_get_hashes_it_once() -> Result<()> {
    let value = vec![0x35; 1024 * 1024];
    let mut pager = external_pager(&value, None)?;
    pager.read_page(1)?;
    checksum_work::reset();
    assert_eq!(
        lookup_stored_value_info(&mut pager, 1, b"key", STORE_FLAG_VALUE_ENVELOPE_V1, None)?,
        Some((value.len(), None))
    );
    assert_eq!(checksum_work::hashed_bytes(), 0);
    checksum_work::reset();
    assert_eq!(
        lookup_stored_value(&mut pager, 1, b"key", STORE_FLAG_VALUE_ENVELOPE_V1, None)?
            .unwrap()
            .value,
        value
    );
    assert_eq!(
        checksum_work::hashed_bytes(),
        value.len() + PAYLOAD_HEADER_SIZE
    );
    checksum_work::reset();
    assert!(lookup_prefix(&mut pager, 1, b"key", 16 + 18)?.is_some());
    assert_eq!(checksum_work::hashed_bytes(), PAYLOAD_HEADER_SIZE);
    Ok(())
}

#[test]
fn valid_external_extent_outside_allocated_range_is_rejected_before_io() -> Result<()> {
    let mut pager = external_pager(&vec![0x53; 1024 * 1024], None)?;
    pager.read_page(1)?;
    pager.set_page_limit(2);
    pager.backend_mut().close()?;
    assert!(matches!(
        lookup_stored_value_info(&mut pager, 1, b"key", STORE_FLAG_VALUE_ENVELOPE_V1, None),
        Err(EngineError::Corruption(_))
    ));
    assert!(matches!(
        lookup_stored_value_size(&mut pager, 1, b"key", STORE_FLAG_VALUE_ENVELOPE_V1, 1, None),
        Err(EngineError::Corruption(_))
    ));
    assert!(matches!(
        lookup_value_expiry(&mut pager, 1, b"key", STORE_FLAG_VALUE_ENVELOPE_V1, None),
        Err(EngineError::Corruption(_))
    ));
    assert!(matches!(
        lookup_stored_value(&mut pager, 1, b"key", STORE_FLAG_VALUE_ENVELOPE_V1, None),
        Err(EngineError::Corruption(_))
    ));
    assert!(matches!(
        lookup_prefix(&mut pager, 1, b"key", 16),
        Err(EngineError::Corruption(_))
    ));
    let mut writer_called = false;
    assert!(matches!(
        lookup_stored_value_into(
            &mut pager,
            1,
            b"key",
            STORE_FLAG_VALUE_ENVELOPE_V1,
            None,
            |_| {
                writer_called = true;
                Ok(())
            }
        ),
        Err(EngineError::Corruption(_))
    ));
    assert!(!writer_called);
    let mut iter = TreeIter::new(&mut pager, 1, &super::RangeSpec::default())?;
    assert!(matches!(
        iter.next(&mut pager),
        Err(EngineError::Corruption(_))
    ));
    Ok(())
}

#[test]
fn raw_external_size_uses_only_cached_descriptor() -> Result<()> {
    let mut pager = external_pager(&vec![0x53; 1024 * 1024], None)?;
    pager.read_page(1)?;
    pager.backend_mut().close()?;
    assert_eq!(
        lookup_stored_value_size(&mut pager, 1, b"key", STORE_FLAG_VALUE_ENVELOPE_V1, 1, None)?,
        Some(1024 * 1024)
    );
    Ok(())
}

#[test]
fn revision_states_preserve_expired_metadata_without_payload_reads() -> Result<()> {
    let revision = ValueRevision {
        epoch: 7,
        ordinal: 11,
    };
    let flags = store_flags_for_user_store(StoreCompression::Snappy);
    let mut pager =
        external_pager_for_store(&vec![0x53; 1024 * 1024], Some(123), Some(revision), flags)?;
    pager.read_page(1)?;
    pager.backend_mut().close()?;
    checksum_work::reset();
    assert_eq!(
        lookup_stored_value_state(&mut pager, 1, b"key", flags, 123, None)?,
        Some(ValueState {
            exists: false,
            expired: true,
            expires_at_ms: Some(123),
            revision: Some(revision),
            max_length: 0,
        })
    );
    assert_eq!(
        lookup_stored_value_size(&mut pager, 1, b"key", flags, 123, None)?,
        None
    );
    assert_eq!(
        lookup_stored_value_state(&mut pager, 1, b"missing", flags, 123, None)?,
        None
    );
    assert_eq!(
        lookup_value_expiry(&mut pager, 1, b"key", flags, None)?,
        Some(Some(123))
    );
    assert_eq!(checksum_work::hashed_bytes(), 0);
    Ok(())
}

#[test]
fn revision_point_and_scan_materializers_preserve_native_revision() -> Result<()> {
    let value = vec![0x35; 1024 * 1024];
    let revision = ValueRevision {
        epoch: 7,
        ordinal: 12,
    };
    let flags = store_flags_for_user_store(StoreCompression::None);
    let mut pager = external_pager_for_store(&value, Some(123), Some(revision), flags)?;
    pager.read_page(1)?;
    checksum_work::reset();
    assert_eq!(
        lookup_stored_value_state(&mut pager, 1, b"key", flags, 122, None)?,
        Some(ValueState {
            exists: true,
            expired: false,
            expires_at_ms: Some(123),
            revision: Some(revision),
            max_length: value.len(),
        })
    );
    assert_eq!(checksum_work::hashed_bytes(), 0);
    let stored = StoredValue::with_expiry(value, Some(123)).with_revision(revision);
    assert_eq!(
        lookup_stored_value(&mut pager, 1, b"key", flags, None)?,
        Some(stored.clone())
    );
    assert_eq!(
        checksum_work::hashed_bytes(),
        stored.value.len() + PAYLOAD_HEADER_SIZE
    );
    let mut iter = TreeIter::new(&mut pager, 1, &super::RangeSpec::default())?;
    let pair = iter
        .next(&mut pager)?
        .ok_or_else(|| EngineError::Internal("external row missing".into()))?;
    assert_eq!(
        materialize_pending_stored_value(&mut pager, pair.value, flags)?,
        stored
    );
    Ok(())
}

#[test]
fn partial_revision_metadata_is_rejected_without_payload_io() -> Result<()> {
    let flags = store_flags_for_user_store(StoreCompression::None);
    let revision = ValueRevision {
        epoch: 7,
        ordinal: 12,
    };
    let mut pager =
        external_pager_for_store(&vec![0x35; 1024 * 1024], None, Some(revision), flags)?;
    let mut leaf = decode_page(&pager.read_page(1)?)?.leaf_cells;
    leaf[0].value[8 + 24..8 + 32].fill(0);
    pager.write_page_image(1, &encode_leaf_page(1, 0, 0, &leaf)?)?;
    pager.backend_mut().close()?;
    checksum_work::reset();
    assert_eq!(
        lookup_stored_value_state(&mut pager, 1, b"key", flags, 123, None).unwrap_err(),
        EngineError::Corruption("revision envelope contains a partial revision".into())
    );
    assert_eq!(checksum_work::hashed_bytes(), 0);
    Ok(())
}
