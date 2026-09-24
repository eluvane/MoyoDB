use crate::bytes::compare_keys;
use crate::catalog::{
    decode_change_feed_floor_txid, decode_change_feed_policy, decode_schema_version,
    decode_store_metadata, encode_change_feed_floor_txid, encode_change_feed_policy,
    encode_schema_version, encode_store_metadata, CatalogState, ChangeFeedPolicy,
    CATALOG_CHANGE_FEED_FLOOR_TXID_KEY, CATALOG_CHANGE_FEED_POLICY_KEY, CATALOG_SCHEMA_VERSION_KEY,
};
use crate::error::{EngineError, Result};
use crate::layout::{PageKind, ValueKind, PAGE_HEADER_SIZE, PAGE_SIZE};
use crate::overflow::{
    free_overflow_chain, read_overflow_prefix, read_overflow_value, write_overflow_chain,
};
use crate::page::{
    decode_internal_cell_ref, decode_leaf_cell_ref, decode_page_header_verified,
    encode_internal_page, encode_leaf_page, internal_cell_size, leaf_cell_size, read_cell_slot,
    should_overflow_value, InternalCell, LeafCell, PageHeaderInfo, MAX_TREE_LEVEL,
};
use crate::pager::Pager;
use crate::storage::backend::FileBackend;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet, VecDeque};

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

/// A key and its new encoded value; `None` deletes the key.
pub type Mutation<'a> = (&'a [u8], Option<&'a [u8]>);

/// Hands out page ids for one commit.
///
/// Ids come from `reusable` first (pages no open snapshot can reach any more),
/// then from the end of the file. Pages the commit stops referencing are
/// recorded in `freed`; the engine decides when they become reusable.
#[derive(Debug, Clone, Default)]
pub struct PageAllocator {
    next_page_id: u64,
    reusable: Vec<u64>,
    freed: Vec<u64>,
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

    /// Retires a page reachable from the committed state this commit replaces.
    pub fn free(&mut self, page_id: u64) {
        self.freed.push(page_id);
    }

    /// Returns an id allocated by this commit that ended up unused. It was
    /// never published, so it can be handed out again immediately.
    fn release_unpublished(&mut self, page_id: u64) {
        self.reusable.push(page_id);
    }

    pub fn next_page_id(&self) -> u64 {
        self.next_page_id
    }

