#[cfg(target_arch = "wasm32")]
mod wasm {
    use crate::btree::{KvPair, RangeSpec};
    use crate::catalog::ChangeFeedPolicy;
    use crate::change_feed::ChangeFeedOptions;
    use crate::engine::{DbStats, Engine, Failpoint, OpenConfig};
    use crate::error::EngineError;
    use crate::storage::backend::FileSet;
    use crate::storage::opfs::OpfsBackend;
    use crate::txn::{BatchOp, BatchOpOutcome, BatchOpRef, TxMode};
    use crate::value::StoreCompression;
    use js_sys::{Array, Object, Reflect, Uint8Array};
    use serde::{Deserialize, Serialize};
    use wasm_bindgen::prelude::*;

    const PACKED_BATCH_OP_DELETE: u8 = 0;
    const PACKED_BATCH_OP_PUT: u8 = 1;
    /// Length marker for a missing value in packed `getMany` output.
    const PACKED_MISSING_VALUE: u32 = u32::MAX;

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

    /// Publishes `generation_name` as the active generation. With
    /// `expected_current` set, the swap fails unless the control file still
    /// names that generation (or is absent, for `null`), and the written
    /// control file is read back and verified before this resolves.
    #[wasm_bindgen(js_name = swapActiveGeneration)]
    pub async fn swap_active_generation(
        name: String,
        generation_name: String,
        expected_current: Option<String>,
    ) -> std::result::Result<(), JsValue> {
        OpfsBackend::swap_active_generation(&name, &generation_name, expected_current)
            .await
            .map_err(js_error)
    }

    /// Active generation from the control file, or `null` for the legacy
    /// layout. A corrupt control file is an error, never `null`.
    #[wasm_bindgen(js_name = readActiveGeneration)]
    pub async fn read_active_generation(name: String) -> std::result::Result<JsValue, JsValue> {
        Ok(OpfsBackend::read_active_generation(&name)
            .await
            .map_err(js_error)?
            .map(|generation| JsValue::from_str(&generation))
            .unwrap_or(JsValue::NULL))
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

    /// `"debug"` or `"release"`; benchmark reports record which build they
    /// measured instead of trusting the build script that was supposed to run.
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
            let engine = Engine::open(name, files, open.config()).map_err(js_error)?;
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
            Self { inner: None }
        }

        #[wasm_bindgen]
        pub async fn open(
            &mut self,
            name: String,
            options: JsValue,
        ) -> std::result::Result<(), JsValue> {
            self.ensure_not_open()?;
            let open: WasmOpenOptions = parse_optional_options(options)?;
            let files = OpfsBackend::open_db(&name, open.config().create_if_missing)
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

        /// Checkpoints (when healthy) and closes. The handle is released even
        /// when closing fails, so a retry never reuses a half-closed engine.
        #[wasm_bindgen]
        pub fn close(&mut self) -> std::result::Result<(), JsValue> {
            match self.inner.take() {
                Some(mut engine) => engine.close().map_err(js_error),
                None => Ok(()),
            }
        }

        /// Closes without a checkpoint. Used for an engine whose files are
        /// about to be replaced or whose memory is not trusted.
        #[wasm_bindgen]
        pub fn abandon(&mut self) -> std::result::Result<(), JsValue> {
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

        /// Streams this database into `target`, a freshly opened empty
        /// generation. Returns the txid the target was published at.
        #[wasm_bindgen]
        pub fn compact_into(
            &mut self,
            target: &mut WasmEngine,
        ) -> std::result::Result<u64, JsValue> {
            let target = target.inner_mut()?;
            self.inner_mut()?.compact_into(target).map_err(js_error)
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

        /// Keys arrive packed; values leave packed as
        /// `u32 count | count x u32 length (u32::MAX = missing) | bytes`,
        /// one buffer instead of one JS array per value.
        #[wasm_bindgen]
        pub fn get_many_packed(
            &mut self,
            tx_id: u64,
            store: String,
            keys: &[u8],
        ) -> std::result::Result<Vec<u8>, JsValue> {
            let keys = parse_packed_binary_list(keys, "packed getMany")?;
            let values = self
                .inner_mut()?
                .get_many(tx_id, &store, &keys)
                .map_err(js_error)?;
            pack_optional_values(&values)
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

        /// Returns one byte per op: for puts whether the key existed before,
        /// for deletes whether a key was deleted. The op kinds are the caller's.
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
            self.inner_mut()?.import_snapshot(data).map_err(js_error)
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

    /// Splits a packed list into slices of the input; nothing is copied.
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

    fn parse_packed_binary_pairs(
        bytes: &[u8],
    ) -> std::result::Result<Vec<(&[u8], &[u8])>, JsValue> {
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

    fn pack_optional_values(values: &[Option<Vec<u8>>]) -> std::result::Result<Vec<u8>, JsValue> {
        let what = "packed getMany output";
        let count = u32::try_from(values.len())
            .map_err(|_| js_error_from_display(format!("{what} has too many values")))?;
        let payload_len: usize = values.iter().flatten().map(Vec::len).sum();
        let mut out = Vec::with_capacity(4 + values.len() * 4 + payload_len);
        out.extend_from_slice(&count.to_le_bytes());
        for value in values {
            let len = match value {
                Some(bytes) => u32::try_from(bytes.len())
                    .ok()
                    .filter(|len| *len != PACKED_MISSING_VALUE)
                    .ok_or_else(|| js_error_from_display(format!("{what} value too large")))?,
                None => PACKED_MISSING_VALUE,
            };
            out.extend_from_slice(&len.to_le_bytes());
        }
        for bytes in values.iter().flatten() {
            out.extend_from_slice(bytes);
        }
        Ok(out)
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
