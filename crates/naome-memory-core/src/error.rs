use crate::{AtomId, Digest32};
use thiserror::Error;

/// Errors are deliberately stable enough for the CLI to map them to its
/// documented exit-code classes without inspecting display text.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MemoryError {
    #[error("value {value} is outside the valid ppm range")]
    InvalidPpm { value: u32 },
    #[error("canonical encoding failed: {0}")]
    CanonicalEncoding(String),
    #[error("invalid 32-byte hexadecimal digest")]
    InvalidDigest,
    #[error("required field is empty: {0}")]
    EmptyField(&'static str),
    #[error("episode has no events")]
    EmptyEpisode,
    #[error("event sequence is not contiguous: expected {expected}, found {found}")]
    NonContiguousEvents { expected: u64, found: u64 },
    #[error("event {sequence} belongs to a different scope")]
    EventScopeMismatch { sequence: u64 },
    #[error("event {sequence} lies outside the episode interval")]
    EventTimeOutOfRange { sequence: u64 },
    #[error(
        "event time is not monotonic at sequence {sequence}: previous {previous_at_us}, found {found_at_us}"
    )]
    NonMonotonicEventTime {
        sequence: u64,
        previous_at_us: u64,
        found_at_us: u64,
    },
    #[error("invalid time interval")]
    InvalidTimeInterval,
    #[error("episode has no substantive observation, action, decision, outcome, or feedback")]
    InsufficientEpisodeContent,
    #[error("inline payload is {actual} bytes; the maximum is {maximum}")]
    InlinePayloadTooLarge { actual: u64, maximum: u64 },
    #[error("artifact is {actual} bytes; the maximum is {maximum}")]
    ArtifactTooLarge { actual: u64, maximum: u64 },
    #[error("inline artifact bytes do not match declared digest {digest}")]
    ArtifactDigestMismatch { digest: Digest32 },
    #[error("exact episodic package is {actual} bytes; the maximum is {maximum}")]
    ExactPackageTooLarge { actual: u64, maximum: u64 },
    #[error("atom body is {actual} bytes; the hard maximum is {maximum}")]
    AtomTooLarge { actual: u64, maximum: u64 },
    #[error("invalid atom-v1 body: {0}")]
    InvalidAtomBody(&'static str),
    #[error("observation references undeclared artifact {digest}")]
    UndeclaredArtifactReference { digest: Digest32 },
    #[error(
        "episode body is {actual} bytes; use deterministic episode chaining at target {target} bytes"
    )]
    EpisodeRequiresSplit { actual: u64, target: u64 },
    #[error("candidate {atom_id} has an invalid body digest")]
    AtomDigestMismatch { atom_id: AtomId },
    #[error("candidate {atom_id} occurs more than once")]
    DuplicateAtom { atom_id: AtomId },
    #[error("candidate {atom_id} is not eligible for this retirement cohort: {reason}")]
    InvalidRetirementCandidate {
        atom_id: AtomId,
        reason: &'static str,
    },
    #[error("candidate {atom_id} is not a valid retrievable memory: {reason}")]
    InvalidRetrievalCandidate {
        atom_id: AtomId,
        reason: &'static str,
    },
    #[error("memory space isolation violation")]
    MemorySpaceMismatch,
    #[error("cohort contains {actual} atoms; the maximum is {maximum}")]
    CohortTooLarge { actual: usize, maximum: usize },
    #[error("retirement cohort is empty")]
    EmptyRetirementCohort,
    #[error("retirement cohort is not closed: as_of_us={as_of_us}, cohort_end_us={cohort_end_us}")]
    RetirementCohortNotClosed { as_of_us: u64, cohort_end_us: u64 },
    #[error("policy identifier or digest does not match poc-v1")]
    PolicyMismatch,
    #[error("retirement input digest is stale: expected {expected}, observed {observed}")]
    StalePlan {
        expected: Digest32,
        observed: Digest32,
    },
    #[error("episodic lottery winners require {actual} bytes; budget is {maximum}")]
    EpisodicBudgetExceeded { actual: u64, maximum: u64 },
    #[error("retrieval requested k={requested}; the maximum is {maximum}")]
    RetrievalLimitExceeded { requested: usize, maximum: usize },
    #[error("retrieval candidate generation returned {actual}; the maximum is {maximum}")]
    CandidateLimitExceeded { actual: usize, maximum: usize },
    #[error("spontaneous recall budget must be in 1..={maximum}")]
    InvalidFlashbackBudget { maximum: usize },
    #[error("repository rejected retirement: {0}")]
    RepositoryRejected(String),
    #[error("repository returned an invalid retirement closure: {0}")]
    InvalidApplyClosure(&'static str),
    #[error("receipt verification failed: {0}")]
    ReceiptRejected(&'static str),
    #[error("unknown contract version: {0}")]
    UnknownVersion(String),
}

pub type Result<T> = std::result::Result<T, MemoryError>;
