use crate::bytes::{compare_keys, key_in_range};
use crate::catalog::{
    decode_change_feed_floor_txid, decode_schema_version, decode_store_metadata,
    encode_change_feed_floor_txid, encode_schema_version, encode_store_metadata, CatalogState,
    CATALOG_CHANGE_FEED_FLOOR_TXID_KEY, CATALOG_SCHEMA_VERSION_KEY,
};
use crate::error::{EngineError, Result};
use crate::layout::{PageKind, StoreMetadata, ValueKind, PAGE_HEADER_SIZE, PAGE_SIZE};
use crate::overflow::{read_overflow_value, write_overflow_chain};
use crate::page::{
    decode_internal_cell_ref, decode_leaf_cell_ref, decode_page_header, encode_internal_page,
    encode_leaf_page, internal_cell_size, leaf_cell_size, read_cell_slot, should_overflow_value,
    InternalCell, LeafCell,
};
use crate::pager::Pager;
use crate::storage::backend::FileBackend;
use crate::txn::MutationValue;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, VecDeque};

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

    fn lower_bound(&self) -> Option<(&[u8], bool)> {
        if let Some(bound) = self.gt.as_deref() {
            Some((bound, false))
        } else {
            self.gte.as_deref().map(|bound| (bound, true))
        }
    }

    fn upper_bound(&self) -> Option<(&[u8], bool)> {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafDescriptor {
    pub page_id: u64,
    pub min_key: Vec<u8>,
}

#[derive(Debug, Clone)]
struct LevelNode {
    min_key: Vec<u8>,
    page_id: u64,
    level: u8,
}

#[derive(Debug, Clone)]
struct CursorFrame {
    page_id: u64,
    child_index: usize,
    child_count: usize,
}

#[derive(Debug, Clone)]
struct LeafCursor {
    current: u64,
    stack: Vec<CursorFrame>,
}

#[derive(Debug, Clone)]
struct LeafWindow {
    start: usize,
    end: usize,
    mutations: Vec<(Vec<u8>, MutationValue)>,
}

enum LookupStep {
    Descend(u64),
    Found(PendingValue),
    NotFound,
}

enum PendingValue {
    Inline(Vec<u8>),
    Overflow { head_page_id: u64, total_len: usize },
}

struct PendingKvPair {
    key: Vec<u8>,
    value: PendingValue,
}

struct LeafScanChunk {
    pairs: Vec<PendingKvPair>,
    stop: bool,
}

pub fn lookup<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    key: &[u8],
) -> Result<Option<Vec<u8>>> {
    if root_page_id == 0 {
        return Ok(None);
    }
    let mut current = root_page_id;
    loop {
        let step = pager.with_page(current, |bytes| lookup_step_in_page(bytes, current, key))?;
        match step {
            LookupStep::Descend(child_page_id) => current = child_page_id,
            LookupStep::Found(value) => return materialize_pending_value(pager, value).map(Some),
            LookupStep::NotFound => return Ok(None),
        }
    }
}

pub fn load_all_entries<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
) -> Result<Vec<KvPair>> {
    if root_page_id == 0 {
        return Ok(Vec::new());
    }
    let Some(mut cursor) = LeafCursor::leftmost(pager, root_page_id)? else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    loop {
        let chunk = pager.with_page(cursor.current, |bytes| {
            collect_leaf_page_entries(bytes, cursor.current, None, None, usize::MAX)
        })?;
        append_pending_pairs(pager, chunk.pairs, &mut out)?;
        if !cursor.advance(pager)? {
            break;
        }
    }
    Ok(out)
}

pub fn scan<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    range: &RangeSpec,
) -> Result<Vec<KvPair>> {
    range.validate()?;
    if root_page_id == 0 || range.limit == Some(0) {
        return Ok(Vec::new());
    }
    if range.reverse {
        return scan_reverse(pager, root_page_id, range);
    }
    scan_forward(pager, root_page_id, range)
}

