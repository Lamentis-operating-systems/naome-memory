use anyhow::{Context as _, Result, ensure};
use naome_memory_core::{
    ActionRecordV1, ApplyRetirementOutcomeV1, AtomId, AtomStateV1, BeliefV1, ContentStatusV1,
    DecisionRecordV1, Digest32, EpisodeBufferV1, EpisodeEventV1, FeedbackSignalV1,
    FormationSignalsV1, InterpretationV1, MemoryRelationKindV1, MemoryRelationV1, MemoryScopeV1,
    ObservationV1, OutcomeV1, PolicyV1, Ppm, ReceiptClosureV1, ReceiptVerificationInputsV1,
    RejectedAlternativeV1, RetentionPermissionV1, RetentionTierV1, RetirementCandidateV1,
    RetirementPlanV1, RetirementRepositoryV1, RetirementSnapshotV1, RetrievalCandidateV1,
    RetrievalCandidatesV1, RetrievalQueryV1, RetrievalResultV1, SealEpisodeRequestV1, SealReasonV1,
    Seed32, SpontaneousRecallRequestV1, VerifiedReceiptV1, apply_retirement, plan_retirement,
    rank_retrieval, seal_episode_chain, verify_receipt,
};
use naome_memory_lab::generate_world;
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

const DAY_US: u64 = 86_400_000_000;

struct FixtureRepository(ApplyRetirementOutcomeV1);

impl RetirementRepositoryV1 for FixtureRepository {
    fn apply_plan_atomically(
        &mut self,
        _plan: &RetirementPlanV1,
    ) -> naome_memory_core::Result<ApplyRetirementOutcomeV1> {
        Ok(self.0.clone())
    }
}

#[derive(Serialize)]
struct BindingManifestV1 {
    contract_version: &'static str,
    bindings: Vec<BindingV1>,
}

#[derive(Serialize)]
struct BindingV1 {
    schema: String,
    instance: String,
}

struct RetirementFixturesV1 {
    snapshot: RetirementSnapshotV1,
    plan: RetirementPlanV1,
    verification_inputs: ReceiptVerificationInputsV1,
    verified_receipt: VerifiedReceiptV1,
}

/// Generate real Serde projections from the typed public contracts.
pub fn generate(output_dir: &Path) -> Result<()> {
    fs::create_dir_all(output_dir)
        .with_context(|| format!("create schema fixture directory {}", output_dir.display()))?;

    let policy = PolicyV1::poc_v1();
    let (event, buffer, request) = episode_inputs();
    let chain = seal_episode_chain(&buffer, &request, &policy)?;
    let retirement = retirement_fixtures(&policy)?;
    let (query, retrieval) = retrieval_fixtures(&policy, &retirement)?;

    let bindings = vec![
        write_bound(output_dir, "episode-event-v1", &event)?,
        write_bound(output_dir, "episode-buffer-v1", &buffer)?,
        write_bound(output_dir, "seal-episode-request-v1", &request)?,
        write_bound(output_dir, "sealed-episode-chain-v1", &chain)?,
        write_bound(output_dir, "retirement-snapshot-v1", &retirement.snapshot)?,
        write_bound(output_dir, "retirement-plan-v1", &retirement.plan)?,
        write_bound(output_dir, "retrieval-query-v1", &query)?,
        write_bound(output_dir, "retrieval-result-v1", &retrieval)?,
        write_bound(
            output_dir,
            "receipt-verification-inputs-v1",
            &retirement.verification_inputs,
        )?,
        write_bound(
            output_dir,
            "verified-receipt-v1",
            &retirement.verified_receipt,
        )?,
    ];
    let manifest_path = output_dir.join("bindings-v1.json");
    write_json(
        &manifest_path,
        &BindingManifestV1 {
            contract_version: "schema-fixture-bindings-v1",
            bindings,
        },
    )?;
    println!(
        "{}",
        serde_json::to_string(&json!({
            "contract_version": "xtask.schema-fixtures-result.v1",
            "binding_count": 10,
            "status": "generated"
        }))?
    );
    Ok(())
}

