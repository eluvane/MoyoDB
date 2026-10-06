use crate::btree::PageAllocator;
use crate::bytes::MAX_STORED_VALUE_BYTES;
use crate::error::{EngineError, Result};
use crate::page::{
    decode_overflow_body_ref, decode_page_header_verified, encode_overflow_value_chunk,
    max_overflow_chunk_len,
};
use crate::pager::{PageReadWindow, Pager};
use crate::prepared_value::ValueSource;
use crate::storage::backend::FileBackend;
use crate::value::{
    decode_value_envelope, store_uses_system_raw_values, store_uses_value_envelope,
    store_value_prefix_len, validate_store_value_prefix, StoredValue, ValueRevision,
    COMPRESSION_VALUE_HEADER_SIZE, VALUE_REVISION_ENVELOPE_HEADER_SIZE,
};

#[derive(Debug, Clone)]
pub struct OverflowChain {
    pub head_page_id: u64,
    pub pages: Vec<(u64, Vec<u8>)>,
}

pub fn write_overflow_chain(value: &[u8], alloc: &mut PageAllocator) -> Result<OverflowChain> {
    write_overflow_value(value, alloc)
}

pub(crate) fn write_overflow_value<V: ValueSource + ?Sized>(
    value: &V,
    alloc: &mut PageAllocator,
) -> Result<OverflowChain> {
    let value = value.parts();
    if value.len() == 0 {
        return Err(EngineError::Serialization(
            "overflow chain requires a non-empty value".into(),
        ));
    }
    let chunk_len = max_overflow_chunk_len();
    let chunk_count = value.len().div_ceil(chunk_len);
    let ids: Vec<u64> = (0..chunk_count).map(|_| alloc.allocate()).collect();
    let mut pages = Vec::with_capacity(chunk_count);
    for idx in 0..chunk_count {
        let page_id = ids[idx];
        let next = ids.get(idx + 1).copied().unwrap_or(0);
        pages.push((
            page_id,
            encode_overflow_value_chunk(page_id, next, &value, idx)?,
        ));
    }
    Ok(OverflowChain {
        head_page_id: ids[0],
        pages,
    })
}

fn validate_declared_len(total_len: usize) -> Result<usize> {
    if total_len == 0 || total_len > MAX_STORED_VALUE_BYTES {
        return Err(EngineError::Corruption(format!(
            "overflow value declares invalid length {total_len}"
        )));
    }
    Ok(total_len.div_ceil(max_overflow_chunk_len()))
}

/// Bounds traversal by the declared length to stop corrupt cycles.
/// A false visit result stops without validating the rest of the chain.
fn walk_chain<B: FileBackend>(
    pager: &mut Pager<B>,
    head_page_id: u64,
    total_len: usize,
    read_ahead: bool,
    mut visit: impl FnMut(u64, &[u8]) -> Result<bool>,
) -> Result<()> {
    let max_pages = validate_declared_len(total_len)?;
    // Keep short chains warm. Large bodies must not evict tree pages.
    let mut window = (read_ahead && max_pages > 64).then(|| PageReadWindow::new(max_pages));
    let mut current = head_page_id;
    let mut seen_pages = 0usize;
    let mut seen_bytes = 0usize;
    while current != 0 {
        seen_pages += 1;
        if seen_pages > max_pages {
            return Err(EngineError::Corruption(format!(
                "overflow chain at page {head_page_id} is longer than its declared length {total_len}"
            )));
        }
        let inspect = |bytes: &[u8]| {
            let header = decode_page_header_verified(bytes)?;
            if header.page_id != current {
                return Err(EngineError::Corruption(format!(
                    "overflow page header id mismatch: expected {current}, got {}",
                    header.page_id
                )));
            }
            let (next, chunk) = decode_overflow_body_ref(bytes, &header)?;
            seen_bytes = seen_bytes
                .checked_add(chunk.len())
                .filter(|len| *len <= total_len)
                .ok_or_else(|| {
                    EngineError::Corruption(format!(
                        "overflow chain exceeded expected length {total_len}"
                    ))
                })?;
            Ok((next, visit(current, chunk)?))
        };
        let (next, keep_going) = match window.as_mut() {
            Some(window) => pager.with_page_no_fill(current, window, inspect),
            None => pager.with_page(current, inspect),
        }?;
        if !keep_going {
            return Ok(());
        }
        current = next;
    }
    if seen_bytes != total_len {
        return Err(EngineError::Corruption(format!(
            "overflow chain length mismatch: expected {total_len}, got {seen_bytes}"
        )));
    }
    Ok(())
}