pub fn build_tree(entries: &[(Vec<u8>, Vec<u8>)], next_page_id: &mut u64) -> Result<BuiltTree> {
    // Keep the public API tolerant of unsorted input, but sort lightweight
    // borrowed pairs only. The leaf builder below already copies into page cells,
    // so cloning every key/value before sorting would double the hot-path work.
    let mut ordered: Vec<(&[u8], &[u8])> = entries
        .iter()
        .map(|(key, value)| (key.as_slice(), value.as_slice()))
        .collect();
    ordered.sort_by(|a, b| a.0.cmp(b.0));
    build_tree_from_sorted_entries(ordered, next_page_id)
}

pub fn build_store_tree(
    map: &BTreeMap<Vec<u8>, Vec<u8>>,
    next_page_id: &mut u64,
) -> Result<BuiltTree> {
    build_tree_from_sorted_entries(
        map.iter()
            .map(|(key, value)| (key.as_slice(), value.as_slice())),
        next_page_id,
    )
}

pub fn build_catalog_tree(
    catalog: &BTreeMap<String, StoreMetadata>,
    schema_version: u64,
    change_feed_floor_txid: u64,
    next_page_id: &mut u64,
) -> Result<BuiltTree> {
    let mut entries = Vec::with_capacity(catalog.len() + 2);
    entries.push((
        CATALOG_SCHEMA_VERSION_KEY.to_vec(),
        encode_schema_version(schema_version)?,
    ));
    entries.push((
        CATALOG_CHANGE_FEED_FLOOR_TXID_KEY.to_vec(),
        encode_change_feed_floor_txid(change_feed_floor_txid)?,
    ));
    for (name, meta) in catalog.iter() {
        entries.push((name.as_bytes().to_vec(), encode_store_metadata(meta)?));
    }
    build_tree(&entries, next_page_id)
}

pub fn read_catalog<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
) -> Result<CatalogState> {
    let mut out = BTreeMap::new();
    let mut schema_version = 0u64;
    let mut change_feed_floor_txid = 0u64;
    let mut saw_schema_version = false;
    let mut saw_change_feed_floor_txid = false;
    let pairs = load_all_entries(pager, root_page_id)?;
    for pair in pairs {
        if pair.key == CATALOG_SCHEMA_VERSION_KEY {
            if saw_schema_version {
                return Err(EngineError::Corruption(
                    "duplicate catalog schema version record".into(),
                ));
            }
            schema_version = decode_schema_version(&pair.value)?;
            saw_schema_version = true;
            continue;
        }
        if pair.key == CATALOG_CHANGE_FEED_FLOOR_TXID_KEY {
            if saw_change_feed_floor_txid {
                return Err(EngineError::Corruption(
                    "duplicate catalog change feed floor record".into(),
                ));
            }
            change_feed_floor_txid = decode_change_feed_floor_txid(&pair.value)?;
            saw_change_feed_floor_txid = true;
            continue;
        }

        let name = String::from_utf8(pair.key)
            .map_err(|err| EngineError::Serialization(format!("catalog key utf8: {err}")))?;
        out.insert(name, decode_store_metadata(&pair.value)?);
    }
    Ok(CatalogState {
        schema_version,
        change_feed_floor_txid,
        stores: out,
    })
}

