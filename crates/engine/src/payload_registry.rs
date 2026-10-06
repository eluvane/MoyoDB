use crate::btree::{apply_mutations, lookup, scan, BuiltTree, PageAllocator, RangeSpec};
use crate::bytes::{read_u32_le, read_u64_le};
use crate::error::{EngineError, Result};
use crate::pager::Pager;
use crate::payload::{payload_page_count, PayloadRef};
use crate::storage::backend::FileBackend;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const PAYLOAD_REGISTRY_STORE_NAME: &str = "__browserdb:payloads";
const RECORD_SIZE: usize = 48;
const RECORD_MAGIC: &[u8; 4] = b"PRG2";
const CURSOR_KEY: &[u8] = &[0xfe];
const QUEUE_PREFIX: u8 = 0xff;
const REUSE_SCAN_LIMIT: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PayloadReservation {
    pub(crate) first_page_id: u64,
    pub(crate) capacity_pages: u64,
}

#[derive(Debug, Clone)]
struct RegistryEntry {
    reference: PayloadRef,
    references: u64,
    capacity_pages: u64,
    retired_txid: u64,
    generation: u64,
}

#[derive(Debug)]
struct QueueCandidate {
    key: Vec<u8>,
    first_page_id: u64,
    capacity_pages: u64,
    retired_txid: u64,
    generation: u64,
    body_len: u32,
    checksum: u32,
}

#[derive(Debug)]
struct ReservedExtent {
    body_len: u32,
    capacity_pages: u64,
    generation: u64,
    queue_key: Option<Vec<u8>>,
}

// Zero-count records stay durable. Payload extent pages never enter tree-page reuse.
pub(crate) struct PayloadRegistry {
    root_page_id: u64,
    entries: BTreeMap<u64, RegistryEntry>,
    changes: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    candidates: Option<Vec<QueueCandidate>>,
    reservations: BTreeMap<u64, ReservedExtent>,
    registered: BTreeSet<u64>,
    #[cfg(test)]
    queue_entries_read: usize,
}

impl PayloadRegistry {
    pub(crate) fn new(root_page_id: u64) -> Self {
        Self {
            root_page_id,
            entries: BTreeMap::new(),
            changes: BTreeMap::new(),
            candidates: None,
            reservations: BTreeMap::new(),
            registered: BTreeSet::new(),
            #[cfg(test)]
            queue_entries_read: 0,
        }
    }

    pub(crate) fn reserve<B: FileBackend>(
        &mut self,
        pager: &mut Pager<B>,
        body_len: u32,
        oldest_snapshot_txid: u64,
        alloc: &mut PageAllocator,
    ) -> Result<PayloadReservation> {
        let needed_pages = payload_page_count(body_len)?;
        self.load_candidates(pager, needed_pages)?;
        loop {
            let candidates = self.candidates.as_mut().ok_or_else(|| {
                EngineError::Internal("payload reuse candidates were not loaded".into())
            })?;
            let Some(index) = candidates.iter().position(|candidate| {
                candidate.capacity_pages >= needed_pages
                    && candidate.retired_txid <= oldest_snapshot_txid
            }) else {
                break;
            };
            let candidate = candidates.remove(index);
            let entry = self.entry(pager, candidate.first_page_id)?.clone();
            // A stale queue entry must not authorize an extent overwrite.
            if entry.references != 0
                || entry.retired_txid != candidate.retired_txid
                || entry.capacity_pages != candidate.capacity_pages
                || entry.generation != candidate.generation
                || entry.reference.body_len != candidate.body_len
                || entry.reference.checksum != candidate.checksum
            {
                self.changes.insert(candidate.key, None);
                continue;
            }
            if candidate
                .first_page_id
                .checked_add(candidate.capacity_pages)
                .is_none_or(|end| end > alloc.next_page_id())
            {
                return Err(EngineError::Corruption(
                    "retired payload extent is beyond the allocated range".into(),
                ));
            }
            let generation = entry.generation.checked_add(1).ok_or_else(|| {
                EngineError::Corruption("payload registry generation overflow".into())
            })?;
            self.reservations.insert(
                candidate.first_page_id,
                ReservedExtent {
                    body_len,
                    capacity_pages: candidate.capacity_pages,
                    generation,
                    queue_key: Some(candidate.key),
                },
            );
            return Ok(PayloadReservation {
                first_page_id: candidate.first_page_id,
                capacity_pages: candidate.capacity_pages,
            });
        }
        let first_page_id = alloc.reserve_fresh_pages(needed_pages)?;
        self.reservations.insert(
            first_page_id,
            ReservedExtent {
                body_len,
                capacity_pages: needed_pages,
                generation: 1,
                queue_key: None,
            },
        );
        Ok(PayloadReservation {
            first_page_id,
            capacity_pages: needed_pages,
        })
    }