pub fn read_overflow_value<B: FileBackend>(
    pager: &mut Pager<B>,
    head_page_id: u64,
    total_len: usize,
) -> Result<Vec<u8>> {
    if head_page_id == 0 {
        return Err(EngineError::Corruption(
            "overflow value is missing its head page".into(),
        ));
    }
    validate_declared_len(total_len)?;
    let mut out = Vec::with_capacity(total_len);
    walk_chain(pager, head_page_id, total_len, true, |_, chunk| {
        out.extend_from_slice(chunk);
        Ok(true)
    })?;
    Ok(out)
}

/// Validates the full chain before decoding the envelope, even for expired values.
/// Keeps the header separate to avoid an extra payload copy.
pub(crate) fn read_overflow_stored_value<B: FileBackend>(
    pager: &mut Pager<B>,
    head_page_id: u64,
    total_len: usize,
    store_flags: u64,
) -> Result<StoredValue> {
    if head_page_id == 0 {
        return Err(EngineError::Corruption(
            "overflow value is missing its head page".into(),
        ));
    }
    validate_declared_len(total_len)?;
    let enveloped =
        store_uses_value_envelope(store_flags) && !store_uses_system_raw_values(store_flags);
    let payload_len = if enveloped {
        total_len.saturating_sub(store_value_prefix_len(store_flags))
    } else {
        total_len
    };
    let mut value = Vec::with_capacity(payload_len);
    let (expires_at_ms, revision) = read_overflow_stored_value_parts_into(
        pager,
        head_page_id,
        total_len,
        store_flags,
        |chunk| {
            value.extend_from_slice(chunk);
            Ok(())
        },
    )?;
    Ok(StoredValue {
        value,
        expires_at_ms,
        revision,
    })
}

/// The writer receives provisional bytes. Discard them if validation fails.
pub(crate) fn read_overflow_stored_value_into<B: FileBackend>(
    pager: &mut Pager<B>,
    head_page_id: u64,
    total_len: usize,
    store_flags: u64,
    write: impl FnMut(&[u8]) -> Result<()>,
) -> Result<Option<u64>> {
    read_overflow_stored_value_parts_into(pager, head_page_id, total_len, store_flags, write)
        .map(|(expiry, _)| expiry)
}

fn read_overflow_stored_value_parts_into<B: FileBackend>(
    pager: &mut Pager<B>,
    head_page_id: u64,
    total_len: usize,
    store_flags: u64,
    mut write: impl FnMut(&[u8]) -> Result<()>,
) -> Result<(Option<u64>, Option<ValueRevision>)> {
    if head_page_id == 0 {
        return Err(EngineError::Corruption(
            "overflow value is missing its head page".into(),
        ));
    }
    validate_declared_len(total_len)?;
    let enveloped =
        store_uses_value_envelope(store_flags) && !store_uses_system_raw_values(store_flags);
    let wanted_prefix = if enveloped {
        store_value_prefix_len(store_flags)
    } else {
        0
    };
    let mut prefix = [0; VALUE_REVISION_ENVELOPE_HEADER_SIZE];
    let mut prefix_len = 0;
    let mut payload_len = 0;
    let mut payload_prefix = [0; COMPRESSION_VALUE_HEADER_SIZE];
    let mut payload_prefix_len = 0;
    walk_chain(pager, head_page_id, total_len, true, |_, chunk| {
        let take = if enveloped {
            (wanted_prefix - prefix_len).min(chunk.len())
        } else {
            0
        };
        prefix[prefix_len..prefix_len + take].copy_from_slice(&chunk[..take]);
        prefix_len += take;
        let payload = &chunk[take..];
        let payload_take = (payload_prefix.len() - payload_prefix_len).min(payload.len());
        payload_prefix[payload_prefix_len..payload_prefix_len + payload_take]
            .copy_from_slice(&payload[..payload_take]);
        payload_prefix_len += payload_take;
        payload_len += payload.len();
        write(payload)?;
        Ok(true)
    })?;
    let envelope = if enveloped {
        decode_value_envelope(store_flags, &prefix[..prefix_len])?
    } else {
        (None, None)
    };
    if !store_uses_system_raw_values(store_flags) {
        validate_store_value_prefix(
            &payload_prefix[..payload_prefix_len],
            payload_len,
            store_flags,
        )
        .map_err(|error| match error {
            EngineError::Corruption(_) => error,
            error => EngineError::Corruption(error.to_string()),
        })?;
    }
    Ok(envelope)
}

