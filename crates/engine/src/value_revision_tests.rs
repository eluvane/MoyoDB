use super::*;

#[test]
fn revision_envelopes_preserve_legacy_bytes_and_validate_native_metadata() -> Result<()> {
    let flags = store_flags_for_user_store(StoreCompression::None);
    let revision = ValueRevision {
        epoch: u64::MAX,
        ordinal: u64::MAX,
    };
    let value =
        StoredValue::with_expiry(b"BDREV001-user-body".to_vec(), Some(17)).with_revision(revision);
    let bytes = value.encode_for_store(flags)?;
    assert_eq!(
        bytes.len(),
        VALUE_REVISION_ENVELOPE_HEADER_SIZE + value.value.len()
    );
    assert_eq!(StoredValue::decode_for_store(flags, &bytes)?, value);
    assert_eq!(
        StoredValue::decode_owned_for_store(flags, bytes.clone())?,
        value
    );
    for end in 0..VALUE_REVISION_ENVELOPE_HEADER_SIZE {
        assert!(matches!(
            decode_value_envelope(flags, &bytes[..end]),
            Err(EngineError::Corruption(_))
        ));
    }
    for range in [16..24, 24..32] {
        let mut corrupt = bytes.clone();
        corrupt[range].fill(0);
        assert!(matches!(
            StoredValue::decode_for_store(flags, &corrupt),
            Err(EngineError::Corruption(_))
        ));
    }
    let mut unknown = bytes.clone();
    unknown[16..32].fill(0);
    assert_eq!(
        StoredValue::decode_owned_for_store(flags, unknown)?.revision,
        None
    );
    let legacy = value.encode_for_store(STORE_FLAG_VALUE_ENVELOPE_V1)?;
    assert_eq!(&legacy[..8], &VALUE_ENVELOPE_MAGIC);
    assert_eq!(legacy.len(), VALUE_ENVELOPE_HEADER_SIZE + value.value.len());
    assert_eq!(
        StoredValue::decode_owned_for_store(STORE_FLAG_VALUE_ENVELOPE_V1, legacy)?.revision,
        None
    );
    let raw = StoredValue::plain(b"BDREV001-raw-body".to_vec());
    assert_eq!(StoredValue::decode_for_store(0, &raw.value)?, raw);
    assert!(validate_store_flags(STORE_FLAG_VALUE_REVISION).is_ok());
    assert_eq!(
        StoredValue::decode_for_store(STORE_FLAG_VALUE_REVISION, &bytes)?,
        value
    );
    assert!(
        validate_store_flags(STORE_FLAG_VALUE_REVISION | STORE_FLAG_SYSTEM_RAW_VALUES).is_err()
    );
    Ok(())
}
