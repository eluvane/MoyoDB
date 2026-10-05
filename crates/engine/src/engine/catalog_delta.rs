use super::CommittedView;
use crate::btree::{apply_mutations, BuiltTree, Mutation, PageAllocator};
use crate::catalog::{
    encode_change_feed_floor_txid, encode_change_feed_policy, encode_schema_version,
    encode_store_metadata, CatalogMap, ChangeFeedPolicy, CATALOG_CHANGE_FEED_FLOOR_TXID_KEY,
    CATALOG_CHANGE_FEED_POLICY_KEY, CATALOG_SCHEMA_VERSION_KEY,
};
use crate::change_feed::SYSTEM_CHANGELOG_STORE_NAME;
use crate::error::Result;
use crate::layout::StoreMetadata;
use crate::pager::Pager;
use crate::storage::backend::FileBackend;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Store metadata changes relative to the committed catalog. A tombstone hides
/// the base entry. Removing a newly created entry cancels its delta.
#[derive(Debug, Default)]
pub(super) struct CatalogDelta {
    stores: BTreeMap<String, Option<StoreMetadata>>,
}

impl CatalogDelta {
    pub(super) fn is_empty(&self) -> bool {
        self.stores.is_empty()
    }

    pub(super) fn get<'a>(&'a self, base: &'a CatalogMap, name: &str) -> Option<&'a StoreMetadata> {
        match self.stores.get(name) {
            Some(value) => value.as_ref(),
            None => base.get(name),
        }
    }

    pub(super) fn set(&mut self, base: &CatalogMap, name: &str, value: Option<StoreMetadata>) {
        if base.get(name) == value.as_ref() {
            self.stores.remove(name);
        } else {
            self.stores.insert(name.to_string(), value);
        }
    }

    pub(super) fn remove(&mut self, base: &CatalogMap, name: &str) -> Option<StoreMetadata> {
        let previous = self.get(base, name).cloned();
        self.set(base, name, None);
        previous
    }

    /// Store keys are ordered. Reserved metadata keys sort after every UTF-8
    /// store name, so appending them preserves mutation order.
    pub(super) fn build_tree<B: FileBackend>(
        &self,
        pager: &mut Pager<B>,
        view: &CommittedView<'_>,
        metadata: (u64, u64, ChangeFeedPolicy),
        alloc: &mut PageAllocator,
    ) -> Result<BuiltTree> {
        let (schema_version, floor, policy) = metadata;
        let mut encoded = Vec::with_capacity(self.stores.len() + 3);
        for (name, value) in &self.stores {
            encoded.push((
                name.as_bytes(),
                value.as_ref().map(encode_store_metadata).transpose()?,
            ));
        }
        // Open can synthesize a floor when no change log exists. Persist that
        // floor when creating the first log so the next open does not lose it.
        // An existing log already has its floor persisted.
        if floor != view.change_feed_floor_txid
            || (floor != 0 && !view.catalog.contains_key(SYSTEM_CHANGELOG_STORE_NAME))
        {
            encoded.push((
                CATALOG_CHANGE_FEED_FLOOR_TXID_KEY,
                Some(encode_change_feed_floor_txid(floor)?),
            ));
        }
        if policy != view.change_feed_policy {
            encoded.push((
                CATALOG_CHANGE_FEED_POLICY_KEY,
                if policy == ChangeFeedPolicy::default() {
                    None
                } else {
                    Some(encode_change_feed_policy(&policy)?)
                },
            ));
        }
        if schema_version != view.schema_version {
            encoded.push((
                CATALOG_SCHEMA_VERSION_KEY,
                Some(encode_schema_version(schema_version)?),
            ));
        }
        let mutations: Vec<Mutation<'_>> = encoded
            .iter()
            .map(|(key, value)| (*key, value.as_deref()))
            .collect();
        apply_mutations(pager, view.catalog_root_page_id, &mutations, alloc)
    }

    fn publish(self, catalog: &mut Arc<CatalogMap>) {
        if self.is_empty() {
            return;
        }
        // Copy only when a live snapshot shares this catalog. Its map and roots
        // must stay unchanged.
        let catalog = Arc::make_mut(catalog);
        for (name, value) in self.stores {
            match value {
                Some(meta) => {
                    catalog.insert(name, meta);
                }
                None => {
                    catalog.remove(&name);
                }
            }
        }
    }
}

pub(super) enum CatalogUpdate {
    Delta(CatalogDelta),
    Replace(CatalogMap),
}

impl CatalogUpdate {
    pub(super) fn publish(self, catalog: &mut Arc<CatalogMap>) {
        match self {
            Self::Delta(delta) => delta.publish(catalog),
            Self::Replace(replacement) => *catalog = Arc::new(replacement),
        }
    }
}
