#![forbid(unsafe_code)]
#![allow(
    clippy::missing_errors_doc,
    reason = "all public fallible APIs return the documented typed StoreError boundary"
)]
#![allow(
    clippy::too_many_arguments,
    reason = "persistence boundaries name every committed field explicitly"
)]
#![allow(
    clippy::too_many_lines,
    reason = "transaction procedures stay linear so their commit closure remains auditable"
)]

mod artifact;
mod error;
mod migration;
mod repository;

pub use artifact::{
    ArtifactDigest, ArtifactRecord, ArtifactStore, GcAction, GcEntry, GcReport, MAX_ARTIFACT_BYTES,
};
pub use error::{Result, StoreError};
pub use repository::{
    AtomBatchInsertOutcome, CohortKey, CohortSnapshot, DraftEvent, IntegrityReport,
    MAX_ATOM_INSERT_BATCH, RepositoryFaultPointV1, SearchCandidate, SearchScope, SnapshotAtom,
    SqliteRepository, StoreConfig,
};

pub const SCHEMA_VERSION: u32 = 1;
