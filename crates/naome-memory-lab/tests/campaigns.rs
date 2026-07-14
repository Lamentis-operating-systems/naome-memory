#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;

use naome_memory_core::{
    ApplyRetirementOutcomeV1, AtomId, AtomStateV1, ContentStatusV1, Digest32, IntegrityStatusV1,
    MemoryError, MemoryKindV1, MemoryPayloadV1, PolicyV1, Ppm, ProofReceiptV1,
    ReceiptVerificationInputsV1, RetentionPermissionV1, RetentionTierV1, RetirementCandidateV1,
    RetirementPlanV1, RetirementRepositoryV1, RetirementSnapshotV1, RetrievalCandidateV1,
    RetrievalCandidatesV1, RetrievalQueryV1, Seed32, SemanticCandidateDispositionV1,
    SpontaneousRecallRequestV1, apply_retirement, plan_retirement, rank_retrieval, verify_receipt,
};
use naome_memory_lab::{DatasetIdV1, SyntheticWorldV1, generate_world_for_dataset};

#[path = "support/reference_receipt.rs"]
mod reference_receipt;
#[path = "support/reference_retirement.rs"]
mod reference_retirement;
#[path = "support/reference_retrieval.rs"]
mod reference_retrieval;

use reference_receipt::verify_reference_receipt;
use reference_retirement::plan_reference_retirement;
use reference_retrieval::rank_reference_retrieval;

const MICROSECONDS_PER_DAY: u64 = 86_400_000_000;
const DEFAULT_PROPERTY_CASES: usize = 64;
const DEFAULT_DIFFERENTIAL_WORLDS: usize = 24;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn campaign_size(variable: &str, default: usize) -> usize {
    std::env::var(variable)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn snapshot_for(
    world: &SyntheticWorldV1,
    policy: &PolicyV1,
) -> naome_memory_core::Result<RetirementSnapshotV1> {
    let atoms = world
        .atoms
        .iter()
        .cloned()
        .map(|body| {
            let expires_at_us = body
                .interval
                .recorded_at_us
                .saturating_add(policy.stm_horizon_us);
            RetirementCandidateV1::from_present_body(
                body,
                AtomStateV1 {
                    retention_tier: RetentionTierV1::ShortTerm,
                    content_status: ContentStatusV1::Present,
                    expires_at_us: Some(expires_at_us),
                    superseded_by: None,
                    feedback_positive: 0,
                    feedback_negative: 0,
                    retrieval_count: 0,
                    retirement_run_id: None,
                },
            )
        })
        .collect::<naome_memory_core::Result<Vec<_>>>()?;
    let latest_expiry_us = atoms
        .iter()
        .filter_map(|candidate| candidate.state.expires_at_us)
        .max()
        .unwrap_or(0);
    let cohort_day = latest_expiry_us / MICROSECONDS_PER_DAY;
    let as_of_us = cohort_day
        .checked_add(1)
        .and_then(|day| day.checked_mul(MICROSECONDS_PER_DAY))
        .ok_or(MemoryError::InvalidAtomBody(
            "retirement cohort boundary overflows u64",
        ))?;
    Ok(RetirementSnapshotV1 {
        contract_version: "retirement-snapshot-v1".to_owned(),
        memory_space_id: "synthetic-space-v1".to_owned(),
        cohort_day,
        as_of_us,
        atoms,
    })
}

#[test]
fn deterministic_property_campaign() -> TestResult {
    let policy = PolicyV1::poc_v1();
    let cases = campaign_size("NAOME_MEMORY_PROPERTY_CASES", DEFAULT_PROPERTY_CASES);
    for case in 0..cases {
        let world_index = u64::try_from(case).unwrap_or(u64::MAX);
        let atom_count = 3 + case % 61;
        let world = generate_world_for_dataset(DatasetIdV1::Adversarial, world_index, atom_count)?;
        let snapshot = snapshot_for(&world, &policy)?;
        let first = plan_retirement(&snapshot, &policy, world.seed)?;
        let replay = plan_retirement(&snapshot, &policy, world.seed)?;
        assert_eq!(first, replay, "case {case}: identical inputs must replay");

        let mut reordered = snapshot.clone();
        reordered.atoms.reverse();
        if !reordered.atoms.is_empty() {
            let rotation = case % reordered.atoms.len();
            reordered.atoms.rotate_left(rotation);
        }
        let reordered_plan = plan_retirement(&reordered, &policy, world.seed)?;
        assert_eq!(
            first, reordered_plan,
            "case {case}: input iteration order must not affect decisions"
        );

        assert!(
            first.semantic_atoms.len() <= first.semantic_count_budget,
            "case {case}: semantic count budget"
        );
        assert!(
            first.semantic_body_bytes <= policy.semantic_body_budget_bytes,
            "case {case}: semantic byte budget"
        );
        assert!(
            first.episodic_winners.len() <= first.episodic_winner_budget,
            "case {case}: episodic count budget"
        );
        assert!(
            first.episodic_winner_bytes <= policy.episodic_pin_budget_bytes,
            "case {case}: episodic byte budget"
        );
        assert!(
            first
                .decisions
                .windows(2)
                .all(|pair| pair[0].atom_id < pair[1].atom_id),
            "case {case}: decisions must be strictly ordered"
        );

        let winner_set = first
            .episodic_winners
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            winner_set.len(),
            first.episodic_winners.len(),
            "case {case}: lottery is without replacement"
        );
        for winner in &first.episodic_winners {
            let source = snapshot
                .atoms
                .iter()
                .find(|candidate| candidate.atom_id == *winner);
            assert!(
                source.is_some_and(|candidate| {
                    candidate.integrity == IntegrityStatusV1::Complete
                        && candidate.body.as_ref().is_some_and(|body| {
                            body.kind() == MemoryKindV1::Episode
                                && body.retention_permission
                                    == RetentionPermissionV1::SemanticAndExactAllowed
                        })
                }),
                "case {case}: every exact winner must be in the eligible population"
            );
        }
        for semantic in &first.semantic_atoms {
            let MemoryPayloadV1::Semantic(payload) = &semantic.body.payload else {
                panic!("case {case}: a planned semantic atom used an episode payload");
            };
            assert!(
                payload.source_ids.len() >= policy.min_semantic_sources,
                "case {case}: semantic support floor"
            );
            assert!(
                payload.source_ids.iter().all(|source| {
                    payload.source_body_digests.get(source) == Some(&source.digest())
                        && snapshot
                            .atoms
                            .iter()
                            .any(|candidate| candidate.atom_id == *source)
                }),
                "case {case}: semantic provenance must close over inputs"
            );
        }
    }
    Ok(())
}

