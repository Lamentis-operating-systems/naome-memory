use naome_memory_core::MemoryError;
use thiserror::Error;

/// Lab failures are evidence failures, not successful negative hypotheses.
#[derive(Debug, Error)]
pub enum LabError {
    #[error("core contract rejected the synthetic input: {0}")]
    Core(#[from] MemoryError),
    #[error("JSON encoding failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("SQLite proof store failed: {0}")]
    Store(#[from] naome_memory_sqlite::StoreError),
    #[error("SQLite crash harness failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("proof filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("unknown proof profile: {0}")]
    UnknownProfile(String),
    #[error("proof receipt is invalid: {0}")]
    InvalidReceipt(String),
    #[error("requested scale exceeds the 100,000 atom PoC envelope: {0}")]
    ScaleEnvelope(usize),
}

pub type Result<T> = std::result::Result<T, LabError>;