/// Reads the value header. It may span chunks, and later pages are not checked.
pub(crate) fn read_overflow_expiry<B: FileBackend>(
    pager: &mut Pager<B>,
    head_page_id: u64,
    total_len: usize,
    store_flags: u64,
) -> Result<Option<u64>> {
    read_overflow_value_envelope(pager, head_page_id, total_len, store_flags)
        .map(|(expiry, _)| expiry)
}

pub(crate) fn read_overflow_value_envelope<B: FileBackend>(
    pager: &mut Pager<B>,
    head_page_id: u64,
    total_len: usize,
    store_flags: u64,
) -> Result<(Option<u64>, Option<ValueRevision>)> {
    if head_page_id == 0 {
        return Err(EngineError::Corruption(
            "overflow value is missing its head page".into(),
        ));
    }
    let wanted = store_value_prefix_len(store_flags).min(total_len);
    let mut prefix = [0; VALUE_REVISION_ENVELOPE_HEADER_SIZE];
    let mut prefix_len = 0;
    if wanted > 0 {
        walk_chain(pager, head_page_id, total_len, false, |_, chunk| {
            let take = (wanted - prefix_len).min(chunk.len());
            prefix[prefix_len..prefix_len + take].copy_from_slice(&chunk[..take]);
            prefix_len += take;
            Ok(prefix_len < wanted)
        })?;
    }
    if store_uses_value_envelope(store_flags) && !store_uses_system_raw_values(store_flags) {
        decode_value_envelope(store_flags, &prefix[..prefix_len])
    } else {
        Ok((None, None))
    }
}

/// Reads at most `prefix_len` leading bytes. Pages after the prefix are not checked.
pub fn read_overflow_prefix<B: FileBackend>(
    pager: &mut Pager<B>,
    head_page_id: u64,
    total_len: usize,
    prefix_len: usize,
) -> Result<Vec<u8>> {
    if head_page_id == 0 {
        return Err(EngineError::Corruption(
            "overflow value is missing its head page".into(),
        ));
    }
    let wanted = prefix_len.min(total_len);
    let mut out = Vec::with_capacity(wanted);
    if wanted == 0 {
        return Ok(out);
    }
    walk_chain(pager, head_page_id, total_len, false, |_, chunk| {
        let take = (wanted - out.len()).min(chunk.len());
        out.extend_from_slice(&chunk[..take]);
        Ok(out.len() < wanted)
    })?;
    Ok(out)
}

/// Retires every page of a committed chain.
pub fn free_overflow_chain<B: FileBackend>(
    pager: &mut Pager<B>,
    head_page_id: u64,
    total_len: usize,
    alloc: &mut PageAllocator,
) -> Result<()> {
    walk_chain(pager, head_page_id, total_len, true, |page_id, _| {
        alloc.free(page_id);
        Ok(true)
    })
}

