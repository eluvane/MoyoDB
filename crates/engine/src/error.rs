use thiserror::Error;

pub type Result<T> = std::result::Result<T, EngineError>;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum EngineError {
    #[error("unsupported platform: {0}")]
    UnsupportedPlatform(String),
    #[error("database busy: {0}")]
    DatabaseBusy(String),
    #[error("database corruption: {0}")]
    Corruption(String),
    #[error("store already exists: {0}")]
    StoreExists(String),
    #[error("store not found: {0}")]
    StoreNotFound(String),
    #[error("write transaction already open")]
    WriteTransactionAlreadyOpen,
    #[error("readonly transaction cannot commit")]
    ReadonlyTransaction,
    #[error("transaction is already closed")]
    TransactionClosed,
    #[error("value too large: {0} bytes")]
    ValueTooLarge(usize),
    #[error("key too large: {0} bytes")]
    KeyTooLarge(usize),
    #[error("store name too long: {0} bytes")]
    StoreNameTooLong(usize),
    #[error("reserved store name: {0}")]
    ReservedStoreName(String),
    #[error("invalid range: {0}")]
    InvalidRange(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("failpoint injected: {0}")]
    InjectedFailure(String),
    #[error("change feed compacted: {0}")]
    ChangeFeedCompacted(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl EngineError {
    pub fn code(&self) -> &'static str {
        match self {
            EngineError::UnsupportedPlatform(_) => "UnsupportedPlatformError",
            EngineError::DatabaseBusy(_) => "DatabaseBusyError",
            EngineError::Corruption(_) => "CorruptionError",
            EngineError::StoreExists(_) => "StoreExistsError",
            EngineError::StoreNotFound(_) => "StoreNotFoundError",
            EngineError::WriteTransactionAlreadyOpen => "WriteTransactionAlreadyOpenError",
            EngineError::ReadonlyTransaction => "ReadonlyTransactionError",
            EngineError::TransactionClosed => "TransactionClosedError",
            EngineError::ValueTooLarge(_) => "ValueTooLargeError",
            EngineError::KeyTooLarge(_) => "KeyTooLargeError",
            EngineError::StoreNameTooLong(_) => "StoreNameTooLongError",
            EngineError::ReservedStoreName(_) => "ReservedStoreNameError",
            EngineError::InvalidRange(_) => "InvalidRangeError",
            EngineError::Storage(_) => "StorageError",
            EngineError::Serialization(_) => "SerializationError",
            EngineError::InjectedFailure(_) => "InjectedFailureError",
            EngineError::ChangeFeedCompacted(_) => "ChangeFeedCompactedError",
            EngineError::Internal(_) => "InternalError",
        }
    }
}
