#[cfg(target_arch = "wasm32")]
mod wasm {
    use crate::btree::{KvPair, RangeSpec};
    use crate::change_feed::ChangeFeedOptions;
    use crate::engine::{DbStats, Engine, Failpoint, OpenConfig};
    use crate::error::EngineError;
    use crate::storage::backend::FileSet;
    use crate::storage::opfs::OpfsBackend;
    use crate::txn::{BatchOp, BatchOpOutcome, TxMode};
    use crate::value::StoreCompression;
    use js_sys::{Array, Object, Reflect, Uint8Array};
    use serde::{Deserialize, Serialize};
    use wasm_bindgen::prelude::*;

    const PACKED_BATCH_OP_DELETE: u8 = 0;
    const PACKED_BATCH_OP_PUT: u8 = 1;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct WasmOpenOptions {
        create_if_missing: Option<bool>,
        cache_pages: Option<usize>,
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
    pub struct WasmEngine {
        inner: Option<Engine<OpfsBackend>>,
    }

    #[wasm_bindgen(js_name = deleteDB)]
    pub async fn delete_db(name: String) -> std::result::Result<(), JsValue> {
        OpfsBackend::remove_db(&name).await.map_err(js_error)
    }

    #[wasm_bindgen(js_name = prepareRebuildTarget)]
    pub async fn prepare_rebuild_target(name: String) -> std::result::Result<JsValue, JsValue> {
        let generation_name = OpfsBackend::prepare_rebuild_target(&name)
            .await
            .map_err(js_error)?;
        js_value_from_serializable(&WasmRebuildTargetInfo { generation_name })
    }

    #[wasm_bindgen(js_name = swapActiveGeneration)]
    pub async fn swap_active_generation(
        name: String,
        generation_name: String,
    ) -> std::result::Result<(), JsValue> {
        OpfsBackend::swap_active_generation(&name, &generation_name)
            .await
            .map_err(js_error)
    }

    #[wasm_bindgen(js_name = cleanupInactiveEntries)]
    pub async fn cleanup_inactive_entries(name: String) -> std::result::Result<(), JsValue> {
        OpfsBackend::cleanup_inactive_entries(&name)
            .await
            .map_err(js_error)
    }

    #[wasm_bindgen(js_name = dbDirectorySize)]
    pub async fn db_directory_size(name: String) -> std::result::Result<f64, JsValue> {
        Ok(OpfsBackend::db_directory_size(&name)
            .await
            .map_err(js_error)? as f64)
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
            let engine = Engine::open(
                name,
                files,
                OpenConfig {
                    create_if_missing: open.create_if_missing.unwrap_or(true),
                    cache_pages: open.cache_pages.unwrap_or(256),
                },
            )
            .map_err(js_error)?;
            self.inner = Some(engine);
            Ok(())
        }
    }

    #[wasm_bindgen]
    impl WasmEngine {
        #[wasm_bindgen(constructor)]
        pub fn new() -> Self {
            Self { inner: None }
        }

        #[wasm_bindgen]
        pub async fn open(
            &mut self,
            name: String,
            options: JsValue,
        ) -> std::result::Result<(), JsValue> {
            self.ensure_not_open()?;
            let open: WasmOpenOptions =
                serde_wasm_bindgen::from_value(options).map_err(js_error_from_display)?;
            let files = OpfsBackend::open_db(&name, open.create_if_missing.unwrap_or(true))
                .await
                .map_err(js_error)?;
            self.finish_open(&name, files, &open)
        }

        #[wasm_bindgen(js_name = openGeneration)]
        pub async fn open_generation(
            &mut self,
            name: String,
            generation_name: String,
            options: JsValue,
        ) -> std::result::Result<(), JsValue> {
            self.ensure_not_open()?;
            let open: WasmOpenOptions =
                serde_wasm_bindgen::from_value(options).map_err(js_error_from_display)?;
            let files = OpfsBackend::open_generation(
                &name,
                &generation_name,
                open.create_if_missing.unwrap_or(true),
            )
            .await
            .map_err(js_error)?;
            self.finish_open(&name, files, &open)
        }

        #[wasm_bindgen]
        pub fn close(&mut self) -> std::result::Result<(), JsValue> {
            if let Some(engine) = self.inner.as_mut() {
                engine.close().map_err(js_error)?;
            }
            self.inner = None;
            Ok(())
        }