#[cfg(test)]
mod tests {
    use super::{
        read_overflow_stored_value, read_overflow_stored_value_into, read_overflow_value_envelope,
    };
    use crate::error::{EngineError, Result};
    use crate::layout::{page_offset, PAGE_SIZE};
    use crate::page::{encode_overflow_page, max_overflow_chunk_len};
    use crate::pager::Pager;
    use crate::storage::memory::MemoryBackend;
    use crate::value::{
        store_flags_for_user_store, StoreCompression, StoredValue, ValueRevision,
        STORE_FLAG_VALUE_ENVELOPE_V1,
    };

    fn pager(images: &[(u64, Vec<u8>)]) -> Result<Pager<MemoryBackend>> {
        let mut main = MemoryBackend::new();
        for (page, image) in images {
            main.write_at(page_offset(*page), image)?;
        }
        Ok(Pager::new(main, 1))
    }

    #[test]
    fn stored_sink_separates_envelope_that_spans_chunks() -> Result<()> {
        let value = vec![0x53; max_overflow_chunk_len() - 8];
        let stored = StoredValue::with_expiry(value.clone(), Some(123));
        let raw = stored.encode_for_store(STORE_FLAG_VALUE_ENVELOPE_V1)?;
        let images = [
            (1, encode_overflow_page(1, 2, &raw[..8])?),
            (2, encode_overflow_page(2, 0, &raw[8..])?),
        ];
        let mut reader = pager(&images)?;
        let mut output = Vec::new();
        let expiry = read_overflow_stored_value_into(
            &mut reader,
            1,
            raw.len(),
            STORE_FLAG_VALUE_ENVELOPE_V1,
            |chunk| {
                output.extend_from_slice(chunk);
                Ok(())
            },
        )?;
        assert_eq!(expiry, Some(123));
        assert_eq!(output, value);
        assert_eq!(
            read_overflow_stored_value(
                &mut pager(&images)?,
                1,
                raw.len(),
                STORE_FLAG_VALUE_ENVELOPE_V1,
            )?,
            stored
        );
        Ok(())
    }

    #[test]
    fn revision_envelope_spans_chunks_and_is_retained_by_owned_reader() -> Result<()> {
        let flags = store_flags_for_user_store(StoreCompression::None);
        let revision = ValueRevision {
            epoch: 13,
            ordinal: 17,
        };
        let stored = StoredValue::with_expiry(vec![0x53; max_overflow_chunk_len() - 8], Some(123))
            .with_revision(revision);
        let raw = stored.encode_for_store(flags)?;
        let images = [
            (1, encode_overflow_page(1, 2, &raw[..24])?),
            (2, encode_overflow_page(2, 0, &raw[24..])?),
        ];
        assert_eq!(
            read_overflow_value_envelope(&mut pager(&images)?, 1, raw.len(), flags)?,
            (Some(123), Some(revision))
        );
        assert_eq!(
            read_overflow_stored_value(&mut pager(&images)?, 1, raw.len(), flags)?,
            stored
        );
        Ok(())
    }

    #[test]
    fn revision_metadata_stops_before_corrupt_tail_but_full_read_rejects_it() -> Result<()> {
        let flags = store_flags_for_user_store(StoreCompression::None);
        let revision = ValueRevision {
            epoch: 13,
            ordinal: 18,
        };
        let stored = StoredValue::with_expiry(vec![0x53; max_overflow_chunk_len() * 2], Some(1))
            .with_revision(revision);
        let raw = stored.encode_for_store(flags)?;
        let chunk_len = max_overflow_chunk_len();
        let mut tail = encode_overflow_page(3, 0, &raw[chunk_len * 2..])?;
        tail[PAGE_SIZE - 1] ^= 0x80;
        let images = [
            (1, encode_overflow_page(1, 2, &raw[..chunk_len])?),
            (
                2,
                encode_overflow_page(2, 3, &raw[chunk_len..chunk_len * 2])?,
            ),
            (3, tail),
        ];
        assert_eq!(
            read_overflow_value_envelope(&mut pager(&images)?, 1, raw.len(), flags)?,
            (Some(1), Some(revision))
        );
        assert!(matches!(
            read_overflow_stored_value(&mut pager(&images)?, 1, raw.len(), flags),
            Err(EngineError::Corruption(message)) if message.contains("checksum")
        ));
        Ok(())
    }

