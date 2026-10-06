use super::*;
use crate::checksum::work;
use crate::layout::ValueKind;
use crate::page::{
    decode_page, encode_internal_page, encode_leaf_page, encode_overflow_page,
    max_overflow_chunk_len, InternalCell, LeafCell,
};
use crate::storage::memory::{MemoryBackend, MemoryBundle};
use crate::{Engine, OpenConfig, TxMode};

fn generated_images(count: usize) -> Result<Vec<(u64, Vec<u8>)>> {
    let mut state = 0x8723_491bu32;
    (0..count)
        .map(|index| {
            let page_id = if index % 7 == 0 {
                u32::MAX as u64 + index as u64 + 1
            } else {
                index as u64 * 3 + 1
            };
            let len = (index * 137 + 19) % (max_overflow_chunk_len() + 1);
            let value: Vec<u8> = (0..len)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    state as u8
                })
                .collect();
            let image = match index % 3 {
                0 => {
                    let value = value[..value.len().min(512)].to_vec();
                    encode_leaf_page(
                        page_id,
                        0,
                        0,
                        &[LeafCell {
                            key: page_id.to_be_bytes().to_vec(),
                            total_value_len: value.len() as u32,
                            value,
                            value_kind: ValueKind::Inline,
                            overflow_head_page_id: 0,
                        }],
                    )?
                }
                1 => encode_internal_page(
                    page_id,
                    1,
                    0,
                    &[
                        InternalCell {
                            separator: b"a".to_vec(),
                            child_page_id: 1,
                        },
                        InternalCell {
                            separator: b"z".to_vec(),
                            child_page_id: page_id - 1,
                        },
                    ],
                )?,
                _ => {
                    let chunk = if value.is_empty() { vec![0] } else { value };
                    encode_overflow_page(page_id, 0, &chunk)?
                }
            };
            Ok((page_id, image))
        })
        .collect()
}

fn commit_for(txid: u64, images: &[(u64, Vec<u8>)]) -> CommitRecord {
    CommitRecord {
        txid,
        new_catalog_root_page_id: images.first().map(|image| image.0).unwrap_or(1),
        new_next_page_id: images.iter().map(|image| image.0).max().unwrap_or(1) + 1,
        changed_page_count: images.len() as u32,
    }
}

#[test]
fn generated_wal_is_byte_identical_to_generic_encoder() -> Result<()> {
    for count in [0, 1, 2, 17, 257] {
        let images = generated_images(count)?;
        for txid in [1, u32::MAX as u64 + 7, u64::MAX] {
            let commit = commit_for(txid, &images);
            for initial_offset in [0, 23] {
                let mut generic = MemoryBackend::new();
                let mut generated = MemoryBackend::new();
                let prefix = vec![0xa7; initial_offset];
                generic.write_at(0, &prefix)?;
                generated.write_at(0, &prefix)?;
                let mut generic_offset = initial_offset as u64;
                let mut generated_offset = initial_offset as u64;
                append_transaction(&mut generic, &mut generic_offset, txid, &images, &commit)?;
                append_generated_transaction(
                    &mut generated,
                    &mut generated_offset,
                    txid,
                    &images,
                    &commit,
                )?;
                assert_eq!(generated_offset, generic_offset);
                assert_eq!(
                    generated.read_at(0, generated_offset as usize)?,
                    generic.read_at(0, generic_offset as usize)?,
                    "pages={count}, txid={txid}, offset={initial_offset}"
                );
                if initial_offset == 0 {
                    let transactions = scan_wal(&generated)?;
                    assert_eq!(transactions.len(), 1);
                    assert_eq!(transactions[0].txid, txid);
                    let recovered: Vec<_> = transactions[0]
                        .page_images
                        .iter()
                        .map(|page| (page.page_id, page.bytes.clone()))
                        .collect();
                    assert_eq!(recovered, images);
                }
            }
        }
    }
    Ok(())
}

#[test]
fn generated_wal_hashes_only_record_prefixes_and_commit() -> Result<()> {
    let prefix_len = WAL_RECORD_HEADER_SIZE + WAL_PAGE_IMAGE_BODY_HEADER_SIZE;
    let commit_len = wal_record_total_len(WAL_COMMIT_BODY_SIZE);
    for count in [1, 17, 257] {
        let images = generated_images(count)?;
        let commit = commit_for(11, &images);
        let mut generic = MemoryBackend::new();
        work::reset();
        append_transaction(&mut generic, &mut 0, 11, &images, &commit)?;
        let before = work::hashed_bytes();
        let mut generated = MemoryBackend::new();
        work::reset();
        append_generated_transaction(&mut generated, &mut 0, 11, &images, &commit)?;
        let after = work::hashed_bytes();
        assert_eq!(before, count * (prefix_len + PAGE_SIZE) + commit_len);
        assert_eq!(after, count * prefix_len + commit_len);
        assert_eq!(before - after, count * PAGE_SIZE);
    }
    Ok(())
}

