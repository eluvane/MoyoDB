#[cfg(target_arch = "wasm32")]
use crate::bytes::encode_db_name;
use crate::error::{EngineError, Result};
#[cfg(target_arch = "wasm32")]
use crate::layout::{MAIN_FILE_KIND, MANIFEST_FILE_KIND, WAL_FILE_KIND};
use crate::storage::backend::{FileBackend, FileSet};

#[cfg(target_arch = "wasm32")]
mod wasm_impl {
    use super::*;
    use js_sys::{Promise, Uint8Array};
    use serde::Deserialize;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::JsFuture;

    #[wasm_bindgen(module = "/js/opfs_shim.js")]
    extern "C" {
        #[wasm_bindgen(catch)]
        fn opfsOpenActiveDb(
            encoded_db_name: &str,
            create_if_missing: bool,
        ) -> std::result::Result<Promise, JsValue>;
        #[wasm_bindgen(catch)]
        fn opfsOpenGenerationDb(
            encoded_db_name: &str,
            generation_name: &str,
            create_if_missing: bool,
        ) -> std::result::Result<Promise, JsValue>;
        #[wasm_bindgen(catch)]
        fn opfsPrepareRebuildTarget(encoded_db_name: &str)
            -> std::result::Result<Promise, JsValue>;
        #[wasm_bindgen(catch)]
        fn opfsSwapActiveGeneration(
            encoded_db_name: &str,
            generation_name: &str,
        ) -> std::result::Result<Promise, JsValue>;
        #[wasm_bindgen(catch)]
        fn opfsCleanupInactiveEntries(
            encoded_db_name: &str,
        ) -> std::result::Result<Promise, JsValue>;
        #[wasm_bindgen(catch)]
        fn opfsDbDirectorySize(encoded_db_name: &str) -> std::result::Result<Promise, JsValue>;
        #[wasm_bindgen(catch)]
        fn opfsRemoveDb(encoded_db_name: &str) -> std::result::Result<Promise, JsValue>;
        fn opfsReadAt(session_id: u32, file_kind: u32, offset: u64, len: usize) -> Uint8Array;
        #[wasm_bindgen(catch)]
        fn opfsWriteAt(
            session_id: u32,
            file_kind: u32,
            offset: u64,
            bytes: &[u8],
        ) -> std::result::Result<u32, JsValue>;
        fn opfsFlush(session_id: u32, file_kind: u32);
        fn opfsLen(session_id: u32, file_kind: u32) -> u64;
        fn opfsTruncate(session_id: u32, file_kind: u32, size: u64);
        fn opfsCloseSession(session_id: u32);
    }

    #[derive(Debug, Clone, Deserialize)]
    struct SessionOpenResult {
        #[serde(rename = "sessionId")]
        session_id: u32,
    }

    #[derive(Debug, Clone, Deserialize)]
    struct RebuildTargetInfo {
        #[serde(rename = "generationName")]
        generation_name: String,
    }

    #[derive(Debug, Clone)]
    pub struct OpfsBackend {
        pub session_id: u32,
        pub file_kind: u32,
    }

    impl OpfsBackend {
        pub fn new(session_id: u32, file_kind: u32) -> Self {
            Self {
                session_id,
                file_kind,
            }
        }

        pub async fn open_db(
            db_name: &str,
            create_if_missing: bool,
        ) -> Result<FileSet<OpfsBackend>> {
            let encoded = encode_db_name(db_name);
            let value =
                JsFuture::from(opfsOpenActiveDb(&encoded, create_if_missing).map_err(js_err)?)
                    .await
                    .map_err(js_err)?;
            files_from_session_value(value)
        }

        pub async fn open_generation(
            db_name: &str,
            generation_name: &str,
            create_if_missing: bool,
        ) -> Result<FileSet<OpfsBackend>> {
            let encoded = encode_db_name(db_name);
            let value = JsFuture::from(
                opfsOpenGenerationDb(&encoded, generation_name, create_if_missing)
                    .map_err(js_err)?,
            )
            .await
            .map_err(js_err)?;
            files_from_session_value(value)
        }

        pub async fn prepare_rebuild_target(db_name: &str) -> Result<String> {
            let encoded = encode_db_name(db_name);
            let value = JsFuture::from(opfsPrepareRebuildTarget(&encoded).map_err(js_err)?)
                .await
                .map_err(js_err)?;
            let info: RebuildTargetInfo = parse_js_value(value, "prepare rebuild target")?;
            Ok(info.generation_name)
        }

        pub async fn swap_active_generation(db_name: &str, generation_name: &str) -> Result<()> {
            let encoded = encode_db_name(db_name);
            JsFuture::from(opfsSwapActiveGeneration(&encoded, generation_name).map_err(js_err)?)
                .await
                .map_err(js_err)?;
            Ok(())
        }

        pub async fn cleanup_inactive_entries(db_name: &str) -> Result<()> {
            let encoded = encode_db_name(db_name);
            JsFuture::from(opfsCleanupInactiveEntries(&encoded).map_err(js_err)?)
                .await
                .map_err(js_err)?;
            Ok(())
        }

        pub async fn db_directory_size(db_name: &str) -> Result<u64> {
            let encoded = encode_db_name(db_name);
            let value = JsFuture::from(opfsDbDirectorySize(&encoded).map_err(js_err)?)
                .await
                .map_err(js_err)?;
            parse_u64(value, "db directory size")
        }

