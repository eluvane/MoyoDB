use crate::bytes::{compare_keys, read_u32_le};
use crate::catalog::{
    decode_change_feed_floor_txid, decode_change_feed_policy, decode_schema_version,
    decode_store_metadata, encode_change_feed_floor_txid, encode_change_feed_policy,
    encode_schema_version, encode_store_metadata, CatalogState, ChangeFeedPolicy,
    CATALOG_CHANGE_FEED_FLOOR_TXID_KEY, CATALOG_CHANGE_FEED_POLICY_KEY, CATALOG_SCHEMA_VERSION_KEY,
};
use crate::error::{EngineError, Result};
use crate::layout::{PageKind, ValueKind, PAGE_HEADER_SIZE, PAGE_SIZE};
use crate::overflow::{
    free_overflow_chain, read_overflow_expiry, read_overflow_prefix, read_overflow_stored_value,
    read_overflow_stored_value_into, read_overflow_value, read_overflow_value_envelope,
    write_overflow_value,
};
use crate::page::{
    decode_internal_cell_ref, decode_leaf_cell_ref, decode_page_header_verified,
    encode_internal_page, encode_leaf_page, internal_cell_size, leaf_cell_size, read_cell_slot,
    should_overflow_value, InternalCell, LeafCell, PageHeaderInfo, MAX_TREE_LEVEL,
};
use crate::pager::Pager;
use crate::payload::{
    decode_payload_descriptor, encode_payload_descriptor, read_payload_into,
    read_payload_metadata_prefix, read_payload_prefix, PayloadRef,
};
use crate::prepared_value::ValueSource;
use crate::storage::backend::FileBackend;
use crate::value::{
    decode_value_envelope, store_compression_from_flags, store_uses_system_raw_values,
    store_uses_value_envelope, store_value_prefix_len, validate_store_value,
    validate_store_value_prefix, StoreCompression, StoredValue, ValueRevision, ValueState,
    COMPRESSION_VALUE_HEADER_SIZE,
};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RangeSpec {
    pub gt: Option<Vec<u8>>,
    pub gte: Option<Vec<u8>>,
    pub lt: Option<Vec<u8>>,
    pub lte: Option<Vec<u8>>,
    pub reverse: bool,
    pub limit: Option<usize>,
}

