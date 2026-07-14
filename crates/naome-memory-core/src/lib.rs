//! Pure, deterministic memory contracts and algorithms.
//!
//! This crate intentionally performs no I/O. Time, policy, candidates, and
//! randomness are explicit inputs so callers can reproduce every decision.

#![forbid(unsafe_code)]
// Public fallibility is expressed by the stable `MemoryError` taxonomy. These
// narrowly scoped style allowances keep the algorithms readable while all
// correctness-oriented Clippy groups remain denied by the crate manifest.
#![allow(
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::return_self_not_must_use,
    clippy::too_many_lines
)]

pub mod canonical;
pub mod error;
pub mod fixed;
pub mod model;
pub mod policy;
pub mod receipt;
pub mod retirement;
pub mod retrieval;
pub mod seal;
pub mod validation;

pub use canonical::{CanonicalBytes, Digest32};
pub use error::{MemoryError, Result};
pub use fixed::Ppm;
pub use model::*;
pub use policy::*;
pub use receipt::*;
pub use retirement::*;
pub use retrieval::*;
pub use seal::*;
pub use validation::*;
