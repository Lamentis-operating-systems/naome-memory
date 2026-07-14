use naome_memory_core::*;
use std::collections::{BTreeMap, BTreeSet};

const DAY_US: u64 = 86_400_000_000;
const RETIRE_AT_US: u64 = 8 * DAY_US;

fn sample_body(
    index: u64,
    session: &str,
    permission: RetentionPermissionV1,
    memory_space: &str,
) -> MemoryAtomBodyV1 {
    MemoryAtomBodyV1 {
        contract_version: "atom-v1".to_owned(),
        scope: MemoryScopeV1 {
            memory_space_id: memory_space.to_owned(),
            repository_id: Some("repo-a".to_owned()),
            task_id: Some("task-a".to_owned()),
            agent_id: Some("agent-a".to_owned()),
            session_id: session.to_owned(),
        },
        interval: TimeIntervalV1 {
            started_at_us: index * 10,
            ended_at_us: index * 10 + 1,
            recorded_at_us: index * 10 + 2,
        },
        trigger: "synthetic-event".to_owned(),
        seal_reason: SealReasonV1::GoalCompleted,
        goal: Some("prove deterministic memory".to_owned()),
        plan: vec!["observe".to_owned(), "verify".to_owned()],
        internal_state_before: BTreeMap::from([("phase".to_owned(), "open".to_owned())]),
        internal_state_after: BTreeMap::from([("phase".to_owned(), "closed".to_owned())]),
        observations: vec![ObservationV1 {
            at_us: index * 10,
            source: "synthetic".to_owned(),
            content: format!("observation-{index}"),
            artifact_digest: None,
        }],
        interpretations: vec![InterpretationV1 {
            content: "structured interpretation".to_owned(),
            confidence: Ppm::from_raw_unchecked(800_000),
            evidence: Vec::new(),
        }],
        beliefs: Vec::new(),
        decisions: vec![DecisionRecordV1 {
            decision: "continue".to_owned(),
            rationale: "synthetic policy".to_owned(),
        }],
        rejected_alternatives: Vec::new(),
        actions: vec![ActionRecordV1 {
            action: "record".to_owned(),
            result: Some("ok".to_owned()),
        }],
        outcome: Some(OutcomeV1 {
            class: "success".to_owned(),
            summary: "synthetic outcome".to_owned(),
            succeeded: true,
        }),
        feedback: Vec::new(),
        formation_signals: FormationSignalsV1 {
            utility: Ppm::from_raw_unchecked(900_000),
            salience: Ppm::from_raw_unchecked(900_000),
            novelty: Ppm::from_raw_unchecked(800_000),
            uncertainty: Ppm::from_raw_unchecked(100_000),
        },
        topic_keys: BTreeSet::from(["determinism".to_owned(), "memory".to_owned()]),
        entity_ids: BTreeSet::from(["naome".to_owned()]),
        outcome_class: Some("success".to_owned()),
        goal_key: Some("memory-proof".to_owned()),
        provenance: ProvenanceV1 {
            producer: "synthetic-test".to_owned(),
            source_event_digests: vec![Digest32::hash_prefixed(
                b"test-event\0",
                &index.to_be_bytes(),
            )],
            policy_digest: PolicyV1::poc_v1().digest().unwrap_or(Digest32::ZERO),
        },
        relations: Vec::new(),
        artifacts: Vec::new(),
        retention_permission: permission,
        payload: MemoryPayloadV1::Episode(EpisodePayloadV1 {
            event_sequence_start: index,
            event_sequence_end: index,
            continues: None,
        }),
    }
}

fn state(tier: RetentionTierV1) -> AtomStateV1 {
    AtomStateV1 {
        retention_tier: tier,
        content_status: ContentStatusV1::Present,
        expires_at_us: Some(RETIRE_AT_US),
        superseded_by: None,
        feedback_positive: 9,
        feedback_negative: 0,
        retrieval_count: 100,
        retirement_run_id: None,
    }
}

fn candidate(index: u64, session: &str, memory_space: &str) -> Result<RetirementCandidateV1> {
    let body = sample_body(
        index,
        session,
        RetentionPermissionV1::SemanticAndExactAllowed,
        memory_space,
    );
    let mut state = state(RetentionTierV1::ShortTerm);
    state.expires_at_us = Some(body.interval.recorded_at_us + PolicyV1::poc_v1().stm_horizon_us);
    RetirementCandidateV1::from_present_body(body, state)
}