pub fn rewrite_store_with_mutations<B: FileBackend>(
    pager: &mut Pager<B>,
    base_root_page_id: u64,
    mutations: &BTreeMap<Vec<u8>, MutationValue>,
    next_page_id: &mut u64,
) -> Result<BuiltTree> {
    if mutations.is_empty() {
        let leafs = collect_leaf_descriptors(pager, base_root_page_id)?;
        if leafs.is_empty() {
            return build_tree(&[], next_page_id);
        }
        return synthesize_tree_from_existing_leaves(&leafs, next_page_id);
    }

    let base_leafs = collect_leaf_descriptors(pager, base_root_page_id)?;
    if base_leafs.is_empty() {
        let mut entries = BTreeMap::new();
        for (key, mutation) in mutations {
            if let MutationValue::Put(value) = mutation {
                entries.insert(key.clone(), value.value.clone());
            }
        }
        return build_store_tree(&entries, next_page_id);
    }

    let windows = mutation_windows(&base_leafs, mutations);
    let mut page_images = Vec::new();
    let mut final_leafs = Vec::with_capacity(base_leafs.len());
    let mut leaf_index = 0usize;

    for window in &windows {
        while leaf_index < window.start {
            final_leafs.push(base_leafs[leaf_index].clone());
            leaf_index += 1;
        }

        let mut window_entries = BTreeMap::new();
        for descriptor in &base_leafs[window.start..=window.end] {
            for pair in read_leaf_entries(pager, descriptor.page_id)? {
                window_entries.insert(pair.key, pair.value);
            }
        }
        for (key, mutation) in &window.mutations {
            match mutation {
                MutationValue::Put(value) => {
                    window_entries.insert(key.clone(), value.value.clone());
                }
                MutationValue::Delete => {
                    window_entries.remove(key.as_slice());
                }
            }
        }

        let (rebuilt_leafs, rebuilt_pages) = build_leaf_run(
            window_entries
                .iter()
                .map(|(key, value)| (key.as_slice(), value.as_slice())),
            next_page_id,
        )?;
        final_leafs.extend(rebuilt_leafs);
        page_images.extend(rebuilt_pages);
        leaf_index = window.end + 1;
    }

    while leaf_index < base_leafs.len() {
        final_leafs.push(base_leafs[leaf_index].clone());
        leaf_index += 1;
    }

    let built = if final_leafs.is_empty() {
        build_tree(&[], next_page_id)?
    } else {
        synthesize_tree_from_existing_leaves(&final_leafs, next_page_id)?
    };
    page_images.extend(built.page_images);
    page_images.sort_by_key(|(page_id, _)| *page_id);
    Ok(BuiltTree {
        root_page_id: built.root_page_id,
        page_images,
    })
}

pub fn read_leaf_entries<B: FileBackend>(
    pager: &mut Pager<B>,
    page_id: u64,
) -> Result<Vec<KvPair>> {
    let chunk = pager.with_page(page_id, |bytes| {
        collect_leaf_page_entries(bytes, page_id, None, None, usize::MAX)
    })?;
    let mut out = Vec::with_capacity(chunk.pairs.len());
    append_pending_pairs(pager, chunk.pairs, &mut out)?;
    Ok(out)
}

fn scan_forward<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    range: &RangeSpec,
) -> Result<Vec<KvPair>> {
    let lower_bound = range.lower_bound();
    let upper_bound = range.upper_bound();
    let cursor = if let Some((lower, _inclusive)) = lower_bound {
        LeafCursor::for_key(pager, root_page_id, lower)?
    } else {
        LeafCursor::leftmost(pager, root_page_id)?
    };
    let Some(mut cursor) = cursor else {
        return Ok(Vec::new());
    };

    let mut out = Vec::new();
    let mut first_leaf = true;
    loop {
        let remaining = range
            .limit
            .map(|limit| limit.saturating_sub(out.len()))
            .unwrap_or(usize::MAX);
        if remaining == 0 {
            return Ok(out);
        }
        let page_lower = if first_leaf { lower_bound } else { None };
        let chunk = pager.with_page(cursor.current, |bytes| {
            collect_leaf_page_entries(bytes, cursor.current, page_lower, upper_bound, remaining)
        })?;
        let stop = chunk.stop;
        append_pending_pairs(pager, chunk.pairs, &mut out)?;
        if stop || matches!(range.limit, Some(limit) if out.len() >= limit) {
            return Ok(out);
        }
        if !cursor.advance(pager)? {
            break;
        }
        first_leaf = false;
    }

    Ok(out)
}

