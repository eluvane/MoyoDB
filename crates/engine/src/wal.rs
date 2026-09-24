use crate::checksum::checksum_with_zeroed_region;
use crate::error::{EngineError, Result};
use crate::layout::{
    unsafe_read_struct, wal_record_total_len, WalCommitBody, WalPageImageBodyHeader,
    WalRecordHeader, WalTag, PAGE_SIZE, WAL_COMMIT_BODY_SIZE, WAL_MAGIC,
    WAL_PAGE_IMAGE_BODY_HEADER_SIZE, WAL_RECORD_CHECKSUM_OFFSET, WAL_RECORD_HEADER_SIZE,
};
use crate::page::verify_page_image;
use crate::pager::Pager;
use crate::storage::backend::FileBackend;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use zerocopy::IntoBytes;

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
    encode_page_image_record_into(&mut record, txid, page_id, bytes)?;
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

    let mut capacity = wal_record_total_len(WAL_COMMIT_BODY_SIZE);
    for (_, bytes) in page_images {
        if bytes.len() != PAGE_SIZE {
            return Err(EngineError::Serialization(format!(
                "wal page image wrong size: {}",
                bytes.len()
            )));
        }
        capacity += wal_record_total_len(WAL_PAGE_IMAGE_BODY_HEADER_SIZE + bytes.len());
    }

    // Encode each WAL record directly into the transaction batch. Keeping the
    // payload and the record as separate Vecs doubles copies in append-heavy paths.
    let mut batch = Vec::with_capacity(capacity);
    for (page_id, bytes) in page_images {
        encode_page_image_record_into(&mut batch, txid, *page_id, bytes)?;
    }
    encode_commit_record_into(&mut batch, commit);

    wal.write_at(*offset, &batch)?;
    *offset += batch.len() as u64;
    Ok(())
}

/// A committed WAL transaction located by offsets only; page bytes stay in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalTransaction {
    pub txid: u64,
    pub commit: CommitRecord,
    /// `(page_id, offset of the page image bytes in the WAL)`.
    pub pages: Vec<(u64, u64)>,
    pub end_offset: u64,
}

const PAGE_IMAGE_PAYLOAD_LEN: usize = WAL_PAGE_IMAGE_BODY_HEADER_SIZE + PAGE_SIZE;

/// Streams the WAL one record at a time and returns every complete, valid
/// transaction in order. Memory is bounded by one record, not the log size.
///
/// A record that fails its checksum, a truncated record and an incomplete batch
/// mark the end of the durable log. A committed batch whose contents are
/// impossible (bad page image, page id out of range, txids going backwards) is
/// corruption: the writer never produces it, so it is not silently dropped.
/// Page images are judged only once their commit record is seen; an
/// uncommitted tail is never applied, so its contents do not matter.
pub fn scan_wal_index<B: FileBackend>(wal: &B) -> Result<Vec<WalTransaction>> {
    let len = wal.len()?;
    let mut offset = 0u64;
    let mut committed: Vec<WalTransaction> = Vec::new();
    let mut pending_txid: Option<u64> = None;
    let mut pending_pages: Vec<(u64, u64)> = Vec::new();
    let mut pending_invalid: Option<EngineError> = None;
    let mut last_txid = 0u64;

    while offset + WAL_RECORD_HEADER_SIZE as u64 <= len {
        let header_bytes = wal.read_at(offset, WAL_RECORD_HEADER_SIZE)?;
        if header_bytes[..4] != WAL_MAGIC {
            break;
        }
        let header: WalRecordHeader = unsafe_read_struct(&header_bytes)?;
        let payload_len = u32::from_le(header.payload_len) as usize;
        let Ok(tag) = WalTag::from_u8(header.tag) else {
            break;
        };
        let expected_payload_len = match tag {
            WalTag::PageImage => PAGE_IMAGE_PAYLOAD_LEN,
            WalTag::Commit => WAL_COMMIT_BODY_SIZE,
        };
        if payload_len != expected_payload_len {
            break;
        }
        let total_len = wal_record_total_len(payload_len) as u64;
        if offset + total_len > len {
            break;
        }
        let record = wal.read_at(offset, total_len as usize)?;
        let expected = checksum_with_zeroed_region(&record, WAL_RECORD_CHECKSUM_OFFSET, 4);
        if expected != u32::from_le(header.checksum) {
            break;
        }
        let payload = &record[WAL_RECORD_HEADER_SIZE..];
        match tag {
            WalTag::PageImage => {
                let body: WalPageImageBodyHeader =
                    unsafe_read_struct(&payload[..WAL_PAGE_IMAGE_BODY_HEADER_SIZE])?;
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
                pending_pages.push((
                    page_id,
                    offset + (WAL_RECORD_HEADER_SIZE + WAL_PAGE_IMAGE_BODY_HEADER_SIZE) as u64,
                ));
            }
            WalTag::Commit => {
                let body: WalCommitBody = unsafe_read_struct(&payload[..WAL_COMMIT_BODY_SIZE])?;
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
                if let Some(previous) = committed.last() {
                    if commit.new_next_page_id < previous.commit.new_next_page_id {
                        return Err(wal_corruption(offset, "next page id went backwards"));
                    }
                }
                last_txid = commit.txid;
                pending_txid = None;
                committed.push(WalTransaction {
                    txid: commit.txid,
                    commit,
                    pages: std::mem::take(&mut pending_pages),
                    end_offset: offset + total_len,
                });
            }
        }
        offset += total_len;
    }
    Ok(committed)
}

fn wal_corruption(offset: u64, message: &str) -> EngineError {
    EngineError::Corruption(format!("wal record at offset {offset}: {message}"))
}

/// Writes the newest image of every page touched by `txs` into the main file.
/// Older images of the same page are skipped, so replay cost is bounded by the
/// number of distinct pages, not the log length.
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
    for (page_id, offset) in latest {
        let bytes = wal.read_at(offset, PAGE_SIZE)?;
        verify_page_image(&bytes, page_id)?;
        pager.write_page_image(page_id, &bytes)?;
    }
    pager.flush()?;
    Ok(())
}

/// Materializing variant of [`scan_wal_index`], kept for tools and tests.
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

fn encode_page_image_record_into(
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
    out.extend_from_slice(bytes);
    finish_record_checksum(out, record_start);
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
    let checksum_start = record_start + WAL_RECORD_CHECKSUM_OFFSET;
    out[checksum_start..checksum_start + 4].copy_from_slice(&checksum.to_le_bytes());
}