impl RangeSpec {
    pub fn validate(&self) -> Result<()> {
        if self.gt.is_some() && self.gte.is_some() {
            return Err(EngineError::InvalidRange(
                "range cannot include both gt and gte".into(),
            ));
        }
        if self.lt.is_some() && self.lte.is_some() {
            return Err(EngineError::InvalidRange(
                "range cannot include both lt and lte".into(),
            ));
        }
        if let (Some((lower, lower_inclusive)), Some((upper, upper_inclusive))) =
            (self.lower_bound(), self.upper_bound())
        {
            match compare_keys(lower, upper) {
                Ordering::Greater => {
                    return Err(EngineError::InvalidRange(
                        "range lower bound exceeds upper bound".into(),
                    ));
                }
                Ordering::Equal if !lower_inclusive || !upper_inclusive => {
                    return Err(EngineError::InvalidRange(
                        "range bounds collapse to an empty exclusive interval".into(),
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub(crate) fn lower_bound(&self) -> Option<(&[u8], bool)> {
        if let Some(bound) = self.gt.as_deref() {
            Some((bound, false))
        } else {
            self.gte.as_deref().map(|bound| (bound, true))
        }
    }

    pub(crate) fn upper_bound(&self) -> Option<(&[u8], bool)> {
        if let Some(bound) = self.lt.as_deref() {
            Some((bound, false))
        } else {
            self.lte.as_deref().map(|bound| (bound, true))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KvPair {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct BuiltTree {
    pub root_page_id: u64,
    pub page_images: PageImages,
}

pub type PageImage = (u64, Vec<u8>);
pub type PageImages = Vec<PageImage>;

fn sorted_page_images(mut images: PageImages) -> PageImages {
    images.sort_unstable_by_key(|(page_id, _)| *page_id);
    images
}

/// A key and its new encoded value; `None` deletes the key.
pub type Mutation<'a> = (&'a [u8], Option<&'a [u8]>);

/// Allocates page ids for one commit.
/// Reuse only ids that no open snapshot can reach.
/// Retired ids stay in `freed` until the engine permits reuse.
#[derive(Debug, Clone, Default)]
pub struct PageAllocator {
    next_page_id: u64,
    reusable: Vec<u64>,
    freed: Vec<u64>,
    added_payload_refs: Vec<PayloadRef>,
    dropped_payload_refs: Vec<PayloadRef>,
}

impl PageAllocator {
    pub fn new(next_page_id: u64) -> Self {
        Self::with_reusable(next_page_id, Vec::new())
    }

    pub fn with_reusable(next_page_id: u64, mut reusable: Vec<u64>) -> Self {
        // Pop the lowest ids first so reused pages stay near the start of the file.
        reusable.sort_unstable_by(|left, right| right.cmp(left));
        Self {
            next_page_id: next_page_id.max(1),
            reusable,
            freed: Vec::new(),
            added_payload_refs: Vec::new(),
            dropped_payload_refs: Vec::new(),
        }
    }

    pub fn allocate(&mut self) -> u64 {
        if let Some(page_id) = self.reusable.pop() {
            return page_id;
        }
        let page_id = self.next_page_id;
        self.next_page_id += 1;
        page_id
    }

    /// Reserve a contiguous range outside the reusable tree-page pool.
    pub(crate) fn reserve_fresh_pages(&mut self, count: u64) -> Result<u64> {
        if count == 0 {
            return Err(EngineError::Internal("empty fresh page reservation".into()));
        }
        let next_page_id = self
            .next_page_id
            .checked_add(count)
            .filter(|next| {
                next.checked_sub(1)
                    .and_then(|pages| pages.checked_mul(PAGE_SIZE as u64))
                    .is_some()
            })
            .ok_or_else(|| EngineError::Storage("fresh page range overflow".into()))?;
        let first_page_id = self.next_page_id;
        self.next_page_id = next_page_id;
        Ok(first_page_id)
    }

    /// Retires a page reachable from the committed state this commit replaces.
    pub fn free(&mut self, page_id: u64) {
        self.freed.push(page_id);
    }

    /// An unpublished id can be reused immediately.
    fn release_unpublished(&mut self, page_id: u64) {
        self.reusable.push(page_id);
    }

    pub fn next_page_id(&self) -> u64 {
        self.next_page_id
    }

    pub fn freed(&self) -> &[u64] {
        &self.freed
    }

    pub(crate) fn added_payload_refs(&self) -> &[PayloadRef] {
        &self.added_payload_refs
    }

    pub(crate) fn dropped_payload_refs(&self) -> &[PayloadRef] {
        &self.dropped_payload_refs
    }

    /// `(next_page_id, unused reusable ids, freed ids)`.
    pub fn into_parts(self) -> (u64, Vec<u64>, Vec<u64>) {
        (self.next_page_id, self.reusable, self.freed)
    }
}

pub fn lookup<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    key: &[u8],
) -> Result<Option<Vec<u8>>> {
    match lookup_pending(pager, root_page_id, key, usize::MAX)? {
        Some(value) => materialize_pending_value(pager, value).map(Some),
        None => Ok(None),
    }
}

/// Returns at most `prefix_len` leading bytes of the value stored under `key`
/// without reading the rest of an overflow chain.
pub fn lookup_prefix<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    key: &[u8],
    prefix_len: usize,
) -> Result<Option<Vec<u8>>> {
    match lookup_pending(pager, root_page_id, key, prefix_len)? {
        Some(PendingValue::Inline(value)) => Ok(Some(value)),
        Some(value) => {
            pending_value_prefix(pager, &value, prefix_len).map(|prefix| Some(prefix.into_owned()))
        }
        None => Ok(None),
    }
}

fn lookup_pending<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    key: &[u8],
    inline_prefix_len: usize,
) -> Result<Option<PendingValue>> {
    let value = lookup_cell(
        pager,
        root_page_id,
        key,
        |cell| pending_value_from_leaf_ref(cell, inline_prefix_len),
        None,
    )?;
    if let Some(value) = value.as_ref() {
        validate_pending_payload_range(pager, value)?;
    }
    Ok(value)
}

/// Retains the current internal path for a sorted batch.
/// Overflow reads can evict these pages from the pager.
/// Leaves and values are read only when their keys are requested.
#[derive(Default)]
pub(crate) struct PointReadBatch {
    ancestors: Vec<PointReadAncestor>,
    // Reused page image. Bulk lookups snapshot internal pages so a one-page
    // cache can evict them during overflow reads without rereading ancestors.
    spare: Vec<u8>,
}

struct PointReadAncestor {
    page_id: u64,
    header: PageHeaderInfo,
    bytes: Vec<u8>,
}

impl PointReadBatch {
    fn with_node<B: FileBackend, R>(
        &mut self,
        pager: &mut Pager<B>,
        depth: usize,
        page_id: u64,
        expected_level: Option<u8>,
        inspect: impl FnOnce(&[u8], &PageHeaderInfo) -> Result<R>,
    ) -> Result<R> {
        if let Some(node) = self
            .ancestors
            .get(depth)
            .filter(|node| node.page_id == page_id)
        {
            if let Some(expected) = expected_level {
                if node.header.level != expected {
                    return Err(EngineError::Corruption(format!(
                        "page {page_id} has level {}, parent expects {expected}",
                        node.header.level
                    )));
                }
            }
            return inspect(&node.bytes, &node.header);
        }
        while self.ancestors.len() > depth {
            let dropped = self
                .ancestors
                .pop()
                .ok_or_else(|| EngineError::Internal("point-read ancestor missing".into()))?;
            self.recycle(dropped.bytes);
        }
        pager.with_page(page_id, |bytes| {
            let header = node_header(bytes, page_id, expected_level)?;
            let result = inspect(bytes, &header)?;
            if header.page_kind == PageKind::Internal {
                let stored = self.snapshot_page(bytes);
                self.ancestors.push(PointReadAncestor {
                    page_id,
                    header,
                    bytes: stored,
                });
            }
            Ok(result)
        })
    }

    fn recycle(&mut self, bytes: Vec<u8>) {
        if bytes.len() == PAGE_SIZE && bytes.capacity() >= self.spare.capacity() {
            self.spare = bytes;
        }
    }

    fn snapshot_page(&mut self, bytes: &[u8]) -> Vec<u8> {
        if self.spare.len() == bytes.len() {
            self.spare.copy_from_slice(bytes);
            std::mem::take(&mut self.spare)
        } else {
            bytes.to_vec()
        }
    }
}

fn lookup_cell<B: FileBackend, T>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    key: &[u8],
    mut read_value: impl FnMut(&crate::page::LeafCellRef<'_>) -> Result<T>,
    mut batch: Option<&mut PointReadBatch>,
) -> Result<Option<T>> {
    if root_page_id == 0 {
        return Ok(None);
    }
    let mut current = root_page_id;
    let mut expected_level = None;
    let mut depth = 0;
    loop {
        let mut inspect = |bytes: &[u8], header: &PageHeaderInfo| match header.page_kind {
            PageKind::Leaf => lookup_leaf_in_page(bytes, header, key, &mut read_value),
            _ => {
                let (_, child_page_id) = choose_internal_child_in_page(bytes, header, key)?;
                Ok(LookupStep::Descend(child_page_id, header.level - 1))
            }
        };
        let step = match batch.as_deref_mut() {
            Some(batch) => batch.with_node(pager, depth, current, expected_level, inspect),
            None => pager.with_page(current, |bytes| {
                let header = node_header(bytes, current, expected_level)?;
                inspect(bytes, &header)
            }),
        }?;
        match step {
            LookupStep::Descend(child_page_id, level) => {
                current = child_page_id;
                expected_level = Some(level);
                depth += 1;
            }
            LookupStep::Found(value) => return Ok(Some(value)),
            LookupStep::NotFound => return Ok(None),
        }
    }
}

pub(crate) fn lookup_stored_value<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    key: &[u8],
    store_flags: u64,
    batch: Option<&mut PointReadBatch>,
) -> Result<Option<StoredValue>> {
    let value = lookup_cell(
        pager,
        root_page_id,
        key,
        |cell| {
            if cell.value_kind == ValueKind::Inline {
                StoredValue::decode_for_store(store_flags, cell.inline_value)
                    .map(|value| PointValue::Inline(value, None))
            } else if cell.value_kind == ValueKind::Overflow {
                Ok(PointValue::Overflow {
                    head_page_id: cell.overflow_head_page_id,
                    total_len: cell.total_value_len as usize,
                })
            } else {
                point_external_value(cell)
            }
        },
        batch,
    )?;
    match value {
        None => Ok(None),
        Some(PointValue::Inline(value, payload)) => {
            validate_optional_payload_range(pager, payload.as_ref())?;
            Ok(Some(value))
        }
        Some(PointValue::Overflow {
            head_page_id,
            total_len,
        }) => read_overflow_stored_value(pager, head_page_id, total_len, store_flags).map(Some),
        Some(PointValue::External { payload, prefix }) => materialize_pending_stored_value(
            pager,
            PendingValue::External { payload, prefix },
            store_flags,
        )
        .map(Some),
    }
}

pub(crate) fn lookup_stored_value_info<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    key: &[u8],
    store_flags: u64,
    batch: Option<&mut PointReadBatch>,
) -> Result<Option<(usize, Option<u64>)>> {
    let value = lookup_cell(
        pager,
        root_page_id,
        key,
        |cell| {
            if cell.value_kind == ValueKind::Inline {
                let (value, expiry) = inline_stored_value_parts(store_flags, cell.inline_value)?;
                Ok(PointValue::Inline((value.len(), expiry), None))
            } else if cell.value_kind == ValueKind::Overflow {
                Ok(PointValue::Overflow {
                    head_page_id: cell.overflow_head_page_id,
                    total_len: cell.total_value_len as usize,
                })
            } else {
                let (payload, prefix) = decode_payload_descriptor(
                    cell.overflow_head_page_id,
                    cell.total_value_len,
                    cell.inline_value,
                )?;
                match external_stored_value_info(&payload, prefix, store_flags) {
                    Ok(info) => Ok(PointValue::Inline(info, Some(payload))),
                    Err(EngineError::Corruption(_)) => Ok(PointValue::External {
                        payload,
                        prefix: prefix.to_vec(),
                    }),
                    Err(error) => Err(error),
                }
            }
        },
        batch,
    )?;
    match value {
        None => Ok(None),
        Some(PointValue::Inline(info, payload)) => {
            validate_optional_payload_range(pager, payload.as_ref())?;
            Ok(Some(info))
        }
        Some(PointValue::Overflow {
            head_page_id,
            total_len,
        }) => {
            match pending_stored_value_info(
                pager,
                &PendingValue::Overflow {
                    head_page_id,
                    total_len,
                },
                store_flags,
            ) {
                Ok(info) => Ok(Some(info)),
                Err(EngineError::Corruption(_)) => {
                    read_overflow_stored_value_into(
                        pager,
                        head_page_id,
                        total_len,
                        store_flags,
                        |_| Ok(()),
                    )?;
                    Err(EngineError::Internal(
                        "overflow metadata changed during lookup".into(),
                    ))
                }
                Err(error) => Err(error),
            }
        }
        Some(PointValue::External { payload, prefix }) => {
            write_external_stored_value(pager, &payload, &prefix, store_flags, |_| Ok(()))?;
            Err(EngineError::Internal(
                "external metadata changed during lookup".into(),
            ))
        }
    }
}

pub(crate) fn lookup_stored_value_size<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    key: &[u8],
    store_flags: u64,
    now_ms: u64,
    batch: Option<&mut PointReadBatch>,
) -> Result<Option<usize>> {
    lookup_stored_value_state(pager, root_page_id, key, store_flags, now_ms, batch).map(|state| {
        state
            .filter(|state| state.exists)
            .map(|state| state.max_length)
    })
}

pub(crate) fn lookup_stored_value_state<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    key: &[u8],
    store_flags: u64,
    now_ms: u64,
    batch: Option<&mut PointReadBatch>,
) -> Result<Option<ValueState>> {
    let value = lookup_cell(
        pager,
        root_page_id,
        key,
        |cell| {
            if cell.value_kind == ValueKind::Inline {
                let mut state = stored_state_from_envelope(
                    cell.inline_value,
                    cell.total_value_len as usize,
                    store_flags,
                    now_ms,
                )?;
                if state.exists {
                    state.max_length = stored_size_from_header(
                        &cell.inline_value[store_value_prefix_len(store_flags)..],
                        state.max_length,
                        store_flags,
                    )?;
                }
                Ok(PointValue::Inline(state, None))
            } else if cell.value_kind == ValueKind::Overflow {
                Ok(PointValue::Overflow {
                    head_page_id: cell.overflow_head_page_id,
                    total_len: cell.total_value_len as usize,
                })
            } else {
                let (payload, prefix) = decode_payload_descriptor(
                    cell.overflow_head_page_id,
                    cell.total_value_len,
                    cell.inline_value,
                )?;
                let mut state = stored_state_from_envelope(
                    prefix,
                    prefix.len() + payload.body_len as usize,
                    store_flags,
                    now_ms,
                )?;
                if !state.exists {
                    Ok(PointValue::Inline(state, Some(payload)))
                } else if store_compression_from_flags(store_flags)? == StoreCompression::None {
                    state.max_length = stored_size_from_header(&[], state.max_length, store_flags)?;
                    Ok(PointValue::Inline(state, Some(payload)))
                } else {
                    Ok(PointValue::External {
                        payload,
                        prefix: prefix.to_vec(),
                    })
                }
            }
        },
        batch,
    )?;
    match value {
        None => Ok(None),
        Some(PointValue::Inline(state, payload)) => {
            validate_optional_payload_range(pager, payload.as_ref())?;
            Ok(Some(state))
        }
        Some(PointValue::Overflow {
            head_page_id,
            total_len,
        }) => {
            let (expiry, revision) =
                read_overflow_value_envelope(pager, head_page_id, total_len, store_flags)?;
            let mut state =
                stored_state_from_parts(total_len, store_flags, now_ms, expiry, revision)?;
            if !state.exists {
                return Ok(Some(state));
            }
            let skip = store_value_prefix_len(store_flags);
            let header = if store_compression_from_flags(store_flags)? != StoreCompression::None {
                read_overflow_prefix(
                    pager,
                    head_page_id,
                    total_len,
                    skip + COMPRESSION_VALUE_HEADER_SIZE,
                )?
            } else {
                Vec::new()
            };
            state.max_length = stored_size_from_header(
                header.get(skip..).unwrap_or_default(),
                state.max_length,
                store_flags,
            )?;
            Ok(Some(state))
        }
        Some(PointValue::External { payload, prefix }) => {
            validate_optional_payload_range(pager, Some(&payload))?;
            let mut state = stored_state_from_envelope(
                &prefix,
                prefix.len() + payload.body_len as usize,
                store_flags,
                now_ms,
            )?;
            if !state.exists {
                return Ok(Some(state));
            };
            let skip = store_value_prefix_len(store_flags);
            if store_compression_from_flags(store_flags)? == StoreCompression::None {
                state.max_length = stored_size_from_header(&[], state.max_length, store_flags)?;
                return Ok(Some(state));
            }
            if state.max_length < COMPRESSION_VALUE_HEADER_SIZE {
                return Err(EngineError::Corruption(
                    "compression value header is truncated".into(),
                ));
            }
            let mut header = [0; COMPRESSION_VALUE_HEADER_SIZE];
            let take = (prefix.len() - skip).min(header.len());
            header[..take].copy_from_slice(&prefix[skip..skip + take]);
            if take < header.len() {
                let body = read_payload_metadata_prefix(
                    pager.backend_ref(),
                    &payload,
                    header.len() - take,
                )?;
                header[take..take + body.len()].copy_from_slice(&body);
                if take + body.len() != header.len() {
                    return Err(EngineError::Corruption(
                        "compression value header is truncated".into(),
                    ));
                }
            }
            state.max_length = stored_size_from_header(&header, state.max_length, store_flags)?;
            Ok(Some(state))
        }
    }
}

fn stored_state_from_envelope(
    prefix: &[u8],
    total_len: usize,
    flags: u64,
    now_ms: u64,
) -> Result<ValueState> {
    let enveloped = store_uses_value_envelope(flags) && !store_uses_system_raw_values(flags);
    let (expiry, revision) = if enveloped {
        decode_value_envelope(flags, prefix)?
    } else {
        (None, None)
    };
    stored_state_from_parts(total_len, flags, now_ms, expiry, revision)
}

fn stored_state_from_parts(
    total_len: usize,
    flags: u64,
    now_ms: u64,
    expires_at_ms: Option<u64>,
    revision: Option<ValueRevision>,
) -> Result<ValueState> {
    let expired = matches!(expires_at_ms, Some(expiry) if now_ms >= expiry);
    let len = total_len
        .checked_sub(store_value_prefix_len(flags))
        .ok_or_else(|| {
            EngineError::Corruption("stored value length is shorter than its envelope".into())
        })?;
    Ok(ValueState {
        exists: !expired,
        expired,
        expires_at_ms,
        revision,
        max_length: if expired { 0 } else { len },
    })
}

fn stored_size_from_header(prefix: &[u8], len: usize, flags: u64) -> Result<usize> {
    if store_uses_system_raw_values(flags) {
        return Ok(len);
    }
    validate_store_value_prefix(prefix, len, flags).map_err(|error| match error {
        EngineError::Corruption(_) => error,
        error => EngineError::Corruption(error.to_string()),
    })?;
    if store_compression_from_flags(flags)? == StoreCompression::None {
        Ok(len)
    } else {
        Ok(len.max(read_u32_le(prefix, 10)? as usize))
    }
}

pub(crate) fn lookup_stored_value_into<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    key: &[u8],
    store_flags: u64,
    batch: Option<&mut PointReadBatch>,
    mut write: impl FnMut(&[u8]) -> Result<()>,
) -> Result<Option<Option<u64>>> {
    let value = lookup_cell(
        pager,
        root_page_id,
        key,
        |cell| {
            if cell.value_kind == ValueKind::Inline {
                let (value, expiry) = inline_stored_value_parts(store_flags, cell.inline_value)?;
                write(value)?;
                Ok(PointValue::Inline(expiry, None))
            } else if cell.value_kind == ValueKind::Overflow {
                Ok(PointValue::Overflow {
                    head_page_id: cell.overflow_head_page_id,
                    total_len: cell.total_value_len as usize,
                })
            } else {
                point_external_value(cell)
            }
        },
        batch,
    )?;
    match value {
        None => Ok(None),
        Some(PointValue::Inline(expiry, payload)) => {
            validate_optional_payload_range(pager, payload.as_ref())?;
            Ok(Some(expiry))
        }
        Some(PointValue::Overflow {
            head_page_id,
            total_len,
        }) => read_overflow_stored_value_into(pager, head_page_id, total_len, store_flags, write)
            .map(Some),
        Some(PointValue::External { payload, prefix }) => {
            write_external_stored_value(pager, &payload, &prefix, store_flags, write).map(Some)
        }
    }
}

/// `None` means missing; `Some(None)` means present without an expiry.
pub(crate) fn lookup_value_expiry<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    key: &[u8],
    store_flags: u64,
    batch: Option<&mut PointReadBatch>,
) -> Result<Option<Option<u64>>> {
    let enveloped =
        store_uses_value_envelope(store_flags) && !store_uses_system_raw_values(store_flags);
    let value = lookup_cell(
        pager,
        root_page_id,
        key,
        |cell| {
            if cell.value_kind == ValueKind::External {
                let (payload, prefix) = decode_payload_descriptor(
                    cell.overflow_head_page_id,
                    cell.total_value_len,
                    cell.inline_value,
                )?;
                let expiry = if enveloped {
                    decode_value_envelope(store_flags, prefix)?.0
                } else {
                    None
                };
                Ok(PointValue::Inline(expiry, Some(payload)))
            } else if !enveloped {
                Ok(PointValue::Inline(None, None))
            } else if cell.value_kind == ValueKind::Inline {
                decode_value_envelope(store_flags, cell.inline_value)
                    .map(|(expiry, _)| expiry)
                    .map(|expiry| PointValue::Inline(expiry, None))
            } else if cell.value_kind == ValueKind::Overflow {
                Ok(PointValue::Overflow {
                    head_page_id: cell.overflow_head_page_id,
                    total_len: cell.total_value_len as usize,
                })
            } else {
                Err(EngineError::Internal("unknown point value kind".into()))
            }
        },
        batch,
    )?;
    match value {
        None => Ok(None),
        Some(PointValue::Inline(expiry, payload)) => {
            validate_optional_payload_range(pager, payload.as_ref())?;
            Ok(Some(expiry))
        }
        Some(PointValue::Overflow {
            head_page_id,
            total_len,
        }) => read_overflow_expiry(pager, head_page_id, total_len, store_flags).map(Some),
        Some(PointValue::External { .. }) => Err(EngineError::Internal(
            "external point expiry was not decoded".into(),
        )),
    }
}

pub fn load_all_entries<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
) -> Result<Vec<KvPair>> {
    scan(pager, root_page_id, &RangeSpec::default())
}