#[test]
fn golden_atom_id_is_stable() -> Result<()> {
    let atom_id = sample_body(
        0,
        "session-a",
        RetentionPermissionV1::SemanticAndExactAllowed,
        "space-a",
    )
    .atom_id()?;
    assert_eq!(
        atom_id.to_hex(),
        "35cf1831456bcf277af8640fa5408b0d8739000d725d5d32b436053a8526bca7"
    );
    Ok(())
}

#[test]
fn seal_rejects_a_gap_and_binds_the_policy() -> Result<()> {
    let policy = PolicyV1::poc_v1();
    let scope = MemoryScopeV1 {
        memory_space_id: "space-a".to_owned(),
        repository_id: Some("repo-a".to_owned()),
        task_id: None,
        agent_id: None,
        session_id: "session-a".to_owned(),
    };
    let event = |sequence| EpisodeEventV1 {
        sequence,
        at_us: sequence + 10,
        scope: scope.clone(),
        observation: Some(ObservationV1 {
            at_us: sequence + 10,
            source: "synthetic".to_owned(),
            content: "bounded".to_owned(),
            artifact_digest: None,
        }),
        interpretation: None,
        belief: None,
        decision: None,
        action: None,
    };
    let events = vec![event(0), event(1)];
    let mut buffer = EpisodeBufferV1 {
        scope,
        started_at_us: 10,
        events,
        continues: None,
    };
    let request = SealEpisodeRequestV1 {
        as_of_us: 20,
        ended_at_us: 20,
        trigger: "checkpoint".to_owned(),
        seal_reason: SealReasonV1::Checkpoint,
        goal: None,
        plan: Vec::new(),
        internal_state_before: BTreeMap::new(),
        internal_state_after: BTreeMap::new(),
        rejected_alternatives: Vec::new(),
        outcome: None,
        feedback: Vec::new(),
        formation_signals: FormationSignalsV1::default(),
        topic_keys: BTreeSet::new(),
        entity_ids: BTreeSet::new(),
        outcome_class: None,
        goal_key: None,
        retention_permission: RetentionPermissionV1::TransientOnly,
        artifacts: Vec::new(),
        relations: Vec::new(),
    };
    let sealed = seal_episode(&buffer, &request, &policy)?;
    assert_eq!(sealed.body.provenance.policy_digest, policy.digest()?);
    buffer.events[1].sequence = 2;
    assert!(matches!(
        seal_episode(&buffer, &request, &policy),
        Err(MemoryError::NonContiguousEvents { .. })
    ));
    Ok(())
}

