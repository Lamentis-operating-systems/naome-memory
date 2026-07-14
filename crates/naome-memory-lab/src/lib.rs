//! Deterministic synthetic proof harness for NAOME Memory.
//!
//! The lab is deliberately separate from the decision core. It provides a
//! slower reference surface, frozen synthetic worlds, integer-only metrics,
//! and receipts that distinguish mechanical invariants from empirical claims.

#![forbid(unsafe_code)]

mod crash;
mod error;
mod metrics;
mod proof;
mod synthetic;

pub use crash::{
    crash_child_requested, crash_harness_source_digest, run_crash_campaign, run_crash_child,
};
pub use error::{LabError, Result};
pub use metrics::{SignedPpm, bootstrap_lower_95, ndcg_at_10};
pub use proof::{
    ByteBudgetEvidenceV1, ClaimStatusV1, CrashCampaignEvidenceV1, CrashScenarioEvidenceV1,
    DecisionLedgerEntryV1, DecisionLedgerV1, GoldenFateScenarioV1, HypothesisStatusV1, LabMetricV1,
    LabProofBundleV1, LabProofReceiptV1, MetricComparatorV1, OverallStatusV1, ProofProfileV1,
    QueryEvaluationV1, run_proof, run_proof_with_crash_evidence, validate_crash_campaign_evidence,
    verify_lab_bundle, verify_lab_receipt,
};
pub use synthetic::{
    DatasetIdV1, SyntheticWorldV1, derive_dataset_world_seed, derive_world_seed, generate_world,
    generate_world_for_dataset, rare_event_key, rare_query_id, world_id,
};

/// Proof harness contract version.
pub const LAB_CONTRACT_VERSION: &str = "lab-proof-receipt-v1";