#[test]
fn slow_independent_differential_campaign() -> TestResult {
    let policy = PolicyV1::poc_v1();
    let worlds = campaign_size(
        "NAOME_MEMORY_DIFFERENTIAL_WORLDS",
        DEFAULT_DIFFERENTIAL_WORLDS,
    );
    for world_index in 0..worlds {
        let index = u64::try_from(world_index).unwrap_or(u64::MAX);
        let world = generate_world_for_dataset(
            DatasetIdV1::Adversarial,
            index.saturating_add(10_000),
            10 + world_index % 37,
        )?;
        let snapshot = snapshot_for(&world, &policy)?;
        let production = plan_retirement(&snapshot, &policy, world.seed)?;
        let reference = plan_reference_retirement(&snapshot, &policy, world.seed)?;

        assert_eq!(
            production, reference.full_plan,
            "world {world_index}: complete retirement plan diverged"
        );
        assert_eq!(
            production.semantic_atoms, reference.semantic_atoms,
            "world {world_index}: complete semantic bodies, IDs, digests, or sizes diverged"
        );
        assert_eq!(
            production.semantic_decisions, reference.semantic_decisions,
            "world {world_index}: candidate score/order/disposition/size diverged"
        );
        assert_eq!(
            production.semantic_count_budget, reference.semantic_count_budget,
            "world {world_index}: semantic count budget diverged"
        );
        assert_eq!(
            production.semantic_body_bytes, reference.semantic_body_bytes,
            "world {world_index}: selected semantic byte accounting diverged"
        );
        assert_eq!(
            production.episodic_population_digest, reference.episodic_population_digest,
            "world {world_index}: lottery population evidence digest diverged"
        );
        assert_eq!(
            production.episodic_population_count, reference.episodic_population_count,
            "world {world_index}: lottery population count diverged"
        );
        assert_eq!(
            production.episodic_winner_budget, reference.episodic_winner_budget,
            "world {world_index}: lottery count budget diverged"
        );
        assert_eq!(
            production.episodic_winner_bytes, reference.episodic_winner_bytes,
            "world {world_index}: selected exact-package bytes diverged"
        );
        assert_eq!(
            production.episodic_winners, reference.episodic_winners,
            "world {world_index}: lottery diverged from independent Fisher-Yates model"
        );
        assert_eq!(
            production.decisions, reference.decisions,
            "world {world_index}: per-source fates or exact artifact closure diverged"
        );
        assert_eq!(
            production.projected_post_state, reference.projected_post_state,
            "world {world_index}: complete projected logical post-state diverged"
        );
        assert_eq!(
            production.projected_state_digest, reference.projected_state_digest,
            "world {world_index}: independent post-state digest diverged"
        );
    }
    // The deep/release workflows invoke this test by exact name, so keep the
    // deterministic disposition and retrieval references inside that surface.
    differential_campaign_exercises_semantic_count_dispositions()?;
    differential_campaign_exercises_semantic_byte_dispositions()?;
    independent_retrieval_and_flashback_differential()?;
    tampering_scope_and_receipt_closure_fail_closed()?;
    Ok(())
}

