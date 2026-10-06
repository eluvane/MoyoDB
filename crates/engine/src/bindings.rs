#[cfg(any(target_arch = "wasm32", test))]
// The SDK uses number IDs and schema versions. Native engines retain full u64 values.
const JS_MAX_SAFE_INTEGER: u64 = (1u64 << 53) - 1;

#[cfg(any(target_arch = "wasm32", test))]
fn validate_js_u64(value: u64, field: &str) -> crate::error::Result<()> {
    if value > JS_MAX_SAFE_INTEGER {
        return Err(crate::error::EngineError::Serialization(format!(
            "{field} exceeds the JavaScript safe integer range"
        )));
    }
    Ok(())
}

#[cfg(any(target_arch = "wasm32", test))]
fn decode_snapshot_for_binding(
    bytes: &[u8],
) -> crate::error::Result<crate::snapshot::SnapshotContents> {
    let snapshot = crate::snapshot::decode_snapshot(bytes)?;
    validate_js_u64(snapshot.schema_version, "snapshot schema version")?;
    let imported_txid = snapshot
        .source_last_committed_txid
        .checked_add(1)
        .ok_or_else(|| {
            crate::error::EngineError::Serialization("snapshot commit txid overflow".into())
        })?;
    validate_js_u64(imported_txid, "snapshot commit txid")?;
    Ok(snapshot)
}

#[cfg(any(target_arch = "wasm32", test))]
fn import_snapshot_for_binding<B: crate::storage::backend::FileBackend>(
    engine: &mut crate::engine::Engine<B>,
    bytes: &[u8],
) -> crate::error::Result<u64> {
    let snapshot = decode_snapshot_for_binding(bytes)?;
    validate_js_u64(engine.next_commit_txid(), "next commit txid")?;
    engine.import_decoded_snapshot(snapshot)
}

#[cfg(any(target_arch = "wasm32", test))]
fn commit_tx_for_binding<B: crate::storage::backend::FileBackend>(
    engine: &mut crate::engine::Engine<B>,
    tx_id: u64,
) -> crate::error::Result<u64> {
    validate_js_u64(engine.next_commit_txid(), "next commit txid")?;
    engine.commit_tx(tx_id)
}

#[cfg(any(target_arch = "wasm32", test))]
fn begin_tx_for_binding<B: crate::storage::backend::FileBackend>(
    engine: &mut crate::engine::Engine<B>,
    mode: crate::engine::TxMode,
) -> crate::error::Result<u64> {
    validate_js_u64(engine.next_transaction_handle(), "transaction handle")?;
    engine.begin_tx(mode)
}

#[cfg(any(target_arch = "wasm32", test))]
fn open_engine_for_binding<B: crate::storage::backend::FileBackend + Clone>(
    name: &str,
    files: crate::storage::backend::FileSet<B>,
    config: crate::engine::OpenConfig,
) -> crate::error::Result<crate::engine::Engine<B>> {
    // Engine::open consumes the files on failure. Keep aliases to release
    // the OPFS session so the same worker can retry.
    let mut cleanup = [
        files.manifest.clone(),
        files.main.clone(),
        files.wal.clone(),
    ];
    let opened = crate::engine::Engine::open(name, files, config).and_then(|mut engine| {
        validate_js_u64(engine.schema_version(), "persisted schema version")?;
        validate_js_u64(engine.stats()?.last_committed_txid, "persisted commit txid")?;
        Ok(engine)
    });
    if opened.is_err() {
        for file in &mut cleanup {
            // Release every file and preserve the initialization error.
            let _ = file.close();
        }
    }
    opened
}

#[cfg(test)]
mod open_binding_tests {
    use super::open_engine_for_binding;
    use crate::engine::OpenConfig;
    use crate::error::{EngineError, Result};
    use crate::storage::memory::MemoryBundle;

    #[test]
    fn failed_open_releases_every_file_without_replacing_the_original_error() {
        let bundle = MemoryBundle::new();
        let result = open_engine_for_binding(
            "missing",
            bundle.files(),
            OpenConfig {
                create_if_missing: false,
                ..OpenConfig::default()
            },
        );
        let error = match result {
            Ok(_) => panic!("a missing database must not open"),
            Err(error) => error,
        };
        assert_eq!(
            error,
            EngineError::Storage("database missing and create_if_missing=false".into())
        );
        for file in [&bundle.manifest, &bundle.main, &bundle.wal] {
            assert!(matches!(file.len(), Err(EngineError::Storage(_))));
        }
    }

    #[test]
    fn corrupt_open_releases_every_file() -> Result<()> {
        let mut bundle = MemoryBundle::new();
        bundle.manifest.write_at(0, &[0x55; 8192])?;
        bundle.manifest.flush()?;
        let result = open_engine_for_binding("corrupt", bundle.files(), OpenConfig::default());
        assert!(matches!(result, Err(EngineError::Corruption(_))));
        for file in [&bundle.manifest, &bundle.main, &bundle.wal] {
            assert!(matches!(file.len(), Err(EngineError::Storage(_))));
        }
        Ok(())
    }

    #[test]
    fn successful_open_keeps_files_available_until_close() -> Result<()> {
        let bundle = MemoryBundle::new();
        let mut engine = open_engine_for_binding("healthy", bundle.files(), OpenConfig::default())?;
        for file in [&bundle.manifest, &bundle.main, &bundle.wal] {
            assert!(file.len().is_ok());
        }
        engine.close()?;
        for file in [&bundle.manifest, &bundle.main, &bundle.wal] {
            assert!(matches!(file.len(), Err(EngineError::Storage(_))));
        }
        Ok(())
    }

    #[test]
    fn binding_open_rejects_unsafe_persisted_snapshot_numbers() -> Result<()> {
        use crate::snapshot::{encode_snapshot, SnapshotContents};

        for (source_last_committed_txid, schema_version) in [(1u64 << 53, 0), (0, (1u64 << 53) + 1)]
        {
            let bundle = MemoryBundle::new();
            let mut native = crate::engine::Engine::open(
                "unsafe-snapshot",
                bundle.files(),
                OpenConfig::default(),
            )?;
            native.import_snapshot(&encode_snapshot(&SnapshotContents {
                source_last_committed_txid,
                schema_version,
                stores: Vec::new(),
            })?)?;
            native.close()?;
            let opened = open_engine_for_binding(
                "unsafe-snapshot",
                bundle.crash_recovered_files(),
                OpenConfig::default(),
            );
            assert!(matches!(opened, Err(EngineError::Serialization(_))));
        }
        Ok(())
    }
}

#[cfg(test)]
mod snapshot_number_binding_tests {
    use super::{
        begin_tx_for_binding, commit_tx_for_binding, import_snapshot_for_binding,
        JS_MAX_SAFE_INTEGER,
    };
    use crate::engine::{Engine, OpenConfig, TxMode};
    use crate::error::{EngineError, Result};
    use crate::snapshot::{encode_snapshot, SnapshotContents};
    use crate::storage::memory::MemoryBundle;

    #[test]
    fn unsafe_snapshot_metadata_fails_before_publication() -> Result<()> {
        let bundle = MemoryBundle::new();
        let mut engine = Engine::open(
            "binding-snapshot-reject",
            bundle.files(),
            OpenConfig::default(),
        )?;
        let tx = begin_tx_for_binding(&mut engine, TxMode::Readwrite)?;
        engine.create_store(tx, "keep")?;
        engine.put(tx, "keep", b"key", b"value")?;
        commit_tx_for_binding(&mut engine, tx)?;
        let before = engine.export_snapshot()?;
        for (source_last_committed_txid, schema_version) in [
            (JS_MAX_SAFE_INTEGER, 0),
            (JS_MAX_SAFE_INTEGER + 1, 0),
            (0, JS_MAX_SAFE_INTEGER + 1),
            (u64::MAX, 0),
        ] {
            let bytes = encode_snapshot(&SnapshotContents {
                source_last_committed_txid,
                schema_version,
                stores: Vec::new(),
            })?;
            assert!(matches!(
                import_snapshot_for_binding(&mut engine, &bytes),
                Err(EngineError::Serialization(_))
            ));
            assert_eq!(engine.export_snapshot()?, before);
        }
        Ok(())
    }