pub fn scan<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    range: &RangeSpec,
) -> Result<Vec<KvPair>> {
    range.validate()?;
    let limit = range.limit.unwrap_or(usize::MAX);
    if limit == 0 {
        return Ok(Vec::new());
    }
    // Known limits request one exact buffer. Unlimited scans reserve after the
    // first leaf is staged, so push does not grow geometrically.
    let mut out = if range.limit.is_some() && limit <= 4096 {
        Vec::with_capacity(limit)
    } else {
        Vec::new()
    };
    let mut iter = TreeIter::new(pager, root_page_id, range)?;
    while let Some(pair) = iter.next(pager)? {
        let pending = iter.ready_remaining();
        if pending > 0 {
            let needed = out.len() + 1 + pending;
            if needed > out.capacity() {
                out.reserve_exact(needed - out.capacity());
            }
        }
        out.push(KvPair {
            key: pair.key,
            value: materialize_pending_value(pager, pair.value)?,
        });
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

pub fn build_tree(entries: &[(Vec<u8>, Vec<u8>)], next_page_id: &mut u64) -> Result<BuiltTree> {
    let mut alloc = PageAllocator::new(*next_page_id);
    let built = build_tree_with(entries, &mut alloc)?;
    *next_page_id = alloc.next_page_id();
    Ok(built)
}

pub fn build_tree_with(
    entries: &[(Vec<u8>, Vec<u8>)],
    alloc: &mut PageAllocator,
) -> Result<BuiltTree> {
    let mut ordered: Vec<(&[u8], &[u8])> = entries
        .iter()
        .map(|(key, value)| (key.as_slice(), value.as_slice()))
        .collect();
    ordered.sort_by(|a, b| a.0.cmp(b.0));
    build_tree_from_sorted(ordered, alloc)
}

pub fn build_store_tree(
    map: &BTreeMap<Vec<u8>, Vec<u8>>,
    alloc: &mut PageAllocator,
) -> Result<BuiltTree> {
    build_tree_from_sorted(
        map.iter()
            .map(|(key, value)| (key.as_slice(), value.as_slice())),
        alloc,
    )
}

/// Bulk-loads a tree from strictly increasing keys.
pub fn build_tree_from_sorted<'a, I>(entries: I, alloc: &mut PageAllocator) -> Result<BuiltTree>
where
    I: IntoIterator<Item = (&'a [u8], &'a [u8])>,
{
    build_tree_from_values(entries, alloc)
}

pub(crate) fn build_tree_from_values<'a, I, V>(
    entries: I,
    alloc: &mut PageAllocator,
) -> Result<BuiltTree>
where
    V: ValueSource + ?Sized + 'a,
    I: IntoIterator<Item = (&'a [u8], &'a V)>,
{
    let mut writer = TreeWriter::new_detached(alloc);
    let mut cells = Vec::new();
    let mut previous: Option<&[u8]> = None;
    for (key, value) in entries {
        if let Some(previous) = previous {
            if compare_keys(previous, key) != Ordering::Less {
                return Err(EngineError::Internal(
                    "bulk tree build requires strictly increasing keys".into(),
                ));
            }
        }
        previous = Some(key);
        cells.push(writer.plan_cell(key, value)?);
    }
    let root_page_id = writer.finish_root(Run::Leaf(cells))?;
    Ok(BuiltTree {
        root_page_id,
        page_images: writer.into_images(),
    })
}

/// Accepts strictly increasing keys one at a time.
/// Callers can drain page images between entries to limit buffered images.
pub struct SortedTreeBuilder {
    cells: Vec<LeafCell>,
    leaf_cost: usize,
    children: Vec<InternalCell>,
    images: PageImages,
    last_key: Option<Vec<u8>>,
}

impl Default for SortedTreeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl SortedTreeBuilder {
    pub fn new() -> Self {
        Self {
            cells: Vec::new(),
            leaf_cost: 0,
            children: Vec::new(),
            images: Vec::new(),
            last_key: None,
        }
    }

    pub fn push(&mut self, key: &[u8], value: &[u8], alloc: &mut PageAllocator) -> Result<()> {
        self.push_value(key, value, alloc)
    }

    pub(crate) fn push_value<V: ValueSource + ?Sized>(
        &mut self,
        key: &[u8],
        value: &V,
        alloc: &mut PageAllocator,
    ) -> Result<()> {
        if let Some(last) = self.last_key.as_deref() {
            if compare_keys(last, key) != Ordering::Less {
                return Err(EngineError::Internal(
                    "sorted tree builder requires strictly increasing keys".into(),
                ));
            }
        }
        self.last_key = Some(key.to_vec());
        let mut writer = TreeWriter::new_detached(alloc);
        let cell = writer.plan_cell(key, value)?;
        self.images.extend(writer.into_images());
        let cost = leaf_cell_cost(&cell);
        if !self.cells.is_empty() && self.leaf_cost + cost > NODE_CAPACITY {
            self.flush_leaf(alloc)?;
        }
        self.leaf_cost += cost;
        self.cells.push(cell);
        Ok(())
    }

    /// Returns completed page images for streaming writes.
    pub fn drain_images(&mut self) -> PageImages {
        sorted_page_images(std::mem::take(&mut self.images))
    }

    pub fn finish(mut self, alloc: &mut PageAllocator) -> Result<BuiltTree> {
        let mut writer = TreeWriter::new_detached(alloc);
        let root_page_id = if self.children.is_empty() {
            writer.finish_root(Run::Leaf(std::mem::take(&mut self.cells)))?
        } else {
            if !self.cells.is_empty() {
                let cells = std::mem::take(&mut self.cells);
                self.children.extend(writer.pack(Run::Leaf(cells))?);
            }
            writer.finish_root(Run::Internal {
                level: 1,
                cells: std::mem::take(&mut self.children),
            })?
        };
        self.images.extend(writer.into_images());
        Ok(BuiltTree {
            root_page_id,
            page_images: sorted_page_images(self.images),
        })
    }

    fn flush_leaf(&mut self, alloc: &mut PageAllocator) -> Result<()> {
        let cells = std::mem::take(&mut self.cells);
        self.leaf_cost = 0;
        let page_id = alloc.allocate();
        let min_key = cells[0].key.clone();
        self.images
            .push((page_id, encode_leaf_page(page_id, 0, 0, &cells)?));
        self.children.push(InternalCell {
            separator: min_key,
            child_page_id: page_id,
        });
        Ok(())
    }
}

pub fn build_catalog_tree(state: &CatalogState, alloc: &mut PageAllocator) -> Result<BuiltTree> {
    let mut entries = BTreeMap::new();
    entries.insert(
        CATALOG_SCHEMA_VERSION_KEY.to_vec(),
        encode_schema_version(state.schema_version)?,
    );
    entries.insert(
        CATALOG_CHANGE_FEED_FLOOR_TXID_KEY.to_vec(),
        encode_change_feed_floor_txid(state.change_feed_floor_txid)?,
    );
    if state.change_feed_policy != ChangeFeedPolicy::default() {
        entries.insert(
            CATALOG_CHANGE_FEED_POLICY_KEY.to_vec(),
            encode_change_feed_policy(&state.change_feed_policy)?,
        );
    }
    for (name, meta) in state.stores.iter() {
        if name.as_bytes().first() == Some(&0xff) {
            return Err(EngineError::Internal(format!(
                "store name {name} collides with the catalog metadata namespace"
            )));
        }
        entries.insert(name.as_bytes().to_vec(), encode_store_metadata(meta)?);
    }
    build_store_tree(&entries, alloc)
}

pub fn read_catalog<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
) -> Result<CatalogState> {
    let mut out = BTreeMap::new();
    let mut schema_version = 0u64;
    let mut change_feed_floor_txid = 0u64;
    let mut change_feed_policy = ChangeFeedPolicy::default();
    for pair in load_all_entries(pager, root_page_id)? {
        if pair.key == CATALOG_SCHEMA_VERSION_KEY {
            schema_version = decode_schema_version(&pair.value)?;
            continue;
        }
        if pair.key == CATALOG_CHANGE_FEED_FLOOR_TXID_KEY {
            change_feed_floor_txid = decode_change_feed_floor_txid(&pair.value)?;
            continue;
        }
        if pair.key == CATALOG_CHANGE_FEED_POLICY_KEY {
            change_feed_policy = decode_change_feed_policy(&pair.value)?;
            continue;
        }
        if pair.key.first() == Some(&0xff) {
            return Err(EngineError::Corruption(
                "unknown catalog metadata record".into(),
            ));
        }
        let name = String::from_utf8(pair.key)
            .map_err(|err| EngineError::Corruption(format!("catalog key utf8: {err}")))?;
        let meta = decode_store_metadata(&pair.value)
            .map_err(|err| EngineError::Corruption(err.to_string()))?;
        out.insert(name, meta);
    }
    Ok(CatalogState {
        schema_version,
        change_feed_floor_txid,
        change_feed_policy,
        stores: out,
    })
}