fn scan_reverse<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    range: &RangeSpec,
) -> Result<Vec<KvPair>> {
    let mut entries = load_all_entries(pager, root_page_id)?;
    entries.retain(|pair| {
        key_in_range(
            &pair.key,
            range.gt.as_deref(),
            range.gte.as_deref(),
            range.lt.as_deref(),
            range.lte.as_deref(),
        )
    });
    entries.reverse();
    if let Some(limit) = range.limit {
        entries.truncate(limit);
    }
    Ok(entries)
}

impl LeafCursor {
    fn leftmost<B: FileBackend>(pager: &mut Pager<B>, root_page_id: u64) -> Result<Option<Self>> {
        if root_page_id == 0 {
            return Ok(None);
        }
        let mut stack = Vec::new();
        let current = descend_leftmost(pager, root_page_id, &mut stack)?;
        Ok(Some(Self { current, stack }))
    }

    fn for_key<B: FileBackend>(
        pager: &mut Pager<B>,
        root_page_id: u64,
        key: &[u8],
    ) -> Result<Option<Self>> {
        if root_page_id == 0 {
            return Ok(None);
        }
        let mut current = root_page_id;
        let mut stack = Vec::new();
        loop {
            let step = pager.with_page(current, |bytes| {
                let header = checked_page_header(bytes, current)?;
                match header.page_kind {
                    PageKind::Leaf => Ok(None),
                    PageKind::Internal => {
                        let (child_index, child_page_id) =
                            choose_internal_child_in_page(bytes, &header, key)?;
                        Ok(Some((
                            child_index,
                            header.cell_count as usize,
                            child_page_id,
                        )))
                    }
                    PageKind::Overflow => Err(EngineError::Corruption(
                        "leaf search descended into overflow page".into(),
                    )),
                }
            })?;
            let Some((child_index, child_count, child_page_id)) = step else {
                return Ok(Some(Self { current, stack }));
            };
            stack.push(CursorFrame {
                page_id: current,
                child_index,
                child_count,
            });
            current = child_page_id;
        }
    }

    fn advance<B: FileBackend>(&mut self, pager: &mut Pager<B>) -> Result<bool> {
        while let Some(mut frame) = self.stack.pop() {
            if frame.child_index + 1 >= frame.child_count {
                continue;
            }
            frame.child_index += 1;
            let next_child = pager.with_page(frame.page_id, |bytes| {
                let header = checked_page_header(bytes, frame.page_id)?;
                if header.page_kind != PageKind::Internal {
                    return Err(EngineError::Corruption(format!(
                        "cursor parent {} is not internal",
                        frame.page_id
                    )));
                }
                child_page_id_at(bytes, &header, frame.child_index)
            })?;
            self.stack.push(frame);
            self.current = descend_leftmost(pager, next_child, &mut self.stack)?;
            return Ok(true);
        }
        Ok(false)
    }
}

fn descend_leftmost<B: FileBackend>(
    pager: &mut Pager<B>,
    mut current: u64,
    stack: &mut Vec<CursorFrame>,
) -> Result<u64> {
    loop {
        let step = pager.with_page(current, |bytes| {
            let header = checked_page_header(bytes, current)?;
            match header.page_kind {
                PageKind::Leaf => Ok(None),
                PageKind::Internal => {
                    if header.cell_count == 0 {
                        return Err(EngineError::Corruption(
                            "internal page has no children".into(),
                        ));
                    }
                    Ok(Some((
                        header.cell_count as usize,
                        child_page_id_at(bytes, &header, 0)?,
                    )))
                }
                PageKind::Overflow => Err(EngineError::Corruption(
                    "leftmost descent reached overflow page".into(),
                )),
            }
        })?;
        let Some((child_count, child_page_id)) = step else {
            return Ok(current);
        };
        stack.push(CursorFrame {
            page_id: current,
            child_index: 0,
            child_count,
        });
        current = child_page_id;
    }
}