    #[test]
    fn final_safe_commit_succeeds_and_next_commit_fails_before_publication() -> Result<()> {
        let bundle = MemoryBundle::new();
        let mut engine = Engine::open(
            "binding-snapshot-boundary",
            bundle.files(),
            OpenConfig::default(),
        )?;
        let bytes = encode_snapshot(&SnapshotContents {
            source_last_committed_txid: JS_MAX_SAFE_INTEGER - 2,
            schema_version: JS_MAX_SAFE_INTEGER,
            stores: Vec::new(),
        })?;
        assert_eq!(
            import_snapshot_for_binding(&mut engine, &bytes)?,
            JS_MAX_SAFE_INTEGER - 1
        );
        let tx = begin_tx_for_binding(&mut engine, TxMode::Readwrite)?;
        assert_eq!(commit_tx_for_binding(&mut engine, tx)?, JS_MAX_SAFE_INTEGER);
        let before = engine.export_snapshot()?;
        let tx = begin_tx_for_binding(&mut engine, TxMode::Readwrite)?;
        engine.create_store(tx, "overflow")?;
        assert!(matches!(
            commit_tx_for_binding(&mut engine, tx),
            Err(EngineError::Serialization(_))
        ));
        engine.rollback_tx(tx)?;
        assert_eq!(engine.export_snapshot()?, before);
        assert_eq!(engine.schema_version(), JS_MAX_SAFE_INTEGER);
        Ok(())
    }
}

// This byte representation is specific to WASM. ChangeFeed keeps its JSON
// and storage formats. Native tests use the same adapter.
#[cfg(any(target_arch = "wasm32", test))]
struct WasmChangeFeed<'a>(&'a crate::change_feed::ChangeFeed);

#[cfg(any(target_arch = "wasm32", test))]
impl serde::Serialize for WasmChangeFeed<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut fields = serializer.serialize_struct("ChangeFeed", 2)?;
        fields.serialize_field("changes", &WasmChangeRecords(&self.0.changes))?;
        fields.serialize_field("latestTxId", &self.0.latest_tx_id)?;
        fields.end()
    }
}

#[cfg(any(target_arch = "wasm32", test))]
struct WasmChangeRecords<'a>(&'a [crate::change_feed::ChangeRecord]);

#[cfg(any(target_arch = "wasm32", test))]
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct WasmChangeRecord<'a> {
    tx_id: u64,
    store: &'a str,
    #[serde(with = "serde_bytes")]
    key: &'a [u8],
    kind: crate::change_feed::ChangeKind,
    #[serde(with = "serde_bytes")]
    value: Option<&'a [u8]>,
}

#[cfg(any(target_arch = "wasm32", test))]
impl serde::Serialize for WasmChangeRecords<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeSeq;
        let mut records = serializer.serialize_seq(Some(self.0.len()))?;
        for record in self.0 {
            records.serialize_element(&WasmChangeRecord {
                tx_id: record.tx_id,
                store: &record.store,
                key: &record.key,
                kind: record.kind,
                value: record.value.as_deref(),
            })?;
        }
        records.end()
    }
}

#[cfg(test)]
mod typed_feed_binding_tests {
    use super::WasmChangeFeed;
    use crate::change_feed::{ChangeFeed, ChangeKind, ChangeRecord};
    use serde::ser::{Impossible, SerializeSeq, SerializeStruct};
    use serde::{Serialize, Serializer};

    #[derive(Default)]
    struct Trace {
        bytes: Vec<Vec<u8>>,
        scalar_bytes: usize,
        sequences: usize,
        none: usize,
        variants: Vec<&'static str>,
    }
    struct TraceSerializer<'a>(&'a mut Trace);
    type TraceResult<T = ()> = Result<T, serde_json::Error>;

    macro_rules! scalar {
        ($($method:ident($kind:ty)),* $(,)?) => {
            $(fn $method(self, _value: $kind) -> TraceResult { Ok(()) })*
        };
    }

    impl<'a> Serializer for TraceSerializer<'a> {
        type Ok = ();
        type Error = serde_json::Error;
        type SerializeSeq = Self;
        type SerializeTuple = Impossible<(), Self::Error>;
        type SerializeTupleStruct = Impossible<(), Self::Error>;
        type SerializeTupleVariant = Impossible<(), Self::Error>;
        type SerializeMap = Impossible<(), Self::Error>;
        type SerializeStruct = Self;
        type SerializeStructVariant = Impossible<(), Self::Error>;

        scalar!(
            serialize_bool(bool),
            serialize_i8(i8),
            serialize_i16(i16),
            serialize_i32(i32),
            serialize_i64(i64),
            serialize_u16(u16),
            serialize_u32(u32),
            serialize_u64(u64),
            serialize_f32(f32),
            serialize_f64(f64),
            serialize_char(char),
            serialize_str(&str)
        );

        fn serialize_u8(self, _value: u8) -> TraceResult {
            self.0.scalar_bytes += 1;
            Ok(())
        }
        fn serialize_bytes(self, value: &[u8]) -> TraceResult {
            self.0.bytes.push(value.to_vec());
            Ok(())
        }
        fn serialize_none(self) -> TraceResult {
            self.0.none += 1;
            Ok(())
        }
        fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> TraceResult {
            value.serialize(self)
        }
        fn serialize_unit(self) -> TraceResult {
            Ok(())
        }
        fn serialize_unit_struct(self, _name: &'static str) -> TraceResult {
            Ok(())
        }
        fn serialize_unit_variant(
            self,
            _name: &'static str,
            _index: u32,
            variant: &'static str,
        ) -> TraceResult {
            self.0.variants.push(variant);
            Ok(())
        }
        fn serialize_newtype_struct<T: ?Sized + Serialize>(
            self,
            _name: &'static str,
            value: &T,
        ) -> TraceResult {
            value.serialize(self)
        }
        fn serialize_newtype_variant<T: ?Sized + Serialize>(
            self,
            _name: &'static str,
            _index: u32,
            _variant: &'static str,
            _value: &T,
        ) -> TraceResult {
            Err(serde::ser::Error::custom("unexpected newtype variant"))
        }
        fn serialize_seq(self, _length: Option<usize>) -> TraceResult<Self::SerializeSeq> {
            self.0.sequences += 1;
            Ok(self)
        }
        fn serialize_tuple(self, _length: usize) -> TraceResult<Self::SerializeTuple> {
            Err(serde::ser::Error::custom("unexpected tuple"))
        }
        fn serialize_tuple_struct(
            self,
            _name: &'static str,
            _length: usize,
        ) -> TraceResult<Self::SerializeTupleStruct> {
            Err(serde::ser::Error::custom("unexpected tuple struct"))
        }
        fn serialize_tuple_variant(
            self,
            _name: &'static str,
            _index: u32,
            _variant: &'static str,
            _length: usize,
        ) -> TraceResult<Self::SerializeTupleVariant> {
            Err(serde::ser::Error::custom("unexpected tuple variant"))
        }
        fn serialize_map(self, _length: Option<usize>) -> TraceResult<Self::SerializeMap> {
            Err(serde::ser::Error::custom("unexpected map"))
        }
        fn serialize_struct(
            self,
            _name: &'static str,
            _length: usize,
        ) -> TraceResult<Self::SerializeStruct> {
            Ok(self)
        }
        fn serialize_struct_variant(
            self,
            _name: &'static str,
            _index: u32,
            _variant: &'static str,
            _length: usize,
        ) -> TraceResult<Self::SerializeStructVariant> {
            Err(serde::ser::Error::custom("unexpected struct variant"))
        }
    }
    impl SerializeSeq for TraceSerializer<'_> {
        type Ok = ();
        type Error = serde_json::Error;
        fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> TraceResult {
            value.serialize(TraceSerializer(self.0))
        }
        fn end(self) -> TraceResult {
            Ok(())
        }
    }
    impl SerializeStruct for TraceSerializer<'_> {
        type Ok = ();
        type Error = serde_json::Error;
        fn serialize_field<T: ?Sized + Serialize>(
            &mut self,
            _name: &'static str,
            value: &T,
        ) -> TraceResult {
            value.serialize(TraceSerializer(self.0))
        }
        fn end(self) -> TraceResult {
            Ok(())
        }
    }

    #[test]
    fn typed_feed_binding_uses_byte_slices_instead_of_scalar_elements() -> TraceResult {
        let feed = ChangeFeed {
            latest_tx_id: 7,
            changes: vec![ChangeRecord {
                tx_id: 6,
                store: "docs".into(),
                key: (0..16).collect(),
                kind: ChangeKind::Put,
                value: Some((16..32).collect()),
            }],
        };
        let mut trace = Trace::default();
        WasmChangeFeed(&feed).serialize(TraceSerializer(&mut trace))?;
        assert_eq!(trace.bytes.len(), 2);
        assert_eq!(trace.bytes.first(), Some(&feed.changes[0].key));
        assert_eq!(trace.bytes.get(1), feed.changes[0].value.as_ref());
        assert_eq!(trace.scalar_bytes, 0);
        assert_eq!(
            trace.sequences, 1,
            "only the changes list should be a sequence"
        );
        Ok(())
    }

    #[test]
    fn typed_feed_binding_preserves_json_shape_kinds_and_absent_values() -> TraceResult {
        let changes = [
            ChangeKind::Put,
            ChangeKind::Delete,
            ChangeKind::Clear,
            ChangeKind::Drop,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, kind)| ChangeRecord {
            tx_id: index as u64 + 1,
            store: "docs".into(),
            key: if kind.is_store_level() {
                vec![]
            } else {
                vec![index as u8]
            },
            kind,
            value: (kind == ChangeKind::Put).then(|| vec![0, 255]),
        })
        .collect();
        let feed = ChangeFeed {
            latest_tx_id: 99,
            changes,
        };
        assert_eq!(
            serde_json::to_value(WasmChangeFeed(&feed))?,
            serde_json::to_value(&feed)?
        );
        let mut trace = Trace::default();
        WasmChangeFeed(&feed).serialize(TraceSerializer(&mut trace))?;
        assert_eq!(trace.none, 3);
        assert_eq!(trace.variants, ["put", "delete", "clear", "drop"]);
        Ok(())
    }
}