#[test]
fn differential_campaign_exercises_semantic_count_dispositions() -> TestResult {
    let policy = PolicyV1::poc_v1();
    let mut world = generate_world_for_dataset(DatasetIdV1::Adversarial, 91_337, 18)?;
    for (index, body) in world.atoms.iter_mut().enumerate() {
        let cluster = index / 3;
        body.topic_keys = BTreeSet::from([format!("budget-cluster-topic-{cluster}")]);
        body.entity_ids = BTreeSet::from([format!("budget-cluster-entity-{cluster}")]);
        let outcome_class = format!("budget-cluster-outcome-{cluster}");
        body.outcome_class = Some(outcome_class.clone());
        if let Some(outcome) = body.outcome.as_mut() {
            outcome.class = outcome_class;
        }
    }
    let mut snapshot = snapshot_for(&world, &policy)?;
    for (index, candidate) in snapshot.atoms.iter_mut().enumerate() {
        let index = u64::try_from(index).unwrap_or(u64::MAX);
        candidate.state.feedback_positive = index % 4;
        candidate.state.feedback_negative = index % 3;
        candidate.state.retrieval_count = index.saturating_mul(17);
    }
    snapshot.atoms.reverse();
    let production = plan_retirement(&snapshot, &policy, world.seed)?;
    let reference = plan_reference_retirement(&snapshot, &policy, world.seed)?;

    assert_eq!(production, reference.full_plan);
    assert_eq!(production.semantic_count_budget, 1);
    assert_eq!(production.semantic_decisions.len(), 6);
    assert_eq!(
        production
            .semantic_decisions
            .iter()
            .filter(|decision| { decision.disposition == SemanticCandidateDispositionV1::Selected })
            .count(),
        1
    );
    assert_eq!(
        production
            .semantic_decisions
            .iter()
            .filter(|decision| {
                decision.disposition == SemanticCandidateDispositionV1::SkippedCountBudget
            })
            .count(),
        5
    );
    assert_eq!(production.semantic_atoms, reference.semantic_atoms);
    assert_eq!(production.semantic_decisions, reference.semantic_decisions);
    assert_eq!(
        production.semantic_body_bytes,
        reference.semantic_body_bytes
    );
    assert_eq!(production.decisions, reference.decisions);
    assert_eq!(
        production.projected_post_state,
        reference.projected_post_state
    );
    assert_eq!(
        production.projected_state_digest,
        reference.projected_state_digest
    );
    Ok(())
}