#[test]
fn retirement_is_deterministic_and_respects_both_ltm_paths() -> Result<()> {
    let atoms = (0_u64..200)
        .map(|index| {
            candidate(
                index,
                if index % 2 == 0 {
                    "session-a"
                } else {
                    "session-b"
                },
                "space-a",
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let snapshot = RetirementSnapshotV1 {
        contract_version: "retirement-snapshot-v1".to_owned(),
        memory_space_id: "space-a".to_owned(),
        cohort_day: 7,
        as_of_us: RETIRE_AT_US,
        atoms,
    };
    let seed = Seed32::new([7; 32]);
    let policy = PolicyV1::poc_v1();
    let first = plan_retirement(&snapshot, &policy, seed)?;
    let second = plan_retirement(&snapshot, &policy, seed)?;
    assert_eq!(first, second);
    assert_eq!(first.episodic_winners.len(), 1);
    assert_eq!(first.semantic_atoms.len(), 1);
    assert!(first.semantic_body_bytes <= policy.semantic_body_budget_bytes);
    assert!(first.episodic_winner_bytes <= policy.episodic_pin_budget_bytes);
    assert!(
        first
            .decisions
            .iter()
            .any(|decision| decision.exact_retained && !decision.semantic_atom_ids.is_empty())
    );

    for byte in 0_u8..=3 {
        let replay = plan_retirement(&snapshot, &policy, Seed32::new([byte; 32]))?;
        assert_eq!(replay.episodic_winners.len(), 1);
        assert!(replay.semantic_atoms.len() <= replay.semantic_count_budget);
    }
    Ok(())
}

#[test]
fn memory_space_isolation_fails_closed() -> Result<()> {
    let snapshot = RetirementSnapshotV1 {
        contract_version: "retirement-snapshot-v1".to_owned(),
        memory_space_id: "space-a".to_owned(),
        cohort_day: 7,
        as_of_us: RETIRE_AT_US,
        atoms: vec![candidate(1, "session-a", "space-b")?],
    };
    assert_eq!(
        plan_retirement(&snapshot, &PolicyV1::poc_v1(), Seed32::new([0; 32])),
        Err(MemoryError::MemorySpaceMismatch)
    );
    Ok(())
}

#[test]
fn explicit_flashback_is_separate_and_never_generalized() -> Result<()> {
    let first_body = sample_body(
        1,
        "session-a",
        RetentionPermissionV1::SemanticAndExactAllowed,
        "space-a",
    );
    let second_body = sample_body(
        2,
        "session-b",
        RetentionPermissionV1::SemanticAndExactAllowed,
        "space-a",
    );
    let make = |body: MemoryAtomBodyV1| -> Result<RetrievalCandidateV1> {
        Ok(RetrievalCandidateV1 {
            atom_id: body.atom_id()?,
            body,
            state: state(RetentionTierV1::EpisodicLongTerm),
            lexical_score: Ppm::from_raw_unchecked(900_000),
            integrity: IntegrityStatusV1::Complete,
        })
    };
    let first = make(first_body)?;
    let second = make(second_body)?;
    let query = RetrievalQueryV1 {
        contract_version: "retrieval-query-v1".to_owned(),
        memory_space_id: "space-a".to_owned(),
        repository_id: Some("repo-a".to_owned()),
        memory_space_wide: false,
        as_of_us: 1_000,
        terms: vec!["memory".to_owned()],
        topic_keys: BTreeSet::from(["memory".to_owned()]),
        entity_ids: BTreeSet::from(["naome".to_owned()]),
        k: Some(1),
        spontaneous: Some(SpontaneousRecallRequestV1 {
            seed: Seed32::new([9; 32]),
            budget: 1,
        }),
    };
    let result = rank_retrieval(
        &query,
        &RetrievalCandidatesV1 {
            ranked: vec![first.clone(), second.clone()],
            episodic_pool: vec![first, second],
        },
        &PolicyV1::poc_v1(),
    )?;
    assert_eq!(result.hits.len(), 1);
    let spontaneous = result.spontaneous.ok_or(MemoryError::ReceiptRejected(
        "missing explicit spontaneous result",
    ))?;
    assert_eq!(spontaneous.hits.len(), 1);
    assert_ne!(spontaneous.hits[0].atom_id, result.hits[0].atom_id);
    assert_eq!(spontaneous.hits[0].generality, GeneralityV1::SingleEpisode);
    assert_eq!(
        spontaneous.hits[0].recall_reason,
        RecallReasonV1::Spontaneous
    );
    Ok(())
}

struct ExactRepository {
    outcome: ApplyRetirementOutcomeV1,
}

impl RetirementRepositoryV1 for ExactRepository {
    fn apply_plan_atomically(
        &mut self,
        _plan: &RetirementPlanV1,
    ) -> Result<ApplyRetirementOutcomeV1> {
        Ok(self.outcome.clone())
    }
}

#[test]
fn receipt_replay_closes_and_tampering_is_rejected() -> Result<()> {
    let snapshot = RetirementSnapshotV1 {
        contract_version: "retirement-snapshot-v1".to_owned(),
        memory_space_id: "space-a".to_owned(),
        cohort_day: 7,
        as_of_us: RETIRE_AT_US,
        atoms: (0_u64..3)
            .map(|index| candidate(index, &format!("session-{index}"), "space-a"))
            .collect::<Result<Vec<_>>>()?,
    };
    let policy = PolicyV1::poc_v1();
    let seed = Seed32::new([3; 32]);
    let plan = plan_retirement(&snapshot, &policy, seed)?;
    let closure = ReceiptClosureV1::from_plan(&plan);
    let mut repository = ExactRepository {
        outcome: ApplyRetirementOutcomeV1 {
            observed_input_digest: plan.input_set_digest,
            state_digest: plan.projected_state_digest,
            applied_decision_count: plan.decisions.len(),
            semantic_atom_ids: closure.semantic_atom_ids.clone(),
            episodic_atom_ids: closure.exact_episode_ids.clone(),
            exact_artifact_digests: closure.exact_artifact_digests.clone(),
            already_applied: false,
        },
    };
    let receipt = apply_retirement(&mut repository, &plan)?;
    let inputs = ReceiptVerificationInputsV1 {
        policy,
        snapshot,
        seed,
        observed_state_digest: plan.projected_state_digest,
        observed_exact_artifact_digests: closure.exact_artifact_digests,
    };
    assert_eq!(
        verify_receipt(&receipt, &inputs)?.status,
        InvariantStatusV1::Verified
    );
    let mut tampered = receipt;
    tampered.state_digest = Digest32::ZERO;
    assert!(verify_receipt(&tampered, &inputs).is_err());
    Ok(())
}
