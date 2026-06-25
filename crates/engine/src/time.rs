use crate::error::{EngineError, Result};

#[cfg(target_arch = "wasm32")]
pub fn now_unix_ms() -> Result<u64> {
    let now = js_sys::Date::now();
    if !now.is_finite() || now < 0.0 {
        return Err(EngineError::Internal(format!(
            "invalid wall-clock timestamp from Date.now(): {now}"
        )));
    }
    Ok(now as u64)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn now_unix_ms() -> Result<u64> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| EngineError::Internal(format!("system clock before unix epoch: {err}")))?;
    u64::try_from(duration.as_millis())
        .map_err(|_| EngineError::Internal("unix millisecond timestamp overflow".into()))
}