    pub(crate) fn register_body(
        &mut self,
        reservation: PayloadReservation,
        reference: PayloadRef,
    ) -> Result<()> {
        let reserved = self
            .reservations
            .get(&reservation.first_page_id)
            .ok_or_else(|| EngineError::Internal("payload extent was not reserved".into()))?;
        if reference.first_page_id != reservation.first_page_id
            || reference.body_len != reserved.body_len
            || reservation.capacity_pages != reserved.capacity_pages
            || reference.page_count()? > reservation.capacity_pages
        {
            return Err(EngineError::Internal(
                "payload body does not match its reservation".into(),
            ));
        }
        let reserved = self
            .reservations
            .remove(&reservation.first_page_id)
            .ok_or_else(|| EngineError::Internal("reserved payload extent disappeared".into()))?;
        if let Some(queue_key) = reserved.queue_key {
            self.changes.insert(queue_key, None);
        }
        self.entries.insert(
            reference.first_page_id,
            RegistryEntry {
                reference,
                references: 0,
                capacity_pages: reservation.capacity_pages,
                retired_txid: 0,
                generation: reserved.generation,
            },
        );
        self.registered.insert(reference.first_page_id);
        Ok(())
    }

    /// Count logical leaf changes. Copying an unchanged COW cell has no delta.
    pub(crate) fn apply_reference_changes<B: FileBackend>(
        &mut self,
        new_refs: &[PayloadRef],
        dropped_refs: &[PayloadRef],
        new_txid: u64,
        pager: &mut Pager<B>,
    ) -> Result<()> {
        if new_txid == 0 {
            return Err(EngineError::Internal(
                "payload reference txid is zero".into(),
            ));
        }
        let mut deltas = BTreeMap::<u64, (PayloadRef, i128)>::new();
        for (references, delta) in [(new_refs, 1i128), (dropped_refs, -1i128)] {
            for reference in references {
                reference.end_page_id()?;
                let entry = deltas
                    .entry(reference.first_page_id)
                    .or_insert((*reference, 0));
                if entry.0 != *reference {
                    return Err(EngineError::Corruption(
                        "payload reference changes mix extent incarnations".into(),
                    ));
                }
                entry.1 += delta;
            }
        }
        // Validate every count before changing the planned registry records.
        let mut updates = Vec::with_capacity(deltas.len());
        for (first_page_id, (reference, delta)) in deltas {
            let mut entry = self.entry(pager, first_page_id)?.clone();
            if entry.reference != reference {
                return Err(EngineError::Corruption(
                    "payload reference does not match its registry incarnation".into(),
                ));
            }
            let references = i128::from(entry.references) + delta;
            let references = u64::try_from(references).map_err(|_| {
                EngineError::Corruption("payload reference count overflow or underflow".into())
            })?;
            if references == entry.references {
                continue;
            }
            if entry.references == 0 && references > 0 && !self.registered.contains(&first_page_id)
            {
                return Err(EngineError::Corruption(
                    "retired payload was referenced without a reservation".into(),
                ));
            }
            entry.references = references;
            entry.retired_txid = if references == 0 { new_txid } else { 0 };
            updates.push((first_page_id, entry));
        }
        for (first_page_id, entry) in updates {
            self.changes.insert(
                first_page_id.to_be_bytes().to_vec(),
                Some(encode_entry(&entry)),
            );
            if entry.references == 0 {
                self.changes.insert(
                    queue_key(entry.capacity_pages, entry.retired_txid, first_page_id),
                    Some(encode_queue_value(&entry)),
                );
            }
            self.entries.insert(first_page_id, entry);
        }
        Ok(())
    }