        pub async fn remove_db(db_name: &str) -> Result<()> {
            let encoded = encode_db_name(db_name);
            JsFuture::from(opfsRemoveDb(&encoded).map_err(js_err)?)
                .await
                .map_err(js_err)?;
            Ok(())
        }
    }

    impl FileBackend for OpfsBackend {
        fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
            Ok(opfsReadAt(self.session_id, self.file_kind, offset, len).to_vec())
        }

        fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
            let written =
                opfsWriteAt(self.session_id, self.file_kind, offset, bytes).map_err(js_err)?;
            if written as usize != bytes.len() {
                return Err(EngineError::Storage(format!(
                    "opfs short write: expected {}, got {written}",
                    bytes.len()
                )));
            }
            Ok(())
        }

        fn flush(&mut self) -> Result<()> {
            opfsFlush(self.session_id, self.file_kind);
            Ok(())
        }

        fn len(&self) -> Result<u64> {
            Ok(opfsLen(self.session_id, self.file_kind))
        }

        fn truncate(&mut self, size: u64) -> Result<()> {
            opfsTruncate(self.session_id, self.file_kind, size);
            Ok(())
        }

        fn close(&mut self) -> Result<()> {
            opfsCloseSession(self.session_id);
            Ok(())
        }
    }

    fn files_from_session_value(value: JsValue) -> Result<FileSet<OpfsBackend>> {
        let session: SessionOpenResult = parse_js_value(value, "open OPFS session")?;
        Ok(FileSet::new(
            OpfsBackend::new(session.session_id, MANIFEST_FILE_KIND),
            OpfsBackend::new(session.session_id, MAIN_FILE_KIND),
            OpfsBackend::new(session.session_id, WAL_FILE_KIND),
        ))
    }

    fn parse_js_value<T>(value: JsValue, what: &str) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        serde_wasm_bindgen::from_value(value)
            .map_err(|err| EngineError::Storage(format!("{what}: {err}")))
    }

    fn parse_u64(value: JsValue, what: &str) -> Result<u64> {
        let Some(raw) = value.as_f64() else {
            return Err(EngineError::Storage(format!(
                "{what}: expected numeric result"
            )));
        };
        if !raw.is_finite() || raw < 0.0 {
            return Err(EngineError::Storage(format!(
                "{what}: invalid numeric result {raw}"
            )));
        }
        Ok(raw as u64)
    }

    fn js_err(err: JsValue) -> EngineError {
        let text = if let Some(s) = err.as_string() {
            s
        } else {
            format!("{err:?}")
        };
        EngineError::Storage(text)
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native_impl {
    use super::*;

    const OPFS_WASM_ONLY: &str = "OPFS backend is only available on wasm32";

    fn unsupported_opfs<T>() -> Result<T> {
        Err(EngineError::UnsupportedPlatform(OPFS_WASM_ONLY.into()))
    }

    fn unsupported_operation<T>(operation: &str) -> Result<T> {
        Err(EngineError::UnsupportedPlatform(format!(
            "opfs {operation}"
        )))
    }

    #[derive(Debug, Clone)]
    pub struct OpfsBackend {
        pub session_id: u32,
        pub file_kind: u32,
    }

    impl OpfsBackend {
        pub fn new(session_id: u32, file_kind: u32) -> Self {
            Self {
                session_id,
                file_kind,
            }
        }

        pub async fn open_db(
            _db_name: &str,
            _create_if_missing: bool,
        ) -> Result<FileSet<OpfsBackend>> {
            unsupported_opfs()
        }

        pub async fn open_generation(
            _db_name: &str,
            _generation_name: &str,
            _create_if_missing: bool,
        ) -> Result<FileSet<OpfsBackend>> {
            unsupported_opfs()
        }

        pub async fn prepare_rebuild_target(_db_name: &str) -> Result<String> {
            unsupported_opfs()
        }

        pub async fn swap_active_generation(_db_name: &str, _generation_name: &str) -> Result<()> {
            unsupported_opfs()
        }

        pub async fn cleanup_inactive_entries(_db_name: &str) -> Result<()> {
            unsupported_opfs()
        }

        pub async fn db_directory_size(_db_name: &str) -> Result<u64> {
            unsupported_opfs()
        }

        pub async fn remove_db(_db_name: &str) -> Result<()> {
            unsupported_opfs()
        }
    }

    impl FileBackend for OpfsBackend {
        fn read_at(&self, _offset: u64, _len: usize) -> Result<Vec<u8>> {
            unsupported_operation("read")
        }
        fn write_at(&mut self, _offset: u64, _bytes: &[u8]) -> Result<()> {
            unsupported_operation("write")
        }
        fn flush(&mut self) -> Result<()> {
            unsupported_operation("flush")
        }
        fn len(&self) -> Result<u64> {
            unsupported_operation("len")
        }
        fn truncate(&mut self, _size: u64) -> Result<()> {
            unsupported_operation("truncate")
        }
        fn close(&mut self) -> Result<()> {
            Ok(())
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm_impl::OpfsBackend;

#[cfg(not(target_arch = "wasm32"))]
pub use native_impl::OpfsBackend;
