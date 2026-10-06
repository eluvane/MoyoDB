mod read_buffer;

#[cfg(test)]
mod encoding_tests;
#[cfg(test)]
mod generated_tests;

use crate::bytes::read_u32_le;
use crate::checksum::{checksum_with_zeroed_region, crc32_with_generated_page};
use crate::error::{EngineError, Result};
use crate::layout::{
    unsafe_read_struct, wal_record_total_len, WalCommitBody, WalPageImageBodyHeader,
    WalRecordHeader, WalTag, PAGE_HEADER_CHECKSUM_OFFSET, PAGE_SIZE, WAL_COMMIT_BODY_SIZE,
    WAL_MAGIC, WAL_PAGE_IMAGE_BODY_HEADER_SIZE, WAL_RECORD_CHECKSUM_OFFSET, WAL_RECORD_HEADER_SIZE,
};
use crate::page::verify_page_image;
use crate::pager::{Pager, PAGE_WRITE_BATCH_PAGES};
use crate::storage::backend::FileBackend;
use read_buffer::WalReadBuffer;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use zerocopy::IntoBytes;

const WAL_ENCODING_BATCH_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PageImageRecord {
    pub txid: u64,
    pub page_id: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommitRecord {
    pub txid: u64,
    pub new_catalog_root_page_id: u64,
    pub new_next_page_id: u64,
    pub changed_page_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WalRecord {
    PageImage(PageImageRecord),
    Commit(CommitRecord),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplayTransaction {
    pub txid: u64,
    pub page_images: Vec<PageImageRecord>,
    pub commit: CommitRecord,
    pub end_offset: u64,
}

pub fn append_page_image_record<B: FileBackend>(
    wal: &mut B,
    offset: &mut u64,
    txid: u64,
    page_id: u64,
    bytes: &[u8],
) -> Result<()> {
    let mut record = Vec::with_capacity(wal_record_total_len(
        WAL_PAGE_IMAGE_BODY_HEADER_SIZE + bytes.len(),
    ));
    encode_page_image_record_into::<false>(&mut record, txid, page_id, bytes)?;
    wal.write_at(*offset, &record)?;
    *offset += record.len() as u64;
    Ok(())
}

pub fn append_commit_record<B: FileBackend>(
    wal: &mut B,
    offset: &mut u64,
    commit: CommitRecord,
) -> Result<()> {
    let mut record = Vec::with_capacity(wal_record_total_len(WAL_COMMIT_BODY_SIZE));
    encode_commit_record_into(&mut record, &commit);
    wal.write_at(*offset, &record)?;
    *offset += record.len() as u64;
    Ok(())
}

pub fn append_transaction<B: FileBackend>(
    wal: &mut B,
    offset: &mut u64,
    txid: u64,
    page_images: &[(u64, Vec<u8>)],
    commit: &CommitRecord,
) -> Result<()> {
    append_transaction_inner::<false, B>(wal, offset, txid, page_images, commit)
}

/// Requires unchanged page images from the engine's page encoders.
/// Reuses their page checksums for the WAL checksum.
/// Use `append_transaction` for external images.
pub(crate) fn append_generated_transaction<B: FileBackend>(
    wal: &mut B,
    offset: &mut u64,
    txid: u64,
    page_images: &[(u64, Vec<u8>)],
    commit: &CommitRecord,
) -> Result<()> {
    append_transaction_inner::<true, B>(wal, offset, txid, page_images, commit)
}

fn append_transaction_inner<const GENERATED: bool, B: FileBackend>(
    wal: &mut B,
    offset: &mut u64,
    txid: u64,
    page_images: &[(u64, Vec<u8>)],
    commit: &CommitRecord,
) -> Result<()> {
    if commit.txid != txid {
        return Err(EngineError::Serialization(format!(
            "wal commit txid mismatch: commit={} batch={}",
            commit.txid, txid
        )));
    }
    if commit.changed_page_count as usize != page_images.len() {
        return Err(EngineError::Serialization(format!(
            "wal commit page count mismatch: commit={} batch={}",
            commit.changed_page_count,
            page_images.len()
        )));
    }

    // Invalid image lengths must fail before the first WAL write.
    for (_, bytes) in page_images {
        if bytes.len() != PAGE_SIZE {
            return Err(EngineError::Serialization(format!(
                "wal page image wrong size: {}",
                bytes.len()
            )));
        }
    }

    let page_record_len = wal_record_total_len(WAL_PAGE_IMAGE_BODY_HEADER_SIZE + PAGE_SIZE);
    let commit_record_len = wal_record_total_len(WAL_COMMIT_BODY_SIZE);
    let capacity = page_images
        .len()
        .saturating_mul(page_record_len)
        .saturating_add(commit_record_len)
        .min(WAL_ENCODING_BATCH_BYTES);
    let mut batch = Vec::with_capacity(capacity);
    for (page_id, bytes) in page_images {
        if batch.len() + page_record_len > WAL_ENCODING_BATCH_BYTES {
            append_encoded_batch(wal, offset, &mut batch)?;
        }
        encode_page_image_record_into::<GENERATED>(&mut batch, txid, *page_id, bytes)?;
    }
    if batch.len() + commit_record_len > WAL_ENCODING_BATCH_BYTES {
        append_encoded_batch(wal, offset, &mut batch)?;
    }
    encode_commit_record_into(&mut batch, commit);
    append_encoded_batch(wal, offset, &mut batch)
}

fn append_encoded_batch<B: FileBackend>(
    wal: &mut B,
    offset: &mut u64,
    batch: &mut Vec<u8>,
) -> Result<()> {
    #[cfg(test)]
    encoding_tests::observe_batch(batch.len(), batch.capacity());

    wal.write_at(*offset, batch)?;
    *offset += batch.len() as u64;
    batch.clear();
    Ok(())
}

/// A committed transaction with page images referenced by WAL offsets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalTransaction {
    pub txid: u64,
    pub commit: CommitRecord,
    /// `(page_id, offset of the page image bytes in the WAL)`.
    pub pages: Vec<(u64, u64)>,
    pub end_offset: u64,
}

const PAGE_IMAGE_PAYLOAD_LEN: usize = WAL_PAGE_IMAGE_BODY_HEADER_SIZE + PAGE_SIZE;

/// Scans the committed log with bounded sequential read buffers.
/// Only records across chunk boundaries are copied. Returned descriptors grow
/// with the committed log.
///
/// Checksum failures, truncated records, and incomplete batches end the log.
/// Invalid committed page images, out-of-range page ids, or non-increasing
/// transaction ids are corruption. Page validation errors are reported only
/// after a matching commit. An uncommitted tail is never applied.
pub fn scan_wal_index<B: FileBackend>(wal: &B) -> Result<Vec<WalTransaction>> {
    let mut committed = Vec::new();
    #[cfg(test)]
    let mut retained_page_offsets = 0usize;
    visit_wal_transactions(wal, |commit, pages, end_offset| {
        #[cfg(test)]
        {
            retained_page_offsets += pages.len();
        }
        committed.push(WalTransaction {
            txid: commit.txid,
            commit,
            pages: std::mem::take(pages),
            end_offset,
        });
        #[cfg(test)]
        scan_work::retain_committed(committed.len(), retained_page_offsets);
        Ok(())
    })?;
    Ok(committed)
}

/// Visits validated transactions. Does not visit an unfinished tail.
/// The visitor may take the page offsets or leave the buffer for reuse.
pub(crate) fn visit_wal_transactions<B: FileBackend>(
    wal: &B,
    mut on_commit: impl FnMut(CommitRecord, &mut Vec<(u64, u64)>, u64) -> Result<()>,
) -> Result<()> {
    let len = wal.len()?;
    let mut reader = WalReadBuffer::new(wal, len);
    let mut offset = 0u64;
    let mut pending_txid: Option<u64> = None;
    let mut pending_pages: Vec<(u64, u64)> = Vec::new();
    let mut pending_invalid: Option<EngineError> = None;
    let mut last_txid = 0u64;
    let mut last_next_page_id: Option<u64> = None;

    while len.saturating_sub(offset) >= WAL_RECORD_HEADER_SIZE as u64 {
        let header: WalRecordHeader = {
            let bytes = reader.read(offset, WAL_RECORD_HEADER_SIZE)?;
            if bytes[..4] != WAL_MAGIC
                && !(bytes[..3] == WAL_MAGIC[..3] && bytes[3].is_ascii_digit())
            {
                break;
            }
            unsafe_read_struct(&bytes)?
        };
        if header.magic != WAL_MAGIC {
            // A complete foreign-version record must not be discarded as a torn tail.
            if foreign_wal_checksum_matches(&mut reader, offset, len, &header)? {
                return Err(wal_corruption(
                    offset,
                    &format!("unsupported WAL format version {}", header.magic[3] as char),
                ));
            }
            break;
        }
        let payload_len = u32::from_le(header.payload_len) as usize;
        // A checksum-valid record this reader does not understand is not a torn
        // tail. Truncating it would destroy a future format and anything after it.
        let tag = match WalTag::from_u8(header.tag) {
            Ok(tag) => tag,
            Err(_) => {
                if foreign_wal_checksum_matches(&mut reader, offset, len, &header)? {
                    return Err(wal_corruption(
                        offset,
                        &format!("unsupported WAL record tag {}", header.tag),
                    ));
                }
                break;
            }
        };
        let expected_payload_len = match tag {
            WalTag::PageImage => PAGE_IMAGE_PAYLOAD_LEN,
            WalTag::Commit => WAL_COMMIT_BODY_SIZE,
        };
        if payload_len != expected_payload_len {
            if foreign_wal_checksum_matches(&mut reader, offset, len, &header)? {
                return Err(wal_corruption(
                    offset,
                    &format!("unsupported WAL payload length {payload_len}"),
                ));
            }
            break;
        }
        let total_len = wal_record_total_len(payload_len) as u64;
        if total_len > len - offset {
            break;
        }
        let record = reader.read(offset, total_len as usize)?;
        let expected = checksum_with_zeroed_region(&record, WAL_RECORD_CHECKSUM_OFFSET, 4);
        if expected != u32::from_le(header.checksum) {
            break;
        }
        if header.reserved0 != 0 || header.reserved1 != 0 || header.reserved2 != 0 {
            return Err(wal_corruption(offset, "unsupported WAL header flags"));
        }
        let payload = &record[WAL_RECORD_HEADER_SIZE..];
        match tag {
            WalTag::PageImage => {
                let body: WalPageImageBodyHeader =
                    unsafe_read_struct(&payload[..WAL_PAGE_IMAGE_BODY_HEADER_SIZE])?;
                if u32::from_le(body.reserved) != 0 {
                    return Err(wal_corruption(offset, "unsupported WAL page image flags"));
                }
                let txid = u64::from_le(body.txid);
                let page_id = u64::from_le(body.page_id);
                match pending_txid {
                    Some(current) if current != txid => break,
                    _ => pending_txid = Some(txid),
                }
                if pending_invalid.is_none() {
                    pending_invalid = if u32::from_le(body.page_len) as usize != PAGE_SIZE {
                        Some(wal_corruption(offset, "page image length is not a page"))
                    } else {
                        verify_page_image(&payload[WAL_PAGE_IMAGE_BODY_HEADER_SIZE..], page_id)
                            .err()
                            .map(|err| wal_corruption(offset, &err.to_string()))
                    };
                }
                #[cfg(test)]
                if pending_pages.len() == pending_pages.capacity() {
                    scan_work::pending_page_growth();
                }
                pending_pages.push((
                    page_id,
                    offset + (WAL_RECORD_HEADER_SIZE + WAL_PAGE_IMAGE_BODY_HEADER_SIZE) as u64,
                ));
            }
            WalTag::Commit => {
                let body: WalCommitBody = unsafe_read_struct(&payload[..WAL_COMMIT_BODY_SIZE])?;
                if u32::from_le(body.reserved) != 0 {
                    return Err(wal_corruption(offset, "unsupported WAL commit flags"));
                }
                let commit = CommitRecord {
                    txid: u64::from_le(body.txid),
                    new_catalog_root_page_id: u64::from_le(body.new_catalog_root_page_id),
                    new_next_page_id: u64::from_le(body.new_next_page_id),
                    changed_page_count: u32::from_le(body.changed_page_count),
                };
                let batch_matches = match pending_txid {
                    Some(txid) => txid == commit.txid,
                    None => commit.changed_page_count == 0,
                };
                if !batch_matches || pending_pages.len() != commit.changed_page_count as usize {
                    break;
                }
                if let Some(err) = pending_invalid.take() {
                    return Err(err);
                }
                if commit.txid <= last_txid {
                    return Err(wal_corruption(offset, "transaction ids go backwards"));
                }
                if commit.new_catalog_root_page_id == 0
                    || commit.new_catalog_root_page_id >= commit.new_next_page_id
                {
                    return Err(wal_corruption(offset, "catalog root is outside the file"));
                }
                if let Some((page_id, _)) = pending_pages
                    .iter()
                    .find(|(page_id, _)| *page_id >= commit.new_next_page_id)
                {
                    return Err(wal_corruption(
                        offset,
                        &format!(
                            "page {page_id} is beyond next page id {}",
                            commit.new_next_page_id
                        ),
                    ));
                }
                if last_next_page_id.is_some_and(|previous| commit.new_next_page_id < previous) {
                    return Err(wal_corruption(offset, "next page id went backwards"));
                }
                last_txid = commit.txid;
                last_next_page_id = Some(commit.new_next_page_id);
                pending_txid = None;
                on_commit(commit, &mut pending_pages, offset + total_len)?;
                pending_pages.clear();
            }
        }
        offset += total_len;
    }
    Ok(())
}

fn foreign_wal_checksum_matches<B: FileBackend>(
    reader: &mut WalReadBuffer<'_, B>,
    offset: u64,
    file_len: u64,
    header: &WalRecordHeader,
) -> Result<bool> {
    let total_len = WAL_RECORD_HEADER_SIZE as u64 + u32::from_le(header.payload_len) as u64;
    if total_len > file_len - offset {
        return Ok(false);
    }
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&header.as_bytes()[..WAL_RECORD_CHECKSUM_OFFSET]);
    hasher.update(&[0; 4]);
    let mut position = offset + WAL_RECORD_HEADER_SIZE as u64;
    let end = offset + total_len;
    while position < end {
        let count = (end - position).min(PAGE_SIZE as u64) as usize;
        hasher.update(&reader.read(position, count)?);
        position += count as u64;
    }
    Ok(hasher.finalize() == u32::from_le(header.checksum))
}

fn wal_corruption(offset: u64, message: &str) -> EngineError {
    EngineError::Corruption(format!("wal record at offset {offset}: {message}"))
}

/// Writes only the latest image of each page touched by `txs`.
/// Page reads and writes scale with the number of distinct pages.
pub fn replay_wal_index<B: FileBackend>(
    pager: &mut Pager<B>,
    wal: &B,
    txs: &[WalTransaction],
) -> Result<()> {
    let mut latest: BTreeMap<u64, u64> = BTreeMap::new();
    for tx in txs {
        for (page_id, offset) in &tx.pages {
            latest.insert(*page_id, *offset);
        }
    }
    replay_latest_pages(pager, wal, latest)
}

pub(crate) fn replay_latest_pages<B: FileBackend>(
    pager: &mut Pager<B>,
    wal: &B,
    latest: BTreeMap<u64, u64>,
) -> Result<()> {
    let mut locations: Vec<(u64, u64)> =
        Vec::with_capacity(latest.len().min(PAGE_WRITE_BATCH_PAGES));
    let mut images: Vec<(u64, Vec<u8>)> =
        Vec::with_capacity(latest.len().min(PAGE_WRITE_BATCH_PAGES));
    let mut buffer = Vec::new();
    for (page_id, offset) in latest {
        if locations.len() == PAGE_WRITE_BATCH_PAGES
            || locations
                .last()
                .is_some_and(|(previous, _)| previous.checked_add(1) != Some(page_id))
        {
            replay_page_batch(pager, wal, &locations, &mut images, &mut buffer)?;
            locations.clear();
        }
        locations.push((page_id, offset));
    }
    replay_page_batch(pager, wal, &locations, &mut images, &mut buffer)?;
    pager.flush()?;
    Ok(())
}

fn replay_page_batch<B: FileBackend>(
    pager: &mut Pager<B>,
    wal: &B,
    locations: &[(u64, u64)],
    images: &mut Vec<(u64, Vec<u8>)>,
    buffer: &mut Vec<u8>,
) -> Result<()> {
    let record_len = wal_record_total_len(PAGE_IMAGE_PAYLOAD_LEN);
    let mut start = 0;
    while start < locations.len() {
        let mut end = start + 1;
        while end < locations.len()
            && locations[end - 1].1.checked_add(record_len as u64) == Some(locations[end].1)
        {
            end += 1;
        }
        // WAL record headers separate adjacent page images.
        // A bounded span reduces backend calls without reading unrelated records.
        let source_len = (end - start - 1) * record_len + PAGE_SIZE;
        let source_offset = locations[start].1;
        source_offset
            .checked_add(source_len as u64)
            .ok_or_else(|| EngineError::Storage("wal replay read range overflow".into()))?;
        let source = wal.read_at(source_offset, source_len)?;
        if end == start + 1 {
            verify_page_image(&source, locations[start].0)?;
            images.push((locations[start].0, source));
        } else {
            for (index, (page_id, _)) in locations[start..end].iter().enumerate() {
                let local = index * record_len;
                let remaining = source.get(local..).unwrap_or_default();
                // Preserve page validation order and errors for short or oversized reads.
                let bytes = if index + 1 == end - start {
                    remaining
                } else {
                    &remaining[..remaining.len().min(PAGE_SIZE)]
                };
                verify_page_image(bytes, *page_id)?;
                images.push((*page_id, bytes.to_vec()));
            }
        }
        start = end;
    }
    pager.write_page_images(images, buffer)
}

/// Returns [`scan_wal_index`] transactions with page bytes loaded.
pub fn scan_wal<B: FileBackend>(wal: &B) -> Result<Vec<ReplayTransaction>> {
    scan_wal_index(wal)?
        .into_iter()
        .map(|tx| {
            let page_images = tx
                .pages
                .iter()
                .map(|(page_id, offset)| {
                    Ok(PageImageRecord {
                        txid: tx.txid,
                        page_id: *page_id,
                        bytes: wal.read_at(*offset, PAGE_SIZE)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(ReplayTransaction {
                txid: tx.txid,
                page_images,
                commit: tx.commit,
                end_offset: tx.end_offset,
            })
        })
        .collect()
}

pub fn replay_wal_transactions<B: FileBackend>(
    pager: &mut Pager<B>,
    txs: &[ReplayTransaction],
) -> Result<()> {
    for tx in txs {
        for page in &tx.page_images {
            verify_page_image(&page.bytes, page.page_id)?;
            pager.write_page_image(page.page_id, &page.bytes)?;
        }
    }
    pager.flush()?;
    Ok(())
}

fn encode_page_image_record_into<const GENERATED: bool>(
    out: &mut Vec<u8>,
    txid: u64,
    page_id: u64,
    bytes: &[u8],
) -> Result<()> {
    if bytes.len() != PAGE_SIZE {
        return Err(EngineError::Serialization(format!(
            "wal page image wrong size: {}",
            bytes.len()
        )));
    }

    let record_start = append_record_header(
        out,
        WalTag::PageImage,
        WAL_PAGE_IMAGE_BODY_HEADER_SIZE + bytes.len(),
    );
    let body_header = WalPageImageBodyHeader {
        txid: txid.to_le(),
        page_id: page_id.to_le(),
        page_len: (bytes.len() as u32).to_le(),
        reserved: 0,
    };
    out.extend_from_slice(body_header.as_bytes());
    let checksum = if GENERATED {
        // The record checksum is zero. Generated page bytes must be unchanged
        // since their page checksum was computed.
        Some(crc32_with_generated_page(
            &out[record_start..],
            read_u32_le(bytes, PAGE_HEADER_CHECKSUM_OFFSET)?,
        ))
    } else {
        None
    };
    out.extend_from_slice(bytes);
    match checksum {
        Some(checksum) => set_record_checksum(out, record_start, checksum),
        None => finish_record_checksum(out, record_start),
    }
    Ok(())
}

fn encode_commit_record_into(out: &mut Vec<u8>, commit: &CommitRecord) {
    let record_start = append_record_header(out, WalTag::Commit, WAL_COMMIT_BODY_SIZE);
    let body = WalCommitBody {
        txid: commit.txid.to_le(),
        new_catalog_root_page_id: commit.new_catalog_root_page_id.to_le(),
        new_next_page_id: commit.new_next_page_id.to_le(),
        changed_page_count: commit.changed_page_count.to_le(),
        reserved: 0,
    };
    out.extend_from_slice(body.as_bytes());
    finish_record_checksum(out, record_start);
}

fn append_record_header(out: &mut Vec<u8>, tag: WalTag, payload_len: usize) -> usize {
    let record_start = out.len();
    let header = WalRecordHeader {
        magic: WAL_MAGIC,
        tag: tag as u8,
        reserved0: 0,
        reserved1: 0,
        reserved2: 0,
        payload_len: (payload_len as u32).to_le(),
        checksum: 0,
    };
    out.extend_from_slice(header.as_bytes());
    record_start
}

fn finish_record_checksum(out: &mut [u8], record_start: usize) {
    let checksum = checksum_with_zeroed_region(&out[record_start..], WAL_RECORD_CHECKSUM_OFFSET, 4);
    set_record_checksum(out, record_start, checksum);
}

fn set_record_checksum(out: &mut [u8], record_start: usize, checksum: u32) {
    let checksum_start = record_start + WAL_RECORD_CHECKSUM_OFFSET;
    out[checksum_start..checksum_start + 4].copy_from_slice(&checksum.to_le_bytes());
}

#[cfg(test)]
pub(crate) mod scan_work {
    use std::cell::Cell;

    #[derive(Clone, Copy, Debug, Default)]
    pub(crate) struct Work {
        pub pending_page_growths: usize,
        pub retained_transactions_peak: usize,
        pub retained_committed_offsets_peak: usize,
    }

    thread_local! {
        static WORK: Cell<Work> = const { Cell::new(Work {
            pending_page_growths: 0,
            retained_transactions_peak: 0,
            retained_committed_offsets_peak: 0,
        }) };
    }

    pub(crate) fn reset() {
        WORK.with(|work| work.set(Work::default()));
    }

    pub(crate) fn snapshot() -> Work {
        WORK.with(Cell::get)
    }

    pub(super) fn pending_page_growth() {
        WORK.with(|work| {
            let mut value = work.get();
            value.pending_page_growths += 1;
            work.set(value);
        });
    }

    pub(crate) fn retain_committed(transactions: usize, offsets: usize) {
        WORK.with(|work| {
            let mut value = work.get();
            value.retained_transactions_peak = value.retained_transactions_peak.max(transactions);
            value.retained_committed_offsets_peak =
                value.retained_committed_offsets_peak.max(offsets);
            work.set(value);
        });
    }
}