/// Applies mutations with strictly increasing keys by copying touched paths.
/// Untouched subtrees and overflow chains remain shared with the previous tree.
/// Replaced pages and chains are retired through `alloc`. Underfull nodes merge,
/// and roots with one child collapse.
pub fn apply_mutations<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    mutations: &[Mutation<'_>],
    alloc: &mut PageAllocator,
) -> Result<BuiltTree> {
    apply_value_mutations(pager, root_page_id, mutations, alloc)
}

pub(crate) fn apply_value_mutations<B: FileBackend, V: ValueSource + ?Sized>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    mutations: &[(&[u8], Option<&V>)],
    alloc: &mut PageAllocator,
) -> Result<BuiltTree> {
    if mutations
        .windows(2)
        .any(|pair| compare_keys(pair[0].0, pair[1].0) != Ordering::Less)
    {
        return Err(EngineError::Internal(
            "tree mutations must be sorted by unique key".into(),
        ));
    }
    if mutations.is_empty() {
        return Ok(BuiltTree {
            root_page_id,
            page_images: Vec::new(),
        });
    }
    let mut writer = TreeWriter::new(pager, alloc);
    let run = if root_page_id == 0 {
        writer.merge_leaf(Vec::new(), mutations)?
    } else {
        writer.rewrite(root_page_id, None, mutations)?
    };
    let root_page_id = writer.finish_root(run)?;
    Ok(BuiltTree {
        root_page_id,
        page_images: writer.into_images(),
    })
}

enum TreeLinks {
    Leaf(Vec<(u64, usize)>, Vec<PayloadRef>),
    Internal { level: u8, children: Vec<u64> },
}

/// Retires every page of a committed tree, including overflow chains.
pub fn free_tree<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    alloc: &mut PageAllocator,
) -> Result<()> {
    retire_tree(pager, root_page_id, alloc, true)
}

pub(crate) fn drop_tree_payload_references<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    alloc: &mut PageAllocator,
) -> Result<()> {
    retire_tree(pager, root_page_id, alloc, false)
}

fn retire_tree<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    alloc: &mut PageAllocator,
    retire_pages: bool,
) -> Result<()> {
    if root_page_id == 0 {
        return Ok(());
    }
    let mut visited = HashSet::new();
    let mut stack = vec![(root_page_id, None::<u8>)];
    while let Some((page_id, expected_level)) = stack.pop() {
        if !visited.insert(page_id) {
            return Err(EngineError::Corruption(format!(
                "page {page_id} is reachable twice in one tree"
            )));
        }
        // Retirement needs validated links, without copies of keys or inline values.
        let links = pager.with_page(page_id, |bytes| {
            let header = node_header(bytes, page_id, expected_level)?;
            let count = header.cell_count as usize;
            let mut previous: Option<&[u8]> = None;
            match header.page_kind {
                PageKind::Leaf => {
                    let mut overflow = Vec::new();
                    let mut external = Vec::new();
                    for index in 0..count {
                        let slot = read_cell_slot(bytes, &header, index)?;
                        let cell = decode_leaf_cell_ref(bytes, slot)?;
                        if previous.is_some_and(|key| compare_keys(key, cell.key) != Ordering::Less)
                        {
                            return Err(EngineError::Corruption(format!(
                                "leaf page {page_id} keys are not strictly increasing"
                            )));
                        }
                        previous = Some(cell.key);
                        if cell.value_kind == ValueKind::Overflow {
                            overflow
                                .push((cell.overflow_head_page_id, cell.total_value_len as usize));
                        } else if cell.value_kind == ValueKind::External {
                            external.push(
                                decode_payload_descriptor(
                                    cell.overflow_head_page_id,
                                    cell.total_value_len,
                                    cell.inline_value,
                                )?
                                .0,
                            );
                        }
                    }
                    Ok(TreeLinks::Leaf(overflow, external))
                }
                _ => {
                    let mut children = Vec::with_capacity(count);
                    for index in 0..count {
                        let slot = read_cell_slot(bytes, &header, index)?;
                        let cell = decode_internal_cell_ref(bytes, slot)?;
                        if previous
                            .is_some_and(|key| compare_keys(key, cell.separator) != Ordering::Less)
                        {
                            return Err(EngineError::Corruption(format!(
                                "internal page {page_id} separators are not strictly increasing"
                            )));
                        }
                        previous = Some(cell.separator);
                        children.push(cell.child_page_id);
                    }
                    Ok(TreeLinks::Internal {
                        level: header.level,
                        children,
                    })
                }
            }
        })?;
        if retire_pages {
            alloc.free(page_id);
        }
        match links {
            TreeLinks::Leaf(overflow, external) => {
                alloc.dropped_payload_refs.extend(external);
                if retire_pages {
                    for (head_page_id, total_len) in overflow {
                        free_overflow_chain(pager, head_page_id, total_len, alloc)?;
                    }
                }
            }
            TreeLinks::Internal { level, children } => {
                for child_page_id in children {
                    stack.push((child_page_id, Some(level - 1)));
                }
            }
        }
    }
    Ok(())
}

/// Collects up to `limit` keys strictly below `upper`, in order.
pub fn collect_keys_below<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    upper: &[u8],
    limit: usize,
) -> Result<Vec<Vec<u8>>> {
    // Borrow the caller bound. An owned RangeSpec would copy it once, and the
    // iterator would copy it again.
    let mut iter = TreeIter::with_bounds(
        pager,
        root_page_id,
        None,
        Some((upper, false)),
        false,
        Some(limit),
        0,
    )?;
    let mut out = if limit <= 4096 {
        Vec::with_capacity(limit)
    } else {
        Vec::new()
    };
    while out.len() < limit {
        let Some(key) = iter.next_key(pager)? else {
            break;
        };
        out.push(key);
    }
    Ok(out)
}

const NODE_CAPACITY: usize = PAGE_SIZE - PAGE_HEADER_SIZE;
const MIN_NODE_FILL: usize = NODE_CAPACITY / 4;

fn leaf_cell_cost(cell: &LeafCell) -> usize {
    let overflow = cell.value_kind == ValueKind::Overflow;
    leaf_cell_size(cell.key.len(), cell.value.len(), overflow) + 2
}

fn internal_cell_cost(cell: &InternalCell) -> usize {
    internal_cell_size(cell.separator.len()) + 2
}

enum Node {
    Leaf(Vec<LeafCell>),
    Internal { level: u8, cells: Vec<InternalCell> },
}

/// Defers page packing until the parent can merge underfull siblings.
enum Run {
    Leaf(Vec<LeafCell>),
    Internal { level: u8, cells: Vec<InternalCell> },
}

impl Run {
    fn level(&self) -> u8 {
        match self {
            Run::Leaf(_) => 0,
            Run::Internal { level, .. } => *level,
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            Run::Leaf(cells) => cells.is_empty(),
            Run::Internal { cells, .. } => cells.is_empty(),
        }
    }

    fn cost(&self) -> usize {
        match self {
            Run::Leaf(cells) => cells.iter().map(leaf_cell_cost).sum(),
            Run::Internal { cells, .. } => cells.iter().map(internal_cell_cost).sum(),
        }
    }

    fn append(&mut self, other: Run) -> Result<()> {
        match (self, other) {
            (Run::Leaf(left), Run::Leaf(right)) => left.extend(right),
            (
                Run::Internal { level, cells: left },
                Run::Internal {
                    level: other_level,
                    cells: right,
                },
            ) if *level == other_level => left.extend(right),
            _ => {
                return Err(EngineError::Corruption(
                    "sibling subtrees have different heights".into(),
                ))
            }
        }
        Ok(())
    }
}