#[test]
fn engine_commit_hashes_each_generated_page_payload_once() -> Result<()> {
    let prefix_len = WAL_RECORD_HEADER_SIZE + WAL_PAGE_IMAGE_BODY_HEADER_SIZE;
    for count in [1, 8, 100] {
        let bundle = MemoryBundle::new();
        let mut engine = Engine::open(
            "generated-wal-work",
            bundle.files(),
            OpenConfig {
                checkpoint_wal_bytes: u64::MAX,
                checkpoint_dirty_pages: usize::MAX,
                ..OpenConfig::default()
            },
        )?;
        let setup = engine.begin_tx(TxMode::Readwrite)?;
        // Disable change-feed hashing to measure page checksum reuse.
        engine.set_change_feed_policy(
            setup,
            crate::catalog::ChangeFeedPolicy {
                enabled: false,
                ..crate::catalog::ChangeFeedPolicy::default()
            },
        )?;
        engine.create_store(setup, "kv")?;
        engine.commit_tx(setup)?;
        let tx = engine.begin_tx(TxMode::Readwrite)?;
        let value = vec![0x31; 16 * 1024];
        for key in 0u32..count {
            engine.put(tx, "kv", &key.to_be_bytes(), &value)?;
        }
        work::reset();
        let txid = engine.commit_tx(tx)?;
        let hashed = work::hashed_bytes();
        let transactions = scan_wal(&bundle.wal)?;
        let committed = transactions.last().expect("a durable commit");
        assert_eq!(committed.txid, txid);
        let decoded_pages = committed
            .page_images
            .iter()
            .map(|page| decode_page(&page.bytes))
            .collect::<Result<Vec<_>>>()?;
        let overflow_values = decoded_pages
            .iter()
            .flat_map(|page| &page.leaf_cells)
            .filter(|cell| cell.value_kind == ValueKind::Overflow)
            .count();
        assert_eq!(overflow_values, count as usize);
        let pages = committed.page_images.len();
        // Page creation hashes each output page once. WAL encoding hashes record
        // prefixes and the commit record; cached input pages are not rehashed.
        assert_eq!(
            hashed,
            pages * (PAGE_SIZE + prefix_len) + wal_record_total_len(WAL_COMMIT_BODY_SIZE),
            "values={count}, pages={pages}"
        );
    }
    Ok(())
}

#[test]
fn generic_transaction_still_checksums_corrupt_page_bytes() -> Result<()> {
    let mut images = generated_images(1)?;
    images[0].1[PAGE_SIZE - 1] ^= 0x80;
    let commit = commit_for(1, &images);
    let mut wal = MemoryBackend::new();
    append_transaction(&mut wal, &mut 0, 1, &images, &commit)?;
    // The WAL record is valid, so a corrupt committed page must be reported,
    // not mistaken for an incomplete WAL tail and silently discarded.
    assert!(matches!(scan_wal(&wal), Err(EngineError::Corruption(_))));
    Ok(())
}

#[derive(Default)]
struct RecordingBackend {
    inner: MemoryBackend,
    writes: Vec<(u64, usize)>,
    flushes: usize,
    fail_after: Option<usize>,
}

impl FileBackend for RecordingBackend {
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.inner.read_at(offset, len)
    }
    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        self.writes.push((offset, bytes.len()));
        if let Some(limit) = self.fail_after {
            self.inner
                .write_at(offset, &bytes[..bytes.len().min(limit)])?;
            return Err(EngineError::Storage("injected partial WAL append".into()));
        }
        self.inner.write_at(offset, bytes)
    }
    fn flush(&mut self) -> Result<()> {
        self.flushes += 1;
        self.inner.flush()
    }
    fn len(&self) -> Result<u64> {
        self.inner.len()
    }
    fn truncate(&mut self, size: u64) -> Result<()> {
        self.inner.truncate(size)
    }
    fn close(&mut self) -> Result<()> {
        self.inner.close()
    }
}

#[test]
fn generated_append_preserves_validation_and_partial_write_order() -> Result<()> {
    let images = generated_images(3)?;
    let commit = commit_for(13, &images);
    let mut invalid_count = commit.clone();
    invalid_count.changed_page_count += 1;
    let mut invalid_txid = commit.clone();
    invalid_txid.txid += 1;
    let mut invalid_images = images.clone();
    invalid_images[2].1.pop();
    for (images, commit) in [
        (&images, &invalid_count),
        (&images, &invalid_txid),
        (&invalid_images, &commit),
    ] {
        let mut wal = RecordingBackend::default();
        let mut offset = 23;
        assert!(matches!(
            append_generated_transaction(&mut wal, &mut offset, 13, images, commit),
            Err(EngineError::Serialization(_))
        ));
        assert!(wal.writes.is_empty());
        assert_eq!(wal.flushes, 0);
        assert_eq!(offset, 23);
    }
    for limit in [0, 17, PAGE_SIZE + 50] {
        let mut generic = RecordingBackend {
            fail_after: Some(limit),
            ..Default::default()
        };
        let mut generated = RecordingBackend {
            fail_after: Some(limit),
            ..Default::default()
        };
        let mut generic_offset = 23;
        let mut generated_offset = 23;
        let before = append_transaction(&mut generic, &mut generic_offset, 13, &images, &commit);
        let after = append_generated_transaction(
            &mut generated,
            &mut generated_offset,
            13,
            &images,
            &commit,
        );
        assert_eq!(before, after);
        assert!(matches!(after, Err(EngineError::Storage(_))));
        assert_eq!((generic_offset, generated_offset), (23, 23));
        assert_eq!(generated.writes, generic.writes);
        assert_eq!(generated.writes.len(), 1);
        assert_eq!((generic.flushes, generated.flushes), (0, 0));
        let len = generic.len()? as usize;
        assert_eq!(generated.read_at(0, len)?, generic.read_at(0, len)?);
    }
    Ok(())
}