fn collect_leaf_descriptors<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
) -> Result<Vec<LeafDescriptor>> {
    if root_page_id == 0 {
        return Ok(Vec::new());
    }
    let Some(mut cursor) = LeafCursor::leftmost(pager, root_page_id)? else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    loop {
        let min_key = pager.with_page(cursor.current, |bytes| {
            let header = checked_page_header(bytes, cursor.current)?;
            if header.page_kind != PageKind::Leaf {
                return Err(EngineError::Corruption(
                    "expected leaf while collecting descriptors".into(),
                ));
            }
            if header.cell_count == 0 {
                return Ok(Vec::new());
            }
            let slot = read_cell_slot(bytes, &header, 0)?;
            Ok(decode_leaf_cell_ref(bytes, slot)?.key.to_vec())
        })?;
        out.push(LeafDescriptor {
            page_id: cursor.current,
            min_key,
        });
        if !cursor.advance(pager)? {
            break;
        }
    }
    Ok(out)
}

fn mutation_windows(
    base_leafs: &[LeafDescriptor],
    mutations: &BTreeMap<Vec<u8>, MutationValue>,
) -> Vec<LeafWindow> {
    let mut by_leaf: BTreeMap<usize, Vec<(Vec<u8>, MutationValue)>> = BTreeMap::new();
    for (key, mutation) in mutations {
        let leaf_index = leaf_index_for_key(base_leafs, key);
        by_leaf
            .entry(leaf_index)
            .or_default()
            .push((key.clone(), mutation.clone()));
    }

    let mut windows = Vec::new();
    let mut pending: Option<LeafWindow> = None;
    for (leaf_index, entries) in by_leaf {
        pending = match pending {
            Some(mut window) if leaf_index <= window.end + 1 => {
                window.end = leaf_index;
                window.mutations.extend(entries);
                Some(window)
            }
            Some(window) => {
                windows.push(window);
                Some(LeafWindow {
                    start: leaf_index,
                    end: leaf_index,
                    mutations: entries,
                })
            }
            None => Some(LeafWindow {
                start: leaf_index,
                end: leaf_index,
                mutations: entries,
            }),
        };
    }
    if let Some(window) = pending {
        windows.push(window);
    }
    windows
}

fn leaf_index_for_key(base_leafs: &[LeafDescriptor], key: &[u8]) -> usize {
    if base_leafs.is_empty() {
        return 0;
    }
    match base_leafs.binary_search_by(|leaf| compare_keys(&leaf.min_key, key)) {
        Ok(index) => index,
        Err(0) => 0,
        Err(index) => index - 1,
    }
}

fn build_tree_from_sorted_entries<'a, I>(entries: I, next_page_id: &mut u64) -> Result<BuiltTree>
where
    I: IntoIterator<Item = (&'a [u8], &'a [u8])>,
{
    let (leafs, page_images) = build_leaf_run(entries, next_page_id)?;
    finish_tree_from_leaf_run(leafs, page_images, next_page_id)
}

fn finish_tree_from_leaf_run(
    leafs: Vec<LeafDescriptor>,
    page_images: PageImages,
    next_page_id: &mut u64,
) -> Result<BuiltTree> {
    if leafs.is_empty() {
        let page_id = allocate_page_id(next_page_id);
        let page = encode_leaf_page(page_id, 0, 0, &[])?;
        return Ok(BuiltTree {
            root_page_id: page_id,
            page_images: vec![(page_id, page)],
        });
    }
    build_tree_from_leaves(leafs, page_images, next_page_id)
}