#[cfg(any(target_arch = "wasm32", test))]
struct ScanBookmark {
    tx_id: u64,
    store: String,
    range: crate::btree::RangeSpec,
    keys_only: bool,
    exhausted: bool,
}

#[cfg(any(target_arch = "wasm32", test))]
impl ScanBookmark {
    fn advance(&mut self, page: &crate::output::PackedScanPage) {
        self.exhausted |= page.exhausted;
        if let Some(limit) = self.range.limit.as_mut() {
            *limit = limit.saturating_sub(page.row_count);
            self.exhausted |= *limit == 0;
        }
        let Some(key) = page.last_key.as_ref() else {
            return;
        };
        if self.range.reverse {
            self.exhausted |= self
                .range
                .lower_bound()
                .is_some_and(|(lower, _)| key.as_slice() <= lower);
            if !self.exhausted {
                self.range.lt = Some(key.clone());
                self.range.lte = None;
            }
        } else {
            self.exhausted |= self
                .range
                .upper_bound()
                .is_some_and(|(upper, _)| key.as_slice() >= upper);
            if !self.exhausted {
                self.range.gt = Some(key.clone());
                self.range.gte = None;
            }
        }
    }
}

#[cfg(test)]
mod scan_bookmark_tests {
    use super::ScanBookmark;
    use crate::btree::RangeSpec;
    use crate::output::PackedScanPage;