enum Child {
    Keep(InternalCell),
    Changed(Run),
}

struct TreeWriter<'a, B: FileBackend> {
    pager: Option<&'a mut Pager<B>>,
    alloc: &'a mut PageAllocator,
    // Root collapse may discard fresh tree nodes. Overflow images stay final:
    // leaf merging moves their references without replacing the new values.
    fresh: BTreeMap<u64, Vec<u8>>,
    overflow_images: PageImages,
}

impl<'a> TreeWriter<'a, crate::storage::memory::MemoryBackend> {
    fn new_detached(alloc: &'a mut PageAllocator) -> Self {
        Self {
            pager: None,
            alloc,
            fresh: BTreeMap::new(),
            overflow_images: Vec::new(),
        }
    }
}

impl<'a, B: FileBackend> TreeWriter<'a, B> {
    fn new(pager: &'a mut Pager<B>, alloc: &'a mut PageAllocator) -> Self {
        Self {
            pager: Some(pager),
            alloc,
            fresh: BTreeMap::new(),
            overflow_images: Vec::new(),
        }
    }

    fn into_images(mut self) -> PageImages {
        self.overflow_images.extend(self.fresh);
        // Keep page-id order for WAL and replay when allocation reuses ids.
        sorted_page_images(self.overflow_images)
    }

    fn pager(&mut self) -> Result<&mut Pager<B>> {
        self.pager
            .as_deref_mut()
            .ok_or_else(|| EngineError::Internal("tree writer has no committed tree".into()))
    }

    fn rewrite<V: ValueSource + ?Sized>(
        &mut self,
        page_id: u64,
        expected_level: Option<u8>,
        mutations: &[(&[u8], Option<&V>)],
    ) -> Result<Run> {
        let node = read_node(self.pager()?, page_id, expected_level, mutations)?;
        self.alloc.free(page_id);
        match node {
            Node::Leaf(cells) => self.merge_leaf(cells, mutations),
            Node::Internal { level, cells } => {
                let child_level = level - 1;
                let routed = route_mutations(&cells, mutations);
                let mut items = Vec::with_capacity(cells.len());
                for (cell, range) in cells.into_iter().zip(routed) {
                    if range.is_empty() {
                        items.push(Child::Keep(cell));
                    } else {
                        let run =
                            self.rewrite(cell.child_page_id, Some(child_level), &mutations[range])?;
                        items.push(Child::Changed(run));
                    }
                }
                let cells = self.normalize_children(child_level, items)?;
                Ok(Run::Internal { level, cells })
            }
        }
    }

    fn merge_leaf<V: ValueSource + ?Sized>(
        &mut self,
        cells: Vec<LeafCell>,
        mutations: &[(&[u8], Option<&V>)],
    ) -> Result<Run> {
        let mut out = Vec::with_capacity(cells.len() + mutations.len());
        let mut existing = cells.into_iter().peekable();
        for (key, value) in mutations {
            while let Some(cell) = existing.peek() {
                if compare_keys(&cell.key, key) == Ordering::Less {
                    out.extend(existing.next());
                } else {
                    break;
                }
            }
            if matches!(existing.peek(), Some(cell) if cell.key.as_slice() == *key) {
                if let Some(old) = existing.next() {
                    self.release_cell(&old)?;
                }
            }
            if let Some(value) = value {
                out.push(self.plan_cell(key, *value)?);
            }
        }
        out.extend(existing);
        Ok(Run::Leaf(out))
    }

    fn release_cell(&mut self, cell: &LeafCell) -> Result<()> {
        if cell.value_kind == ValueKind::External {
            let (reference, _) = decode_payload_descriptor(
                cell.overflow_head_page_id,
                cell.total_value_len,
                &cell.value,
            )?;
            self.alloc.dropped_payload_refs.push(reference);
            return Ok(());
        }
        if cell.value_kind != ValueKind::Overflow {
            return Ok(());
        }
        let Some(pager) = self.pager.as_deref_mut() else {
            return Err(EngineError::Internal(
                "cannot release a committed overflow chain without a pager".into(),
            ));
        };
        free_overflow_chain(
            pager,
            cell.overflow_head_page_id,
            cell.total_value_len as usize,
            self.alloc,
        )
    }

    fn plan_cell<V: ValueSource + ?Sized>(&mut self, key: &[u8], value: &V) -> Result<LeafCell> {
        let parts = value.parts();
        let total_value_len =
            u32::try_from(parts.len()).map_err(|_| EngineError::ValueTooLarge(parts.len()))?;
        if let Some(reference) = value.external_reference() {
            if reference.body_len as usize != parts.payload().len() {
                return Err(EngineError::Internal(
                    "external reference body length mismatch".into(),
                ));
            }
            let descriptor = encode_payload_descriptor(&reference, parts.prefix())?;
            self.alloc.added_payload_refs.push(reference);
            return Ok(LeafCell {
                key: key.to_vec(),
                value: descriptor,
                value_kind: ValueKind::External,
                total_value_len,
                overflow_head_page_id: reference.first_page_id,
            });
        }
        if should_overflow_value(parts.len()) {
            let chain = write_overflow_value(value, self.alloc)?;
            self.overflow_images.extend(chain.pages);
            Ok(LeafCell {
                key: key.to_vec(),
                value: Vec::new(),
                value_kind: ValueKind::Overflow,
                total_value_len,
                overflow_head_page_id: chain.head_page_id,
            })
        } else {
            Ok(LeafCell {
                key: key.to_vec(),
                value: parts.to_vec(),
                value_kind: ValueKind::Inline,
                total_value_len,
                overflow_head_page_id: 0,
            })
        }
    }

    fn normalize_children(
        &mut self,
        child_level: u8,
        items: Vec<Child>,
    ) -> Result<Vec<InternalCell>> {
        let mut items: Vec<Child> = items
            .into_iter()
            .filter(|item| !matches!(item, Child::Changed(run) if run.is_empty()))
            .collect();

        let mut index = 0usize;
        while index < items.len() {
            let underfull =
                matches!(&items[index], Child::Changed(run) if run.cost() < MIN_NODE_FILL);
            if !underfull || items.len() < 2 {
                index += 1;
                continue;
            }
            let neighbour = if index + 1 < items.len() {
                index + 1
            } else {
                index - 1
            };
            let (low, high) = (index.min(neighbour), index.max(neighbour));
            let right = items.remove(high);
            let left = items.remove(low);
            let mut merged = self.child_into_run(left, child_level)?;
            merged.append(self.child_into_run(right, child_level)?)?;
            items.insert(low, Child::Changed(merged));
            index = low;
        }

        let mut cells = Vec::with_capacity(items.len());
        for item in items {
            match item {
                Child::Keep(cell) => cells.push(cell),
                Child::Changed(run) => {
                    if run.level() != child_level {
                        return Err(EngineError::Internal(
                            "rewritten child changed height".into(),
                        ));
                    }
                    cells.extend(self.pack(run)?);
                }
            }
        }
        Ok(cells)
    }

    fn child_into_run(&mut self, child: Child, child_level: u8) -> Result<Run> {
        match child {
            Child::Changed(run) => Ok(run),
            Child::Keep(cell) => {
                let node = read_node::<_, [u8]>(
                    self.pager()?,
                    cell.child_page_id,
                    Some(child_level),
                    &[],
                )?;
                self.alloc.free(cell.child_page_id);
                Ok(match node {
                    Node::Leaf(cells) => Run::Leaf(cells),
                    Node::Internal { level, cells } => Run::Internal { level, cells },
                })
            }
        }
    }

    /// Returns parent cells whose separators are each page's first key.
    fn pack(&mut self, run: Run) -> Result<Vec<InternalCell>> {
        if run.is_empty() {
            return Ok(Vec::new());
        }
        match run {
            Run::Leaf(cells) => {
                let costs: Vec<usize> = cells.iter().map(leaf_cell_cost).collect();
                let groups = split_groups(&costs);
                let ids: Vec<u64> = groups.iter().map(|_| self.alloc.allocate()).collect();
                let mut out = Vec::with_capacity(groups.len());
                let mut cells = cells.into_iter();
                for (index, len) in groups.into_iter().enumerate() {
                    let group: Vec<LeafCell> = cells.by_ref().take(len).collect();
                    let page_id = ids[index];
                    let right = ids.get(index + 1).copied().unwrap_or(0);
                    self.fresh
                        .insert(page_id, encode_leaf_page(page_id, 0, right, &group)?);
                    out.push(InternalCell {
                        separator: group[0].key.clone(),
                        child_page_id: page_id,
                    });
                }
                Ok(out)
            }
            Run::Internal { level, cells } => {
                if level == 0 || level > MAX_TREE_LEVEL {
                    return Err(EngineError::Internal(format!(
                        "tree height {level} is out of range"
                    )));
                }
                let costs: Vec<usize> = cells.iter().map(internal_cell_cost).collect();
                let groups = split_groups(&costs);
                let ids: Vec<u64> = groups.iter().map(|_| self.alloc.allocate()).collect();
                let mut out = Vec::with_capacity(groups.len());
                let mut cells = cells.into_iter();
                for (index, len) in groups.into_iter().enumerate() {
                    let group: Vec<InternalCell> = cells.by_ref().take(len).collect();
                    let page_id = ids[index];
                    let right = ids.get(index + 1).copied().unwrap_or(0);
                    self.fresh.insert(
                        page_id,
                        encode_internal_page(page_id, level, right, &group)?,
                    );
                    out.push(InternalCell {
                        separator: group[0].separator.clone(),
                        child_page_id: page_id,
                    });
                }
                Ok(out)
            }
        }
    }

    fn finish_root(&mut self, run: Run) -> Result<u64> {
        if run.is_empty() {
            let page_id = self.alloc.allocate();
            self.fresh
                .insert(page_id, encode_leaf_page(page_id, 0, 0, &[])?);
            return Ok(page_id);
        }
        let mut level = run.level();
        let mut cells = match run {
            Run::Internal { cells, .. } if cells.len() == 1 => {
                level = level
                    .checked_sub(1)
                    .ok_or_else(|| EngineError::Internal("internal root has level 0".into()))?;
                cells
            }
            run => self.pack(run)?,
        };
        while cells.len() > 1 {
            level = level
                .checked_add(1)
                .filter(|level| *level <= MAX_TREE_LEVEL)
                .ok_or_else(|| EngineError::Internal("tree grew beyond the height limit".into()))?;
            cells = self.pack(Run::Internal { level, cells })?;
        }
        let mut root = cells
            .pop()
            .ok_or_else(|| EngineError::Internal("tree root disappeared".into()))?
            .child_page_id;
        while let Some(only_child) = self.single_child(root, level)? {
            if self.fresh.remove(&root).is_some() {
                self.alloc.release_unpublished(root);
            } else {
                self.alloc.free(root);
            }
            root = only_child;
            level -= 1;
        }
        Ok(root)
    }