fn build_leaf_run<'a, I>(
    entries: I,
    next_page_id: &mut u64,
) -> Result<(Vec<LeafDescriptor>, PageImages)>
where
    I: IntoIterator<Item = (&'a [u8], &'a [u8])>,
{
    let mut page_images = Vec::new();
    let mut leaf_groups: Vec<Vec<LeafCell>> = Vec::new();
    let mut current_group: Vec<LeafCell> = Vec::new();
    let mut current_payload = 0usize;

    for (key, value) in entries {
        let (cell, overflow_pages) = plan_leaf_cell(key, value, next_page_id)?;
        page_images.extend(overflow_pages);
        let cell_size = leaf_cell_size(
            key.len(),
            if cell.value_kind == ValueKind::Inline {
                cell.value.len()
            } else {
                0
            },
            cell.value_kind == ValueKind::Overflow,
        );
        let projected =
            PAGE_HEADER_SIZE + ((current_group.len() + 1) * 2) + current_payload + cell_size;
        if !current_group.is_empty() && projected > PAGE_SIZE {
            leaf_groups.push(std::mem::take(&mut current_group));
            current_payload = 0;
        }
        current_payload += cell_size;
        current_group.push(cell);
    }
    if !current_group.is_empty() {
        leaf_groups.push(current_group);
    }

    let mut leaf_ids = Vec::with_capacity(leaf_groups.len());
    for _ in 0..leaf_groups.len() {
        leaf_ids.push(allocate_page_id(next_page_id));
    }

    let mut descriptors = Vec::with_capacity(leaf_groups.len());
    for idx in 0..leaf_groups.len() {
        let page_id = leaf_ids[idx];
        let right = leaf_ids.get(idx + 1).copied().unwrap_or(0);
        let image = encode_leaf_page(page_id, 0, right, &leaf_groups[idx])?;
        let min_key = leaf_groups[idx]
            .first()
            .map(|cell| cell.key.clone())
            .unwrap_or_default();
        page_images.push((page_id, image));
        descriptors.push(LeafDescriptor { page_id, min_key });
    }

    Ok((descriptors, page_images))
}

fn synthesize_tree_from_existing_leaves(
    leaves: &[LeafDescriptor],
    next_page_id: &mut u64,
) -> Result<BuiltTree> {
    let nodes: Vec<LevelNode> = leaves
        .iter()
        .map(|leaf| LevelNode {
            min_key: leaf.min_key.clone(),
            page_id: leaf.page_id,
            level: 0,
        })
        .collect();
    build_tree_from_nodes(nodes, Vec::new(), next_page_id)
}

fn build_tree_from_leaves(
    leaves: Vec<LeafDescriptor>,
    page_images: PageImages,
    next_page_id: &mut u64,
) -> Result<BuiltTree> {
    let nodes: Vec<LevelNode> = leaves
        .into_iter()
        .map(|leaf| LevelNode {
            min_key: leaf.min_key,
            page_id: leaf.page_id,
            level: 0,
        })
        .collect();
    build_tree_from_nodes(nodes, page_images, next_page_id)
}