fn retirement_fixtures(policy: &PolicyV1) -> Result<RetirementFixturesV1> {
    let world = generate_world(0, 8)?;
    let expires_at_us = world
        .atoms
        .iter()
        .map(|body| {
            body.interval
                .recorded_at_us
                .saturating_add(policy.stm_horizon_us)
        })
        .max()
        .context("synthetic schema world has no atoms")?;
    let cohort_day = expires_at_us / DAY_US;
    let retirement_as_of_us = cohort_day
        .checked_add(1)
        .and_then(|day| day.checked_mul(DAY_US))
        .context("typed fixture retirement cohort boundary overflow")?;
    let atoms = world
        .atoms
        .into_iter()
        .map(|body| {
            let state = AtomStateV1 {
                retention_tier: RetentionTierV1::ShortTerm,
                content_status: ContentStatusV1::Present,
                expires_at_us: Some(
                    body.interval
                        .recorded_at_us
                        .saturating_add(policy.stm_horizon_us),
                ),
                superseded_by: None,
                feedback_positive: 9,
                feedback_negative: 1,
                retrieval_count: 3,
                retirement_run_id: None,
            };
            RetirementCandidateV1::from_present_body(body, state)
        })
        .collect::<naome_memory_core::Result<Vec<_>>>()?;
    ensure!(
        atoms.iter().all(|candidate| {
            candidate.state.expires_at_us.map(|expiry| expiry / DAY_US) == Some(cohort_day)
        }),
        "typed fixture atoms unexpectedly span retirement cohorts"
    );
    let snapshot = RetirementSnapshotV1 {
        contract_version: "retirement-snapshot-v1".to_owned(),
        memory_space_id: "synthetic-space-v1".to_owned(),
        cohort_day,
        as_of_us: retirement_as_of_us,
        atoms,
    };
    let seed = Seed32::new([0x37; 32]);
    let plan = plan_retirement(&snapshot, policy, seed)?;
    ensure!(
        !plan.semantic_atoms.is_empty(),
        "typed fixture plan must exercise the semantic payload projection"
    );
    let closure = ReceiptClosureV1::from_plan(&plan);
    let outcome = ApplyRetirementOutcomeV1 {
        observed_input_digest: plan.input_set_digest,
        state_digest: plan.projected_state_digest,
        applied_decision_count: plan.decisions.len(),
        semantic_atom_ids: closure.semantic_atom_ids.clone(),
        episodic_atom_ids: closure.exact_episode_ids.clone(),
        exact_artifact_digests: closure.exact_artifact_digests.clone(),
        already_applied: false,
    };
    let receipt = apply_retirement(&mut FixtureRepository(outcome), &plan)?;
    let verification_inputs = ReceiptVerificationInputsV1 {
        policy: policy.clone(),
        snapshot: snapshot.clone(),
        seed,
        observed_state_digest: plan.projected_state_digest,
        observed_exact_artifact_digests: closure.exact_artifact_digests,
    };
    let verified_receipt = verify_receipt(&receipt, &verification_inputs)?;
    Ok(RetirementFixturesV1 {
        snapshot,
        plan,
        verification_inputs,
        verified_receipt,
    })
}

fn retrieval_fixtures(
    policy: &PolicyV1,
    retirement: &RetirementFixturesV1,
) -> Result<(RetrievalQueryV1, RetrievalResultV1)> {
    let semantic = retirement
        .plan
        .semantic_atoms
        .first()
        .context("typed fixture plan has no semantic atom")?;
    let exact_id = retirement
        .plan
        .episodic_winners
        .first()
        .copied()
        .context("typed fixture plan has no episodic winner")?;
    let exact_body = retirement
        .snapshot
        .atoms
        .iter()
        .find(|candidate| candidate.atom_id == exact_id)
        .and_then(|candidate| candidate.body.clone())
        .context("typed fixture episodic winner has no body")?;
    let query = RetrievalQueryV1 {
        contract_version: "retrieval-query-v1".to_owned(),
        memory_space_id: "synthetic-space-v1".to_owned(),
        repository_id: Some("synthetic-repository".to_owned()),
        memory_space_wide: false,
        as_of_us: retirement.snapshot.as_of_us,
        terms: vec!["build-proof".to_owned()],
        topic_keys: BTreeSet::from(["build-proof".to_owned()]),
        entity_ids: BTreeSet::from(["entity:naome-memory".to_owned()]),
        k: Some(1),
        spontaneous: Some(SpontaneousRecallRequestV1 {
            seed: Seed32::new([0xa7; 32]),
            budget: 1,
        }),
    };
    let result = rank_retrieval(
        &query,
        &RetrievalCandidatesV1 {
            ranked: vec![RetrievalCandidateV1 {
                atom_id: semantic.atom_id,
                body: semantic.body.clone(),
                state: present_state(RetentionTierV1::SemanticLongTerm),
                lexical_score: Ppm::from_raw_unchecked(900_000),
                integrity: naome_memory_core::IntegrityStatusV1::Complete,
            }],
            episodic_pool: vec![RetrievalCandidateV1 {
                atom_id: exact_id,
                body: exact_body,
                state: present_state(RetentionTierV1::EpisodicLongTerm),
                lexical_score: Ppm::ZERO,
                integrity: naome_memory_core::IntegrityStatusV1::Complete,
            }],
        },
        policy,
    )?;
    Ok((query, result))
}