    fn single_child(&mut self, page_id: u64, expected_level: u8) -> Result<Option<u64>> {
        let inspect = |bytes: &[u8]| -> Result<Option<u64>> {
            let header = node_header(bytes, page_id, Some(expected_level))?;
            if header.page_kind == PageKind::Internal && header.cell_count == 1 {
                Ok(Some(child_page_id_at(bytes, &header, 0)?))
            } else {
                Ok(None)
            }
        };
        if let Some(bytes) = self.fresh.get(&page_id) {
            return inspect(bytes);
        }
        self.pager()?.with_page(page_id, inspect)
    }
}

/// Keys below the first separator still route to the first child, as in lookup.
fn route_mutations<V: ?Sized>(
    children: &[InternalCell],
    mutations: &[(&[u8], Option<&V>)],
) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::with_capacity(children.len());
    let mut start = 0usize;
    for index in 0..children.len() {
        let end = match children.get(index + 1) {
            Some(next) => {
                start
                    + mutations[start..].partition_point(|(key, _)| {
                        compare_keys(key, &next.separator) == Ordering::Less
                    })
            }
            None => mutations.len(),
        };
        ranges.push(start..end);
        start = end;
    }
    ranges
}

/// Balances page fill to avoid a nearly empty final page.
fn split_groups(costs: &[usize]) -> Vec<usize> {
    let total: usize = costs.iter().sum();
    if total <= NODE_CAPACITY {
        return vec![costs.len()];
    }
    let pages = total.div_ceil(NODE_CAPACITY);
    let target = total.div_ceil(pages);
    let mut groups = Vec::with_capacity(pages + 1);
    let mut current = 0usize;
    let mut count = 0usize;
    for cost in costs {
        if count > 0 && (current + cost > NODE_CAPACITY || current + cost / 2 > target) {
            groups.push(count);
            current = 0;
            count = 0;
        }
        current += cost;
        count += 1;
    }
    if count > 0 {
        groups.push(count);
    }
    groups
}

fn read_node<B: FileBackend, V: ?Sized>(
    pager: &mut Pager<B>,
    page_id: u64,
    expected_level: Option<u8>,
    mutations: &[(&[u8], Option<&V>)],
) -> Result<Node> {
    pager.with_page(page_id, |bytes| {
        let header = node_header(bytes, page_id, expected_level)?;
        let count = header.cell_count as usize;
        match header.page_kind {
            PageKind::Leaf => {
                let mut cells: Vec<LeafCell> = Vec::with_capacity(count);
                let mut mutation_index = 0;
                for index in 0..count {
                    let slot = read_cell_slot(bytes, &header, index)?;
                    let cell = decode_leaf_cell_ref(bytes, slot)?;
                    if let Some(previous) = cells.last() {
                        if compare_keys(&previous.key, cell.key) != Ordering::Less {
                            return Err(EngineError::Corruption(format!(
                                "leaf page {page_id} keys are not strictly increasing"
                            )));
                        }
                    }
                    while mutations
                        .get(mutation_index)
                        .is_some_and(|(key, _)| compare_keys(key, cell.key) == Ordering::Less)
                    {
                        mutation_index += 1;
                    }
                    let replaced = mutations
                        .get(mutation_index)
                        .is_some_and(|(key, _)| *key == cell.key);
                    cells.push(LeafCell {
                        key: cell.key.to_vec(),
                        // External descriptors carry the reference to retire.
                        // Other replaced values need only their cell metadata.
                        value: if replaced && cell.value_kind != ValueKind::External {
                            Vec::new()
                        } else {
                            cell.inline_value.to_vec()
                        },
                        value_kind: cell.value_kind,
                        total_value_len: cell.total_value_len,
                        overflow_head_page_id: cell.overflow_head_page_id,
                    });
                }
                Ok(Node::Leaf(cells))
            }
            _ => {
                let mut cells: Vec<InternalCell> = Vec::with_capacity(count);
                for index in 0..count {
                    let slot = read_cell_slot(bytes, &header, index)?;
                    let cell = decode_internal_cell_ref(bytes, slot)?;
                    if let Some(previous) = cells.last() {
                        if compare_keys(&previous.separator, cell.separator) != Ordering::Less {
                            return Err(EngineError::Corruption(format!(
                                "internal page {page_id} separators are not strictly increasing"
                            )));
                        }
                    }
                    cells.push(InternalCell {
                        separator: cell.separator.to_vec(),
                        child_page_id: cell.child_page_id,
                    });
                }
                Ok(Node::Internal {
                    level: header.level,
                    cells,
                })
            }
        }
    })
}

/// Checks tree-node identity and any parent-implied level.
/// Bytes must be verified or freshly encoded because this skips the checksum.
fn node_header(bytes: &[u8], page_id: u64, expected_level: Option<u8>) -> Result<PageHeaderInfo> {
    let header = decode_page_header_verified(bytes)?;
    if header.page_id != page_id {
        return Err(EngineError::Corruption(format!(
            "page header id mismatch: expected {page_id}, got {}",
            header.page_id
        )));
    }
    if header.page_kind == PageKind::Overflow {
        return Err(EngineError::Corruption(format!(
            "tree traversal reached overflow page {page_id}"
        )));
    }
    if let Some(expected) = expected_level {
        if header.level != expected {
            return Err(EngineError::Corruption(format!(
                "page {page_id} has level {}, parent expects {expected}",
                header.level
            )));
        }
    }
    Ok(header)
}

enum LookupStep<T> {
    Descend(u64, u8),
    Found(T),
    NotFound,
}

enum PointValue<T> {
    Inline(T, Option<PayloadRef>),
    Overflow {
        head_page_id: u64,
        total_len: usize,
    },
    External {
        payload: PayloadRef,
        prefix: Vec<u8>,
    },
}

pub(crate) enum PendingValue {
    Inline(Vec<u8>),
    Overflow {
        head_page_id: u64,
        total_len: usize,
    },
    External {
        payload: PayloadRef,
        prefix: Vec<u8>,
    },
}

pub(crate) struct PendingKvPair {
    pub key: Vec<u8>,
    pub value: PendingValue,
}

#[derive(Debug, Clone)]
struct CursorFrame {
    page_id: u64,
    level: u8,
    child_index: usize,
    child_count: usize,
}

#[derive(Clone, Copy)]
enum Descent<'k> {
    Leftmost,
    Rightmost,
    Key(&'k [u8]),
}

#[derive(Debug, Clone)]
struct LeafCursor {
    current: u64,
    stack: Vec<CursorFrame>,
}

impl LeafCursor {
    fn open<B: FileBackend>(
        pager: &mut Pager<B>,
        root_page_id: u64,
        descent: Descent<'_>,
    ) -> Result<Option<Self>> {
        if root_page_id == 0 {
            return Ok(None);
        }
        let mut stack = Vec::new();
        let current = descend(pager, root_page_id, None, &mut stack, descent)?;
        Ok(Some(Self { current, stack }))
    }

    fn advance<B: FileBackend>(&mut self, pager: &mut Pager<B>) -> Result<bool> {
        while let Some(mut frame) = self.stack.pop() {
            if frame.child_index + 1 >= frame.child_count {
                continue;
            }
            frame.child_index += 1;
            let next_child = sibling_child(pager, &frame)?;
            let child_level = frame.level - 1;
            self.stack.push(frame);
            self.current = descend(
                pager,
                next_child,
                Some(child_level),
                &mut self.stack,
                Descent::Leftmost,
            )?;
            return Ok(true);
        }
        Ok(false)
    }

    fn retreat<B: FileBackend>(&mut self, pager: &mut Pager<B>) -> Result<bool> {
        while let Some(mut frame) = self.stack.pop() {
            if frame.child_index == 0 {
                continue;
            }
            frame.child_index -= 1;
            let previous_child = sibling_child(pager, &frame)?;
            let child_level = frame.level - 1;
            self.stack.push(frame);
            self.current = descend(
                pager,
                previous_child,
                Some(child_level),
                &mut self.stack,
                Descent::Rightmost,
            )?;
            return Ok(true);
        }
        Ok(false)
    }
}

fn sibling_child<B: FileBackend>(pager: &mut Pager<B>, frame: &CursorFrame) -> Result<u64> {
    pager.with_page(frame.page_id, |bytes| {
        let header = node_header(bytes, frame.page_id, Some(frame.level))?;
        if header.page_kind != PageKind::Internal || header.cell_count as usize != frame.child_count
        {
            return Err(EngineError::Corruption(format!(
                "cursor parent {} changed shape",
                frame.page_id
            )));
        }
        child_page_id_at(bytes, &header, frame.child_index)
    })
}

fn descend<B: FileBackend>(
    pager: &mut Pager<B>,
    mut current: u64,
    mut expected_level: Option<u8>,
    stack: &mut Vec<CursorFrame>,
    descent: Descent<'_>,
) -> Result<u64> {
    loop {
        let step = pager.with_page(current, |bytes| {
            let header = node_header(bytes, current, expected_level)?;
            if header.page_kind == PageKind::Leaf {
                return Ok(None);
            }
            let child_count = header.cell_count as usize;
            let (child_index, child_page_id) = match descent {
                Descent::Leftmost => (0, child_page_id_at(bytes, &header, 0)?),
                Descent::Rightmost => (
                    child_count - 1,
                    child_page_id_at(bytes, &header, child_count - 1)?,
                ),
                Descent::Key(key) => choose_internal_child_in_page(bytes, &header, key)?,
            };
            Ok(Some((
                header.level,
                child_index,
                child_count,
                child_page_id,
            )))
        })?;
        let Some((level, child_index, child_count, child_page_id)) = step else {
            return Ok(current);
        };
        stack.push(CursorFrame {
            page_id: current,
            level,
            child_index,
            child_count,
        });
        current = child_page_id;
        expected_level = Some(level - 1);
    }
}

struct ValidatedLeafWindow {
    header: PageHeaderInfo,
    start: usize,
    end: usize,
    hit_bound: bool,
}

