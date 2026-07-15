use crate::{
    AtomId, CanonicalBytes as _, ContentStatusV1, Digest32, MemoryError, MemoryKindV1,
    PlannedSemanticAtomV1, PolicyV1, RetentionTierV1, RetirementDecisionV1, RetirementPlanV1,
    RetirementSnapshotV1, Seed32, SemanticCandidateDecisionV1, SemanticCandidateDispositionV1,
    plan_retirement, validate_memory_atom_body,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const RUN_HASH_DOMAIN: &[u8] = b"naome-memory:retirement-run:v1\0";
const RECEIPT_HASH_DOMAIN: &[u8] = b"naome-memory:proof-receipt:v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvariantStatusV1 {
    Verified,
    Rejected,
    Inconclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HypothesisStatusV1 {
    Supported,
    Rejected,
    Inconclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverallProofStatusV1 {
    Passed,
    Failed,
    Inconclusive,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptClosureV1 {
    pub source_atom_ids: Vec<AtomId>,
    pub semantic_atom_ids: Vec<AtomId>,
    pub exact_episode_ids: Vec<AtomId>,
    pub exact_artifact_digests: BTreeMap<AtomId, Vec<Digest32>>,
}

impl ReceiptClosureV1 {
    pub fn from_plan(plan: &RetirementPlanV1) -> Self {
        let source_atom_ids = plan
            .decisions
            .iter()
            .map(|decision| decision.atom_id)
            .collect();
        let semantic_atom_ids = plan
            .semantic_atoms
            .iter()
            .map(|semantic| semantic.atom_id)
            .collect();
        let exact_episode_ids = plan.episodic_winners.clone();
        let exact_artifact_digests = plan
            .decisions
            .iter()
            .filter(|decision| decision.exact_retained)
            .map(|decision| (decision.atom_id, decision.exact_artifact_digests.clone()))
            .collect();
        Self {
            source_atom_ids,
            semantic_atom_ids,
            exact_episode_ids,
            exact_artifact_digests,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofReceiptV1 {
    pub contract_version: String,
    pub receipt_id: Digest32,
    pub run_id: Digest32,
    pub memory_space_id: String,
    pub cohort_day: u64,
    pub as_of_us: u64,
    pub policy_id: String,
    pub policy_digest: Digest32,
    pub seed: Seed32,
    pub input_set_digest: Digest32,
    pub rng_identifier: String,
    pub population_digest: Digest32,
    pub episodic_population_count: usize,
    pub episodic_winner_budget: usize,
    pub episodic_winner_bytes: u64,
    pub semantic_count_budget: usize,
    pub semantic_body_bytes: u64,
    pub plan_digest: Digest32,
    pub decision_digest: Digest32,
    pub state_digest: Digest32,
    pub closure: ReceiptClosureV1,
    pub invariants: BTreeMap<String, InvariantStatusV1>,
    pub overall_status: OverallProofStatusV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ReceiptAuthorityV1<'a> {
    contract_version: &'a str,
    run_id: Digest32,
    memory_space_id: &'a str,
    cohort_day: u64,
    as_of_us: u64,
    policy_id: &'a str,
    policy_digest: Digest32,
    seed: Seed32,
    input_set_digest: Digest32,
    rng_identifier: &'a str,
    population_digest: Digest32,
    episodic_population_count: usize,
    episodic_winner_budget: usize,
    episodic_winner_bytes: u64,
    semantic_count_budget: usize,
    semantic_body_bytes: u64,
    plan_digest: Digest32,
    decision_digest: Digest32,
    state_digest: Digest32,
    closure: &'a ReceiptClosureV1,
    invariants: &'a BTreeMap<String, InvariantStatusV1>,
    overall_status: OverallProofStatusV1,
}

impl ProofReceiptV1 {
    pub fn recompute_receipt_id(&self) -> crate::Result<Digest32> {
        let authority = ReceiptAuthorityV1 {
            contract_version: &self.contract_version,
            run_id: self.run_id,
            memory_space_id: &self.memory_space_id,
            cohort_day: self.cohort_day,
            as_of_us: self.as_of_us,
            policy_id: &self.policy_id,
            policy_digest: self.policy_digest,
            seed: self.seed,
            input_set_digest: self.input_set_digest,
            rng_identifier: &self.rng_identifier,
            population_digest: self.population_digest,
            episodic_population_count: self.episodic_population_count,
            episodic_winner_budget: self.episodic_winner_budget,
            episodic_winner_bytes: self.episodic_winner_bytes,
            semantic_count_budget: self.semantic_count_budget,
            semantic_body_bytes: self.semantic_body_bytes,
            plan_digest: self.plan_digest,
            decision_digest: self.decision_digest,
            state_digest: self.state_digest,
            closure: &self.closure,
            invariants: &self.invariants,
            overall_status: self.overall_status,
        };
        Ok(Digest32::hash_prefixed(
            RECEIPT_HASH_DOMAIN,
            &authority.canonical_bytes()?,
        ))
    }
}

/// Result supplied by a store only after its stale-input check and atomic
/// transaction have completed. The core verifies the closure before issuing a
/// receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyRetirementOutcomeV1 {
    pub observed_input_digest: Digest32,
    pub state_digest: Digest32,
    pub applied_decision_count: usize,
    pub semantic_atom_ids: Vec<AtomId>,
    pub episodic_atom_ids: Vec<AtomId>,
    pub exact_artifact_digests: BTreeMap<AtomId, Vec<Digest32>>,
    pub already_applied: bool,
}

/// I/O implementations own transaction mechanics; this trait is the narrow
/// atomic boundary consumed by the pure proof layer.
pub trait RetirementRepositoryV1 {
    fn apply_plan_atomically(
        &mut self,
        plan: &RetirementPlanV1,
    ) -> crate::Result<ApplyRetirementOutcomeV1>;
}

pub fn apply_retirement<R: RetirementRepositoryV1>(
    repository: &mut R,
    plan: &RetirementPlanV1,
) -> crate::Result<ProofReceiptV1> {
    verify_plan_structure(plan)?;
    let outcome = repository.apply_plan_atomically(plan)?;
    if outcome.observed_input_digest != plan.input_set_digest {
        return Err(MemoryError::StalePlan {
            expected: plan.input_set_digest,
            observed: outcome.observed_input_digest,
        });
    }
    if outcome.state_digest != plan.projected_state_digest {
        return Err(MemoryError::InvalidApplyClosure("state digest mismatch"));
    }
    if outcome.applied_decision_count != plan.decisions.len() {
        return Err(MemoryError::InvalidApplyClosure("decision count mismatch"));
    }
    let expected_closure = ReceiptClosureV1::from_plan(plan);
    if outcome.semantic_atom_ids != expected_closure.semantic_atom_ids
        || outcome.episodic_atom_ids != expected_closure.exact_episode_ids
        || normalize_artifacts(outcome.exact_artifact_digests)
            != expected_closure.exact_artifact_digests
    {
        return Err(MemoryError::InvalidApplyClosure(
            "semantic, episodic, or artifact closure mismatch",
        ));
    }

    let plan_digest = plan.digest()?;
    let decision_digest = plan.decision_digest()?;
    let run_id = Digest32::hash_prefixed(RUN_HASH_DOMAIN, plan_digest.as_bytes());
    let invariants = invariant_map();
    let mut receipt = ProofReceiptV1 {
        contract_version: "proof-receipt-v1".to_owned(),
        receipt_id: Digest32::ZERO,
        run_id,
        memory_space_id: plan.memory_space_id.clone(),
        cohort_day: plan.cohort_day,
        as_of_us: plan.as_of_us,
        policy_id: plan.policy_id.clone(),
        policy_digest: plan.policy_digest,
        seed: plan.seed,
        input_set_digest: plan.input_set_digest,
        rng_identifier: plan.rng_identifier.clone(),
        population_digest: plan.episodic_population_digest,
        episodic_population_count: plan.episodic_population_count,
        episodic_winner_budget: plan.episodic_winner_budget,
        episodic_winner_bytes: plan.episodic_winner_bytes,
        semantic_count_budget: plan.semantic_count_budget,
        semantic_body_bytes: plan.semantic_body_bytes,
        plan_digest,
        decision_digest,
        state_digest: outcome.state_digest,
        closure: expected_closure,
        invariants,
        overall_status: OverallProofStatusV1::Passed,
    };
    receipt.receipt_id = receipt.recompute_receipt_id()?;
    Ok(receipt)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptVerificationInputsV1 {
    pub policy: PolicyV1,
    pub snapshot: RetirementSnapshotV1,
    pub seed: Seed32,
    pub observed_state_digest: Digest32,
    pub observed_exact_artifact_digests: BTreeMap<AtomId, Vec<Digest32>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedReceiptV1 {
    pub receipt_id: Digest32,
    pub run_id: Digest32,
    pub plan_digest: Digest32,
    pub status: InvariantStatusV1,
}

pub fn verify_receipt(
    receipt: &ProofReceiptV1,
    inputs: &ReceiptVerificationInputsV1,
) -> crate::Result<VerifiedReceiptV1> {
    if receipt.contract_version != "proof-receipt-v1" {
        return Err(MemoryError::UnknownVersion(
            receipt.contract_version.clone(),
        ));
    }
    inputs.policy.validate_poc_v1()?;
    let plan = plan_retirement(&inputs.snapshot, &inputs.policy, inputs.seed)?;
    verify_plan_structure(&plan)?;
    let plan_digest = plan.digest()?;
    let expected_run_id = Digest32::hash_prefixed(RUN_HASH_DOMAIN, plan_digest.as_bytes());
    if receipt.receipt_id != receipt.recompute_receipt_id()? {
        return Err(MemoryError::ReceiptRejected("receipt id mismatch"));
    }
    if receipt.run_id != expected_run_id
        || receipt.memory_space_id != plan.memory_space_id
        || receipt.cohort_day != plan.cohort_day
        || receipt.as_of_us != plan.as_of_us
        || receipt.policy_id != plan.policy_id
        || receipt.policy_digest != plan.policy_digest
        || receipt.plan_digest != plan_digest
        || receipt.decision_digest != plan.decision_digest()?
        || receipt.policy_digest != inputs.policy.digest()?
        || receipt.seed != inputs.seed
        || receipt.input_set_digest != plan.input_set_digest
        || receipt.rng_identifier != plan.rng_identifier
        || receipt.population_digest != plan.episodic_population_digest
        || receipt.episodic_population_count != plan.episodic_population_count
        || receipt.episodic_winner_budget != plan.episodic_winner_budget
        || receipt.episodic_winner_bytes != plan.episodic_winner_bytes
        || receipt.semantic_count_budget != plan.semantic_count_budget
        || receipt.semantic_body_bytes != plan.semantic_body_bytes
        || receipt.state_digest != plan.projected_state_digest
        || receipt.state_digest != inputs.observed_state_digest
        || receipt.closure != ReceiptClosureV1::from_plan(&plan)
    {
        return Err(MemoryError::ReceiptRejected(
            "input, policy, seed, decision, state, or closure mismatch",
        ));
    }
    if normalize_artifacts(inputs.observed_exact_artifact_digests.clone())
        != receipt.closure.exact_artifact_digests
    {
        return Err(MemoryError::ReceiptRejected(
            "observed exact artifacts do not close",
        ));
    }
    if receipt.overall_status != OverallProofStatusV1::Passed
        || receipt.invariants != invariant_map()
    {
        return Err(MemoryError::ReceiptRejected(
            "receipt does not claim verified invariants",
        ));
    }
    Ok(VerifiedReceiptV1 {
        receipt_id: receipt.receipt_id,
        run_id: receipt.run_id,
        plan_digest: receipt.plan_digest,
        status: InvariantStatusV1::Verified,
    })
}

pub fn verify_plan_structure(plan: &RetirementPlanV1) -> crate::Result<()> {
    if plan.contract_version != "retirement-plan-v1" {
        return Err(MemoryError::UnknownVersion(plan.contract_version.clone()));
    }
    if plan.policy_id != "poc-v1" {
        return Err(MemoryError::PolicyMismatch);
    }
    let policy = PolicyV1::poc_v1();
    if plan.policy_digest != policy.digest()?
        || plan.rng_identifier != "chacha20-rand_chacha-0.3/partial-fisher-yates-v1"
    {
        return Err(MemoryError::PolicyMismatch);
    }
    verify_projected_post_state(plan, &policy)?;
    if plan.projected_post_state.digest()? != plan.projected_state_digest {
        return Err(MemoryError::ReceiptRejected(
            "projected state digest mismatch",
        ));
    }
    ensure_sorted_unique_decisions(&plan.decisions)?;
    ensure_semantic_evidence(
        &plan.semantic_decisions,
        &plan.semantic_atoms,
        plan.semantic_count_budget,
        &policy,
    )?;
    let winners = plan
        .episodic_winners
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if plan
        .episodic_winners
        .windows(2)
        .any(|pair| pair[0] >= pair[1])
    {
        return Err(MemoryError::ReceiptRejected(
            "episodic winners are not strictly atom-id ordered",
        ));
    }
    let decision_winners = plan
        .decisions
        .iter()
        .filter(|decision| decision.exact_retained)
        .map(|decision| decision.atom_id)
        .collect::<BTreeSet<_>>();
    let expected_episodic_budget = policy
        .episodic_rate
        .ceil_count(plan.episodic_population_count)
        .min(policy.episodic_count_max);
    let expected_semantic_budget = policy
        .semantic_rate
        .ceil_count(plan.decisions.len())
        .min(policy.semantic_count_max);
    let semantic_body_bytes = plan.semantic_atoms.iter().try_fold(0_u64, |total, atom| {
        total
            .checked_add(atom.canonical_size_bytes)
            .ok_or(MemoryError::ReceiptRejected(
                "semantic body byte count overflow",
            ))
    })?;
    if winners != decision_winners
        || plan.episodic_winner_budget != expected_episodic_budget
        || plan.episodic_winners.len() != plan.episodic_winner_budget
        || plan.semantic_count_budget != expected_semantic_budget
        || plan.semantic_body_bytes != semantic_body_bytes
        || plan.episodic_winner_bytes > policy.episodic_pin_budget_bytes
    {
        return Err(MemoryError::ReceiptRejected(
            "lottery or semantic budget mismatch",
        ));
    }
    Ok(())
}

fn verify_projected_post_state(plan: &RetirementPlanV1, policy: &PolicyV1) -> crate::Result<()> {
    let post = &plan.projected_post_state;
    if post.memory_space_id != plan.memory_space_id
        || post.source_atoms.len() != plan.decisions.len()
        || post.semantic_atoms.len() != plan.semantic_atoms.len()
    {
        return Err(MemoryError::ReceiptRejected(
            "projected post-state closure mismatch",
        ));
    }
    for (state, decision) in post.source_atoms.iter().zip(&plan.decisions) {
        let expected_tier = if decision.exact_retained {
            RetentionTierV1::EpisodicLongTerm
        } else {
            RetentionTierV1::HeaderOnly
        };
        let expected_status = if decision.exact_retained {
            ContentStatusV1::Present
        } else {
            ContentStatusV1::Forgotten
        };
        if state.atom_id != decision.atom_id
            || state.atom_kind != MemoryKindV1::Episode
            || state.session_id.as_deref().is_none_or(str::is_empty)
            || state.header_body_digest != decision.atom_id.digest()
            || state.header_body_bytes == 0
            || state.header_body_bytes > policy.atom_hard_max_bytes
            || state.stored_body_digest
                != decision.exact_retained.then_some(decision.atom_id.digest())
            || state.retention_tier != expected_tier
            || state.content_status != expected_status
            || state.expires_at_us.is_some()
            || state.superseded_by.is_some()
            || !state.retirement_run_bound
            || state.search_projection_present != decision.exact_retained
            || state.pinned_artifact_digests != decision.exact_artifact_digests
            || decision.forget_episode_body == decision.exact_retained
            || (decision.exact_retained
                && state.retention_permission
                    != crate::RetentionPermissionV1::SemanticAndExactAllowed)
        {
            return Err(MemoryError::ReceiptRejected(
                "projected source post-state mismatch",
            ));
        }
    }
    for (state, semantic) in post.semantic_atoms.iter().zip(&plan.semantic_atoms) {
        if state.atom_id != semantic.atom_id
            || state.atom_kind != MemoryKindV1::Semantic
            || state.repository_id != semantic.body.scope.repository_id
            || state.task_id != semantic.body.scope.task_id
            || state.agent_id != semantic.body.scope.agent_id
            || state.session_id.as_deref() != Some(semantic.body.scope.session_id.as_str())
            || state.recorded_at_us != semantic.body.interval.recorded_at_us
            || state.header_body_digest != semantic.body_digest
            || state.header_body_bytes != semantic.canonical_size_bytes
            || state.retention_permission != semantic.body.retention_permission
            || state.provenance_digest
                != Digest32::hash_prefixed(
                    b"naome-memory:provenance:v1\0",
                    &semantic.body.provenance.canonical_bytes()?,
                )
            || state.stored_body_digest != Some(semantic.body_digest)
            || state.retention_tier != RetentionTierV1::SemanticLongTerm
            || state.content_status != ContentStatusV1::Present
            || state.expires_at_us.is_some()
            || state.superseded_by.is_some()
            || state.feedback_positive != 0
            || state.feedback_negative != 0
            || state.retrieval_count != 0
            || state.retirement_run_bound
            || !state.search_projection_present
            || !state.pinned_artifact_digests.is_empty()
        {
            return Err(MemoryError::ReceiptRejected(
                "projected semantic post-state mismatch",
            ));
        }
    }
    Ok(())
}

fn ensure_sorted_unique_decisions(decisions: &[RetirementDecisionV1]) -> crate::Result<()> {
    for pair in decisions.windows(2) {
        if pair[0].atom_id >= pair[1].atom_id {
            return Err(MemoryError::ReceiptRejected(
                "decisions are not strictly atom-id ordered",
            ));
        }
    }
    Ok(())
}

fn ensure_semantic_atoms(
    semantic_atoms: &[PlannedSemanticAtomV1],
    policy: &PolicyV1,
) -> crate::Result<()> {
    let mut atom_ids = BTreeSet::new();
    for semantic in semantic_atoms {
        if !atom_ids.insert(semantic.atom_id) {
            return Err(MemoryError::ReceiptRejected(
                "semantic atom id occurs more than once",
            ));
        }
        validate_memory_atom_body(&semantic.body, policy)?;
        if semantic.body.contract_version != "atom-v1"
            || semantic.body.atom_id()? != semantic.atom_id
            || semantic.body_digest != semantic.atom_id.digest()
            || semantic.body.encoded_size_bytes()? != semantic.canonical_size_bytes
        {
            return Err(MemoryError::ReceiptRejected(
                "semantic atom authority mismatch",
            ));
        }
    }
    Ok(())
}

fn ensure_semantic_evidence(
    decisions: &[SemanticCandidateDecisionV1],
    semantic_atoms: &[PlannedSemanticAtomV1],
    count_budget: usize,
    policy: &PolicyV1,
) -> crate::Result<()> {
    ensure_semantic_atoms(semantic_atoms, policy)?;
    let mut candidate_ids = BTreeSet::new();
    for decision in decisions {
        if !candidate_ids.insert(decision.atom_id) {
            return Err(MemoryError::ReceiptRejected(
                "semantic candidate id occurs more than once",
            ));
        }
        if decision.source_ids.len() < policy.min_semantic_sources
            || decision
                .source_ids
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
        {
            return Err(MemoryError::ReceiptRejected(
                "semantic candidate source closure is incomplete or unordered",
            ));
        }
    }
    for pair in decisions.windows(2) {
        if pair[0].score.total < pair[1].score.total
            || (pair[0].score.total == pair[1].score.total && pair[0].atom_id >= pair[1].atom_id)
        {
            return Err(MemoryError::ReceiptRejected(
                "semantic candidates are not ordered by score then atom id",
            ));
        }
    }

    let mut selected_count = 0_usize;
    let mut selected_bytes = 0_u64;
    let mut selected_index = 0_usize;
    for decision in decisions {
        let (expected, next_bytes) = expected_semantic_disposition(
            decision.canonical_size_bytes,
            selected_count,
            selected_bytes,
            count_budget,
            policy,
        )?;
        if decision.disposition != expected {
            return Err(MemoryError::ReceiptRejected(
                "semantic candidate disposition does not follow policy budgets",
            ));
        }
        if expected == SemanticCandidateDispositionV1::Selected {
            let semantic =
                semantic_atoms
                    .get(selected_index)
                    .ok_or(MemoryError::ReceiptRejected(
                        "selected semantic candidate body is missing",
                    ))?;
            let payload = match &semantic.body.payload {
                crate::MemoryPayloadV1::Semantic(payload) => payload,
                crate::MemoryPayloadV1::Episode(_) => {
                    return Err(MemoryError::ReceiptRejected(
                        "selected semantic candidate has episode payload",
                    ));
                }
            };
            if semantic.atom_id != decision.atom_id
                || semantic.canonical_size_bytes != decision.canonical_size_bytes
                || payload.source_ids != decision.source_ids
                || payload.score != decision.score
            {
                return Err(MemoryError::ReceiptRejected(
                    "selected semantic candidate does not match its evidence",
                ));
            }
            selected_count = selected_count.saturating_add(1);
            selected_bytes = next_bytes;
            selected_index = selected_index.saturating_add(1);
        }
    }
    if selected_index != semantic_atoms.len() {
        return Err(MemoryError::ReceiptRejected(
            "semantic atom exists without selected candidate evidence",
        ));
    }
    Ok(())
}

fn expected_semantic_disposition(
    candidate_bytes: u64,
    selected_count: usize,
    selected_bytes: u64,
    count_budget: usize,
    policy: &PolicyV1,
) -> crate::Result<(SemanticCandidateDispositionV1, u64)> {
    let next_bytes =
        selected_bytes
            .checked_add(candidate_bytes)
            .ok_or(MemoryError::ReceiptRejected(
                "semantic candidate byte count overflow",
            ))?;
    let disposition = if candidate_bytes > policy.atom_hard_max_bytes {
        SemanticCandidateDispositionV1::SkippedAtomHardLimit
    } else if selected_count >= count_budget {
        SemanticCandidateDispositionV1::SkippedCountBudget
    } else if next_bytes > policy.semantic_body_budget_bytes {
        SemanticCandidateDispositionV1::SkippedByteBudget
    } else {
        SemanticCandidateDispositionV1::Selected
    };
    Ok((disposition, next_bytes))
}

fn invariant_map() -> BTreeMap<String, InvariantStatusV1> {
    [
        "budgets_structurally_bounded",
        "plan_structure_verified",
        "repository_outcome_closure_verified",
    ]
    .into_iter()
    .map(|name| (name.to_owned(), InvariantStatusV1::Verified))
    .collect()
}

fn normalize_artifacts(
    mut artifacts: BTreeMap<AtomId, Vec<Digest32>>,
) -> BTreeMap<AtomId, Vec<Digest32>> {
    for digests in artifacts.values_mut() {
        digests.sort_unstable();
        digests.dedup();
    }
    artifacts
}

#[cfg(test)]
mod mutation_tests {
    use super::*;
    use crate::{
        ConsolidationScoreV1, FormationSignalsV1, MemoryAtomBodyV1, MemoryPayloadV1,
        MemoryRelationKindV1, MemoryRelationV1, MemoryScopeV1, Ppm, ProvenanceV1,
        RetentionPermissionV1, SealReasonV1, SemanticPayloadV1, TimeIntervalV1,
    };

    fn test_atom_id(label: &[u8]) -> AtomId {
        AtomId(Digest32::hash_prefixed(
            b"naome-memory:receipt-mutation-test:v1\0",
            label,
        ))
    }

    fn source_ids(namespace: u8) -> Vec<AtomId> {
        let mut ids = (0_u8..3)
            .map(|index| test_atom_id(&[namespace, index]))
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids
    }

    fn score(total: u32) -> ConsolidationScoreV1 {
        ConsolidationScoreV1 {
            support: Ppm::ZERO,
            diversity: Ppm::ZERO,
            utility: Ppm::ZERO,
            salience: Ppm::ZERO,
            cohesion: Ppm::ZERO,
            novelty: Ppm::ZERO,
            uncertainty: Ppm::ZERO,
            total: Ppm::from_raw_unchecked(total),
        }
    }

    fn planned_semantic(index: u8) -> crate::Result<PlannedSemanticAtomV1> {
        let policy = PolicyV1::poc_v1();
        let sources = source_ids(index);
        let semantic_score = score(700_000);
        let body = MemoryAtomBodyV1 {
            contract_version: "atom-v1".to_owned(),
            scope: MemoryScopeV1 {
                memory_space_id: "space-a".to_owned(),
                repository_id: Some("repo-a".to_owned()),
                task_id: Some("task-a".to_owned()),
                agent_id: Some("agent-a".to_owned()),
                session_id: format!("semantic-{index}"),
            },
            interval: TimeIntervalV1 {
                started_at_us: 1,
                ended_at_us: 2,
                recorded_at_us: 3,
            },
            trigger: "semantic".to_owned(),
            seal_reason: SealReasonV1::Checkpoint,
            goal: Some(format!("semantic-{index}")),
            plan: Vec::new(),
            internal_state_before: BTreeMap::new(),
            internal_state_after: BTreeMap::new(),
            observations: Vec::new(),
            interpretations: Vec::new(),
            beliefs: Vec::new(),
            decisions: Vec::new(),
            rejected_alternatives: Vec::new(),
            actions: Vec::new(),
            outcome: None,
            feedback: Vec::new(),
            formation_signals: FormationSignalsV1::default(),
            topic_keys: BTreeSet::new(),
            entity_ids: BTreeSet::new(),
            outcome_class: None,
            goal_key: None,
            provenance: ProvenanceV1 {
                producer: "receipt-mutation-test".to_owned(),
                source_event_digests: sources.iter().map(|source| source.digest()).collect(),
                policy_digest: policy.digest()?,
            },
            relations: sources
                .iter()
                .copied()
                .map(|target| MemoryRelationV1 {
                    kind: MemoryRelationKindV1::DerivedFrom,
                    target,
                })
                .collect(),
            artifacts: Vec::new(),
            retention_permission: RetentionPermissionV1::SemanticAllowed,
            payload: MemoryPayloadV1::Semantic(SemanticPayloadV1 {
                source_body_digests: sources
                    .iter()
                    .copied()
                    .map(|source_id| (source_id, source_id.digest()))
                    .collect(),
                source_ids: sources,
                score: semantic_score,
                consensus_numerator: policy.consensus_numerator,
                consensus_denominator: policy.consensus_denominator,
                supersedes: None,
            }),
        };
        validate_memory_atom_body(&body, &policy)?;
        let atom_id = body.atom_id()?;
        Ok(PlannedSemanticAtomV1 {
            atom_id,
            body_digest: atom_id.digest(),
            canonical_size_bytes: body.encoded_size_bytes()?,
            body,
        })
    }

    fn decision_for(
        semantic: &PlannedSemanticAtomV1,
        disposition: SemanticCandidateDispositionV1,
    ) -> SemanticCandidateDecisionV1 {
        let MemoryPayloadV1::Semantic(payload) = &semantic.body.payload else {
            return SemanticCandidateDecisionV1 {
                atom_id: semantic.atom_id,
                source_ids: Vec::new(),
                score: score(0),
                canonical_size_bytes: semantic.canonical_size_bytes,
                disposition,
            };
        };
        SemanticCandidateDecisionV1 {
            atom_id: semantic.atom_id,
            source_ids: payload.source_ids.clone(),
            score: payload.score,
            canonical_size_bytes: semantic.canonical_size_bytes,
            disposition,
        }
    }

    fn skipped_decision(
        atom_id: AtomId,
        total: u32,
        policy: &PolicyV1,
    ) -> SemanticCandidateDecisionV1 {
        SemanticCandidateDecisionV1 {
            atom_id,
            source_ids: source_ids(7),
            score: score(total),
            canonical_size_bytes: policy.atom_hard_max_bytes.saturating_add(1),
            disposition: SemanticCandidateDispositionV1::SkippedAtomHardLimit,
        }
    }

    #[test]
    fn semantic_atom_authority_checks_each_field_independently() -> crate::Result<()> {
        let policy = PolicyV1::poc_v1();
        let semantic = planned_semantic(1)?;
        assert_eq!(
            ensure_semantic_atoms(std::slice::from_ref(&semantic), &policy),
            Ok(())
        );

        let mut wrong_policy = semantic.clone();
        wrong_policy.body.provenance.policy_digest = Digest32::ZERO;
        wrong_policy.atom_id = wrong_policy.body.atom_id()?;
        wrong_policy.body_digest = wrong_policy.atom_id.digest();
        wrong_policy.canonical_size_bytes = wrong_policy.body.encoded_size_bytes()?;
        assert_eq!(
            ensure_semantic_atoms(&[wrong_policy], &policy),
            Err(MemoryError::PolicyMismatch)
        );

        let mut wrong_contract = semantic.clone();
        wrong_contract.body.contract_version = "atom-v2".to_owned();
        wrong_contract.atom_id = wrong_contract.body.atom_id()?;
        wrong_contract.body_digest = wrong_contract.atom_id.digest();
        wrong_contract.canonical_size_bytes = wrong_contract.body.encoded_size_bytes()?;
        assert!(matches!(
            ensure_semantic_atoms(&[wrong_contract], &policy),
            Err(MemoryError::UnknownVersion(_))
        ));

        let mut wrong_body_authority = semantic.clone();
        wrong_body_authority.body.trigger = "episodic".to_owned();

        let mut wrong_digest = semantic.clone();
        wrong_digest.body_digest = Digest32::ZERO;

        let mut wrong_size = semantic;
        wrong_size.canonical_size_bytes = wrong_size.canonical_size_bytes.saturating_add(1);

        for changed in [wrong_body_authority, wrong_digest, wrong_size] {
            assert_eq!(
                ensure_semantic_atoms(&[changed], &policy),
                Err(MemoryError::ReceiptRejected(
                    "semantic atom authority mismatch"
                ))
            );
        }
        Ok(())
    }

    #[test]
    fn semantic_candidate_ordering_covers_score_and_atom_ties() {
        let policy = PolicyV1::poc_v1();
        let mut atom_ids = [test_atom_id(b"candidate-a"), test_atom_id(b"candidate-b")];
        atom_ids.sort_unstable();
        let [low, high] = atom_ids;

        let descending_score = [
            skipped_decision(high, 700_000, &policy),
            skipped_decision(low, 600_000, &policy),
        ];
        assert_eq!(
            ensure_semantic_evidence(&descending_score, &[], 2, &policy),
            Ok(())
        );

        let increasing_score = [
            skipped_decision(low, 600_000, &policy),
            skipped_decision(high, 700_000, &policy),
        ];
        assert_eq!(
            ensure_semantic_evidence(&increasing_score, &[], 2, &policy),
            Err(MemoryError::ReceiptRejected(
                "semantic candidates are not ordered by score then atom id"
            ))
        );

        let equal_score_ordered = [
            skipped_decision(low, 600_000, &policy),
            skipped_decision(high, 600_000, &policy),
        ];
        assert_eq!(
            ensure_semantic_evidence(&equal_score_ordered, &[], 2, &policy),
            Ok(())
        );

        let equal_score_reversed = [
            skipped_decision(high, 600_000, &policy),
            skipped_decision(low, 600_000, &policy),
        ];
        assert_eq!(
            ensure_semantic_evidence(&equal_score_reversed, &[], 2, &policy),
            Err(MemoryError::ReceiptRejected(
                "semantic candidates are not ordered by score then atom id"
            ))
        );
    }

    #[test]
    fn semantic_disposition_covers_all_exact_policy_boundaries() {
        let policy = PolicyV1::poc_v1();
        assert_eq!(
            expected_semantic_disposition(policy.atom_hard_max_bytes, 0, 0, 1, &policy),
            Ok((
                SemanticCandidateDispositionV1::Selected,
                policy.atom_hard_max_bytes
            ))
        );
        assert_eq!(
            expected_semantic_disposition(
                policy.atom_hard_max_bytes.saturating_add(1),
                0,
                0,
                1,
                &policy,
            ),
            Ok((
                SemanticCandidateDispositionV1::SkippedAtomHardLimit,
                policy.atom_hard_max_bytes.saturating_add(1)
            ))
        );
        assert_eq!(
            expected_semantic_disposition(1, 1, 0, 1, &policy),
            Ok((SemanticCandidateDispositionV1::SkippedCountBudget, 1))
        );
        let selected_bytes = policy
            .semantic_body_budget_bytes
            .saturating_sub(policy.atom_hard_max_bytes);
        assert_eq!(
            expected_semantic_disposition(
                policy.atom_hard_max_bytes,
                0,
                selected_bytes,
                1,
                &policy,
            ),
            Ok((
                SemanticCandidateDispositionV1::Selected,
                policy.semantic_body_budget_bytes
            ))
        );
        assert_eq!(
            expected_semantic_disposition(
                policy.atom_hard_max_bytes,
                0,
                selected_bytes.saturating_add(1),
                1,
                &policy,
            ),
            Ok((
                SemanticCandidateDispositionV1::SkippedByteBudget,
                policy.semantic_body_budget_bytes.saturating_add(1)
            ))
        );
        assert_eq!(
            expected_semantic_disposition(1, 0, u64::MAX, 1, &policy),
            Err(MemoryError::ReceiptRejected(
                "semantic candidate byte count overflow"
            ))
        );
    }

    #[test]
    fn selected_semantic_candidate_matches_every_authority_field() -> crate::Result<()> {
        let policy = PolicyV1::poc_v1();
        let semantic = planned_semantic(2)?;
        let decision = decision_for(&semantic, SemanticCandidateDispositionV1::Selected);
        assert_eq!(
            ensure_semantic_evidence(
                std::slice::from_ref(&decision),
                std::slice::from_ref(&semantic),
                1,
                &policy,
            ),
            Ok(())
        );

        let mut wrong_atom = decision.clone();
        wrong_atom.atom_id = test_atom_id(b"wrong-candidate");
        let mut wrong_size = decision.clone();
        wrong_size.canonical_size_bytes = wrong_size.canonical_size_bytes.saturating_add(1);
        let mut wrong_sources = decision.clone();
        wrong_sources.source_ids = source_ids(99);
        let mut wrong_score = decision;
        wrong_score.score.total = Ppm::from_raw_unchecked(699_999);

        for changed in [wrong_atom, wrong_size, wrong_sources, wrong_score] {
            assert_eq!(
                ensure_semantic_evidence(&[changed], std::slice::from_ref(&semantic), 1, &policy),
                Err(MemoryError::ReceiptRejected(
                    "selected semantic candidate does not match its evidence"
                ))
            );
        }
        Ok(())
    }
}