#[test]
fn differential_campaign_exercises_semantic_byte_dispositions() -> TestResult {
    let policy = PolicyV1::poc_v1();
    let mut world = generate_world_for_dataset(DatasetIdV1::Adversarial, 91_338, 1_700)?;
    for (index, body) in world.atoms.iter_mut().enumerate() {
        body.retention_permission = RetentionPermissionV1::TransientOnly;
        if index >= 102 {
            continue;
        }
        let cluster = index / 3;
        body.retention_permission = RetentionPermissionV1::SemanticAllowed;
        body.topic_keys = BTreeSet::from([format!("byte-cluster-topic-{cluster}")]);
        body.entity_ids = BTreeSet::from([format!("byte-cluster-entity-{cluster}")]);
        let outcome_class = format!("byte-cluster-outcome-{cluster}");
        body.outcome_class = Some(outcome_class.clone());
        if let Some(outcome) = body.outcome.as_mut() {
            outcome.class = outcome_class;
        }
        body.plan = vec![format!("byte-cluster-{cluster}:{}", "x".repeat(247_000))];
    }
    let snapshot = snapshot_for(&world, &policy)?;
    let production = plan_retirement(&snapshot, &policy, world.seed)?;
    let reference = plan_reference_retirement(&snapshot, &policy, world.seed)?;

    assert_eq!(production, reference.full_plan);
    assert_eq!(production.semantic_count_budget, 34);
    assert_eq!(production.semantic_decisions.len(), 34);
    assert!(production.semantic_decisions.iter().any(|decision| {
        decision.disposition == SemanticCandidateDispositionV1::SkippedByteBudget
    }));
    assert!(production.semantic_body_bytes <= policy.semantic_body_budget_bytes);
    assert_eq!(production.semantic_atoms, reference.semantic_atoms);
    assert_eq!(production.semantic_decisions, reference.semantic_decisions);
    assert_eq!(
        production.semantic_body_bytes,
        reference.semantic_body_bytes
    );
    assert_eq!(production.decisions, reference.decisions);
    assert_eq!(
        production.projected_post_state,
        reference.projected_post_state
    );
    assert_eq!(
        production.projected_state_digest,
        reference.projected_state_digest
    );
    Ok(())
}

#[test]
fn independent_retrieval_and_flashback_differential() -> TestResult {
    let policy = PolicyV1::poc_v1();
    let world = generate_world_for_dataset(DatasetIdV1::Adversarial, 92_001, 32)?;
    let snapshot = snapshot_for(&world, &policy)?;
    let plan = plan_retirement(&snapshot, &policy, world.seed)?;
    let candidates = differential_retrieval_candidates(&snapshot, &plan)?;
    let query = differential_retrieval_query(&snapshot);
    let production = rank_retrieval(&query, &candidates, &policy)?;
    let reference = rank_reference_retrieval(&query, &candidates, &policy)?;
    assert_eq!(production, reference);
    assert_eq!(production.hits.len(), 7);
    assert_eq!(
        production
            .spontaneous
            .as_ref()
            .map(|result| result.hits.len()),
        Some(3)
    );
    Ok(())
}

fn differential_retrieval_candidates(
    snapshot: &RetirementSnapshotV1,
    plan: &RetirementPlanV1,
) -> naome_memory_core::Result<RetrievalCandidatesV1> {
    let mut ranked = snapshot
        .atoms
        .iter()
        .take(14)
        .enumerate()
        .map(|(index, candidate)| {
            episode_retrieval_candidate(
                candidate,
                Ppm::from_raw_unchecked(u32::try_from((index * 79_919) % 1_000_001).unwrap_or(0)),
                IntegrityStatusV1::Complete,
                u64::try_from(index % 5).unwrap_or(0),
                u64::try_from(index % 3).unwrap_or(0),
            )
        })
        .collect::<naome_memory_core::Result<Vec<_>>>()?;
    ranked.extend(
        plan.semantic_atoms
            .iter()
            .map(|semantic| RetrievalCandidateV1 {
                atom_id: semantic.atom_id,
                body: semantic.body.clone(),
                state: AtomStateV1 {
                    retention_tier: RetentionTierV1::SemanticLongTerm,
                    content_status: ContentStatusV1::Present,
                    expires_at_us: None,
                    superseded_by: None,
                    feedback_positive: 4,
                    feedback_negative: 1,
                    retrieval_count: 999,
                    retirement_run_id: None,
                },
                lexical_score: Ppm::from_raw_unchecked(620_000),
                integrity: IntegrityStatusV1::Complete,
            }),
    );
    if let Some(candidate) = snapshot.atoms.get(30) {
        ranked.push(episode_retrieval_candidate(
            candidate,
            Ppm::ONE,
            IntegrityStatusV1::MissingArtifact,
            0,
            0,
        )?);
    }
    if let Some(candidate) = snapshot.atoms.get(31) {
        let mut off_scope =
            episode_retrieval_candidate(candidate, Ppm::ONE, IntegrityStatusV1::Complete, 0, 0)?;
        "other-memory-space".clone_into(&mut off_scope.body.scope.memory_space_id);
        off_scope.atom_id = off_scope.body.atom_id()?;
        ranked.push(off_scope);
    }
    let episodic_pool = snapshot
        .atoms
        .iter()
        .map(|candidate| {
            episode_retrieval_candidate(
                candidate,
                Ppm::ZERO,
                IntegrityStatusV1::Complete,
                candidate.state.feedback_positive,
                candidate.state.feedback_negative,
            )
        })
        .collect::<naome_memory_core::Result<Vec<_>>>()?;
    Ok(RetrievalCandidatesV1 {
        ranked,
        episodic_pool,
    })
}