        #[wasm_bindgen]
        pub fn begin_tx(&mut self, mode: String) -> std::result::Result<u64, JsValue> {
            let engine = self.inner_mut()?;
            let tx_mode = mode.parse::<TxMode>().map_err(js_error)?;
            engine.begin_tx(tx_mode).map_err(js_error)
        }

        #[wasm_bindgen]
        pub fn commit_tx(&mut self, tx_id: u64) -> std::result::Result<u64, JsValue> {
            self.inner_mut()?.commit_tx(tx_id).map_err(js_error)
        }

        #[wasm_bindgen]
        pub fn rollback_tx(&mut self, tx_id: u64) -> std::result::Result<(), JsValue> {
            self.inner_mut()?.rollback_tx(tx_id).map_err(js_error)
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
            key: Vec<u8>,
        ) -> std::result::Result<JsValue, JsValue> {
            match self
                .inner_mut()?
                .get(tx_id, &store, &key)
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
            key: Vec<u8>,
        ) -> std::result::Result<bool, JsValue> {
            self.inner_mut()?.has(tx_id, &store, &key).map_err(js_error)
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

        #[wasm_bindgen]
        pub fn get_many_packed(
            &mut self,
            tx_id: u64,
            store: String,
            keys: Uint8Array,
        ) -> std::result::Result<JsValue, JsValue> {
            let keys = parse_packed_binary_list(keys, "packed getMany")?;
            let values = self
                .inner_mut()?
                .get_many(tx_id, &store, &keys)
                .map_err(js_error)?;
            Ok(uint8_array_options_to_js_array(values).into())
        }

        #[wasm_bindgen]
        pub fn put(
            &mut self,
            tx_id: u64,
            store: String,
            key: Vec<u8>,
            value: Vec<u8>,
            options: JsValue,
        ) -> std::result::Result<(), JsValue> {
            let options: WasmPutOptions = parse_optional_options(options)?;
            self.inner_mut()?
                .put_with_ttl(tx_id, &store, &key, &value, options.ttl)
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

        #[wasm_bindgen]
        pub fn put_many_packed(
            &mut self,
            tx_id: u64,
            store: String,
            entries: Uint8Array,
            options: JsValue,
        ) -> std::result::Result<JsValue, JsValue> {
            let entries = parse_packed_binary_pairs(entries)?;
            let options: WasmPutOptions = parse_optional_options(options)?;
            let report =
                self.inner_mut()?
                    .put_many_with_ttl_report(tx_id, &store, &entries, options.ttl);
            match report.error {
                Some(error) => Err(js_error_with_partial(error, &report.completed)),
                None => js_value_from_serializable(&report.completed),
            }
        }

        #[wasm_bindgen]
        pub fn delete(
            &mut self,
            tx_id: u64,
            store: String,
            key: Vec<u8>,
        ) -> std::result::Result<bool, JsValue> {
            self.inner_mut()?
                .delete(tx_id, &store, &key)
                .map_err(js_error)
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

        #[wasm_bindgen]
        pub fn delete_many_packed(
            &mut self,
            tx_id: u64,
            store: String,
            keys: Uint8Array,
        ) -> std::result::Result<JsValue, JsValue> {
            let keys = parse_packed_binary_list(keys, "packed deleteMany")?;
            let report = self.inner_mut()?.delete_many_report(tx_id, &store, &keys);
            match report.error {
                Some(error) => Err(js_error_with_partial(error, &report.completed)),
                None => js_value_from_serializable(&report.completed),
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

        #[wasm_bindgen]
        pub fn apply_batch_packed(
            &mut self,
            tx_id: u64,
            store: String,
            ops: Uint8Array,
        ) -> std::result::Result<JsValue, JsValue> {
            let ops = parse_packed_batch_ops(ops)?;
            let report = self.inner_mut()?.apply_batch_report(tx_id, &store, &ops);
            let completed: Vec<WasmBatchOpOutcome> =
                report.completed.into_iter().map(Into::into).collect();
            match report.error {
                Some(error) => Err(js_error_with_partial(error, &completed)),
                None => js_value_from_serializable(&completed),
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
            js_value_from_serializable(&feed)
        }

        #[wasm_bindgen]
        pub fn get_schema_version(&mut self) -> std::result::Result<u64, JsValue> {
            Ok(self.inner_mut()?.schema_version())
        }

        #[wasm_bindgen]
        pub fn export_snapshot(&mut self) -> std::result::Result<Uint8Array, JsValue> {
            let bytes = self.inner_mut()?.export_snapshot().map_err(js_error)?;
            Ok(Uint8Array::from(bytes.as_slice()))
        }

        #[wasm_bindgen]
        pub fn list_store_configs(&mut self) -> std::result::Result<JsValue, JsValue> {
            let stores = self.inner_mut()?.visible_store_configs();
            js_value_from_serializable(&stores)
        }

        #[wasm_bindgen]
        pub fn import_snapshot(&mut self, data: Vec<u8>) -> std::result::Result<u64, JsValue> {
            self.inner_mut()?.import_snapshot(&data).map_err(js_error)
        }

        #[wasm_bindgen]
        pub fn reset(&mut self) -> std::result::Result<u64, JsValue> {
            self.inner_mut()?.reset().map_err(js_error)
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

        fn inner_mut(&mut self) -> std::result::Result<&mut Engine<OpfsBackend>, JsValue> {
            self.inner
                .as_mut()
                .ok_or_else(|| js_error_from_display("engine not open"))
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

    fn parse_packed_binary_list(
        value: Uint8Array,
        what: &str,
    ) -> std::result::Result<Vec<Vec<u8>>, JsValue> {
        let bytes = value.to_vec();
        if bytes.len() < 4 {
            return Err(js_error_from_display(format!("invalid {what} payload")));
        }
        let count = read_u32_le(&bytes, 0, what)?;
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
            let byte_length = read_u32_le(&bytes, metadata_offset, what)?;
            metadata_offset += 4;
            let end = checked_add_usize(read_offset, byte_length, what)?;
            if end > bytes.len() {
                return Err(js_error_from_display(format!(
                    "{what} item exceeds payload length"
                )));
            }
            items.push(bytes[read_offset..end].to_vec());
            read_offset = end;
        }
        if read_offset != bytes.len() {
            return Err(js_error_from_display(format!(
                "{what} payload has trailing bytes"
            )));
        }
        Ok(items)
    }

    fn parse_packed_binary_pairs(
        value: Uint8Array,
    ) -> std::result::Result<Vec<(Vec<u8>, Vec<u8>)>, JsValue> {
        let bytes = value.to_vec();
        let what = "packed putMany";
        if bytes.len() < 4 {
            return Err(js_error_from_display("invalid packed putMany payload"));
        }
        let item_count = read_u32_le(&bytes, 0, what)?;
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
            let key_length = read_u32_le(&bytes, metadata_offset, what)?;
            let value_length = read_u32_le(&bytes, metadata_offset + 4, what)?;
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
            entries.push((
                bytes[read_offset..key_end].to_vec(),
                bytes[key_end..value_end].to_vec(),
            ));
            read_offset = value_end;
        }
        if read_offset != bytes.len() {
            return Err(js_error_from_display(
                "packed putMany payload has trailing bytes",
            ));
        }
        Ok(entries)
    }

    fn parse_packed_batch_ops(value: Uint8Array) -> std::result::Result<Vec<BatchOp>, JsValue> {
        let bytes = value.to_vec();
        let what = "packed batch";
        if bytes.len() < 4 {
            return Err(js_error_from_display("invalid packed batch payload"));
        }
        let count = read_u32_le(&bytes, 0, what)?;
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
            let key_length = read_u32_le(&bytes, metadata_offset + 1, what)?;
            let value_length = read_u32_le(&bytes, metadata_offset + 5, what)?;
            metadata_offset += 9;

            let key_end = checked_add_usize(read_offset, key_length, what)?;
            if key_end > bytes.len() {
                return Err(js_error_from_display(
                    "packed batch key exceeds payload length",
                ));
            }
            let key = bytes[read_offset..key_end].to_vec();
            read_offset = key_end;

            if kind == PACKED_BATCH_OP_DELETE {
                if value_length != 0 {
                    return Err(js_error_from_display(
                        "packed delete operation has a value payload",
                    ));
                }
                ops.push(BatchOp::Delete { key });
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
            ops.push(BatchOp::Put {
                key,
                value: bytes[read_offset..value_end].to_vec(),
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
