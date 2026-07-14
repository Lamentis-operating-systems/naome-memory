//! Independent receipt projection for the test-only reference plan.

use std::collections::BTreeMap;

use naome_memory_core::{
    AtomId, CanonicalBytes as _, Digest32, InvariantStatusV1, MemoryError, OverallProofStatusV1,
    ProofReceiptV1, ReceiptClosureV1, ReceiptVerificationInputsV1, VerifiedReceiptV1,
};
use serde::Serialize;

use super::reference_retirement::ReferenceRetirementContractV1;

const PLAN_HASH_DOMAIN: &[u8] = b"naome-memory:retirement-plan:v1\0";
const DECISION_HASH_DOMAIN: &[u8] = b"naome-memory:retirement-decisions:v1\0";
const RUN_HASH_DOMAIN: &[u8] = b"naome-memory:retirement-run:v1\0";
const RECEIPT_HASH_DOMAIN: &[u8] = b"naome-memory:proof-receipt:v1\0";

#[derive(Serialize)]
struct ReferenceReceiptAuthorityV1<'a> {
    contract_version: &'a str,
    run_id: Digest32,
    memory_space_id: &'a str,
    cohort_day: u64,
    as_of_us: u64,
    policy_id: &'a str,
    policy_digest: Digest32,
    seed: naome_memory_core::Seed32,
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

pub fn verify_reference_receipt(
    receipt: &ProofReceiptV1,
    inputs: &ReceiptVerificationInputsV1,
    reference: &ReferenceRetirementContractV1,
) -> naome_memory_core::Result<VerifiedReceiptV1> {
    let expected = reference_receipt(reference)?;
    let mut observed_artifacts = inputs.observed_exact_artifact_digests.clone();
    normalize_artifacts(&mut observed_artifacts);
    if inputs.observed_state_digest != reference.full_plan.projected_state_digest
        || observed_artifacts != expected.closure.exact_artifact_digests
        || receipt != &expected
    {
        return Err(MemoryError::ReceiptRejected(
            "independent receipt projection mismatch",
        ));
    }
    Ok(VerifiedReceiptV1 {
        receipt_id: expected.receipt_id,
        run_id: expected.run_id,
        plan_digest: expected.plan_digest,
        status: InvariantStatusV1::Verified,
    })
}

fn reference_receipt(
    reference: &ReferenceRetirementContractV1,
) -> naome_memory_core::Result<ProofReceiptV1> {
    let plan = &reference.full_plan;
    let plan_digest = Digest32::hash_prefixed(PLAN_HASH_DOMAIN, &plan.canonical_bytes()?);
    let decision_digest =
        Digest32::hash_prefixed(DECISION_HASH_DOMAIN, &plan.decisions.canonical_bytes()?);
    let run_id = Digest32::hash_prefixed(RUN_HASH_DOMAIN, plan_digest.as_bytes());
    let closure = reference_closure(reference);
    let invariants = reference_invariants();
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
        state_digest: plan.projected_state_digest,
        closure,
        invariants,
        overall_status: OverallProofStatusV1::Passed,
    };
    receipt.receipt_id = reference_receipt_id(&receipt)?;
    Ok(receipt)
}

fn reference_closure(reference: &ReferenceRetirementContractV1) -> ReceiptClosureV1 {
    let plan = &reference.full_plan;
    ReceiptClosureV1 {
        source_atom_ids: plan
            .decisions
            .iter()
            .map(|decision| decision.atom_id)
            .collect(),
        semantic_atom_ids: plan
            .semantic_atoms
            .iter()
            .map(|semantic| semantic.atom_id)
            .collect(),
        exact_episode_ids: plan.episodic_winners.clone(),
        exact_artifact_digests: plan
            .decisions
            .iter()
            .filter(|decision| decision.exact_retained)
            .map(|decision| (decision.atom_id, decision.exact_artifact_digests.clone()))
            .collect(),
    }
}

fn reference_receipt_id(receipt: &ProofReceiptV1) -> naome_memory_core::Result<Digest32> {
    let authority = ReferenceReceiptAuthorityV1 {
        contract_version: &receipt.contract_version,
        run_id: receipt.run_id,
        memory_space_id: &receipt.memory_space_id,
        cohort_day: receipt.cohort_day,
        as_of_us: receipt.as_of_us,
        policy_id: &receipt.policy_id,
        policy_digest: receipt.policy_digest,
        seed: receipt.seed,
        input_set_digest: receipt.input_set_digest,
        rng_identifier: &receipt.rng_identifier,
        population_digest: receipt.population_digest,
        episodic_population_count: receipt.episodic_population_count,
        episodic_winner_budget: receipt.episodic_winner_budget,
        episodic_winner_bytes: receipt.episodic_winner_bytes,
        semantic_count_budget: receipt.semantic_count_budget,
        semantic_body_bytes: receipt.semantic_body_bytes,
        plan_digest: receipt.plan_digest,
        decision_digest: receipt.decision_digest,
        state_digest: receipt.state_digest,
        closure: &receipt.closure,
        invariants: &receipt.invariants,
        overall_status: receipt.overall_status,
    };
    Ok(Digest32::hash_prefixed(
        RECEIPT_HASH_DOMAIN,
        &authority.canonical_bytes()?,
    ))
}

fn reference_invariants() -> BTreeMap<String, InvariantStatusV1> {
    [
        "budgets_structurally_bounded",
        "plan_structure_verified",
        "repository_outcome_closure_verified",
    ]
    .into_iter()
    .map(|name| (name.to_owned(), InvariantStatusV1::Verified))
    .collect()
}

fn normalize_artifacts(artifacts: &mut BTreeMap<AtomId, Vec<Digest32>>) {
    for digests in artifacts.values_mut() {
        digests.sort_unstable();
        digests.dedup();
    }
}