    pub(crate) fn finish<B: FileBackend>(
        self,
        pager: &mut Pager<B>,
        alloc: &mut PageAllocator,
    ) -> Result<Option<BuiltTree>> {
        if !self.reservations.is_empty() {
            return Err(EngineError::Internal(
                "reserved payload has no committed references".into(),
            ));
        }
        for page_id in &self.registered {
            let entry = self.entries.get(page_id).ok_or_else(|| {
                EngineError::Internal("registered payload entry disappeared".into())
            })?;
            if entry.references == 0 {
                return Err(EngineError::Internal(
                    "reserved payload has no committed references".into(),
                ));
            }
        }
        if self.changes.is_empty() {
            return Ok(None);
        }
        let mutations: Vec<_> = self
            .changes
            .iter()
            .map(|(key, value)| (key.as_slice(), value.as_deref()))
            .collect();
        apply_mutations(pager, self.root_page_id, &mutations, alloc).map(Some)
    }

    fn entry<B: FileBackend>(
        &mut self,
        pager: &mut Pager<B>,
        first_page_id: u64,
    ) -> Result<&RegistryEntry> {
        if !self.entries.contains_key(&first_page_id) {
            let bytes = lookup(pager, self.root_page_id, &first_page_id.to_be_bytes())?
                .ok_or_else(|| {
                    EngineError::Corruption("payload registry record is missing".into())
                })?;
            self.entries
                .insert(first_page_id, decode_entry(first_page_id, &bytes)?);
        }
        self.entries.get(&first_page_id).ok_or_else(|| {
            EngineError::Internal("loaded payload registry entry disappeared".into())
        })
    }

    fn load_candidates<B: FileBackend>(
        &mut self,
        pager: &mut Pager<B>,
        needed_pages: u64,
    ) -> Result<()> {
        if self.candidates.is_some() {
            return Ok(());
        }
        let minimum = queue_key(needed_pages, 0, 0);
        let cursor = lookup(pager, self.root_page_id, CURSOR_KEY)?;
        if let Some(cursor) = cursor.as_ref() {
            decode_queue_key(cursor)?;
        }
        let after_cursor = cursor.as_ref().is_some_and(|cursor| cursor >= &minimum);
        let mut rows = scan(
            pager,
            self.root_page_id,
            &RangeSpec {
                gt: if after_cursor { cursor.clone() } else { None },
                gte: if after_cursor {
                    None
                } else {
                    Some(minimum.clone())
                },
                limit: Some(REUSE_SCAN_LIMIT),
                ..RangeSpec::default()
            },
        )?;
        if after_cursor && rows.len() < REUSE_SCAN_LIMIT {
            let mut wrapped = scan(
                pager,
                self.root_page_id,
                &RangeSpec {
                    gte: Some(minimum),
                    lte: cursor.clone(),
                    limit: Some(REUSE_SCAN_LIMIT - rows.len()),
                    ..RangeSpec::default()
                },
            )?;
            rows.append(&mut wrapped);
        }
        #[cfg(test)]
        {
            self.queue_entries_read += rows.len();
        }
        let next_cursor = rows.last().map(|row| row.key.clone());
        // Durable progress lets bounded passes reach candidates after reopen.
        if next_cursor != cursor {
            self.changes.insert(CURSOR_KEY.to_vec(), next_cursor);
        }
        let candidates = rows
            .into_iter()
            .map(|row| {
                let (capacity_pages, retired_txid, first_page_id) = decode_queue_key(&row.key)?;
                if row.value.len() != 16 {
                    return Err(EngineError::Corruption(
                        "invalid payload queue value".into(),
                    ));
                }
                Ok(QueueCandidate {
                    key: row.key,
                    first_page_id,
                    capacity_pages,
                    retired_txid,
                    generation: read_u64_le(&row.value, 0)?,
                    body_len: read_u32_le(&row.value, 8)?,
                    checksum: read_u32_le(&row.value, 12)?,
                })
            })
            .collect::<Result<_>>()?;
        self.candidates = Some(candidates);
        Ok(())
    }
}