    #[test]
    fn bookmark_resumes_exclusively_and_stops_at_the_inclusive_bound() {
        for reverse in [false, true] {
            let mut cursor = ScanBookmark {
                tx_id: 7,
                store: "s".into(),
                range: RangeSpec {
                    gte: Some(vec![1]),
                    lte: Some(vec![9]),
                    reverse,
                    limit: Some(10),
                    ..RangeSpec::default()
                },
                keys_only: true,
                exhausted: false,
            };
            assert_eq!(
                (cursor.tx_id, cursor.store.as_str(), cursor.keys_only),
                (7, "s", true)
            );
            cursor.advance(&PackedScanPage {
                packet: vec![],
                row_count: 2,
                last_key: Some(vec![5]),
                exhausted: false,
            });
            assert_eq!(cursor.range.limit, Some(8));
            assert!(!cursor.exhausted);
            if reverse {
                assert_eq!(cursor.range.lt, Some(vec![5]));
                assert!(cursor.range.lte.is_none());
            } else {
                assert_eq!(cursor.range.gt, Some(vec![5]));
                assert!(cursor.range.gte.is_none());
            }
            let end_key = if reverse { 1 } else { 9 };
            cursor.advance(&PackedScanPage {
                packet: vec![],
                row_count: 1,
                last_key: Some(vec![end_key]),
                exhausted: false,
            });
            assert!(cursor.exhausted);
            assert!(cursor.range.validate().is_ok());
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::WasmChangeFeed;
    use crate::btree::{KvPair, RangeSpec};
    use crate::catalog::ChangeFeedPolicy;
    use crate::change_feed::ChangeFeedOptions;
    use crate::engine::{DbStats, Engine, Failpoint, OpenConfig};
    use crate::error::EngineError;
    use crate::storage::backend::database_name_from_utf16;
    use crate::storage::backend::FileSet;
    use crate::storage::opfs::OpfsBackend;
    use crate::txn::{BatchOp, BatchOpOutcome, BatchOpRef, IndexOpRef, TxMode};
    use crate::value::StoreCompression;
    use js_sys::{Array, JsString, Object, Reflect, Uint8Array};
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    use wasm_bindgen::prelude::*;

    const PACKED_BATCH_OP_DELETE: u8 = 0;
    const PACKED_BATCH_OP_PUT: u8 = 1;

    #[derive(Debug, Clone, Default, Serialize, Deserialize)]
    #[serde(default)]
    struct WasmOpenOptions {
        create_if_missing: Option<bool>,
        cache_pages: Option<usize>,
        checkpoint_wal_bytes: Option<u64>,
        checkpoint_dirty_pages: Option<usize>,
    }

    impl WasmOpenOptions {
        fn config(&self) -> OpenConfig {
            let defaults = OpenConfig::default();
            OpenConfig {
                create_if_missing: self.create_if_missing.unwrap_or(defaults.create_if_missing),
                cache_pages: self.cache_pages.unwrap_or(defaults.cache_pages),
                checkpoint_wal_bytes: self
                    .checkpoint_wal_bytes
                    .unwrap_or(defaults.checkpoint_wal_bytes),
                checkpoint_dirty_pages: self
                    .checkpoint_dirty_pages
                    .unwrap_or(defaults.checkpoint_dirty_pages),
            }
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct WasmBytes(#[serde(with = "serde_bytes")] Vec<u8>);

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct WasmEntry(
        #[serde(with = "serde_bytes")] Vec<u8>,
        #[serde(with = "serde_bytes")] Vec<u8>,
    );

    #[derive(Debug, Clone, Default, Serialize, Deserialize)]
    struct WasmPutOptions {
        ttl: Option<u64>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(untagged)]
    enum WasmCompressionSetting {
        Disabled(bool),
        Kind(StoreCompression),
    }

    #[derive(Debug, Clone, Default, Serialize, Deserialize)]
    struct WasmCreateStoreOptions {
        compression: Option<WasmCompressionSetting>,
    }

    impl WasmCreateStoreOptions {
        fn compression_kind(&self) -> std::result::Result<StoreCompression, String> {
            match self.compression.as_ref() {
                None | Some(WasmCompressionSetting::Disabled(false)) => Ok(StoreCompression::None),
                Some(WasmCompressionSetting::Kind(kind)) => Ok(*kind),
                Some(WasmCompressionSetting::Disabled(true)) => {
                    Err("compression must be \"gzip\", \"deflate\", or false".into())
                }
            }
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "kind", rename_all = "lowercase")]
    enum WasmBatchOp {
        Put {
            #[serde(with = "serde_bytes")]
            key: Vec<u8>,
            #[serde(with = "serde_bytes")]
            value: Vec<u8>,
        },
        Delete {
            #[serde(with = "serde_bytes")]
            key: Vec<u8>,
        },
    }

    impl From<WasmBatchOp> for BatchOp {
        fn from(value: WasmBatchOp) -> Self {
            match value {
                WasmBatchOp::Put { key, value } => BatchOp::Put { key, value },
                WasmBatchOp::Delete { key } => BatchOp::Delete { key },
            }
        }
    }

    #[derive(Debug, Deserialize)]
    #[serde(tag = "kind", rename_all = "lowercase")]
    enum WasmIndexOp {
        Put {
            store: String,
            #[serde(with = "serde_bytes")]
            key: Vec<u8>,
        },
        Delete {
            store: String,
            #[serde(with = "serde_bytes")]
            key: Vec<u8>,
        },
    }

    impl WasmIndexOp {
        fn as_ref(&self) -> IndexOpRef<'_> {
            match self {
                Self::Put { store, key } => IndexOpRef::Put { store, key },
                Self::Delete { store, key } => IndexOpRef::Delete { store, key },
            }
        }
    }

    #[derive(Debug, Clone, Serialize)]
    #[serde(tag = "kind", rename_all = "lowercase")]
    enum WasmBatchOpOutcome {
        Put {
            #[serde(rename = "baselineExists")]
            baseline_exists: bool,
        },
        Delete {
            deleted: bool,
        },
    }

    impl From<BatchOpOutcome> for WasmBatchOpOutcome {
        fn from(value: BatchOpOutcome) -> Self {
            match value {
                BatchOpOutcome::Put { baseline_exists } => Self::Put { baseline_exists },
                BatchOpOutcome::Delete { deleted } => Self::Delete { deleted },
            }
        }
    }

    fn outcome_flag(outcome: &BatchOpOutcome) -> u8 {
        match outcome {
            BatchOpOutcome::Put { baseline_exists } => u8::from(*baseline_exists),
            BatchOpOutcome::Delete { deleted } => u8::from(*deleted),
        }
    }

    #[derive(Debug, Clone, Serialize)]
    struct WasmKvPair {
        #[serde(with = "serde_bytes")]
        key: Vec<u8>,
        #[serde(with = "serde_bytes")]
        value: Vec<u8>,
    }

    impl From<KvPair> for WasmKvPair {
        fn from(value: KvPair) -> Self {
            Self {
                key: value.key,
                value: value.value,
            }
        }
    }

    #[derive(Debug, Clone, Serialize)]
    struct WasmRebuildTargetInfo {
        #[serde(rename = "generationName")]
        generation_name: String,
    }

    #[wasm_bindgen]
    #[derive(Default)]
    pub struct WasmEngine {
        inner: Option<Engine<OpfsBackend>>,
        scan_cursors: HashMap<u32, super::ScanBookmark>,
        next_scan_cursor: u32,
    }

    fn js_database_name(name: &JsString) -> std::result::Result<String, JsValue> {
        // Each UTF-16 unit becomes at least one UTF-8 byte, so a longer JS
        // string cannot satisfy the 127-byte limit. Skip the unit copy.
        if name.length() > 127 {
            return Err(js_error(EngineError::Storage(
                "database name must contain 1 to 127 UTF-8 bytes".into(),
            )));
        }
        let units: Vec<u16> = name.iter().collect();
        database_name_from_utf16(&units).map_err(js_error)
    }

    #[wasm_bindgen(js_name = deleteDB)]
    pub async fn delete_db(name: &JsString) -> std::result::Result<(), JsValue> {
        let name = js_database_name(name)?;
        OpfsBackend::remove_db(&name).await.map_err(js_error)
    }

    #[wasm_bindgen(js_name = prepareRebuildTarget)]
    pub async fn prepare_rebuild_target(name: &JsString) -> std::result::Result<JsValue, JsValue> {
        let name = js_database_name(name)?;
        let generation_name = OpfsBackend::prepare_rebuild_target(&name)
            .await
            .map_err(js_error)?;
        js_value_from_serializable(&WasmRebuildTargetInfo { generation_name })
    }

    /// Publishes `generation_name` only if the active generation matches
    /// `expected_current`. `None` requires no active generation.
    /// Verifies the written control file before returning.
    #[wasm_bindgen(js_name = swapActiveGeneration)]
    pub async fn swap_active_generation(
        name: &JsString,
        generation_name: String,
        expected_current: Option<String>,
    ) -> std::result::Result<(), JsValue> {
        let name = js_database_name(name)?;
        OpfsBackend::swap_active_generation(&name, &generation_name, expected_current)
            .await
            .map_err(js_error)
    }

    /// Returns the active generation, or `null` for the legacy layout.
    /// A corrupt control file permits legacy fallback only while legacy data
    /// files remain. Otherwise it is an error.
    #[wasm_bindgen(js_name = readActiveGeneration)]
    pub async fn read_active_generation(name: &JsString) -> std::result::Result<JsValue, JsValue> {
        let name = js_database_name(name)?;
        Ok(OpfsBackend::read_active_generation(&name)
            .await
            .map_err(js_error)?
            .map(|generation| JsValue::from_str(&generation))
            .unwrap_or(JsValue::NULL))
    }

    #[wasm_bindgen(js_name = cleanupInactiveEntries)]
    pub async fn cleanup_inactive_entries(name: &JsString) -> std::result::Result<(), JsValue> {
        let name = js_database_name(name)?;
        OpfsBackend::cleanup_inactive_entries(&name)
            .await
            .map_err(js_error)
    }

    #[wasm_bindgen(js_name = dbDirectorySize)]
    pub async fn db_directory_size(name: &JsString) -> std::result::Result<f64, JsValue> {
        let name = js_database_name(name)?;
        Ok(OpfsBackend::db_directory_size(&name)
            .await
            .map_err(js_error)? as f64)
    }

    /// Returns `"debug"` or `"release"` for the loaded WASM build.
    #[wasm_bindgen(js_name = buildProfile)]
    pub fn build_profile() -> String {
        if cfg!(debug_assertions) {
            "debug".into()
        } else {
            "release".into()
        }
    }

    impl WasmEngine {
        fn ensure_not_open(&self) -> std::result::Result<(), JsValue> {
            if self.inner.is_some() {
                return Err(js_error(EngineError::Internal(
                    "engine already open".into(),
                )));
            }
            Ok(())
        }

        fn finish_open(
            &mut self,
            name: &str,
            files: FileSet<OpfsBackend>,
            open: &WasmOpenOptions,
        ) -> std::result::Result<(), JsValue> {
            let engine =
                super::open_engine_for_binding(name, files, open.config()).map_err(js_error)?;
            self.inner = Some(engine);
            Ok(())
        }

        fn inner_mut(&mut self) -> std::result::Result<&mut Engine<OpfsBackend>, JsValue> {
            self.inner
                .as_mut()
                .ok_or_else(|| js_error(EngineError::Closed))
        }
    }

    #[wasm_bindgen]
    impl WasmEngine {
        #[wasm_bindgen(constructor)]
        pub fn new() -> Self {
            Self::default()
        }

        #[wasm_bindgen]
        pub async fn open(
            &mut self,
            name: &JsString,
            options: JsValue,
        ) -> std::result::Result<(), JsValue> {
            self.ensure_not_open()?;
            let name = js_database_name(name)?;
            let open: WasmOpenOptions = parse_optional_options(options)?;
            let files = OpfsBackend::open_db(&name, open.config().create_if_missing)
                .await
                .map_err(js_error)?;
            self.finish_open(&name, files, &open)
        }

        #[wasm_bindgen(js_name = openGeneration)]
        pub async fn open_generation(
            &mut self,
            name: &JsString,
            generation_name: String,
            options: JsValue,
        ) -> std::result::Result<(), JsValue> {
            self.ensure_not_open()?;
            let name = js_database_name(name)?;
            let open: WasmOpenOptions = parse_optional_options(options)?;
            let files = OpfsBackend::open_generation(
                &name,
                &generation_name,
                open.config().create_if_missing,
            )
            .await
            .map_err(js_error)?;
            self.finish_open(&name, files, &open)
        }

        /// Checkpoints a healthy engine and closes it. Releases the binding
        /// handle even on error so a retry cannot reuse a partially closed engine.
        #[wasm_bindgen]
        pub fn close(&mut self) -> std::result::Result<(), JsValue> {
            self.scan_cursors.clear();
            match self.inner.take() {
                Some(mut engine) => engine.close().map_err(js_error),
                None => Ok(()),
            }
        }

        /// Closes without a checkpoint. Used for an engine whose files are
        /// about to be replaced or whose memory is not trusted.
        #[wasm_bindgen]
        pub fn abandon(&mut self) -> std::result::Result<(), JsValue> {
            self.scan_cursors.clear();
            match self.inner.take() {
                Some(mut engine) => engine.abandon().map_err(js_error),
                None => Ok(()),
            }
        }

        #[wasm_bindgen]
        pub fn recover(&mut self) -> std::result::Result<JsValue, JsValue> {
            let report = self.inner_mut()?.recover().map_err(js_error)?;
            js_value_from_serializable(&report)
        }

        #[wasm_bindgen]
        pub fn health(&mut self) -> std::result::Result<JsValue, JsValue> {
            let health = self.inner_mut()?.health().clone();
            js_value_from_serializable(&health)
        }

        #[wasm_bindgen]
        pub fn needs_recovery(&mut self) -> std::result::Result<bool, JsValue> {
            Ok(self.inner_mut()?.needs_recovery())
        }

        #[wasm_bindgen]
        pub fn checkpoint(&mut self) -> std::result::Result<(), JsValue> {
            self.inner_mut()?.checkpoint().map_err(js_error)
        }

        /// Streams this database into a new, empty target generation.
        /// Returns the target txid. The caller must activate the generation.
        #[wasm_bindgen]
        pub fn compact_into(
            &mut self,
            target: &mut WasmEngine,
        ) -> std::result::Result<u64, JsValue> {
            let target = target.inner_mut()?;
            let engine = self.inner_mut()?;
            super::validate_js_u64(
                engine
                    .stats()
                    .map_err(js_error)?
                    .last_committed_txid
                    .saturating_add(1),
                "compaction commit txid",
            )
            .map_err(js_error)?;
            engine.compact_into(target).map_err(js_error)
        }

        /// Omits the named internal stores during a rebuild. Compaction
        /// also omits the change log and retains the other stores.
        #[wasm_bindgen]
        pub fn compact_into_skipping_stores(
            &mut self,
            target: &mut WasmEngine,
            skip_stores: JsValue,
        ) -> std::result::Result<u64, JsValue> {
            let skip_stores: Vec<String> =
                serde_wasm_bindgen::from_value(skip_stores).map_err(js_error_from_display)?;
            let target = target.inner_mut()?;
            let engine = self.inner_mut()?;
            super::validate_js_u64(
                engine
                    .stats()
                    .map_err(js_error)?
                    .last_committed_txid
                    .saturating_add(1),
                "compaction commit txid",
            )
            .map_err(js_error)?;
            engine
                .compact_into_skipping_stores(target, &skip_stores)
                .map_err(js_error)
        }

        #[wasm_bindgen]
        pub fn begin_tx(&mut self, mode: String) -> std::result::Result<u64, JsValue> {
            let engine = self.inner_mut()?;
            let tx_mode = mode.parse::<TxMode>().map_err(js_error)?;
            super::begin_tx_for_binding(engine, tx_mode).map_err(js_error)
        }

        #[wasm_bindgen]
        pub fn commit_tx(&mut self, tx_id: u64) -> std::result::Result<u64, JsValue> {
            let committed =
                super::commit_tx_for_binding(self.inner_mut()?, tx_id).map_err(js_error)?;
            self.scan_cursors.retain(|_, cursor| cursor.tx_id != tx_id);
            Ok(committed)
        }

        #[wasm_bindgen]
        pub fn rollback_tx(&mut self, tx_id: u64) -> std::result::Result<(), JsValue> {
            self.inner_mut()?.rollback_tx(tx_id).map_err(js_error)?;
            self.scan_cursors.retain(|_, cursor| cursor.tx_id != tx_id);
            Ok(())
        }

        #[wasm_bindgen]
        pub fn create_store(
            &mut self,
            tx_id: u64,
            name: String,
            options: JsValue,
        ) -> std::result::Result<(), JsValue> {
            let options: WasmCreateStoreOptions = parse_optional_options(options)?;
            let compression = options.compression_kind().map_err(js_error_from_display)?;
            self.inner_mut()?
                .create_store_with_compression(tx_id, &name, compression)
                .map_err(js_error)
        }

        #[wasm_bindgen]
        pub fn drop_store(&mut self, tx_id: u64, name: String) -> std::result::Result<(), JsValue> {
            self.inner_mut()?.drop_store(tx_id, &name).map_err(js_error)
        }

        #[wasm_bindgen]
        pub fn clear_store(
            &mut self,
            tx_id: u64,
            name: String,
        ) -> std::result::Result<(), JsValue> {
            self.inner_mut()?
                .clear_store(tx_id, &name)
                .map_err(js_error)
        }

        #[wasm_bindgen]
        pub fn get(
            &mut self,
            tx_id: u64,
            store: String,
            key: &[u8],
        ) -> std::result::Result<JsValue, JsValue> {
            match self
                .inner_mut()?
                .get(tx_id, &store, key)
                .map_err(js_error)?
            {
                Some(bytes) => Ok(Uint8Array::from(bytes.as_slice()).into()),
                None => Ok(JsValue::NULL),
            }
        }

        #[wasm_bindgen]
        pub fn has(
            &mut self,
            tx_id: u64,
            store: String,
            key: &[u8],
        ) -> std::result::Result<bool, JsValue> {
            self.inner_mut()?.has(tx_id, &store, key).map_err(js_error)
        }

        /// Checks existence using one TTL timestamp without loading value bodies.
        #[wasm_bindgen]
        pub fn has_many(
            &mut self,
            tx_id: u64,
            store: String,
            keys: JsValue,
        ) -> std::result::Result<JsValue, JsValue> {
            let keys = parse_uint8_array_list(keys)?;
            let values = self
                .inner_mut()?
                .has_many(tx_id, &store, &keys)
                .map_err(js_error)?;
            js_value_from_serializable(&values)
        }

        #[wasm_bindgen]
        pub fn get_many(
            &mut self,
            tx_id: u64,
            store: String,
            keys: JsValue,
        ) -> std::result::Result<JsValue, JsValue> {
            let keys = parse_uint8_array_list(keys)?;
            let values = self
                .inner_mut()?
                .get_many(tx_id, &store, &keys)
                .map_err(js_error)?;
            Ok(uint8_array_options_to_js_array(values).into())
        }

        /// Returns max(encoded, decoded) lengths. u32::MAX marks missing or expired values.
        #[wasm_bindgen]
        pub fn get_many_value_sizes(
            &mut self,
            tx_id: u64,
            store: String,
            keys: &[u8],
        ) -> std::result::Result<Vec<u32>, JsValue> {
            let keys = parse_packed_binary_list(keys, "packed getMany sizes")?;
            let sizes = self
                .inner_mut()?
                .get_many_value_sizes(tx_id, &store, &keys)
                .map_err(js_error)?;
            sizes
                .into_iter()
                .map(|length| match length {
                    None => Ok(u32::MAX),
                    Some(length) => u32::try_from(length)
                        .ok()
                        .filter(|length| *length != u32::MAX)
                        .ok_or_else(|| js_error(EngineError::ValueTooLarge(length))),
                })
                .collect()
        }

        /// Returns count followed by 32-byte rows: flags, expiry, epoch, ordinal, max length.
        #[wasm_bindgen]
        pub fn get_many_value_states(
            &mut self,
            tx_id: u64,
            store: String,
            keys: &[u8],
        ) -> std::result::Result<Vec<u8>, JsValue> {
            let keys = parse_packed_binary_list(keys, "packed getMany states")?;
            let states = self
                .inner_mut()?
                .get_many_value_states(tx_id, &store, &keys)
                .map_err(js_error)?;
            crate::output::pack_value_states(&states).map_err(js_error)
        }

        #[wasm_bindgen]
        #[allow(clippy::too_many_arguments)]
        pub fn put_index_entry_checked(
            &mut self,
            tx_id: u64,
            primary_store: String,
            index_store: String,
            primary_key: &[u8],
            index_key: &[u8],
            expected_epoch: u64,
            expected_ordinal: u64,
        ) -> std::result::Result<(), JsValue> {
            self.inner_mut()?
                .put_index_entry_checked(
                    tx_id,
                    &primary_store,
                    &index_store,
                    primary_key,
                    index_key,
                    expected_epoch,
                    expected_ordinal,
                )
                .map_err(js_error)
        }

        /// Returns packed values as
        /// `u32 count | count x u32 length (u32::MAX = missing) | bytes`.
        /// All integers are little-endian.
        #[wasm_bindgen]
        pub fn get_many_packed(
            &mut self,
            tx_id: u64,
            store: String,
            keys: &[u8],
        ) -> std::result::Result<Vec<u8>, JsValue> {
            let keys = parse_packed_binary_list(keys, "packed getMany")?;
            self.inner_mut()?
                .get_many_packed(tx_id, &store, &keys)
                .map_err(js_error)
        }

        #[wasm_bindgen]
        pub fn put(
            &mut self,
            tx_id: u64,
            store: String,
            key: &[u8],
            value: &[u8],
            options: JsValue,
        ) -> std::result::Result<bool, JsValue> {
            let options: WasmPutOptions = parse_optional_options(options)?;
            self.inner_mut()?
                .put_reporting_baseline(tx_id, &store, key, value, options.ttl)
                .map_err(js_error)
        }

        #[wasm_bindgen]
        pub fn put_many(
            &mut self,
            tx_id: u64,
            store: String,
            entries: JsValue,
            options: JsValue,
        ) -> std::result::Result<JsValue, JsValue> {
            let entries: Vec<WasmEntry> =
                serde_wasm_bindgen::from_value(entries).map_err(js_error_from_display)?;
            let entries: Vec<(Vec<u8>, Vec<u8>)> = entries
                .into_iter()
                .map(|WasmEntry(key, value)| (key, value))
                .collect();
            let options: WasmPutOptions = parse_optional_options(options)?;
            let report =
                self.inner_mut()?
                    .put_many_with_ttl_report(tx_id, &store, &entries, options.ttl);
            match report.error {
                Some(error) => Err(js_error_with_partial(error, &report.completed)),
                None => js_value_from_serializable(&report.completed),
            }
        }

        /// Returns one byte per entry: 1 if the key existed before the put.
        #[wasm_bindgen]
        pub fn put_many_packed(
            &mut self,
            tx_id: u64,
            store: String,
            entries: &[u8],
            options: JsValue,
        ) -> std::result::Result<Vec<u8>, JsValue> {
            let entries = parse_packed_binary_pairs(entries)?;
            let options: WasmPutOptions = parse_optional_options(options)?;
            let report =
                self.inner_mut()?
                    .put_many_with_ttl_report(tx_id, &store, &entries, options.ttl);
            match report.error {
                Some(error) => Err(js_error_with_partial(error, &report.completed)),
                None => Ok(report
                    .completed
                    .iter()
                    .map(|flag| u8::from(*flag))
                    .collect()),
            }
        }

        #[wasm_bindgen]
        pub fn delete(
            &mut self,
            tx_id: u64,
            store: String,
            key: &[u8],
        ) -> std::result::Result<bool, JsValue> {
            self.inner_mut()?
                .delete(tx_id, &store, key)
                .map_err(js_error)
        }

        /// Returns one baseline byte per completed row. A failed row can have
        /// staged writes and is excluded from the error's partial outcomes.
        #[wasm_bindgen]
        pub fn put_many_indexed_packed(
            &mut self,
            tx_id: u64,
            store: String,
            entries: &[u8],
            index_ops: JsValue,
            options: JsValue,
        ) -> std::result::Result<Vec<u8>, JsValue> {
            let entries = parse_packed_binary_pairs(entries)?;
            let index_ops: Vec<Vec<WasmIndexOp>> =
                serde_wasm_bindgen::from_value(index_ops).map_err(js_error_from_display)?;
            if index_ops.len() != entries.len() {
                return Err(js_error_from_display("indexed put row count mismatch"));
            }
            let index_refs: Vec<Vec<IndexOpRef<'_>>> = index_ops
                .iter()
                .map(|row| row.iter().map(WasmIndexOp::as_ref).collect())
                .collect();
            let options: WasmPutOptions = parse_optional_options(options)?;
            let report = self.inner_mut()?.put_many_indexed_report(
                tx_id,
                &store,
                &entries,
                &index_refs,
                options.ttl,
            );
            match report.error {
                Some(error) => Err(js_error_with_partial(error, &report.completed)),
                None => Ok(report
                    .completed
                    .iter()
                    .map(|flag| u8::from(*flag))
                    .collect()),
            }
        }

        #[wasm_bindgen]
        pub fn delete_many(
            &mut self,
            tx_id: u64,
            store: String,
            keys: JsValue,
        ) -> std::result::Result<JsValue, JsValue> {
            let keys: Vec<WasmBytes> =
                serde_wasm_bindgen::from_value(keys).map_err(js_error_from_display)?;
            let keys: Vec<Vec<u8>> = keys.into_iter().map(|WasmBytes(bytes)| bytes).collect();
            let report = self.inner_mut()?.delete_many_report(tx_id, &store, &keys);
            match report.error {
                Some(error) => Err(js_error_with_partial(error, &report.completed)),
                None => js_value_from_serializable(&report.completed),
            }
        }

        /// Returns one byte per key: 1 if the key existed and was deleted.
        #[wasm_bindgen]
        pub fn delete_many_packed(
            &mut self,
            tx_id: u64,
            store: String,
            keys: &[u8],
        ) -> std::result::Result<Vec<u8>, JsValue> {
            let keys = parse_packed_binary_list(keys, "packed deleteMany")?;
            let report = self.inner_mut()?.delete_many_report(tx_id, &store, &keys);
            match report.error {
                Some(error) => Err(js_error_with_partial(error, &report.completed)),
                None => Ok(report
                    .completed
                    .iter()
                    .map(|flag| u8::from(*flag))
                    .collect()),
            }
        }

        #[wasm_bindgen]
        pub fn apply_batch(
            &mut self,
            tx_id: u64,
            store: String,
            ops: JsValue,
        ) -> std::result::Result<JsValue, JsValue> {
            let ops: Vec<WasmBatchOp> =
                serde_wasm_bindgen::from_value(ops).map_err(js_error_from_display)?;
            let ops: Vec<BatchOp> = ops.into_iter().map(Into::into).collect();
            let report = self.inner_mut()?.apply_batch_report(tx_id, &store, &ops);
            let completed: Vec<WasmBatchOpOutcome> =
                report.completed.into_iter().map(Into::into).collect();
            match report.error {
                Some(error) => Err(js_error_with_partial(error, &completed)),
                None => js_value_from_serializable(&completed),
            }
        }

        /// Returns one byte per operation in input order. For puts, 1 means
        /// the key existed before the write. For deletes, 1 means it was deleted.
        #[wasm_bindgen]
        pub fn apply_batch_packed(
            &mut self,
            tx_id: u64,
            store: String,
            ops: &[u8],
        ) -> std::result::Result<Vec<u8>, JsValue> {
            let ops = parse_packed_batch_ops(ops)?;
            let report = self
                .inner_mut()?
                .apply_batch_refs_report(tx_id, &store, &ops);
            match report.error {
                Some(error) => {
                    let completed: Vec<WasmBatchOpOutcome> =
                        report.completed.into_iter().map(Into::into).collect();
                    Err(js_error_with_partial(error, &completed))
                }
                None => Ok(report.completed.iter().map(outcome_flag).collect()),
            }
        }

        #[wasm_bindgen]
        pub fn scan(
            &mut self,
            tx_id: u64,
            store: String,
            range: JsValue,
        ) -> std::result::Result<JsValue, JsValue> {
            let range: RangeSpec =
                serde_wasm_bindgen::from_value(range).map_err(js_error_from_display)?;
            let pairs = self
                .inner_mut()?
                .scan(tx_id, &store, &range)
                .map_err(js_error)?;
            let pairs: Vec<WasmKvPair> = pairs.into_iter().map(Into::into).collect();
            js_value_from_serializable(&pairs)
        }

        #[wasm_bindgen]
        pub fn open_scan_cursor(
            &mut self,
            tx_id: u64,
            store: String,
            range: JsValue,
            keys_only: bool,
        ) -> std::result::Result<u32, JsValue> {
            let range: RangeSpec =
                serde_wasm_bindgen::from_value(range).map_err(js_error_from_display)?;
            range.validate().map_err(js_error)?;
            self.inner_mut()?
                .has_many::<&[u8]>(tx_id, &store, &[])
                .map_err(js_error)?;
            let id = self.next_scan_cursor.checked_add(1).ok_or_else(|| {
                js_error(EngineError::Internal("scan cursor id exhausted".into()))
            })?;
            self.next_scan_cursor = id;
            self.scan_cursors.insert(
                id,
                super::ScanBookmark {
                    tx_id,
                    store,
                    exhausted: range.limit == Some(0),
                    range,
                    keys_only,
                },
            );
            Ok(id)
        }

        #[wasm_bindgen]
        pub fn scan_cursor_next(
            &mut self,
            cursor_id: u32,
            max_rows: u32,
            max_bytes: u32,
        ) -> std::result::Result<JsValue, JsValue> {
            if max_rows == 0 || max_bytes < 4 {
                return Err(js_error(EngineError::InvalidRange(
                    "scan budgets must include a row and the 4-byte header".into(),
                )));
            }
            let mut cursor = self
                .scan_cursors
                .remove(&cursor_id)
                .ok_or_else(|| js_error(EngineError::TransactionClosed))?;
            let result = (|| {
                let page = if cursor.exhausted {
                    crate::output::PackedScanPage {
                        packet: vec![0; 4],
                        row_count: 0,
                        last_key: None,
                        exhausted: true,
                    }
                } else {
                    self.inner_mut()?
                        .scan_packed_page(
                            cursor.tx_id,
                            &cursor.store,
                            &cursor.range,
                            max_rows as usize,
                            max_bytes as usize,
                            cursor.keys_only,
                        )
                        .map_err(js_error)?
                };
                cursor.advance(&page);
                let result = Object::new();
                set_js_property(
                    &result,
                    "packet",
                    &Uint8Array::from(page.packet.as_slice()).into(),
                );
                set_js_property(
                    &result,
                    "rowCount",
                    &JsValue::from_f64(page.row_count as f64),
                );
                set_js_property(&result, "exhausted", &JsValue::from_bool(cursor.exhausted));
                Ok(result.into())
            })();
            self.scan_cursors.insert(cursor_id, cursor);
            result
        }

        #[wasm_bindgen]
        pub fn close_scan_cursor(&mut self, cursor_id: u32) -> std::result::Result<(), JsValue> {
            self.scan_cursors.remove(&cursor_id);
            Ok(())
        }

        #[wasm_bindgen]
        pub fn stats(&mut self) -> std::result::Result<JsValue, JsValue> {
            let stats: DbStats = self.inner_mut()?.stats().map_err(js_error)?;
            js_value_from_serializable(&stats)
        }

        #[wasm_bindgen]
        pub fn changes_since(
            &mut self,
            tx_id: u64,
            options: JsValue,
        ) -> std::result::Result<JsValue, JsValue> {
            let options: ChangeFeedOptions = parse_optional_options(options)?;
            let feed = self
                .inner_mut()?
                .changes_since(tx_id, options)
                .map_err(js_error)?;
            js_value_from_serializable(&WasmChangeFeed(&feed))
        }

        #[wasm_bindgen]
        pub fn change_feed_policy(&mut self) -> std::result::Result<JsValue, JsValue> {
            let policy = self.inner_mut()?.change_feed_policy();
            js_value_from_serializable(&policy)
        }

        #[wasm_bindgen]
        pub fn set_change_feed_policy(
            &mut self,
            tx_id: u64,
            policy: JsValue,
        ) -> std::result::Result<(), JsValue> {
            let policy: ChangeFeedPolicy =
                serde_wasm_bindgen::from_value(policy).map_err(js_error_from_display)?;
            self.inner_mut()?
                .set_change_feed_policy(tx_id, policy)
                .map_err(js_error)
        }

        #[wasm_bindgen]
        pub fn get_schema_version(&mut self) -> std::result::Result<u64, JsValue> {
            Ok(self.inner_mut()?.schema_version())
        }

        #[wasm_bindgen]
        pub fn export_snapshot(&mut self) -> std::result::Result<Vec<u8>, JsValue> {
            self.inner_mut()?.export_snapshot().map_err(js_error)
        }

        #[wasm_bindgen]
        pub fn list_store_configs(&mut self) -> std::result::Result<JsValue, JsValue> {
            let stores = self.inner_mut()?.visible_store_configs();
            js_value_from_serializable(&stores)
        }

        #[wasm_bindgen]
        pub fn import_snapshot(&mut self, data: &[u8]) -> std::result::Result<u64, JsValue> {
            super::import_snapshot_for_binding(self.inner_mut()?, data).map_err(js_error)
        }

        #[wasm_bindgen]
        pub fn import_snapshot_into(
            &mut self,
            target: &mut WasmEngine,
            data: &[u8],
        ) -> std::result::Result<u64, JsValue> {
            let snapshot = super::decode_snapshot_for_binding(data).map_err(js_error)?;
            let engine = self.inner_mut()?;
            super::validate_js_u64(engine.next_commit_txid(), "next commit txid")
                .map_err(js_error)?;
            engine
                .import_snapshot_contents_into(target.inner_mut()?, snapshot)
                .map_err(js_error)
        }

        #[wasm_bindgen]
        pub fn reset(&mut self) -> std::result::Result<u64, JsValue> {
            let engine = self.inner_mut()?;
            super::validate_js_u64(engine.next_commit_txid(), "next commit txid")
                .map_err(js_error)?;
            engine.reset().map_err(js_error)
        }

        #[wasm_bindgen]
        pub fn list_stores(&mut self) -> std::result::Result<JsValue, JsValue> {
            let stores = self.inner_mut()?.store_names();
            js_value_from_serializable(&stores)
        }

        #[wasm_bindgen]
        pub fn set_schema_version(
            &mut self,
            tx_id: u64,
            version: u64,
        ) -> std::result::Result<(), JsValue> {
            super::validate_js_u64(version, "schema version").map_err(js_error)?;
            self.inner_mut()?
                .set_schema_version(tx_id, version)
                .map_err(js_error)
        }

        #[wasm_bindgen]
        pub fn set_failpoint(&mut self, failpoint: JsValue) -> std::result::Result<(), JsValue> {
            let parsed = if failpoint.is_null() || failpoint.is_undefined() {
                None
            } else {
                Some(
                    Failpoint::parse(
                        &failpoint
                            .as_string()
                            .ok_or_else(|| js_error_from_display("failpoint must be string"))?,
                    )
                    .map_err(js_error)?,
                )
            };
            self.inner_mut()?.set_failpoint(parsed);
            Ok(())
        }
    }

    fn js_value_from_serializable<T: Serialize>(
        value: &T,
    ) -> std::result::Result<JsValue, JsValue> {
        serde_wasm_bindgen::to_value(value).map_err(js_error_from_display)
    }

    fn parse_uint8_array_list(value: JsValue) -> std::result::Result<Vec<Vec<u8>>, JsValue> {
        if !Array::is_array(&value) {
            return Err(js_error_from_display(
                "expected an array of Uint8Array values",
            ));
        }
        let array = Array::from(&value);
        let mut out = Vec::with_capacity(array.length() as usize);
        for index in 0..array.length() {
            let item = array.get(index);
            let bytes = item
                .dyn_into::<Uint8Array>()
                .map_err(|_| js_error_from_display("expected Uint8Array key"))?;
            out.push(bytes.to_vec());
        }
        Ok(out)
    }

    /// Returns slices that borrow the packed payload without copying its bytes.
    fn parse_packed_binary_list<'a>(
        bytes: &'a [u8],
        what: &str,
    ) -> std::result::Result<Vec<&'a [u8]>, JsValue> {
        if bytes.len() < 4 {
            return Err(js_error_from_display(format!("invalid {what} payload")));
        }
        let count = read_u32_le(bytes, 0, what)?;
        let metadata_bytes = checked_byte_count(count, 4, what)?;
        let payload_offset = checked_add_usize(4, metadata_bytes, what)?;
        if payload_offset > bytes.len() {
            return Err(js_error_from_display(format!(
                "{what} metadata exceeds payload length"
            )));
        }

        let mut items = Vec::with_capacity(count);
        let mut metadata_offset = 4;
        let mut read_offset = payload_offset;
        for _ in 0..count {
            let byte_length = read_u32_le(bytes, metadata_offset, what)?;
            metadata_offset += 4;
            let end = checked_add_usize(read_offset, byte_length, what)?;
            if end > bytes.len() {
                return Err(js_error_from_display(format!(
                    "{what} item exceeds payload length"
                )));
            }
            items.push(&bytes[read_offset..end]);
            read_offset = end;
        }
        if read_offset != bytes.len() {
            return Err(js_error_from_display(format!(
                "{what} payload has trailing bytes"
            )));
        }
        Ok(items)
    }

    type PackedBinaryPair<'a> = (&'a [u8], &'a [u8]);

    fn parse_packed_binary_pairs(
        bytes: &[u8],
    ) -> std::result::Result<Vec<PackedBinaryPair<'_>>, JsValue> {
        let what = "packed putMany";
        if bytes.len() < 4 {
            return Err(js_error_from_display("invalid packed putMany payload"));
        }
        let item_count = read_u32_le(bytes, 0, what)?;
        if item_count % 2 != 0 {
            return Err(js_error_from_display(
                "packed putMany payload has an odd item count",
            ));
        }
        let metadata_bytes = checked_byte_count(item_count, 4, what)?;
        let payload_offset = checked_add_usize(4, metadata_bytes, what)?;
        if payload_offset > bytes.len() {
            return Err(js_error_from_display(
                "packed putMany metadata exceeds payload length",
            ));
        }

        let entry_count = item_count / 2;
        let mut entries = Vec::with_capacity(entry_count);
        let mut metadata_offset = 4;
        let mut read_offset = payload_offset;
        for _ in 0..entry_count {
            let key_length = read_u32_le(bytes, metadata_offset, what)?;
            let value_length = read_u32_le(bytes, metadata_offset + 4, what)?;
            metadata_offset += 8;
            let key_end = checked_add_usize(read_offset, key_length, what)?;
            if key_end > bytes.len() {
                return Err(js_error_from_display(
                    "packed putMany key exceeds payload length",
                ));
            }
            let value_end = checked_add_usize(key_end, value_length, what)?;
            if value_end > bytes.len() {
                return Err(js_error_from_display(
                    "packed putMany value exceeds payload length",
                ));
            }
            entries.push((&bytes[read_offset..key_end], &bytes[key_end..value_end]));
            read_offset = value_end;
        }
        if read_offset != bytes.len() {
            return Err(js_error_from_display(
                "packed putMany payload has trailing bytes",
            ));
        }
        Ok(entries)
    }

    fn parse_packed_batch_ops(bytes: &[u8]) -> std::result::Result<Vec<BatchOpRef<'_>>, JsValue> {
        let what = "packed batch";
        if bytes.len() < 4 {
            return Err(js_error_from_display("invalid packed batch payload"));
        }
        let count = read_u32_le(bytes, 0, what)?;
        let metadata_bytes = checked_byte_count(count, 9, what)?;
        let payload_offset = checked_add_usize(4, metadata_bytes, what)?;
        if payload_offset > bytes.len() {
            return Err(js_error_from_display(
                "packed batch metadata exceeds payload length",
            ));
        }

        let mut ops = Vec::with_capacity(count);
        let mut metadata_offset = 4;
        let mut read_offset = payload_offset;
        for _ in 0..count {
            let kind = bytes[metadata_offset];
            let key_length = read_u32_le(bytes, metadata_offset + 1, what)?;
            let value_length = read_u32_le(bytes, metadata_offset + 5, what)?;
            metadata_offset += 9;

            let key_end = checked_add_usize(read_offset, key_length, what)?;
            if key_end > bytes.len() {
                return Err(js_error_from_display(
                    "packed batch key exceeds payload length",
                ));
            }
            let key = &bytes[read_offset..key_end];
            read_offset = key_end;

            if kind == PACKED_BATCH_OP_DELETE {
                if value_length != 0 {
                    return Err(js_error_from_display(
                        "packed delete operation has a value payload",
                    ));
                }
                ops.push(BatchOpRef::Delete { key });
                continue;
            }

            if kind != PACKED_BATCH_OP_PUT {
                return Err(js_error_from_display(format!(
                    "packed batch operation has invalid kind byte: {kind}"
                )));
            }
            let value_end = checked_add_usize(read_offset, value_length, what)?;
            if value_end > bytes.len() {
                return Err(js_error_from_display(
                    "packed batch value exceeds payload length",
                ));
            }
            ops.push(BatchOpRef::Put {
                key,
                value: &bytes[read_offset..value_end],
            });
            read_offset = value_end;
        }
        if read_offset != bytes.len() {
            return Err(js_error_from_display(
                "packed batch payload has trailing bytes",
            ));
        }
        Ok(ops)
    }