fn episode_inputs() -> (EpisodeEventV1, EpisodeBufferV1, SealEpisodeRequestV1) {
    let scope = MemoryScopeV1 {
        memory_space_id: "schema-space-v1".to_owned(),
        repository_id: Some("schema-repository".to_owned()),
        task_id: Some("schema-task".to_owned()),
        agent_id: Some("schema-agent".to_owned()),
        session_id: "schema-session".to_owned(),
    };
    let evidence = Digest32::hash_prefixed(b"schema-fixture:evidence:v1\0", b"evidence");
    let payload = b"typed schema fixture".to_vec();
    let artifact_digest = Digest32::hash_prefixed(&[], &payload);
    let event = EpisodeEventV1 {
        sequence: 7,
        at_us: 1_010,
        scope: scope.clone(),
        observation: Some(ObservationV1 {
            at_us: 1_010,
            source: "synthetic-schema-fixture".to_owned(),
            content: "typed observation".to_owned(),
            artifact_digest: Some(artifact_digest),
        }),
        interpretation: Some(InterpretationV1 {
            content: "typed interpretation".to_owned(),
            confidence: Ppm::from_raw_unchecked(800_000),
            evidence: vec![evidence],
        }),
        belief: Some(BeliefV1 {
            proposition: "the fixture is typed".to_owned(),
            confidence: Ppm::from_raw_unchecked(750_000),
            evidence: vec![evidence],
        }),
        decision: Some(DecisionRecordV1 {
            decision: "seal".to_owned(),
            rationale: "exercise every public field".to_owned(),
        }),
        action: Some(ActionRecordV1 {
            action: "generate fixture".to_owned(),
            result: Some("generated".to_owned()),
        }),
    };
    let buffer = EpisodeBufferV1 {
        scope,
        started_at_us: 1_000,
        events: vec![event.clone()],
        continues: None,
    };
    let request = SealEpisodeRequestV1 {
        as_of_us: 1_020,
        ended_at_us: 1_015,
        trigger: "schema-fixture".to_owned(),
        seal_reason: SealReasonV1::Checkpoint,
        goal: Some("validate public JSON projections".to_owned()),
        plan: vec![
            "construct typed value".to_owned(),
            "validate schema".to_owned(),
        ],
        internal_state_before: BTreeMap::from([("phase".to_owned(), "draft".to_owned())]),
        internal_state_after: BTreeMap::from([("phase".to_owned(), "sealed".to_owned())]),
        rejected_alternatives: vec![RejectedAlternativeV1 {
            alternative: "hand-author JSON".to_owned(),
            reason: "it can drift from Serde".to_owned(),
        }],
        outcome: Some(OutcomeV1 {
            class: "success".to_owned(),
            summary: "typed fixture generated".to_owned(),
            succeeded: true,
        }),
        feedback: vec![FeedbackSignalV1 {
            source: "schema-validator".to_owned(),
            positive: true,
            note: Some("Draft 2020-12".to_owned()),
        }],
        formation_signals: FormationSignalsV1 {
            utility: Ppm::from_raw_unchecked(900_000),
            salience: Ppm::from_raw_unchecked(700_000),
            novelty: Ppm::from_raw_unchecked(500_000),
            uncertainty: Ppm::from_raw_unchecked(50_000),
        },
        topic_keys: BTreeSet::from(["json-schema".to_owned(), "memory".to_owned()]),
        entity_ids: BTreeSet::from(["entity:naome-memory".to_owned()]),
        outcome_class: Some("success".to_owned()),
        goal_key: Some("goal:schema-contract".to_owned()),
        retention_permission: RetentionPermissionV1::SemanticAndExactAllowed,
        artifacts: vec![naome_memory_core::ArtifactRefV1 {
            digest: artifact_digest,
            size_bytes: u64::try_from(payload.len()).unwrap_or(u64::MAX),
            media_type: "text/plain".to_owned(),
            retention_allowed: true,
            inline_payload: Some(payload),
        }],
        relations: vec![MemoryRelationV1 {
            kind: MemoryRelationKindV1::Supports,
            target: AtomId(evidence),
        }],
    };
    (event, buffer, request)
}

fn present_state(tier: RetentionTierV1) -> AtomStateV1 {
    AtomStateV1 {
        retention_tier: tier,
        content_status: ContentStatusV1::Present,
        expires_at_us: None,
        superseded_by: None,
        feedback_positive: 4,
        feedback_negative: 1,
        retrieval_count: 9,
        retirement_run_id: Some(Digest32::hash_prefixed(
            b"schema-fixture:retirement-run:v1\0",
            b"applied",
        )),
    }
}

fn write_bound<T: Serialize>(output_dir: &Path, contract: &str, value: &T) -> Result<BindingV1> {
    let instance = output_dir.join(format!("{contract}.json"));
    write_json(&instance, value)?;
    Ok(BindingV1 {
        schema: format!("schemas/{contract}.schema.json"),
        instance: format!("{contract}.json"),
    })
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    fs::write(path, bytes).with_context(|| format!("write typed fixture {}", path.display()))
}