fn build_tree_from_nodes(
    mut current_level: Vec<LevelNode>,
    mut page_images: PageImages,
    next_page_id: &mut u64,
) -> Result<BuiltTree> {
    if current_level.is_empty() {
        return Err(EngineError::Serialization(
            "cannot synthesize tree from zero leaf descriptors".into(),
        ));
    }

    while current_level.len() > 1 {
        let level = current_level[0].level + 1;
        let mut groups: Vec<Vec<LevelNode>> = Vec::new();
        let mut current: Vec<LevelNode> = Vec::new();
        let mut payload = 0usize;
        for node in current_level.into_iter() {
            let cell_size = internal_cell_size(node.min_key.len());
            let projected = PAGE_HEADER_SIZE + ((current.len() + 1) * 2) + payload + cell_size;
            if !current.is_empty() && projected > PAGE_SIZE {
                groups.push(std::mem::take(&mut current));
                payload = 0;
            }
            payload += cell_size;
            current.push(node);
        }
        if !current.is_empty() {
            groups.push(current);
        }

        let mut ids = VecDeque::with_capacity(groups.len());
        for _ in 0..groups.len() {
            ids.push_back(allocate_page_id(next_page_id));
        }

        let mut next_level = Vec::with_capacity(groups.len());
        for group in groups.into_iter() {
            let page_id = ids
                .pop_front()
                .ok_or_else(|| EngineError::Serialization("missing internal page id".into()))?;
            let right = ids.front().copied().unwrap_or(0);
            let cells: Vec<InternalCell> = group
                .iter()
                .map(|node| InternalCell {
                    separator: node.min_key.clone(),
                    child_page_id: node.page_id,
                })
                .collect();
            let image = encode_internal_page(page_id, level, right, &cells)?;
            let min_key = cells
                .first()
                .map(|cell| cell.separator.clone())
                .unwrap_or_default();
            page_images.push((page_id, image));
            next_level.push(LevelNode {
                min_key,
                page_id,
                level,
            });
        }
        current_level = next_level;
    }

    page_images.sort_by_key(|(page_id, _)| *page_id);
    Ok(BuiltTree {
        root_page_id: current_level[0].page_id,
        page_images,
    })
}

fn lookup_step_in_page(bytes: &[u8], page_id: u64, key: &[u8]) -> Result<LookupStep> {
    let header = checked_page_header(bytes, page_id)?;
    match header.page_kind {
        PageKind::Leaf => lookup_leaf_in_page(bytes, &header, key),
        PageKind::Internal => {
            let (_, child_page_id) = choose_internal_child_in_page(bytes, &header, key)?;
            Ok(LookupStep::Descend(child_page_id))
        }
        PageKind::Overflow => Err(EngineError::Corruption(
            "lookup descended into overflow page".into(),
        )),
    }
}

fn lookup_leaf_in_page(
    bytes: &[u8],
    header: &crate::page::PageHeaderInfo,
    key: &[u8],
) -> Result<LookupStep> {
    let mut lo = 0usize;
    let mut hi = header.cell_count as usize;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let slot = read_cell_slot(bytes, header, mid)?;
        let cell = decode_leaf_cell_ref(bytes, slot)?;
        match compare_keys(cell.key, key) {
            Ordering::Less => lo = mid + 1,
            Ordering::Greater => hi = mid,
            Ordering::Equal => return Ok(LookupStep::Found(pending_value_from_leaf_ref(&cell))),
        }
    }
    Ok(LookupStep::NotFound)
}