fn encode_entry(entry: &RegistryEntry) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(RECORD_SIZE);
    bytes.extend_from_slice(RECORD_MAGIC);
    bytes.extend_from_slice(&entry.reference.body_len.to_le_bytes());
    bytes.extend_from_slice(&entry.reference.checksum.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&entry.references.to_le_bytes());
    bytes.extend_from_slice(&entry.capacity_pages.to_le_bytes());
    bytes.extend_from_slice(&entry.retired_txid.to_le_bytes());
    bytes.extend_from_slice(&entry.generation.to_le_bytes());
    bytes
}

fn decode_entry(first_page_id: u64, bytes: &[u8]) -> Result<RegistryEntry> {
    if bytes.len() != RECORD_SIZE || &bytes[..4] != RECORD_MAGIC || read_u32_le(bytes, 12)? != 0 {
        return Err(EngineError::Corruption(
            "invalid payload registry record".into(),
        ));
    }
    let entry = RegistryEntry {
        reference: PayloadRef {
            first_page_id,
            body_len: read_u32_le(bytes, 4)?,
            checksum: read_u32_le(bytes, 8)?,
        },
        references: read_u64_le(bytes, 16)?,
        capacity_pages: read_u64_le(bytes, 24)?,
        retired_txid: read_u64_le(bytes, 32)?,
        generation: read_u64_le(bytes, 40)?,
    };
    if first_page_id == 0
        || entry.reference.page_count()? > entry.capacity_pages
        || first_page_id.checked_add(entry.capacity_pages).is_none()
        || entry.generation == 0
        || (entry.references == 0) != (entry.retired_txid != 0)
    {
        return Err(EngineError::Corruption(
            "invalid payload registry state".into(),
        ));
    }
    Ok(entry)
}

fn queue_key(capacity_pages: u64, retired_txid: u64, first_page_id: u64) -> Vec<u8> {
    let mut key = Vec::with_capacity(25);
    key.push(QUEUE_PREFIX);
    key.extend_from_slice(&capacity_pages.to_be_bytes());
    key.extend_from_slice(&retired_txid.to_be_bytes());
    key.extend_from_slice(&first_page_id.to_be_bytes());
    key
}

fn decode_queue_key(key: &[u8]) -> Result<(u64, u64, u64)> {
    if key.len() != 25 || key[0] != QUEUE_PREFIX {
        return Err(EngineError::Corruption("invalid payload queue key".into()));
    }
    let capacity = u64::from_be_bytes(
        key[1..9]
            .try_into()
            .map_err(|_| EngineError::Corruption("invalid payload queue capacity".into()))?,
    );
    let retired =
        u64::from_be_bytes(key[9..17].try_into().map_err(|_| {
            EngineError::Corruption("invalid payload queue retirement txid".into())
        })?);
    let first = u64::from_be_bytes(
        key[17..25]
            .try_into()
            .map_err(|_| EngineError::Corruption("invalid payload queue page id".into()))?,
    );
    if capacity == 0 || retired == 0 || first == 0 {
        return Err(EngineError::Corruption(
            "invalid payload queue state".into(),
        ));
    }
    Ok((capacity, retired, first))
}

fn encode_queue_value(entry: &RegistryEntry) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(16);
    bytes.extend_from_slice(&entry.generation.to_le_bytes());
    bytes.extend_from_slice(&entry.reference.body_len.to_le_bytes());
    bytes.extend_from_slice(&entry.reference.checksum.to_le_bytes());
    bytes
}

#[cfg(test)]
#[path = "payload_registry_tests.rs"]
mod tests;
