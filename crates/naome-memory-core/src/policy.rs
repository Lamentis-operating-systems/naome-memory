use crate::{CanonicalBytes as _, Digest32, MemoryError, Ppm, Result};
use serde::{Deserialize, Serialize};

pub const POLICY_HASH_DOMAIN: &[u8] = b"naome-memory:policy:v1\0";

/// The committed v0.1 policy. There is intentionally no merge/override API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyV1 {
    pub policy_id: String,
    pub contract_version: String,
    pub stm_horizon_us: u64,
    pub max_cohort_atoms: usize,
    pub atom_target_bytes: u64,
    pub atom_hard_max_bytes: u64,
    pub inline_payload_max_bytes: u64,
    pub artifact_max_bytes: u64,
    pub exact_package_max_bytes: u64,
    pub similarity_topic_weight: Ppm,
    pub similarity_entity_weight: Ppm,
    pub similarity_outcome_weight: Ppm,
    pub cluster_similarity_threshold: Ppm,
    pub min_semantic_sources: usize,
    pub min_semantic_sessions: usize,
    pub support_saturation_atoms: usize,
    pub consolidation_support_weight: Ppm,
    pub consolidation_diversity_weight: Ppm,
    pub consolidation_utility_weight: Ppm,
    pub consolidation_salience_weight: Ppm,
    pub consolidation_cohesion_weight: Ppm,
    pub consolidation_novelty_weight: Ppm,
    pub consolidation_uncertainty_weight: Ppm,
    pub consolidation_threshold: Ppm,
    pub semantic_rate: Ppm,
    pub semantic_count_max: usize,
    pub semantic_body_budget_bytes: u64,
    pub consensus_numerator: u32,
    pub consensus_denominator: u32,
    pub episodic_rate: Ppm,
    pub episodic_count_max: usize,
    pub episodic_pin_budget_bytes: u64,
    pub retrieval_candidate_max: usize,
    pub retrieval_default_k: usize,
    pub retrieval_max_k: usize,
    pub retrieval_lexical_weight: Ppm,
    pub retrieval_semantic_weight: Ppm,
    pub retrieval_entity_weight: Ppm,
    pub retrieval_utility_weight: Ppm,
    pub retrieval_recency_weight: Ppm,
    pub retrieval_recency_horizon_us: u64,
    pub flashback_budget_max: usize,
}

impl PolicyV1 {
    pub fn poc_v1() -> Self {
        const DAY_US: u64 = 24 * 60 * 60 * 1_000_000;
        Self {
            policy_id: "poc-v1".to_owned(),
            contract_version: "policy-v1".to_owned(),
            stm_horizon_us: 7 * DAY_US,
            max_cohort_atoms: 100_000,
            atom_target_bytes: 128 * 1024,
            atom_hard_max_bytes: 256 * 1024,
            inline_payload_max_bytes: 4 * 1024,
            artifact_max_bytes: 64 * 1024 * 1024,
            exact_package_max_bytes: 64 * 1024 * 1024,
            similarity_topic_weight: Ppm::from_raw_unchecked(600_000),
            similarity_entity_weight: Ppm::from_raw_unchecked(250_000),
            similarity_outcome_weight: Ppm::from_raw_unchecked(150_000),
            cluster_similarity_threshold: Ppm::from_raw_unchecked(650_000),
            min_semantic_sources: 3,
            min_semantic_sessions: 2,
            support_saturation_atoms: 10,
            consolidation_support_weight: Ppm::from_raw_unchecked(250_000),
            consolidation_diversity_weight: Ppm::from_raw_unchecked(200_000),
            consolidation_utility_weight: Ppm::from_raw_unchecked(200_000),
            consolidation_salience_weight: Ppm::from_raw_unchecked(150_000),
            consolidation_cohesion_weight: Ppm::from_raw_unchecked(100_000),
            consolidation_novelty_weight: Ppm::from_raw_unchecked(100_000),
            consolidation_uncertainty_weight: Ppm::from_raw_unchecked(150_000),
            consolidation_threshold: Ppm::from_raw_unchecked(550_000),
            semantic_rate: Ppm::from_raw_unchecked(20_000),
            semantic_count_max: 100,
            semantic_body_budget_bytes: 8 * 1024 * 1024,
            consensus_numerator: 2,
            consensus_denominator: 3,
            episodic_rate: Ppm::from_raw_unchecked(5_000),
            episodic_count_max: 25,
            episodic_pin_budget_bytes: 256 * 1024 * 1024,
            retrieval_candidate_max: 512,
            retrieval_default_k: 10,
            retrieval_max_k: 100,
            retrieval_lexical_weight: Ppm::from_raw_unchecked(450_000),
            retrieval_semantic_weight: Ppm::from_raw_unchecked(250_000),
            retrieval_entity_weight: Ppm::from_raw_unchecked(150_000),
            retrieval_utility_weight: Ppm::from_raw_unchecked(100_000),
            retrieval_recency_weight: Ppm::from_raw_unchecked(50_000),
            retrieval_recency_horizon_us: 365 * DAY_US,
            flashback_budget_max: 3,
        }
    }

    pub fn digest(&self) -> Result<Digest32> {
        Ok(Digest32::hash_prefixed(
            POLICY_HASH_DOMAIN,
            &self.canonical_bytes()?,
        ))
    }

    pub fn validate_poc_v1(&self) -> Result<()> {
        let canonical = Self::poc_v1();
        if self != &canonical {
            return Err(MemoryError::PolicyMismatch);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::PolicyV1;

    #[test]
    fn policy_is_hash_bound_and_not_runtime_overridable() {
        let policy = PolicyV1::poc_v1();
        let original = policy.digest();
        let mut modified = policy;
        modified.semantic_count_max = 101;
        assert_ne!(original, modified.digest());
        assert!(modified.validate_poc_v1().is_err());
    }
}