    fn read_u32_le(bytes: &[u8], offset: usize, what: &str) -> std::result::Result<usize, JsValue> {
        let end = checked_add_usize(offset, 4, what)?;
        if end > bytes.len() {
            return Err(js_error_from_display(format!(
                "{what} u32 field exceeds payload length"
            )));
        }
        let mut raw = [0; 4];
        raw.copy_from_slice(&bytes[offset..end]);
        Ok(u32::from_le_bytes(raw) as usize)
    }

    fn checked_byte_count(
        count: usize,
        item_bytes: usize,
        what: &str,
    ) -> std::result::Result<usize, JsValue> {
        count.checked_mul(item_bytes).ok_or_else(|| {
            js_error_from_display(format!("{what} metadata exceeds platform usize limit"))
        })
    }

    fn checked_add_usize(
        left: usize,
        right: usize,
        what: &str,
    ) -> std::result::Result<usize, JsValue> {
        left.checked_add(right).ok_or_else(|| {
            js_error_from_display(format!("{what} payload exceeds platform usize limit"))
        })
    }

    fn uint8_array_options_to_js_array(values: Vec<Option<Vec<u8>>>) -> Array {
        let array = Array::new_with_length(values.len() as u32);
        for (index, value) in values.into_iter().enumerate() {
            let item = match value {
                Some(bytes) => Uint8Array::from(bytes.as_slice()).into(),
                None => JsValue::NULL,
            };
            array.set(index as u32, item);
        }
        array
    }