    pub fn freed(&self) -> &[u64] {
        &self.freed
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
    match lookup_pending(pager, root_page_id, key)? {
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
    match lookup_pending(pager, root_page_id, key)? {
        Some(PendingValue::Inline(mut value)) => {
            value.truncate(prefix_len);
            Ok(Some(value))
        }
        Some(PendingValue::Overflow {
            head_page_id,
            total_len,
        }) => read_overflow_prefix(pager, head_page_id, total_len, prefix_len).map(Some),
        None => Ok(None),
    }
}

fn lookup_pending<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    key: &[u8],
) -> Result<Option<PendingValue>> {
    if root_page_id == 0 {
        return Ok(None);
    }
    let mut current = root_page_id;
    let mut expected_level = None;
    loop {
        let step = pager.with_page(current, |bytes| {
            let header = node_header(bytes, current, expected_level)?;
            match header.page_kind {
                PageKind::Leaf => lookup_leaf_in_page(bytes, &header, key),
                _ => {
                    let (_, child_page_id) = choose_internal_child_in_page(bytes, &header, key)?;
                    Ok(LookupStep::Descend(child_page_id, header.level - 1))
                }
            }
        })?;
        match step {
            LookupStep::Descend(child_page_id, level) => {
                current = child_page_id;
                expected_level = Some(level);
            }
            LookupStep::Found(value) => return Ok(Some(value)),
            LookupStep::NotFound => return Ok(None),
        }
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
    let mut out = Vec::new();
    if limit == 0 {
        return Ok(out);
    }
    let mut iter = TreeIter::new(pager, root_page_id, range)?;
    while let Some(pair) = iter.next(pager)? {
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
    // Sort borrowed pairs only; the leaf builder copies into cells anyway.
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

/// Incremental bulk loader: callers push sorted entries one at a time, so a
/// whole store never has to be materialized.
pub struct SortedTreeBuilder {
    cells: Vec<LeafCell>,
    leaf_cost: usize,
    children: Vec<InternalCell>,
    images: BTreeMap<u64, Vec<u8>>,
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
            images: BTreeMap::new(),
            last_key: None,
        }
    }

    pub fn push(&mut self, key: &[u8], value: &[u8], alloc: &mut PageAllocator) -> Result<()> {
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

    /// Hands back the page images produced so far so callers can stream them.
    pub fn drain_images(&mut self) -> PageImages {
        std::mem::take(&mut self.images).into_iter().collect()
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
            page_images: self.images.into_iter().collect(),
        })
    }

    fn flush_leaf(&mut self, alloc: &mut PageAllocator) -> Result<()> {
        let cells = std::mem::take(&mut self.cells);
        self.leaf_cost = 0;
        let page_id = alloc.allocate();
        let min_key = cells[0].key.clone();
        self.images
            .insert(page_id, encode_leaf_page(page_id, 0, 0, &cells)?);
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

/// Applies sorted mutations by copying only the root-to-leaf paths they touch.
///
/// Untouched subtrees and the overflow chains of untouched cells are shared
/// with the previous tree. Replaced pages and dropped chains are retired
/// through `alloc`. Underfull nodes merge with a neighbour and single-child
/// roots collapse, so the tree stays balanced under deletes.
pub fn apply_mutations<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    mutations: &[Mutation<'_>],
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

/// Retires every page of a committed tree, including overflow chains.
pub fn free_tree<B: FileBackend>(
    pager: &mut Pager<B>,
    root_page_id: u64,
    alloc: &mut PageAllocator,
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
        let node = read_node(pager, page_id, expected_level)?;
        alloc.free(page_id);
        match node {
            Node::Leaf(cells) => {
                for cell in cells {
                    if cell.value_kind == ValueKind::Overflow {
                        free_overflow_chain(
                            pager,
                            cell.overflow_head_page_id,
                            cell.total_value_len as usize,
                            alloc,
                        )?;
                    }
                }
            }
            Node::Internal { level, cells } => {
                for cell in cells {
                    stack.push((cell.child_page_id, Some(level - 1)));
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
    let range = RangeSpec {
        lt: Some(upper.to_vec()),
        ..RangeSpec::default()
    };
    let mut iter = TreeIter::new(pager, root_page_id, &range)?;
    let mut out = Vec::new();
    while out.len() < limit {
        let Some(pair) = iter.next(pager)? else {
            break;
        };
        out.push(pair.key);
    }
    Ok(out)
}

// Leaves and internal nodes use the same usable area: page minus header.
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

/// The full content a node has after a commit touched it. It is packed into
/// pages only once its parent knows whether it must absorb a neighbour.
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
    fresh: BTreeMap<u64, Vec<u8>>,
}

impl<'a> TreeWriter<'a, crate::storage::memory::MemoryBackend> {
    fn new_detached(alloc: &'a mut PageAllocator) -> Self {
        Self {
            pager: None,
            alloc,
            fresh: BTreeMap::new(),
        }
    }
}

impl<'a, B: FileBackend> TreeWriter<'a, B> {
    fn new(pager: &'a mut Pager<B>, alloc: &'a mut PageAllocator) -> Self {
        Self {
            pager: Some(pager),
            alloc,
            fresh: BTreeMap::new(),
        }
    }

    fn into_images(self) -> PageImages {
        self.fresh.into_iter().collect()
    }

    fn pager(&mut self) -> Result<&mut Pager<B>> {
        self.pager
            .as_deref_mut()
            .ok_or_else(|| EngineError::Internal("tree writer has no committed tree".into()))
    }

    fn rewrite(
        &mut self,
        page_id: u64,
        expected_level: Option<u8>,
        mutations: &[Mutation<'_>],
    ) -> Result<Run> {
        let node = read_node(self.pager()?, page_id, expected_level)?;
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

    fn merge_leaf(&mut self, cells: Vec<LeafCell>, mutations: &[Mutation<'_>]) -> Result<Run> {
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
                out.push(self.plan_cell(key, value)?);
            }
        }
        out.extend(existing);
        Ok(Run::Leaf(out))
    }

    fn release_cell(&mut self, cell: &LeafCell) -> Result<()> {
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

    fn plan_cell(&mut self, key: &[u8], value: &[u8]) -> Result<LeafCell> {
        let total_value_len =
            u32::try_from(value.len()).map_err(|_| EngineError::ValueTooLarge(value.len()))?;
        if should_overflow_value(value.len()) {
            let chain = write_overflow_chain(value, self.alloc)?;
            self.fresh.extend(chain.pages);
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
                value: value.to_vec(),
                value_kind: ValueKind::Inline,
                total_value_len,
                overflow_head_page_id: 0,
            })
        }
    }

    /// Drops emptied children, merges underfull ones into a neighbour and
    /// packs every changed child into fresh pages.
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
                let node = read_node(self.pager()?, cell.child_page_id, Some(child_level))?;
                self.alloc.free(cell.child_page_id);
                Ok(match node {
                    Node::Leaf(cells) => Run::Leaf(cells),
                    Node::Internal { level, cells } => Run::Internal { level, cells },
                })
            }
        }
    }

    /// Splits a run into evenly filled pages and returns their parent cells.
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
            Run::Internal { cells, .. } if cells.len() == 1 => cells,
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
        while let Some(only_child) = self.single_child(root)? {
            if self.fresh.remove(&root).is_some() {
                self.alloc.release_unpublished(root);
            } else {
                self.alloc.free(root);
            }
            root = only_child;
        }
        Ok(root)
    }

    fn single_child(&mut self, page_id: u64) -> Result<Option<u64>> {
        let inspect = |bytes: &[u8]| -> Result<Option<u64>> {
            let header = node_header(bytes, page_id, None)?;
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

/// For each child, the index range of mutations routed to it. Keys below the
/// first separator belong to the first child, matching lookup routing.
fn route_mutations(
    children: &[InternalCell],
    mutations: &[Mutation<'_>],
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

/// Group sizes for packing cells into pages of `NODE_CAPACITY`, aiming for
/// equal fill instead of leaving a nearly empty last page.
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

fn read_node<B: FileBackend>(
    pager: &mut Pager<B>,
    page_id: u64,
    expected_level: Option<u8>,
) -> Result<Node> {
    pager.with_page(page_id, |bytes| {
        let header = node_header(bytes, page_id, expected_level)?;
        let count = header.cell_count as usize;
        match header.page_kind {
            PageKind::Leaf => {
                let mut cells: Vec<LeafCell> = Vec::with_capacity(count);
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
                    cells.push(LeafCell {
                        key: cell.key.to_vec(),
                        value: cell.inline_value.to_vec(),
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

/// Header of a tree node whose bytes the pager already verified: checks the
/// page id, that it is a tree page, and that its level is what the parent implies.
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

enum LookupStep {
    Descend(u64, u8),
    Found(PendingValue),
    NotFound,
}

pub(crate) enum PendingValue {
    Inline(Vec<u8>),
    Overflow { head_page_id: u64, total_len: usize },
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

/// Streams entries of one tree in key order (or reverse), one leaf at a time.
/// Values stay unread until the caller materializes them.
pub(crate) struct TreeIter {
    cursor: Option<LeafCursor>,
    buffer: VecDeque<PendingKvPair>,
    lower: Option<(Vec<u8>, bool)>,
    upper: Option<(Vec<u8>, bool)>,
    reverse: bool,
    loaded_first: bool,
    exhausted: bool,
}

impl TreeIter {
    pub(crate) fn new<B: FileBackend>(
        pager: &mut Pager<B>,
        root_page_id: u64,
        range: &RangeSpec,
    ) -> Result<Self> {
        range.validate()?;
        let lower = range
            .lower_bound()
            .map(|(key, inclusive)| (key.to_vec(), inclusive));
        let upper = range
            .upper_bound()
            .map(|(key, inclusive)| (key.to_vec(), inclusive));
        let descent = if range.reverse {
            match upper.as_ref() {
                Some((key, _)) => Descent::Key(key.as_slice()),
                None => Descent::Rightmost,
            }
        } else {
            match lower.as_ref() {
                Some((key, _)) => Descent::Key(key.as_slice()),
                None => Descent::Leftmost,
            }
        };
        let cursor = LeafCursor::open(pager, root_page_id, descent)?;
        Ok(Self {
            exhausted: cursor.is_none(),
            cursor,
            buffer: VecDeque::new(),
            lower,
            upper,
            reverse: range.reverse,
            loaded_first: false,
        })
    }

    pub(crate) fn next<B: FileBackend>(
        &mut self,
        pager: &mut Pager<B>,
    ) -> Result<Option<PendingKvPair>> {
        loop {
            if let Some(pair) = self.buffer.pop_front() {
                return Ok(Some(pair));
            }
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
            let lower = self
                .lower
                .as_ref()
                .map(|(key, inclusive)| (key.as_slice(), *inclusive));
            let upper = self
                .upper
                .as_ref()
                .map(|(key, inclusive)| (key.as_slice(), *inclusive));
            let reverse = self.reverse;
            let (pairs, hit_bound) = pager.with_page(page_id, |bytes| {
                collect_leaf_window(bytes, page_id, lower, upper, reverse)
            })?;
            self.buffer.extend(pairs);
            if hit_bound {
                self.exhausted = true;
            }
        }
    }
}

/// Entries of one leaf inside the bounds, in iteration order, and whether the
/// bound in the direction of travel cut this leaf (so later leaves are out of range).
fn collect_leaf_window(
    bytes: &[u8],
    page_id: u64,
    lower: Option<(&[u8], bool)>,
    upper: Option<(&[u8], bool)>,
    reverse: bool,
) -> Result<(Vec<PendingKvPair>, bool)> {
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
    let mut pairs = Vec::with_capacity(end.saturating_sub(start));
    for index in start..end.max(start) {
        let slot = read_cell_slot(bytes, &header, index)?;
        let cell = decode_leaf_cell_ref(bytes, slot)?;
        pairs.push(PendingKvPair {
            key: cell.key.to_vec(),
            value: pending_value_from_leaf_ref(&cell),
        });
    }
    let hit_bound = if reverse {
        pairs.reverse();
        start > 0
    } else {
        end < count
    };
    Ok((pairs, hit_bound))
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

fn lookup_leaf_in_page(bytes: &[u8], header: &PageHeaderInfo, key: &[u8]) -> Result<LookupStep> {
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

fn child_page_id_at(bytes: &[u8], header: &PageHeaderInfo, index: usize) -> Result<u64> {
    let slot = read_cell_slot(bytes, header, index)?;
    Ok(decode_internal_cell_ref(bytes, slot)?.child_page_id)
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
    }
}

pub(crate) fn pending_value_prefix<B: FileBackend>(
    pager: &mut Pager<B>,
    value: &PendingValue,
    prefix_len: usize,
) -> Result<Vec<u8>> {
    match value {
        PendingValue::Inline(value) => Ok(value[..prefix_len.min(value.len())].to_vec()),
        PendingValue::Overflow {
            head_page_id,
            total_len,
        } => read_overflow_prefix(pager, *head_page_id, *total_len, prefix_len),
    }
}