/// Reads one tree in key order or reverse order.
/// A finite limit restricts copying after the full leaf window is validated.
/// Unlimited scans buffer one leaf to avoid rereads after overflow I/O.
pub(crate) struct TreeIter<'a> {
    cursor: Option<LeafCursor>,
    ready: Vec<PendingKvPair>,
    ready_at: usize,
    window: Option<ValidatedLeafWindow>,
    lower: Option<(Cow<'a, [u8]>, bool)>,
    upper: Option<(Cow<'a, [u8]>, bool)>,
    max_buffered_entries: usize,
    inline_prefix_len: usize,
    reverse: bool,
    loaded_first: bool,
    exhausted: bool,
}

impl TreeIter<'static> {
    pub(crate) fn new<B: FileBackend>(
        pager: &mut Pager<B>,
        root_page_id: u64,
        range: &RangeSpec,
    ) -> Result<Self> {
        range.validate()?;
        let lower = range
            .lower_bound()
            .map(|(key, inclusive)| (Cow::Owned(key.to_vec()), inclusive));
        let upper = range
            .upper_bound()
            .map(|(key, inclusive)| (Cow::Owned(key.to_vec()), inclusive));
        TreeIter::open(
            pager,
            root_page_id,
            lower,
            upper,
            range.reverse,
            range.limit,
            usize::MAX,
        )
    }
}