    fn parse_optional_options<T>(value: JsValue) -> std::result::Result<T, JsValue>
    where
        T: for<'de> Deserialize<'de> + Default,
    {
        if value.is_null() || value.is_undefined() {
            Ok(T::default())
        } else {
            serde_wasm_bindgen::from_value(value).map_err(js_error_from_display)
        }
    }

    fn js_error(err: EngineError) -> JsValue {
        let message = err.to_string();
        js_error_object(err.code(), &message).into()
    }

    fn js_error_with_partial<T: Serialize>(err: EngineError, partial: &T) -> JsValue {
        let message = err.to_string();
        let obj = js_error_object(err.code(), &message);
        if let Ok(partial) = js_value_from_serializable(partial) {
            set_js_property(&obj, "partial", &partial);
        }
        obj.into()
    }

    fn js_error_from_display<E: std::fmt::Display>(err: E) -> JsValue {
        let message = err.to_string();
        js_error_object("SerializationError", &message).into()
    }

    fn js_error_object(code: &str, message: &str) -> Object {
        let obj = Object::new();
        set_js_string_property(&obj, "code", code);
        set_js_string_property(&obj, "message", message);
        set_js_string_property(&obj, "name", code);
        obj
    }

    fn set_js_string_property(obj: &Object, key: &str, value: &str) {
        set_js_property(obj, key, &JsValue::from_str(value));
    }

    fn set_js_property(obj: &Object, key: &str, value: &JsValue) {
        let _ = Reflect::set(obj, &JsValue::from_str(key), value);
    }

    pub use WasmEngine as ExportedWasmEngine;
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    pub struct ExportedWasmEngine;
}

#[cfg(target_arch = "wasm32")]
pub use wasm::ExportedWasmEngine as WasmEngine;

#[cfg(not(target_arch = "wasm32"))]
pub use native::ExportedWasmEngine as WasmEngine;