fn choose_internal_child_in_page(
    bytes: &[u8],
    header: &crate::page::PageHeaderInfo,
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
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let slot = read_cell_slot(bytes, header, mid)?;
        let cell = decode_internal_cell_ref(bytes, slot)?;
        if compare_keys(cell.separator, key) != Ordering::Greater {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    let index = lo.saturating_sub(1);
    Ok((index, child_page_id_at(bytes, header, index)?))
}

fn child_page_id_at(
    bytes: &[u8],
    header: &crate::page::PageHeaderInfo,
    index: usize,
) -> Result<u64> {
    let slot = read_cell_slot(bytes, header, index)?;
    Ok(decode_internal_cell_ref(bytes, slot)?.child_page_id)
}

fn collect_leaf_page_entries(
    bytes: &[u8],
    page_id: u64,
    lower_bound: Option<(&[u8], bool)>,
    upper_bound: Option<(&[u8], bool)>,
    remaining: usize,
) -> Result<LeafScanChunk> {
    let header = checked_page_header(bytes, page_id)?;
    if header.page_kind != PageKind::Leaf {
        return Err(EngineError::Corruption(
            "expected leaf while scanning".into(),
        ));
    }

    let cell_count = header.cell_count as usize;
    let start = match lower_bound {
        Some((lower, inclusive)) => leaf_lower_bound_index(bytes, &header, lower, inclusive)?,
        None => 0,
    };
    let mut pairs = Vec::new();
    let mut stop = false;
    for index in start..cell_count {
        if pairs.len() >= remaining {
            stop = true;
            break;
        }
        let slot = read_cell_slot(bytes, &header, index)?;
        let cell = decode_leaf_cell_ref(bytes, slot)?;
        if let Some((upper, inclusive)) = upper_bound {
            match compare_keys(cell.key, upper) {
                Ordering::Greater => {
                    stop = true;
                    break;
                }
                Ordering::Equal if !inclusive => {
                    stop = true;
                    break;
                }
                Ordering::Equal | Ordering::Less => {}
            }
        }
        pairs.push(PendingKvPair {
            key: cell.key.to_vec(),
            value: pending_value_from_leaf_ref(&cell),
        });
    }
    Ok(LeafScanChunk { pairs, stop })
}

fn leaf_lower_bound_index(
    bytes: &[u8],
    header: &crate::page::PageHeaderInfo,
    lower: &[u8],
    inclusive: bool,
) -> Result<usize> {
    let mut lo = 0usize;
    let mut hi = header.cell_count as usize;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let slot = read_cell_slot(bytes, header, mid)?;
        let cell = decode_leaf_cell_ref(bytes, slot)?;
        let ordering = compare_keys(cell.key, lower);
        if ordering == Ordering::Less || (ordering == Ordering::Equal && !inclusive) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    Ok(lo)
}

fn pending_value_from_leaf_ref(cell: &crate::page::LeafCellRef<'_>) -> PendingValue {
    if cell.value_kind == ValueKind::Inline {
        PendingValue::Inline(cell.inline_value.to_vec())
    } else {
        PendingValue::Overflow {
            head_page_id: cell.overflow_head_page_id,
            total_len: cell.total_value_len as usize,
        }
    }
}

fn append_pending_pairs<B: FileBackend>(
    pager: &mut Pager<B>,
    pairs: Vec<PendingKvPair>,
    out: &mut Vec<KvPair>,
) -> Result<()> {
    out.reserve(pairs.len());
    for pair in pairs {
        out.push(KvPair {
            key: pair.key,
            value: materialize_pending_value(pager, pair.value)?,
        });
    }
    Ok(())
}

fn materialize_pending_value<B: FileBackend>(
    pager: &mut Pager<B>,
    value: PendingValue,
) -> Result<Vec<u8>> {
    match value {
        PendingValue::Inline(value) => Ok(value),
        PendingValue::Overflow {
            head_page_id,
            total_len,
        } => read_overflow_value(pager, head_page_id, total_len),
    }
}

fn checked_page_header(bytes: &[u8], page_id: u64) -> Result<crate::page::PageHeaderInfo> {
    let header = decode_page_header(bytes)?;
    if header.page_id != page_id {
        return Err(EngineError::Corruption(format!(
            "page header id mismatch: expected {page_id}, got {}",
            header.page_id
        )));
    }
    Ok(header)
}

fn plan_leaf_cell(
    key: &[u8],
    value: &[u8],
    next_page_id: &mut u64,
) -> Result<(LeafCell, PageImages)> {
    if should_overflow_value(value.len()) {
        let chain = write_overflow_chain(value, next_page_id)?;
        Ok((
            LeafCell {
                key: key.to_vec(),
                value: Vec::new(),
                value_kind: ValueKind::Overflow,
                total_value_len: value.len() as u32,
                overflow_head_page_id: chain.head_page_id,
            },
            chain.pages,
        ))
    } else {
        Ok((
            LeafCell {
                key: key.to_vec(),
                value: value.to_vec(),
                value_kind: ValueKind::Inline,
                total_value_len: value.len() as u32,
                overflow_head_page_id: 0,
            },
            Vec::new(),
        ))
    }
}

fn allocate_page_id(next_page_id: &mut u64) -> u64 {
    let id = *next_page_id;
    *next_page_id += 1;
    id
}
