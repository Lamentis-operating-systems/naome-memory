use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("memory core contract error: {0}")]
    Core(#[from] naome_memory_core::MemoryError),
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("integer is outside SQLite's signed 64-bit range: {field}={value}")]
    IntegerOutOfRange { field: &'static str, value: u64 },
    #[error("invalid digest length for {field}: expected 32 bytes, got {actual}")]
    InvalidDigestLength { field: &'static str, actual: usize },
    #[error("unsupported schema version {found}; expected {expected}")]
    UnsupportedSchema { found: u32, expected: u32 },
    #[error("database integrity failure: {0}")]
    Integrity(String),
    #[error("artifact exceeds the 64 MiB limit: {actual} bytes")]
    ArtifactTooLarge { actual: u64 },
    #[error("artifact content does not match its digest: {path}")]
    ArtifactDigestMismatch { path: PathBuf },
    #[error("artifact is missing: {path}")]
    ArtifactMissing { path: PathBuf },
    #[error("artifact path stored in SQLite is invalid: {0}")]
    InvalidArtifactPath(String),
    #[error("stale retirement plan: expected input digest {expected}, current digest {actual}")]
    StalePlan { expected: String, actual: String },
    #[error("retirement plan conflicts with an existing run")]
    RetirementConflict,
    #[error("retirement plan is internally invalid: {0}")]
    InvalidRetirementPlan(String),
    #[error("episode draft conflict: {0}")]
    DraftConflict(String),
    #[error("record already exists with different immutable content: {0}")]
    ImmutableConflict(String),
    #[error("invalid store input: {0}")]
    InvalidInput(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;