impl<'a> TreeIter<'a> {
    fn with_bounds<B: FileBackend>(
        pager: &mut Pager<B>,
        root_page_id: u64,
        lower: Option<(&'a [u8], bool)>,
        upper: Option<(&'a [u8], bool)>,
        reverse: bool,
        limit: Option<usize>,
        inline_prefix_len: usize,
    ) -> Result<Self> {
        Self::open(
            pager,
            root_page_id,
            lower.map(|(key, inclusive)| (Cow::Borrowed(key), inclusive)),
            upper.map(|(key, inclusive)| (Cow::Borrowed(key), inclusive)),
            reverse,
            limit,
            inline_prefix_len,
        )
    }

    fn open<B: FileBackend>(
        pager: &mut Pager<B>,
        root_page_id: u64,
        lower: Option<(Cow<'a, [u8]>, bool)>,
        upper: Option<(Cow<'a, [u8]>, bool)>,
        reverse: bool,
        limit: Option<usize>,
        inline_prefix_len: usize,
    ) -> Result<Self> {
        let descent = if reverse {
            match upper.as_ref() {
                Some((key, _)) => Descent::Key(key.as_ref()),
                None => Descent::Rightmost,
            }
        } else {
            match lower.as_ref() {
                Some((key, _)) => Descent::Key(key.as_ref()),
                None => Descent::Leftmost,
            }
        };
        let cursor = LeafCursor::open(pager, root_page_id, descent)?;
        Ok(Self {
            exhausted: cursor.is_none(),
            cursor,
            ready: Vec::new(),
            ready_at: 0,
            window: None,
            lower,
            upper,
            // Filtering and staged deletes may require more entries than range.limit.
            max_buffered_entries: if limit.is_some() { 1 } else { usize::MAX },
            inline_prefix_len,
            reverse,
            loaded_first: false,
        })
    }

    fn ready_remaining(&self) -> usize {
        self.ready.len().saturating_sub(self.ready_at)
    }

    fn pop_ready(&mut self) -> Option<PendingKvPair> {
        if self.ready_at >= self.ready.len() {
            return None;
        }
        let index = self.ready_at;
        self.ready_at += 1;
        Some(std::mem::replace(
            &mut self.ready[index],
            blank_pending_pair(),
        ))
    }

    fn next_key<B: FileBackend>(&mut self, pager: &mut Pager<B>) -> Result<Option<Vec<u8>>> {
        self.next(pager).map(|pair| pair.map(|pair| pair.key))
    }

    pub(crate) fn next<B: FileBackend>(
        &mut self,
        pager: &mut Pager<B>,
    ) -> Result<Option<PendingKvPair>> {
        loop {
            if let Some(pair) = self.pop_ready() {
                validate_pending_payload_range(pager, &pair.value)?;
                return Ok(Some(pair));
            }
            if let Some(window) = self.window.as_mut() {
                if window.start < window.end {
                    let page_id = self
                        .cursor
                        .as_ref()
                        .ok_or_else(|| {
                            EngineError::Internal("leaf window without a cursor".into())
                        })?
                        .current;
                    let index = if self.reverse {
                        window.end - 1
                    } else {
                        window.start
                    };
                    let inline_prefix_len = self.inline_prefix_len;
                    let pair = pager.with_page(page_id, |bytes| {
                        let slot = read_cell_slot(bytes, &window.header, index)?;
                        let cell = decode_leaf_cell_ref(bytes, slot)?;
                        Ok(PendingKvPair {
                            key: cell.key.to_vec(),
                            value: pending_value_from_leaf_ref(&cell, inline_prefix_len)?,
                        })
                    })?;
                    if self.reverse {
                        window.end -= 1;
                    } else {
                        window.start += 1;
                    }
                    validate_pending_payload_range(pager, &pair.value)?;
                    return Ok(Some(pair));
                }
            }
            self.window = None;
            if self.exhausted {
                return Ok(None);
            }
            let Some(cursor) = self.cursor.as_mut() else {
                self.exhausted = true;
                return Ok(None);
            };
            if self.loaded_first {
                let moved = if self.reverse {
                    cursor.retreat(pager)?
                } else {
                    cursor.advance(pager)?
                };
                if !moved {
                    self.exhausted = true;
                    return Ok(None);
                }
            }
            self.loaded_first = true;
            let page_id = cursor.current;
            let mut ready = std::mem::take(&mut self.ready);
            self.ready_at = 0;
            let reverse = self.reverse;
            let max_buffered_entries = self.max_buffered_entries;
            let inline_prefix_len = self.inline_prefix_len;
            let window = {
                let lower = self
                    .lower
                    .as_ref()
                    .map(|(key, inclusive)| (key.as_ref(), *inclusive));
                let upper = self
                    .upper
                    .as_ref()
                    .map(|(key, inclusive)| (key.as_ref(), *inclusive));
                pager.with_page(page_id, |bytes| {
                    collect_leaf_window(
                        bytes,
                        page_id,
                        (lower, upper),
                        reverse,
                        max_buffered_entries,
                        inline_prefix_len,
                        &mut ready,
                    )
                })?
            };
            self.ready = ready;
            if window.hit_bound {
                self.exhausted = true;
            }
            self.window = Some(window);
        }
    }
}

fn blank_pending_pair() -> PendingKvPair {
    PendingKvPair {
        key: Vec::new(),
        value: PendingValue::Inline(Vec::new()),
    }
}

type ScanBound<'a> = Option<(&'a [u8], bool)>;

/// Validates every cell in the window before returning a batch in scan order.
fn collect_leaf_window(
    bytes: &[u8],
    page_id: u64,
    bounds: (ScanBound<'_>, ScanBound<'_>),
    reverse: bool,
    max_buffered_entries: usize,
    inline_prefix_len: usize,
    pairs: &mut Vec<PendingKvPair>,
) -> Result<ValidatedLeafWindow> {
    let (lower, upper) = bounds;
    let header = node_header(bytes, page_id, Some(0))?;
    let count = header.cell_count as usize;
    let start = match lower {
        Some((key, inclusive)) => leaf_partition(bytes, &header, |cell| {
            let ordering = compare_keys(cell, key);
            ordering == Ordering::Less || (ordering == Ordering::Equal && !inclusive)
        })?,
        None => 0,
    };
    let end = match upper {
        Some((key, inclusive)) => leaf_partition(bytes, &header, |cell| {
            let ordering = compare_keys(cell, key);
            ordering == Ordering::Less || (ordering == Ordering::Equal && inclusive)
        })?,
        None => count,
    };
    let buffered_count = end.saturating_sub(start).min(max_buffered_entries);
    let (copy_start, copy_end) = if reverse {
        (end.max(start) - buffered_count, end.max(start))
    } else {
        (start, start + buffered_count)
    };
    pairs.clear();
    if buffered_count > pairs.capacity() {
        pairs.reserve_exact(buffered_count - pairs.capacity());
    }
    for index in start..end.max(start) {
        let slot = read_cell_slot(bytes, &header, index)?;
        let cell = decode_leaf_cell_ref(bytes, slot)?;
        if index >= copy_start && index < copy_end {
            pairs.push(PendingKvPair {
                key: cell.key.to_vec(),
                value: pending_value_from_leaf_ref(&cell, inline_prefix_len)?,
            });
        }
    }
    let hit_bound = if reverse {
        pairs.reverse();
        start > 0
    } else {
        end < count
    };
    Ok(ValidatedLeafWindow {
        header,
        start: if reverse { start } else { copy_end },
        end: if reverse { copy_start } else { end.max(start) },
        hit_bound,
    })
}

/// First index whose key does not satisfy `before`.
fn leaf_partition(
    bytes: &[u8],
    header: &PageHeaderInfo,
    before: impl Fn(&[u8]) -> bool,
) -> Result<usize> {
    let mut lo = 0usize;
    let mut hi = header.cell_count as usize;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let slot = read_cell_slot(bytes, header, mid)?;
        let cell = decode_leaf_cell_ref(bytes, slot)?;
        if before(cell.key) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    Ok(lo)
}

fn lookup_leaf_in_page<T>(
    bytes: &[u8],
    header: &PageHeaderInfo,
    key: &[u8],
    mut read_value: impl FnMut(&crate::page::LeafCellRef<'_>) -> Result<T>,
) -> Result<LookupStep<T>> {
    let mut lo = 0usize;
    let mut hi = header.cell_count as usize;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let slot = read_cell_slot(bytes, header, mid)?;
        let cell = decode_leaf_cell_ref(bytes, slot)?;
        match compare_keys(cell.key, key) {
            Ordering::Less => lo = mid + 1,
            Ordering::Greater => hi = mid,
            Ordering::Equal => {
                return read_value(&cell).map(LookupStep::Found);
            }
        }
    }
    Ok(LookupStep::NotFound)
}

fn choose_internal_child_in_page(
    bytes: &[u8],
    header: &PageHeaderInfo,
    key: &[u8],
) -> Result<(usize, u64)> {
    let child_count = header.cell_count as usize;
    if child_count == 0 {
        return Err(EngineError::Corruption(
            "internal page has no children".into(),
        ));
    }
    let mut lo = 0usize;
    let mut hi = child_count;
    // The rightmost separator <= key, plus child 0 when every probe is greater.
    // Both were already decoded, so the chosen child does not need a second read.
    let mut chosen: Option<(usize, u64)> = None;
    let mut leftmost_child = None;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let slot = read_cell_slot(bytes, header, mid)?;
        let cell = decode_internal_cell_ref(bytes, slot)?;
        if mid == 0 {
            leftmost_child = Some(cell.child_page_id);
        }
        if compare_keys(cell.separator, key) != Ordering::Greater {
            chosen = Some((mid, cell.child_page_id));
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    let index = lo.saturating_sub(1);
    if let Some((chosen_index, child_page_id)) = chosen {
        if chosen_index == index {
            return Ok((index, child_page_id));
        }
    }
    if index == 0 {
        if let Some(child_page_id) = leftmost_child {
            return Ok((index, child_page_id));
        }
    }
    Ok((index, child_page_id_at(bytes, header, index)?))
}

fn child_page_id_at(bytes: &[u8], header: &PageHeaderInfo, index: usize) -> Result<u64> {
    let slot = read_cell_slot(bytes, header, index)?;
    Ok(decode_internal_cell_ref(bytes, slot)?.child_page_id)
}

fn pending_value_from_leaf_ref(
    cell: &crate::page::LeafCellRef<'_>,
    inline_prefix_len: usize,
) -> Result<PendingValue> {
    if cell.value_kind == ValueKind::Inline {
        Ok(PendingValue::Inline(
            cell.inline_value[..inline_prefix_len.min(cell.inline_value.len())].to_vec(),
        ))
    } else if cell.value_kind == ValueKind::Overflow {
        Ok(PendingValue::Overflow {
            head_page_id: cell.overflow_head_page_id,
            total_len: cell.total_value_len as usize,
        })
    } else {
        let (payload, prefix) = decode_payload_descriptor(
            cell.overflow_head_page_id,
            cell.total_value_len,
            cell.inline_value,
        )?;
        Ok(PendingValue::External {
            payload,
            prefix: prefix[..inline_prefix_len.min(prefix.len())].to_vec(),
        })
    }
}

fn point_external_value<T>(cell: &crate::page::LeafCellRef<'_>) -> Result<PointValue<T>> {
    let (payload, prefix) = decode_payload_descriptor(
        cell.overflow_head_page_id,
        cell.total_value_len,
        cell.inline_value,
    )?;
    Ok(PointValue::External {
        payload,
        prefix: prefix.to_vec(),
    })
}

fn validate_optional_payload_range<B: FileBackend>(
    pager: &Pager<B>,
    payload: Option<&PayloadRef>,
) -> Result<()> {
    if let Some(payload) = payload {
        pager.validate_page_range(payload.first_page_id, payload.end_page_id()?)?;
    }
    Ok(())
}

fn validate_pending_payload_range<B: FileBackend>(
    pager: &Pager<B>,
    value: &PendingValue,
) -> Result<()> {
    if let PendingValue::External { payload, .. } = value {
        validate_optional_payload_range(pager, Some(payload))?;
    }
    Ok(())
}

pub(crate) fn materialize_pending_value<B: FileBackend>(
    pager: &mut Pager<B>,
    value: PendingValue,
) -> Result<Vec<u8>> {
    match value {
        PendingValue::Inline(value) => Ok(value),
        PendingValue::Overflow {
            head_page_id,
            total_len,
        } => read_overflow_value(pager, head_page_id, total_len),
        PendingValue::External { payload, prefix } => {
            validate_optional_payload_range(pager, Some(&payload))?;
            let mut value = Vec::with_capacity(prefix.len() + payload.body_len as usize);
            value.extend_from_slice(&prefix);
            read_payload_into(pager.backend_ref(), &payload, |chunk| {
                value.extend_from_slice(chunk);
                Ok(())
            })?;
            Ok(value)
        }
    }
}

pub(crate) fn materialize_pending_stored_value<B: FileBackend>(
    pager: &mut Pager<B>,
    value: PendingValue,
    store_flags: u64,
) -> Result<StoredValue> {
    match value {
        PendingValue::Inline(value) => StoredValue::decode_owned_for_store(store_flags, value),
        PendingValue::Overflow {
            head_page_id,
            total_len,
        } => read_overflow_stored_value(pager, head_page_id, total_len, store_flags),
        PendingValue::External { payload, prefix } => {
            validate_optional_payload_range(pager, Some(&payload))?;
            let header_len = if store_uses_value_envelope(store_flags)
                && !store_uses_system_raw_values(store_flags)
            {
                store_value_prefix_len(store_flags)
            } else {
                0
            };
            let mut value = Vec::with_capacity(
                (prefix.len() + payload.body_len as usize).saturating_sub(header_len),
            );
            let expiry =
                write_external_stored_value(pager, &payload, &prefix, store_flags, |chunk| {
                    value.extend_from_slice(chunk);
                    Ok(())
                })?;
            let revision = if header_len > 0 {
                decode_value_envelope(store_flags, &prefix)?.1
            } else {
                None
            };
            Ok(StoredValue {
                value,
                expires_at_ms: expiry,
                revision,
            })
        }
    }
}

fn external_stored_value_info(
    payload: &PayloadRef,
    prefix: &[u8],
    store_flags: u64,
) -> Result<(usize, Option<u64>)> {
    let total_len = prefix.len() + payload.body_len as usize;
    if store_uses_value_envelope(store_flags) && !store_uses_system_raw_values(store_flags) {
        let expiry = decode_value_envelope(store_flags, prefix)?.0;
        Ok((total_len - store_value_prefix_len(store_flags), expiry))
    } else {
        Ok((total_len, None))
    }
}

fn write_external_stored_value<B: FileBackend>(
    pager: &mut Pager<B>,
    payload: &PayloadRef,
    prefix: &[u8],
    store_flags: u64,
    mut write: impl FnMut(&[u8]) -> Result<()>,
) -> Result<Option<u64>> {
    validate_optional_payload_range(pager, Some(payload))?;
    let enveloped =
        store_uses_value_envelope(store_flags) && !store_uses_system_raw_values(store_flags);
    let skip = if enveloped {
        store_value_prefix_len(store_flags).min(prefix.len())
    } else {
        0
    };
    let mut payload_prefix = [0; COMPRESSION_VALUE_HEADER_SIZE];
    let mut payload_prefix_len = (prefix.len() - skip).min(payload_prefix.len());
    payload_prefix[..payload_prefix_len].copy_from_slice(&prefix[skip..skip + payload_prefix_len]);
    write(&prefix[skip..])?;
    read_payload_into(pager.backend_ref(), payload, |chunk| {
        let take = (payload_prefix.len() - payload_prefix_len).min(chunk.len());
        payload_prefix[payload_prefix_len..payload_prefix_len + take]
            .copy_from_slice(&chunk[..take]);
        payload_prefix_len += take;
        write(chunk)
    })?;
    let expiry = if enveloped {
        decode_value_envelope(store_flags, prefix)?.0
    } else {
        None
    };
    let payload_len = prefix.len() - skip + payload.body_len as usize;
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
    Ok(expiry)
}

fn inline_stored_value_parts(store_flags: u64, bytes: &[u8]) -> Result<(&[u8], Option<u64>)> {
    if store_uses_system_raw_values(store_flags) {
        return Ok((bytes, None));
    }
    let (value, expiry) = if store_uses_value_envelope(store_flags) {
        let expiry = decode_value_envelope(store_flags, bytes)?.0;
        (&bytes[store_value_prefix_len(store_flags)..], expiry)
    } else {
        (bytes, None)
    };
    validate_store_value(value, store_flags).map_err(|error| match error {
        EngineError::Corruption(_) => error,
        error => EngineError::Corruption(error.to_string()),
    })?;
    Ok((value, expiry))
}

pub(crate) fn pending_stored_value_info<B: FileBackend>(
    pager: &mut Pager<B>,
    value: &PendingValue,
    store_flags: u64,
) -> Result<(usize, Option<u64>)> {
    match value {
        PendingValue::Inline(value) => {
            let (value, expiry) = inline_stored_value_parts(store_flags, value)?;
            Ok((value.len(), expiry))
        }
        PendingValue::Overflow {
            head_page_id,
            total_len,
        } => {
            if store_uses_value_envelope(store_flags) && !store_uses_system_raw_values(store_flags)
            {
                let expiry = read_overflow_expiry(pager, *head_page_id, *total_len, store_flags)?;
                Ok((total_len - store_value_prefix_len(store_flags), expiry))
            } else {
                Ok((*total_len, None))
            }
        }
        PendingValue::External { payload, prefix } => {
            validate_optional_payload_range(pager, Some(payload))?;
            external_stored_value_info(payload, prefix, store_flags)
        }
    }
}

pub(crate) fn write_pending_stored_value<B: FileBackend>(
    pager: &mut Pager<B>,
    value: PendingValue,
    store_flags: u64,
    mut write: impl FnMut(&[u8]) -> Result<()>,
) -> Result<Option<u64>> {
    match value {
        PendingValue::Inline(value) => {
            let (value, expiry) = inline_stored_value_parts(store_flags, &value)?;
            write(value)?;
            Ok(expiry)
        }
        PendingValue::Overflow {
            head_page_id,
            total_len,
        } => read_overflow_stored_value_into(pager, head_page_id, total_len, store_flags, write),
        PendingValue::External { payload, prefix } => {
            write_external_stored_value(pager, &payload, &prefix, store_flags, write)
        }
    }
}

pub(crate) fn pending_value_prefix<'a, B: FileBackend>(
    pager: &mut Pager<B>,
    value: &'a PendingValue,
    prefix_len: usize,
) -> Result<Cow<'a, [u8]>> {
    match value {
        PendingValue::Inline(value) => Ok(Cow::Borrowed(&value[..prefix_len.min(value.len())])),
        PendingValue::Overflow {
            head_page_id,
            total_len,
        } => Ok(Cow::Owned(read_overflow_prefix(
            pager,
            *head_page_id,
            *total_len,
            prefix_len,
        )?)),
        PendingValue::External { payload, prefix } => {
            validate_optional_payload_range(pager, Some(payload))?;
            let wanted = prefix_len.min(prefix.len() + payload.body_len as usize);
            let prefix_take = wanted.min(prefix.len());
            let mut value = Vec::with_capacity(wanted);
            value.extend_from_slice(&prefix[..prefix_take]);
            if wanted > prefix_take {
                value.extend_from_slice(&read_payload_prefix(
                    pager.backend_ref(),
                    payload,
                    wanted - prefix_take,
                )?);
            }
            Ok(Cow::Owned(value))
        }
    }
}

#[cfg(test)]
#[path = "btree/external_read_tests.rs"]
mod external_read_tests;