fn episode_retrieval_candidate(
    candidate: &RetirementCandidateV1,
    lexical_score: Ppm,
    integrity: IntegrityStatusV1,
    feedback_positive: u64,
    feedback_negative: u64,
) -> naome_memory_core::Result<RetrievalCandidateV1> {
    let mut state = candidate.state.clone();
    state.retention_tier = RetentionTierV1::EpisodicLongTerm;
    state.expires_at_us = None;
    state.feedback_positive = feedback_positive;
    state.feedback_negative = feedback_negative;
    Ok(RetrievalCandidateV1 {
        atom_id: candidate.atom_id,
        body: candidate
            .body
            .clone()
            .ok_or(MemoryError::AtomDigestMismatch {
                atom_id: candidate.atom_id,
            })?,
        state,
        lexical_score,
        integrity,
    })
}

fn differential_retrieval_query(snapshot: &RetirementSnapshotV1) -> RetrievalQueryV1 {
    RetrievalQueryV1 {
        contract_version: "retrieval-query-v1".to_owned(),
        memory_space_id: snapshot.memory_space_id.clone(),
        repository_id: Some("synthetic-repository".to_owned()),
        memory_space_wide: false,
        as_of_us: snapshot.as_of_us,
        terms: vec!["build".to_owned(), "proof".to_owned()],
        topic_keys: BTreeSet::from(["build-proof".to_owned(), "memory".to_owned()]),
        entity_ids: BTreeSet::from(["entity:naome-memory".to_owned()]),
        k: Some(7),
        spontaneous: Some(SpontaneousRecallRequestV1 {
            seed: Seed32::new([0x5A; 32]),
            budget: 3,
        }),
    }
}

#[derive(Default)]
struct ClosureRepository {
    applied: bool,
}

impl RetirementRepositoryV1 for ClosureRepository {
    fn apply_plan_atomically(
        &mut self,
        plan: &RetirementPlanV1,
    ) -> naome_memory_core::Result<ApplyRetirementOutcomeV1> {
        let already_applied = self.applied;
        self.applied = true;
        Ok(ApplyRetirementOutcomeV1 {
            observed_input_digest: plan.input_set_digest,
            state_digest: plan.projected_state_digest,
            applied_decision_count: plan.decisions.len(),
            semantic_atom_ids: plan
                .semantic_atoms
                .iter()
                .map(|semantic| semantic.atom_id)
                .collect(),
            episodic_atom_ids: plan.episodic_winners.clone(),
            exact_artifact_digests: exact_artifacts(plan),
            already_applied,
        })
    }
}

fn exact_artifacts(plan: &RetirementPlanV1) -> BTreeMap<AtomId, Vec<Digest32>> {
    plan.decisions
        .iter()
        .filter(|decision| decision.exact_retained)
        .map(|decision| (decision.atom_id, decision.exact_artifact_digests.clone()))
        .collect()
}

fn valid_receipt_fixture() -> TestResult<(
    ProofReceiptV1,
    ReceiptVerificationInputsV1,
    RetirementSnapshotV1,
)> {
    let policy = PolicyV1::poc_v1();
    let mut world = generate_world_for_dataset(DatasetIdV1::Adversarial, 70_001, 32)?;
    for (index, body) in world.atoms.iter_mut().enumerate() {
        body.artifacts.push(naome_memory_core::ArtifactRefV1 {
            digest: Digest32::hash_prefixed(
                b"naome-memory:receipt-artifact-fixture:v1\0",
                &u64::try_from(index).unwrap_or(u64::MAX).to_be_bytes(),
            ),
            size_bytes: 16,
            media_type: "application/octet-stream".to_owned(),
            retention_allowed: true,
            inline_payload: None,
        });
    }
    let snapshot = snapshot_for(&world, &policy)?;
    let plan = plan_retirement(&snapshot, &policy, world.seed)?;
    let mut repository = ClosureRepository::default();
    let receipt = apply_retirement(&mut repository, &plan)?;
    let inputs = ReceiptVerificationInputsV1 {
        policy,
        snapshot: snapshot.clone(),
        seed: world.seed,
        observed_state_digest: plan.projected_state_digest,
        observed_exact_artifact_digests: exact_artifacts(&plan),
    };
    Ok((receipt, inputs, snapshot))
}

