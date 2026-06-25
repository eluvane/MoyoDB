use crate::checksum::checksum_with_zeroed_region;
use crate::error::{EngineError, Result};
use crate::layout::{
    unsafe_read_struct, wal_record_total_len, WalCommitBody, WalPageImageBodyHeader,
    WalRecordHeader, WalTag, PAGE_SIZE, WAL_COMMIT_BODY_SIZE, WAL_MAGIC,
    WAL_PAGE_IMAGE_BODY_HEADER_SIZE, WAL_RECORD_CHECKSUM_OFFSET, WAL_RECORD_HEADER_SIZE,
};
use crate::pager::Pager;
use crate::storage::backend::FileBackend;
use serde::{Deserialize, Serialize};
use zerocopy::AsBytes;

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

pub fn scan_wal<B: FileBackend>(wal: &B) -> Result<Vec<ReplayTransaction>> {
    let len = wal.len()? as usize;
    if len == 0 {
        return Ok(Vec::new());
    }
    let bytes = wal.read_at(0, len)?;
    let mut offset = 0usize;
    let mut tx_page_images: std::collections::BTreeMap<u64, Vec<PageImageRecord>> =
        std::collections::BTreeMap::new();
    let mut committed = Vec::new();

    while offset + WAL_RECORD_HEADER_SIZE <= bytes.len() {
        let header_bytes = &bytes[offset..offset + WAL_RECORD_HEADER_SIZE];
        if header_bytes[..4] != WAL_MAGIC {
            break;
        }
        let header: WalRecordHeader = unsafe_read_struct(header_bytes)?;
        let payload_len = u32::from_le(header.payload_len) as usize;
        let total_len = wal_record_total_len(payload_len);
        if offset + total_len > bytes.len() {
            break;
        }
        let record_bytes = &bytes[offset..offset + total_len];
        let expected = checksum_with_zeroed_region(record_bytes, WAL_RECORD_CHECKSUM_OFFSET, 4);
        let got = u32::from_le(header.checksum);
        if expected != got {
            break;
        }
        let payload = &record_bytes[WAL_RECORD_HEADER_SIZE..];
        match WalTag::from_u8(header.tag)? {
            WalTag::PageImage => {
                if payload.len() < WAL_PAGE_IMAGE_BODY_HEADER_SIZE {
                    break;
                }
                let body: WalPageImageBodyHeader =
                    unsafe_read_struct(&payload[..WAL_PAGE_IMAGE_BODY_HEADER_SIZE])?;
                let txid = u64::from_le(body.txid);
                let page_id = u64::from_le(body.page_id);
                let page_len = u32::from_le(body.page_len) as usize;
                if page_len != PAGE_SIZE {
                    break;
                }
                if payload.len() < WAL_PAGE_IMAGE_BODY_HEADER_SIZE + page_len {
                    break;
                }
                let page = payload
                    [WAL_PAGE_IMAGE_BODY_HEADER_SIZE..WAL_PAGE_IMAGE_BODY_HEADER_SIZE + page_len]
                    .to_vec();
                tx_page_images
                    .entry(txid)
                    .or_default()
                    .push(PageImageRecord {
                        txid,
                        page_id,
                        bytes: page,
                    });
            }
            WalTag::Commit => {
                if payload.len() < WAL_COMMIT_BODY_SIZE {
                    break;
                }
                let body: WalCommitBody = unsafe_read_struct(&payload[..WAL_COMMIT_BODY_SIZE])?;
                let txid = u64::from_le(body.txid);
                let commit = CommitRecord {
                    txid,
                    new_catalog_root_page_id: u64::from_le(body.new_catalog_root_page_id),
                    new_next_page_id: u64::from_le(body.new_next_page_id),
                    changed_page_count: u32::from_le(body.changed_page_count),
                };
                let Some(page_images) = tx_page_images.remove(&txid) else {
                    break;
                };
                if page_images.len() != commit.changed_page_count as usize {
                    break;
                }
                committed.push(ReplayTransaction {
                    txid,
                    page_images,
                    commit,
                    end_offset: (offset + total_len) as u64,
                });
            }
        }
        offset += total_len;
    }
    Ok(committed)
}

pub fn replay_wal_transactions<B: FileBackend>(
    pager: &mut Pager<B>,
    txs: &[ReplayTransaction],
) -> Result<()> {
    for tx in txs {
        for page in &tx.page_images {
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