    #[test]
    fn stored_sink_checks_expired_tail_before_envelope_errors() -> Result<()> {
        let stored = StoredValue::with_expiry(vec![0x71; max_overflow_chunk_len()], Some(1));
        let mut raw = stored.encode_for_store(STORE_FLAG_VALUE_ENVELOPE_V1)?;
        raw[0] ^= 0x80;
        let mut tail = encode_overflow_page(2, 0, &raw[max_overflow_chunk_len()..])?;
        tail[PAGE_SIZE - 1] ^= 0x80;
        let images = [
            (
                1,
                encode_overflow_page(1, 2, &raw[..max_overflow_chunk_len()])?,
            ),
            (2, tail),
        ];
        let error = read_overflow_stored_value_into(
            &mut pager(&images)?,
            1,
            raw.len(),
            STORE_FLAG_VALUE_ENVELOPE_V1,
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(matches!(error, EngineError::Corruption(message) if message.contains("checksum")));
        Ok(())
    }

    #[test]
    fn stored_sink_propagates_callback_failure() -> Result<()> {
        let raw = StoredValue::plain(vec![0x35; max_overflow_chunk_len()])
            .encode_for_store(STORE_FLAG_VALUE_ENVELOPE_V1)?;
        let images = [(
            1,
            encode_overflow_page(1, 2, &raw[..max_overflow_chunk_len()])?,
        )];
        let error = read_overflow_stored_value_into(
            &mut pager(&images)?,
            1,
            raw.len(),
            STORE_FLAG_VALUE_ENVELOPE_V1,
            |_| Err(EngineError::Storage("sink failed".into())),
        )
        .unwrap_err();
        assert_eq!(error, EngineError::Storage("sink failed".into()));
        Ok(())
    }

    #[test]
    fn compressed_header_across_chunks_is_checked_after_full_chain() -> Result<()> {
        let value = vec![0x71; max_overflow_chunk_len() - 17];
        let mut frame = Vec::new();
        frame.extend_from_slice(b"BDBZVAL1");
        frame.extend_from_slice(&[1, 0]);
        frame.extend_from_slice(&(value.len() as u32).to_le_bytes());
        frame.extend_from_slice(&crc32fast::hash(&value).to_le_bytes());
        frame.extend_from_slice(&value);
        for flags in [
            STORE_FLAG_VALUE_ENVELOPE_V1
                | (StoreCompression::Snappy.bits() << crate::value::STORE_FLAG_COMPRESSION_SHIFT),
            store_flags_for_user_store(StoreCompression::Snappy),
        ] {
            let raw = StoredValue::plain(frame.clone()).encode_for_store(flags)?;
            let prefix_len = crate::value::store_value_prefix_len(flags);
            let split = prefix_len + 1;
            let images = [
                (1, encode_overflow_page(1, 2, &raw[..split])?),
                (2, encode_overflow_page(2, 0, &raw[split..])?),
            ];
            assert_eq!(
                read_overflow_stored_value(&mut pager(&images)?, 1, raw.len(), flags)?.value,
                frame
            );
            let mut invalid = raw;
            invalid[prefix_len + 10..prefix_len + 14]
                .copy_from_slice(&(crate::bytes::MAX_VALUE_BYTES as u32 + 1).to_le_bytes());
            let images = [
                (1, encode_overflow_page(1, 2, &invalid[..split])?),
                (2, encode_overflow_page(2, 0, &invalid[split..])?),
            ];
            assert!(matches!(
                read_overflow_stored_value(&mut pager(&images)?, 1, invalid.len(), flags),
                Err(EngineError::Corruption(_))
            ));
        }
        Ok(())
    }
}