#[test]
fn tampering_scope_and_receipt_closure_fail_closed() -> TestResult {
    let (receipt, inputs, snapshot) = valid_receipt_fixture()?;
    let reference = plan_reference_retirement(&inputs.snapshot, &inputs.policy, inputs.seed)?;
    assert_eq!(
        reference.full_plan,
        plan_retirement(&inputs.snapshot, &inputs.policy, inputs.seed)?
    );
    assert_eq!(
        verify_receipt(&receipt, &inputs)?,
        verify_reference_receipt(&receipt, &inputs, &reference)?
    );

    let mut tampered_seed = receipt.clone();
    tampered_seed.seed = Seed32::new([0xA5; 32]);
    tampered_seed.receipt_id = tampered_seed.recompute_receipt_id()?;
    assert!(verify_receipt(&tampered_seed, &inputs).is_err());
    assert!(verify_reference_receipt(&tampered_seed, &inputs, &reference).is_err());

    let mut missing_closure = receipt.clone();
    missing_closure.closure.exact_episode_ids.clear();
    missing_closure.receipt_id = missing_closure.recompute_receipt_id()?;
    assert!(verify_receipt(&missing_closure, &inputs).is_err());
    assert!(verify_reference_receipt(&missing_closure, &inputs, &reference).is_err());

    let mut wrong_state = inputs.clone();
    wrong_state.observed_state_digest = Digest32::ZERO;
    assert!(verify_receipt(&receipt, &wrong_state).is_err());
    assert!(verify_reference_receipt(&receipt, &wrong_state, &reference).is_err());

    let mut missing_artifacts = inputs.clone();
    missing_artifacts.observed_exact_artifact_digests.clear();
    if !receipt.closure.exact_artifact_digests.is_empty() {
        assert!(verify_receipt(&receipt, &missing_artifacts).is_err());
        assert!(verify_reference_receipt(&receipt, &missing_artifacts, &reference).is_err());
    }

    let mut wrong_space = snapshot.clone();
    wrong_space.memory_space_id = "other-space".to_owned();
    assert!(matches!(
        plan_retirement(&wrong_space, &inputs.policy, inputs.seed),
        Err(MemoryError::MemorySpaceMismatch)
    ));

    let mut duplicate = snapshot.clone();
    if let Some(first) = duplicate.atoms.first().cloned() {
        duplicate.atoms.push(first);
    }
    assert!(matches!(
        plan_retirement(&duplicate, &inputs.policy, inputs.seed),
        Err(MemoryError::DuplicateAtom { .. })
    ));

    let mut digest_tamper = snapshot;
    if let Some(first) = digest_tamper.atoms.first_mut() {
        first.body_digest = Digest32::ZERO;
    }
    assert!(matches!(
        plan_retirement(&digest_tamper, &inputs.policy, inputs.seed),
        Err(MemoryError::AtomDigestMismatch { .. })
    ));
    Ok(())
}

#[test]
fn selected_exact_packages_reject_the_entire_over_budget_plan() -> TestResult {
    let policy = PolicyV1::poc_v1();
    let mut world = generate_world_for_dataset(DatasetIdV1::Adversarial, 80_001, 1_000)?;
    for (index, body) in world.atoms.iter_mut().enumerate() {
        let discriminator = u8::try_from(index % 251).unwrap_or(0);
        body.topic_keys = BTreeSet::from([format!("unique-topic-{index}")]);
        body.entity_ids.clear();
        let outcome_class = format!("unique-outcome-{index}");
        body.outcome_class = Some(outcome_class.clone());
        if let Some(outcome) = body.outcome.as_mut() {
            outcome.class = outcome_class;
        }
        body.artifacts.push(naome_memory_core::ArtifactRefV1 {
            digest: Digest32::hash_prefixed(
                b"naome-memory:test-large-artifact:v1\0",
                &[discriminator],
            ),
            size_bytes: 63 * 1024 * 1024,
            media_type: "application/octet-stream".to_owned(),
            retention_allowed: true,
            inline_payload: None,
        });
    }
    let snapshot = snapshot_for(&world, &policy)?;
    let result = plan_retirement(&snapshot, &policy, world.seed);
    assert!(
        matches!(&result, Err(MemoryError::EpisodicBudgetExceeded { .. })),
        "over-budget lottery returned {result:?}"
    );
    Ok(())
}
