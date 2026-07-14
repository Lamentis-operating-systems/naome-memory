#![allow(clippy::too_many_lines)]

use naome_memory_core::*;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr as _;

const DAY_US: u64 = 86_400_000_000;
const RETIRE_AT_US: u64 = 8 * DAY_US;

fn scope(session: &str) -> MemoryScopeV1 {
    MemoryScopeV1 {
        memory_space_id: "space-a".to_owned(),
        repository_id: Some("repo-a".to_owned()),
        task_id: Some("task-a".to_owned()),
        agent_id: Some("agent-a".to_owned()),
        session_id: session.to_owned(),
    }
}

fn body(index: u64, session: &str) -> MemoryAtomBodyV1 {
    MemoryAtomBodyV1 {
        contract_version: "atom-v1".to_owned(),
        scope: scope(session),
        interval: TimeIntervalV1 {
            started_at_us: index * 10,
            ended_at_us: index * 10 + 1,
            recorded_at_us: index * 10 + 2,
        },
        trigger: "event".to_owned(),
        seal_reason: SealReasonV1::Checkpoint,
        goal: Some("remember".to_owned()),
        plan: vec!["observe".to_owned()],
        internal_state_before: BTreeMap::from([("phase".to_owned(), "open".to_owned())]),
        internal_state_after: BTreeMap::from([("phase".to_owned(), "closed".to_owned())]),
        observations: vec![ObservationV1 {
            at_us: index * 10,
            source: "test".to_owned(),
            content: format!("observation-{index}"),
            artifact_digest: None,
        }],
        interpretations: Vec::new(),
        beliefs: Vec::new(),
        decisions: Vec::new(),
        rejected_alternatives: Vec::new(),
        actions: Vec::new(),
        outcome: Some(OutcomeV1 {
            class: "success".to_owned(),
            summary: "done".to_owned(),
            succeeded: true,
        }),
        feedback: Vec::new(),
        formation_signals: FormationSignalsV1 {
            utility: Ppm::from_raw_unchecked(900_000),
            salience: Ppm::from_raw_unchecked(900_000),
            novelty: Ppm::from_raw_unchecked(800_000),
            uncertainty: Ppm::from_raw_unchecked(100_000),
        },
        topic_keys: BTreeSet::from(["memory".to_owned(), "proof".to_owned()]),
        entity_ids: BTreeSet::from(["naome".to_owned()]),
        outcome_class: Some("success".to_owned()),
        goal_key: Some("memory-proof".to_owned()),
        provenance: ProvenanceV1 {
            producer: "coverage-test".to_owned(),
            source_event_digests: vec![Digest32::hash_prefixed(b"event\0", &index.to_be_bytes())],
            policy_digest: PolicyV1::poc_v1().digest().unwrap_or(Digest32::ZERO),
        },
        relations: Vec::new(),
        artifacts: Vec::new(),
        retention_permission: RetentionPermissionV1::SemanticAndExactAllowed,
        payload: MemoryPayloadV1::Episode(EpisodePayloadV1 {
            event_sequence_start: index,
            event_sequence_end: index,
            continues: None,
        }),
    }
}

fn atom_state(tier: RetentionTierV1) -> AtomStateV1 {
    AtomStateV1 {
        retention_tier: tier,
        content_status: ContentStatusV1::Present,
        expires_at_us: Some(RETIRE_AT_US),
        superseded_by: None,
        feedback_positive: 9,
        feedback_negative: 0,
        retrieval_count: u64::MAX,
        retirement_run_id: None,
    }
}

fn retirement_candidate(index: u64, session: &str) -> Result<RetirementCandidateV1> {
    let body = body(index, session);
    let mut state = atom_state(RetentionTierV1::ShortTerm);
    state.expires_at_us = Some(body.interval.recorded_at_us + PolicyV1::poc_v1().stm_horizon_us);
    RetirementCandidateV1::from_present_body(body, state)
}

fn snapshot() -> Result<RetirementSnapshotV1> {
    Ok(RetirementSnapshotV1 {
        contract_version: "retirement-snapshot-v1".to_owned(),
        memory_space_id: "space-a".to_owned(),
        cohort_day: 7,
        as_of_us: RETIRE_AT_US,
        atoms: (0_u64..3)
            .map(|index| retirement_candidate(index, &format!("session-{index}")))
            .collect::<Result<Vec<_>>>()?,
    })
}

fn valid_episode() -> (EpisodeBufferV1, SealEpisodeRequestV1) {
    let scope = scope("session-a");
    let event = EpisodeEventV1 {
        sequence: 7,
        at_us: 11,
        scope: scope.clone(),
        observation: Some(ObservationV1 {
            at_us: 11,
            source: "test".to_owned(),
            content: "bounded".to_owned(),
            artifact_digest: None,
        }),
        interpretation: None,
        belief: None,
        decision: None,
        action: None,
    };
    (
        EpisodeBufferV1 {
            scope,
            started_at_us: 10,
            events: vec![event],
            continues: None,
        },
        SealEpisodeRequestV1 {
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
        },
    )
}

#[test]
fn canonical_encoding_and_digest_interfaces_cover_all_value_kinds() -> Result<()> {
    let authority = json!({
        "null": null,
        "false": false,
        "true": true,
        "unsigned": 4_u64,
        "signed": -4_i64,
        "string": "exact UTF-8 ☃",
        "array": [null, true, -1]
    });
    let encoded = canonical::canonical_binary(&authority)?;
    assert!(!encoded.is_empty());
    assert!(matches!(
        canonical::canonical_binary(&1.25_f64),
        Err(MemoryError::CanonicalEncoding(message)) if message.contains("floating point")
    ));

    let digest = Digest32::hash_prefixed(b"domain\0", &encoded);
    let text = digest.to_string();
    assert_eq!(Digest32::from_str(&text)?, digest);
    assert!(Digest32::from_str(&format!("{digest:?}")).is_err());
    assert!(Digest32::from_str("00").is_err());
    assert!(Digest32::from_str(&"g".repeat(64)).is_err());
    assert_eq!(
        serde_json::from_str::<Digest32>(&format!("\"{text}\""))
            .map_err(|error| MemoryError::CanonicalEncoding(error.to_string()))?,
        digest
    );
    assert!(serde_json::from_str::<Digest32>("\"invalid\"").is_err());

    let atom_id = AtomId(digest);
    assert_eq!(AtomId::from_str(&atom_id.to_string())?, atom_id);
    Ok(())
}

#[test]
fn fixed_point_helpers_cover_zero_rounding_and_saturation() -> Result<()> {
    assert_eq!(Ppm::ratio(5, 0), Ppm::ZERO);
    assert_eq!(Ppm::ONE.complement(), Ppm::ZERO);
    assert_eq!(Ppm::mean([]), Ppm::ZERO);
    assert_eq!(Ppm::mean([Ppm::ZERO, Ppm::ONE]).get(), 500_000);
    assert_eq!(
        Ppm::new(500_000)?.multiply(Ppm::new(500_000)?).get(),
        250_000
    );
    assert_eq!(Ppm::ZERO.ceil_count(100), 0);
    assert_eq!(Ppm::ONE.ceil_count(0), 0);
    assert_eq!(Ppm::try_from(7)?.get(), 7);
    assert_eq!(u32::from(Ppm::ONE), Ppm::SCALE);
    Ok(())
}

#[test]
fn seal_rejects_invalid_scope_time_sequence_and_content() {
    let policy = PolicyV1::poc_v1();
    let (buffer, request) = valid_episode();

    let mut changed = buffer.clone();
    changed.scope.memory_space_id.clear();
    assert_eq!(
        seal_episode(&changed, &request, &policy),
        Err(MemoryError::EmptyField("memory_space_id"))
    );
    let mut changed = buffer.clone();
    changed.scope.session_id.clear();
    assert_eq!(
        seal_episode(&changed, &request, &policy),
        Err(MemoryError::EmptyField("session_id"))
    );
    for (name, clear) in [
        ("repository_id", 0_u8),
        ("task_id", 1_u8),
        ("agent_id", 2_u8),
    ] {
        let mut changed = buffer.clone();
        match clear {
            0 => changed.scope.repository_id = Some(String::new()),
            1 => changed.scope.task_id = Some(String::new()),
            _ => changed.scope.agent_id = Some(String::new()),
        }
        assert_eq!(
            seal_episode(&changed, &request, &policy),
            Err(MemoryError::EmptyField(name))
        );
    }

    let mut changed = request.clone();
    changed.trigger.clear();
    assert_eq!(
        seal_episode(&buffer, &changed, &policy),
        Err(MemoryError::EmptyField("trigger"))
    );
    let mut changed = buffer.clone();
    changed.events.clear();
    assert_eq!(
        seal_episode(&changed, &request, &policy),
        Err(MemoryError::EmptyEpisode)
    );
    let mut changed = request.clone();
    changed.ended_at_us = 9;
    assert_eq!(
        seal_episode(&buffer, &changed, &policy),
        Err(MemoryError::InvalidTimeInterval)
    );
    let mut changed = request.clone();
    changed.as_of_us = 19;
    changed.ended_at_us = 20;
    assert_eq!(
        seal_episode(&buffer, &changed, &policy),
        Err(MemoryError::InvalidTimeInterval)
    );

    let mut changed = buffer.clone();
    changed.events[0].scope.session_id = "other".to_owned();
    assert_eq!(
        seal_episode(&changed, &request, &policy),
        Err(MemoryError::EventScopeMismatch { sequence: 7 })
    );
    let mut changed = buffer.clone();
    changed.events[0].at_us = 9;
    assert_eq!(
        seal_episode(&changed, &request, &policy),
        Err(MemoryError::EventTimeOutOfRange { sequence: 7 })
    );
    let mut changed = buffer.clone();
    changed.events[0].at_us = 21;
    assert_eq!(
        seal_episode(&changed, &request, &policy),
        Err(MemoryError::EventTimeOutOfRange { sequence: 7 })
    );
    let mut changed = buffer.clone();
    changed.events[0].sequence = u64::MAX;
    assert!(matches!(
        seal_episode(&changed, &request, &policy),
        Err(MemoryError::NonContiguousEvents { .. })
    ));
    let mut changed = buffer;
    changed.events[0].observation = None;
    assert_eq!(
        seal_episode(&changed, &request, &policy),
        Err(MemoryError::InsufficientEpisodeContent)
    );
}

#[test]
#[allow(
    clippy::unnecessary_wraps,
    reason = "this contract test shares the Result-returning test shape used by adjacent sealing cases"
)]
fn seal_rejects_invalid_artifacts_and_size_envelopes() -> Result<()> {
    let policy = PolicyV1::poc_v1();
    let (buffer, request) = valid_episode();
    let artifact = |size_bytes: u64, inline_payload: Option<Vec<u8>>| ArtifactRefV1 {
        digest: inline_payload
            .as_ref()
            .map_or(Digest32::ZERO, |bytes| Digest32::hash_prefixed(&[], bytes)),
        size_bytes,
        media_type: "application/octet-stream".to_owned(),
        retention_allowed: true,
        inline_payload,
    };

    let mut changed = request.clone();
    changed.artifacts = vec![artifact(policy.artifact_max_bytes + 1, None)];
    assert!(matches!(
        seal_episode(&buffer, &changed, &policy),
        Err(MemoryError::ArtifactTooLarge { .. })
    ));
    let large_inline =
        vec![7_u8; usize::try_from(policy.inline_payload_max_bytes + 1).unwrap_or(0)];
    let mut changed = request.clone();
    changed.artifacts = vec![artifact(
        policy.inline_payload_max_bytes + 1,
        Some(large_inline),
    )];
    assert!(matches!(
        seal_episode(&buffer, &changed, &policy),
        Err(MemoryError::InlinePayloadTooLarge { .. })
    ));
    let mut changed = request.clone();
    changed.artifacts = vec![ArtifactRefV1 {
        digest: Digest32::ZERO,
        size_bytes: 3,
        media_type: "application/octet-stream".to_owned(),
        retention_allowed: true,
        inline_payload: Some(vec![1, 2, 3]),
    }];
    assert!(matches!(
        seal_episode(&buffer, &changed, &policy),
        Err(MemoryError::ArtifactDigestMismatch { .. })
    ));

    let mut changed = request.clone();
    changed.retention_permission = RetentionPermissionV1::SemanticAndExactAllowed;
    changed.artifacts = vec![artifact(policy.exact_package_max_bytes, None)];
    assert!(matches!(
        seal_episode(&buffer, &changed, &policy),
        Err(MemoryError::ExactPackageTooLarge { .. })
    ));
    let mut changed = request.clone();
    changed.goal = Some("x".repeat(300_000));
    assert!(matches!(
        seal_episode(&buffer, &changed, &policy),
        Err(MemoryError::AtomTooLarge { .. })
    ));
    let mut changed = request;
    changed.goal = Some("x".repeat(140_000));
    assert!(matches!(
        seal_episode(&buffer, &changed, &policy),
        Err(MemoryError::EpisodeRequiresSplit { .. })
    ));
    Ok(())
}

#[test]
fn oversized_episode_is_deterministically_split_into_a_lossless_continuation_chain() -> Result<()> {
    let policy = PolicyV1::poc_v1();
    let (mut buffer, mut request) = valid_episode();
    let template = buffer
        .events
        .first()
        .cloned()
        .ok_or(MemoryError::EmptyEpisode)?;
    buffer.events = (0_u64..6)
        .map(|offset| {
            let mut event = template.clone();
            event.sequence = 7 + offset;
            event.at_us = 11 + offset;
            event.observation = Some(ObservationV1 {
                at_us: event.at_us,
                source: "synthetic-split-test".to_owned(),
                content: format!("{offset}:{}", "x".repeat(40_000)),
                artifact_digest: None,
            });
            event
        })
        .collect();
    request.ended_at_us = 20;
    request.as_of_us = 20;

    assert!(matches!(
        seal_episode(&buffer, &request, &policy),
        Err(MemoryError::EpisodeRequiresSplit { .. })
    ));
    let chain = seal_episode_chain(&buffer, &request, &policy)?;
    assert_eq!(chain, seal_episode_chain(&buffer, &request, &policy)?);
    assert!(chain.episodes.len() > 1);
    assert_eq!(chain.source_event_count, buffer.events.len());

    let expected_event_digests = buffer
        .events
        .iter()
        .map(|event| {
            Ok(Digest32::hash_prefixed(
                b"naome-memory:event:v1\0",
                &event.canonical_bytes()?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let observed_event_digests = chain
        .episodes
        .iter()
        .flat_map(|episode| episode.body.provenance.source_event_digests.iter().copied())
        .collect::<Vec<_>>();
    assert_eq!(observed_event_digests, expected_event_digests);

    let mut previous = buffer.continues;
    let mut next_sequence = 7_u64;
    for episode in &chain.episodes {
        assert!(episode.canonical_size_bytes <= policy.atom_target_bytes);
        assert!(!episode.target_size_exceeded);
        let MemoryPayloadV1::Episode(payload) = &episode.body.payload else {
            return Err(MemoryError::CanonicalEncoding(
                "episode chain contained semantic payload".to_owned(),
            ));
        };
        assert_eq!(payload.event_sequence_start, next_sequence);
        assert_eq!(payload.continues, previous);
        if let Some(previous) = previous {
            assert!(episode.body.relations.contains(&MemoryRelationV1 {
                kind: MemoryRelationKindV1::Continues,
                target: previous,
            }));
        }
        next_sequence = payload.event_sequence_end.saturating_add(1);
        previous = Some(episode.atom_id);
    }
    assert_eq!(next_sequence, 13);
    Ok(())
}

#[test]
fn chain_partition_preserves_a_complete_route_past_unanchored_tail_events() -> Result<()> {
    let policy = PolicyV1::poc_v1();
    let (mut buffer, mut request) = valid_episode();
    let template = buffer
        .events
        .first()
        .cloned()
        .ok_or(MemoryError::EmptyEpisode)?;
    buffer.events = (0_u64..4)
        .map(|offset| {
            let mut event = template.clone();
            event.sequence = 7 + offset;
            event.at_us = 11 + offset;
            if offset < 2 {
                event.observation = Some(ObservationV1 {
                    at_us: event.at_us,
                    source: "synthetic-complete-route-test".to_owned(),
                    content: format!("{offset}:{}", "x".repeat(38_000)),
                    artifact_digest: None,
                });
            } else {
                event.observation = None;
                event.interpretation = Some(InterpretationV1 {
                    content: format!("{offset}:{}", "y".repeat(38_000)),
                    confidence: Ppm::new(500_000).unwrap_or(Ppm::ZERO),
                    evidence: Vec::new(),
                });
            }
            event
        })
        .collect();
    let initial_previous = body(99, "previous").atom_id()?;
    buffer.continues = Some(initial_previous);
    request.relations = vec![MemoryRelationV1 {
        kind: MemoryRelationKindV1::Continues,
        target: initial_previous,
    }];
    request.ended_at_us = 20;
    request.as_of_us = 20;

    let chain = seal_episode_chain(&buffer, &request, &policy)?;
    assert_eq!(chain.episodes.len(), 2);
    let boundaries = chain
        .episodes
        .iter()
        .map(|episode| match &episode.body.payload {
            MemoryPayloadV1::Episode(payload) => {
                Ok((payload.event_sequence_start, payload.event_sequence_end))
            }
            MemoryPayloadV1::Semantic(_) => Err(MemoryError::InvalidAtomBody(
                "episode chain contained semantic payload",
            )),
        })
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(boundaries, vec![(7, 7), (8, 10)]);

    let mut previous = initial_previous;
    for episode in &chain.episodes {
        let MemoryPayloadV1::Episode(payload) = &episode.body.payload else {
            return Err(MemoryError::InvalidAtomBody(
                "episode chain contained semantic payload",
            ));
        };
        assert_eq!(payload.continues, Some(previous));
        assert_eq!(
            episode
                .body
                .relations
                .iter()
                .filter(|relation| relation.kind == MemoryRelationKindV1::Continues)
                .map(|relation| relation.target)
                .collect::<Vec<_>>(),
            vec![previous]
        );
        previous = episode.atom_id;
    }
    Ok(())
}

#[test]
fn chain_partition_handles_thousands_of_events_with_exact_stream_closure() -> Result<()> {
    let policy = PolicyV1::poc_v1();
    let (mut buffer, mut request) = valid_episode();
    let template = buffer
        .events
        .first()
        .cloned()
        .ok_or(MemoryError::EmptyEpisode)?;
    buffer.events = (0_u64..2_048)
        .map(|offset| {
            let mut event = template.clone();
            event.sequence = 7 + offset;
            event.at_us = 11 + offset;
            event.observation = Some(ObservationV1 {
                at_us: event.at_us,
                source: "synthetic-scale-test".to_owned(),
                content: format!("event-{offset}"),
                artifact_digest: None,
            });
            event
        })
        .collect();
    request.ended_at_us = 3_000;
    request.as_of_us = 3_000;

    let chain = seal_episode_chain(&buffer, &request, &policy)?;
    assert!(chain.episodes.len() > 1);
    assert_eq!(chain.source_event_count, 2_048);
    assert_eq!(
        chain
            .episodes
            .iter()
            .map(|episode| episode.body.provenance.source_event_digests.len())
            .sum::<usize>(),
        2_048
    );
    assert!(
        chain
            .episodes
            .iter()
            .all(|episode| episode.canonical_size_bytes <= policy.atom_target_bytes)
    );
    Ok(())
}

#[test]
fn chain_partition_fails_closed_when_one_event_cannot_fit_the_target() {
    let policy = PolicyV1::poc_v1();
    let (mut buffer, request) = valid_episode();
    buffer.events[0].observation = Some(ObservationV1 {
        at_us: buffer.events[0].at_us,
        source: "synthetic-indivisible-test".to_owned(),
        content: "x".repeat(140_000),
        artifact_digest: None,
    });
    assert!(matches!(
        seal_episode_chain(&buffer, &request, &policy),
        Err(MemoryError::EpisodeRequiresSplit { .. })
    ));
}

#[test]
fn seal_collects_rich_events_and_normalizes_continuation_relations() -> Result<()> {
    let policy = PolicyV1::poc_v1();
    let (mut buffer, mut request) = valid_episode();
    let previous = body(99, "previous").atom_id()?;
    buffer.continues = Some(previous);
    buffer.events[0].interpretation = Some(InterpretationV1 {
        content: "possible cause".to_owned(),
        confidence: Ppm::new(700_000)?,
        evidence: Vec::new(),
    });
    buffer.events[0].belief = Some(BeliefV1 {
        proposition: "bounded proposition".to_owned(),
        confidence: Ppm::new(600_000)?,
        evidence: Vec::new(),
    });
    buffer.events[0].decision = Some(DecisionRecordV1 {
        decision: "continue".to_owned(),
        rationale: "evidence".to_owned(),
    });
    buffer.events[0].action = Some(ActionRecordV1 {
        action: "record".to_owned(),
        result: Some("ok".to_owned()),
    });
    request.relations = vec![
        MemoryRelationV1 {
            kind: MemoryRelationKindV1::DerivedFrom,
            target: previous,
        },
        MemoryRelationV1 {
            kind: MemoryRelationKindV1::Supersedes,
            target: previous,
        },
        MemoryRelationV1 {
            kind: MemoryRelationKindV1::Contradicts,
            target: previous,
        },
        MemoryRelationV1 {
            kind: MemoryRelationKindV1::Supports,
            target: previous,
        },
        MemoryRelationV1 {
            kind: MemoryRelationKindV1::Continues,
            target: previous,
        },
    ];
    let sealed = seal_episode(&buffer, &request, &policy)?;
    assert_eq!(sealed.body.interpretations.len(), 1);
    assert_eq!(sealed.body.beliefs.len(), 1);
    assert_eq!(sealed.body.decisions.len(), 1);
    assert_eq!(sealed.body.actions.len(), 1);
    assert_eq!(sealed.body.relations.len(), 5);
    assert!(matches!(sealed.body.payload, MemoryPayloadV1::Episode(_)));
    Ok(())
}

#[test]
fn model_helpers_preserve_kind_artifact_and_state_contracts() -> Result<()> {
    let mut episode = body(1, "session-a");
    let first = Digest32::hash_prefixed(b"artifact\0", b"one");
    let second = Digest32::hash_prefixed(b"artifact\0", b"two");
    episode.artifacts = vec![
        ArtifactRefV1 {
            digest: second,
            size_bytes: 2,
            media_type: "test".to_owned(),
            retention_allowed: true,
            inline_payload: None,
        },
        ArtifactRefV1 {
            digest: first,
            size_bytes: 1,
            media_type: "test".to_owned(),
            retention_allowed: true,
            inline_payload: None,
        },
        ArtifactRefV1 {
            digest: second,
            size_bytes: 2,
            media_type: "test".to_owned(),
            retention_allowed: true,
            inline_payload: None,
        },
        ArtifactRefV1 {
            digest: Digest32::ZERO,
            size_bytes: 100,
            media_type: "test".to_owned(),
            retention_allowed: false,
            inline_payload: None,
        },
    ];
    assert_eq!(episode.kind(), MemoryKindV1::Episode);
    assert_eq!(episode.retained_artifact_digests(), vec![first, second]);
    assert!(episode.exact_package_bytes()? > episode.encoded_size_bytes()?);

    let mut artifact_overflow = episode.clone();
    artifact_overflow.artifacts[0].size_bytes = u64::MAX;
    artifact_overflow.artifacts[1].size_bytes = 1;
    assert!(matches!(
        artifact_overflow.exact_package_bytes(),
        Err(MemoryError::CanonicalEncoding(message)) if message.contains("artifact byte total")
    ));
    let mut package_overflow = episode;
    package_overflow.artifacts.truncate(1);
    package_overflow.artifacts[0].size_bytes = u64::MAX;
    assert!(matches!(
        package_overflow.exact_package_bytes(),
        Err(MemoryError::CanonicalEncoding(message)) if message.contains("exact package")
    ));

    let short = atom_state(RetentionTierV1::ShortTerm);
    assert!(!short.is_episodic_ltm());
    assert!(atom_state(RetentionTierV1::DualLongTerm).is_episodic_ltm());
    assert_eq!(short.validated_utility(), Ppm::new(10_000_000 / 11)?);
    Ok(())
}

fn retrieval_candidate(
    mut body: MemoryAtomBodyV1,
    tier: RetentionTierV1,
) -> Result<RetrievalCandidateV1> {
    body.scope.repository_id = Some("repo-a".to_owned());
    Ok(RetrievalCandidateV1 {
        atom_id: body.atom_id()?,
        body,
        state: atom_state(tier),
        lexical_score: Ppm::new(500_000)?,
        integrity: IntegrityStatusV1::Complete,
    })
}

fn query() -> RetrievalQueryV1 {
    RetrievalQueryV1 {
        contract_version: "retrieval-query-v1".to_owned(),
        memory_space_id: "space-a".to_owned(),
        repository_id: Some("repo-a".to_owned()),
        memory_space_wide: false,
        as_of_us: 1_000,
        terms: vec!["memory".to_owned()],
        topic_keys: BTreeSet::from(["memory".to_owned()]),
        entity_ids: BTreeSet::from(["naome".to_owned()]),
        k: Some(10),
        spontaneous: None,
    }
}

#[test]
fn retrieval_rejects_contract_limits_duplicates_and_tampering() -> Result<()> {
    let policy = PolicyV1::poc_v1();
    let first = retrieval_candidate(body(1, "one"), RetentionTierV1::EpisodicLongTerm)?;
    let empty = RetrievalCandidatesV1 {
        ranked: Vec::new(),
        episodic_pool: Vec::new(),
    };
    let mut changed = query();
    changed.contract_version = "retrieval-query-v2".to_owned();
    assert!(matches!(
        rank_retrieval(&changed, &empty, &policy),
        Err(MemoryError::UnknownVersion(_))
    ));
    let mut changed = query();
    changed.memory_space_id.clear();
    assert_eq!(
        rank_retrieval(&changed, &empty, &policy),
        Err(MemoryError::EmptyField("memory_space_id"))
    );
    let mut changed = query();
    changed.repository_id = None;
    assert_eq!(
        rank_retrieval(&changed, &empty, &policy),
        Err(MemoryError::EmptyField("repository_id"))
    );
    let mut changed = query();
    changed.k = Some(0);
    assert!(matches!(
        rank_retrieval(&changed, &empty, &policy),
        Err(MemoryError::RetrievalLimitExceeded { requested: 0, .. })
    ));
    let mut changed = query();
    changed.k = Some(policy.retrieval_max_k + 1);
    assert!(matches!(
        rank_retrieval(&changed, &empty, &policy),
        Err(MemoryError::RetrievalLimitExceeded { .. })
    ));
    let mut changed = query();
    changed.terms = vec![" \t".to_owned()];
    changed.topic_keys = BTreeSet::from([String::new()]);
    changed.entity_ids = BTreeSet::from(["  ".to_owned()]);
    changed.spontaneous = Some(SpontaneousRecallRequestV1 {
        seed: Seed32::new([3; 32]),
        budget: 1,
    });
    assert_eq!(
        rank_retrieval(&changed, &empty, &policy),
        Err(MemoryError::EmptyField("retrieval_query"))
    );

    let too_many = RetrievalCandidatesV1 {
        ranked: vec![first.clone(); policy.retrieval_candidate_max + 1],
        episodic_pool: Vec::new(),
    };
    assert!(matches!(
        rank_retrieval(&query(), &too_many, &policy),
        Err(MemoryError::CandidateLimitExceeded { .. })
    ));
    let duplicate = RetrievalCandidatesV1 {
        ranked: vec![first.clone(), first.clone()],
        episodic_pool: Vec::new(),
    };
    assert!(matches!(
        rank_retrieval(&query(), &duplicate, &policy),
        Err(MemoryError::DuplicateAtom { .. })
    ));
    let mut corrupted = first;
    corrupted.atom_id = AtomId::default();
    assert!(matches!(
        rank_retrieval(
            &query(),
            &RetrievalCandidatesV1 {
                ranked: vec![corrupted],
                episodic_pool: Vec::new(),
            },
            &policy,
        ),
        Err(MemoryError::AtomDigestMismatch { .. })
    ));

    let mut forgotten = retrieval_candidate(body(6, "forgotten"), RetentionTierV1::HeaderOnly)?;
    forgotten.state.content_status = ContentStatusV1::Forgotten;
    assert!(matches!(
        rank_retrieval(
            &query(),
            &RetrievalCandidatesV1 {
                ranked: vec![forgotten],
                episodic_pool: Vec::new(),
            },
            &policy,
        ),
        Err(MemoryError::InvalidRetrievalCandidate { .. })
    ));

    let wrong_tier = retrieval_candidate(body(7, "wrong tier"), RetentionTierV1::SemanticLongTerm)?;
    assert!(matches!(
        rank_retrieval(
            &query(),
            &RetrievalCandidatesV1 {
                ranked: vec![wrong_tier],
                episodic_pool: Vec::new(),
            },
            &policy,
        ),
        Err(MemoryError::InvalidRetrievalCandidate { .. })
    ));
    Ok(())
}

#[test]
fn retrieval_rejects_future_candidates_in_ranked_and_spontaneous_pools() -> Result<()> {
    let policy = PolicyV1::poc_v1();
    let future = retrieval_candidate(body(101, "future"), RetentionTierV1::EpisodicLongTerm)?;

    let ranked_error = rank_retrieval(
        &query(),
        &RetrievalCandidatesV1 {
            ranked: vec![future.clone()],
            episodic_pool: Vec::new(),
        },
        &policy,
    );
    assert!(matches!(
        ranked_error,
        Err(MemoryError::InvalidRetrievalCandidate {
            atom_id,
            reason: "candidate recorded_at_us is later than query as_of_us",
        }) if atom_id == future.atom_id
    ));

    let mut flashback_query = query();
    flashback_query.spontaneous = Some(SpontaneousRecallRequestV1 {
        seed: Seed32::new([4; 32]),
        budget: 1,
    });
    let spontaneous_error = rank_retrieval(
        &flashback_query,
        &RetrievalCandidatesV1 {
            ranked: Vec::new(),
            episodic_pool: vec![future.clone()],
        },
        &policy,
    );
    assert!(matches!(
        spontaneous_error,
        Err(MemoryError::InvalidRetrievalCandidate {
            atom_id,
            reason: "candidate recorded_at_us is later than query as_of_us",
        }) if atom_id == future.atom_id
    ));
    Ok(())
}

#[test]
fn retrieval_filters_scope_integrity_and_ranks_ties_by_atom_id() -> Result<()> {
    let policy = PolicyV1::poc_v1();
    let first = retrieval_candidate(body(1, "one"), RetentionTierV1::ShortTerm)?;
    let second = retrieval_candidate(body(2, "two"), RetentionTierV1::ShortTerm)?;
    let mut incomplete = retrieval_candidate(body(3, "three"), RetentionTierV1::ShortTerm)?;
    incomplete.integrity = IntegrityStatusV1::MissingArtifact;
    let mut other_space = retrieval_candidate(body(4, "four"), RetentionTierV1::ShortTerm)?;
    other_space.body.scope.memory_space_id = "space-b".to_owned();
    other_space.atom_id = other_space.body.atom_id()?;
    let mut other_repo = retrieval_candidate(body(5, "five"), RetentionTierV1::ShortTerm)?;
    other_repo.body.scope.repository_id = Some("repo-b".to_owned());
    other_repo.atom_id = other_repo.body.atom_id()?;

    let result = rank_retrieval(
        &query(),
        &RetrievalCandidatesV1 {
            ranked: vec![
                second.clone(),
                other_repo.clone(),
                incomplete,
                first.clone(),
                other_space,
            ],
            episodic_pool: Vec::new(),
        },
        &policy,
    )?;
    assert_eq!(result.hits.len(), 2);
    let mut expected = vec![first.atom_id, second.atom_id];
    expected.sort_unstable();
    assert_eq!(
        result
            .hits
            .iter()
            .map(|hit| hit.atom_id)
            .collect::<Vec<_>>(),
        expected
    );
    assert!(result.spontaneous.is_none());

    let mut wide = query();
    wide.memory_space_wide = true;
    wide.repository_id = None;
    wide.topic_keys.clear();
    wide.entity_ids.clear();
    wide.as_of_us = other_repo.body.interval.recorded_at_us + policy.retrieval_recency_horizon_us;
    wide.k = None;
    let result = rank_retrieval(
        &wide,
        &RetrievalCandidatesV1 {
            ranked: vec![other_repo],
            episodic_pool: Vec::new(),
        },
        &policy,
    )?;
    assert_eq!(result.hits.len(), 1);
    let score = result.hits[0]
        .score
        .ok_or(MemoryError::ReceiptRejected("missing score"))?;
    assert_eq!(score.semantic, Ppm::ZERO);
    assert_eq!(score.entity, Ppm::ZERO);
    assert_eq!(score.recency, Ppm::ZERO);
    Ok(())
}

#[test]
fn retrieval_flashback_budget_is_explicit_and_bounded() {
    let policy = PolicyV1::poc_v1();
    let empty = RetrievalCandidatesV1 {
        ranked: Vec::new(),
        episodic_pool: Vec::new(),
    };
    for budget in [0, policy.flashback_budget_max + 1] {
        let mut changed = query();
        changed.spontaneous = Some(SpontaneousRecallRequestV1 {
            seed: Seed32::new([2; 32]),
            budget,
        });
        assert!(matches!(
            rank_retrieval(&changed, &empty, &policy),
            Err(MemoryError::InvalidFlashbackBudget { .. })
        ));
    }
}

#[test]
fn retirement_rejects_unknown_duplicate_missing_and_tampered_candidates() -> Result<()> {
    let policy = PolicyV1::poc_v1();
    let seed = Seed32::new([1; 32]);
    let mut changed = snapshot()?;
    changed.contract_version = "retirement-snapshot-v2".to_owned();
    assert!(matches!(
        plan_retirement(&changed, &policy, seed),
        Err(MemoryError::UnknownVersion(_))
    ));
    let mut changed = snapshot()?;
    changed.memory_space_id.clear();
    assert_eq!(
        plan_retirement(&changed, &policy, seed),
        Err(MemoryError::EmptyField("memory_space_id"))
    );
    let mut changed = snapshot()?;
    changed.atoms.push(changed.atoms[0].clone());
    assert!(matches!(
        plan_retirement(&changed, &policy, seed),
        Err(MemoryError::DuplicateAtom { .. })
    ));
    let mut changed = snapshot()?;
    changed.atoms[0].body_digest = Digest32::ZERO;
    assert!(matches!(
        plan_retirement(&changed, &policy, seed),
        Err(MemoryError::AtomDigestMismatch { .. })
    ));
    let mut changed = snapshot()?;
    changed.atoms[0].body = None;
    assert!(matches!(
        plan_retirement(&changed, &policy, seed),
        Err(MemoryError::AtomDigestMismatch { .. })
    ));
    let mut changed = snapshot()?;
    changed.atoms[0].exact_package_bytes += 1;
    assert!(matches!(
        plan_retirement(&changed, &policy, seed),
        Err(MemoryError::AtomDigestMismatch { .. })
    ));

    let mut left = body(10, "left");
    let mut right = body(11, "right");
    left.topic_keys.clear();
    left.entity_ids.clear();
    left.outcome_class = None;
    right.topic_keys.clear();
    right.entity_ids.clear();
    right.outcome_class = Some("failure".to_owned());
    assert_eq!(semantic_similarity(&left, &right, &policy), Ppm::ZERO);
    Ok(())
}

#[test]
fn retirement_rejects_invalid_lifecycle_cohort_and_episode_state() -> Result<()> {
    let policy = PolicyV1::poc_v1();
    let seed = Seed32::new([19; 32]);
    let rejects_lifecycle = |changed: &RetirementSnapshotV1| {
        assert!(matches!(
            plan_retirement(changed, &policy, seed),
            Err(MemoryError::InvalidRetirementCandidate { .. })
        ));
    };

    let mut changed = snapshot()?;
    changed.atoms[0].state.retention_tier = RetentionTierV1::SemanticLongTerm;
    rejects_lifecycle(&changed);

    let mut changed = snapshot()?;
    changed.atoms[0].state.content_status = ContentStatusV1::Forgotten;
    rejects_lifecycle(&changed);

    let mut changed = snapshot()?;
    changed.atoms[0].state.retirement_run_id = Some(Digest32::hash_prefixed(b"run\0", b"one"));
    rejects_lifecycle(&changed);

    let mut changed = snapshot()?;
    changed.atoms[0].state.superseded_by = Some(changed.atoms[1].atom_id);
    rejects_lifecycle(&changed);

    let mut changed = snapshot()?;
    changed.atoms[0].state.expires_at_us = None;
    rejects_lifecycle(&changed);

    let mut changed = snapshot()?;
    changed.atoms[0].state.expires_at_us = changed.atoms[0]
        .state
        .expires_at_us
        .and_then(|value| value.checked_sub(1));
    rejects_lifecycle(&changed);

    let mut changed = snapshot()?;
    changed.atoms[0].state.expires_at_us = changed.atoms[0]
        .state
        .expires_at_us
        .and_then(|value| value.checked_add(1));
    rejects_lifecycle(&changed);

    let mut changed = snapshot()?;
    changed.atoms[0].state.expires_at_us = Some(2 * 86_400_000_000);
    rejects_lifecycle(&changed);

    let mut changed = snapshot()?;
    changed.atoms[0].state.expires_at_us = Some(RETIRE_AT_US + 1);
    rejects_lifecycle(&changed);

    let mut changed = snapshot()?;
    changed.atoms[0].integrity = IntegrityStatusV1::MissingArtifact;
    rejects_lifecycle(&changed);

    let mut changed = snapshot()?;
    let mut invalid_interval = body(90, "invalid-interval");
    invalid_interval.interval.started_at_us = 100;
    invalid_interval.interval.ended_at_us = 99;
    changed.atoms[0] = RetirementCandidateV1::from_present_body(
        invalid_interval,
        atom_state(RetentionTierV1::ShortTerm),
    )?;
    rejects_lifecycle(&changed);

    let mut changed = snapshot()?;
    let mut semantic = body(91, "semantic");
    semantic.payload = MemoryPayloadV1::Semantic(SemanticPayloadV1 {
        source_ids: Vec::new(),
        source_body_digests: BTreeMap::new(),
        score: ConsolidationScoreV1 {
            support: Ppm::ZERO,
            diversity: Ppm::ZERO,
            utility: Ppm::ZERO,
            salience: Ppm::ZERO,
            cohesion: Ppm::ZERO,
            novelty: Ppm::ZERO,
            uncertainty: Ppm::ZERO,
            total: Ppm::ZERO,
        },
        consensus_numerator: 2,
        consensus_denominator: 3,
        supersedes: None,
    });
    changed.atoms[0] =
        RetirementCandidateV1::from_present_body(semantic, atom_state(RetentionTierV1::ShortTerm))?;
    rejects_lifecycle(&changed);

    let mut changed = snapshot()?;
    let mut invalid_scope = body(92, "invalid-scope");
    invalid_scope.scope.session_id.clear();
    changed.atoms[0] = RetirementCandidateV1::from_present_body(
        invalid_scope,
        atom_state(RetentionTierV1::ShortTerm),
    )?;
    rejects_lifecycle(&changed);
    Ok(())
}

fn projected_digest(plan: &RetirementPlanV1) -> Result<Digest32> {
    plan.projected_post_state.digest()
}

fn outcome_for(plan: &RetirementPlanV1) -> ApplyRetirementOutcomeV1 {
    let closure = ReceiptClosureV1::from_plan(plan);
    ApplyRetirementOutcomeV1 {
        observed_input_digest: plan.input_set_digest,
        state_digest: plan.projected_state_digest,
        applied_decision_count: plan.decisions.len(),
        semantic_atom_ids: closure.semantic_atom_ids,
        episodic_atom_ids: closure.exact_episode_ids,
        exact_artifact_digests: closure.exact_artifact_digests,
        already_applied: false,
    }
}

struct OutcomeRepository(ApplyRetirementOutcomeV1);

impl RetirementRepositoryV1 for OutcomeRepository {
    fn apply_plan_atomically(
        &mut self,
        _plan: &RetirementPlanV1,
    ) -> Result<ApplyRetirementOutcomeV1> {
        Ok(self.0.clone())
    }
}

#[test]
fn apply_retirement_rejects_every_incomplete_repository_closure() -> Result<()> {
    let plan = plan_retirement(&snapshot()?, &PolicyV1::poc_v1(), Seed32::new([3; 32]))?;
    let mut stale = outcome_for(&plan);
    stale.observed_input_digest = Digest32::ZERO;
    assert!(matches!(
        apply_retirement(&mut OutcomeRepository(stale), &plan),
        Err(MemoryError::StalePlan { .. })
    ));
    let mut changed = outcome_for(&plan);
    changed.state_digest = Digest32::ZERO;
    assert_eq!(
        apply_retirement(&mut OutcomeRepository(changed), &plan),
        Err(MemoryError::InvalidApplyClosure("state digest mismatch"))
    );
    let mut changed = outcome_for(&plan);
    changed.applied_decision_count += 1;
    assert_eq!(
        apply_retirement(&mut OutcomeRepository(changed), &plan),
        Err(MemoryError::InvalidApplyClosure("decision count mismatch"))
    );
    let mut changed = outcome_for(&plan);
    changed.semantic_atom_ids.clear();
    assert!(matches!(
        apply_retirement(&mut OutcomeRepository(changed), &plan),
        Err(MemoryError::InvalidApplyClosure(_))
    ));
    Ok(())
}

#[test]
fn plan_structure_rejects_version_policy_state_order_authority_and_budgets() -> Result<()> {
    let plan = plan_retirement(&snapshot()?, &PolicyV1::poc_v1(), Seed32::new([4; 32]))?;
    let mut changed = plan.clone();
    changed.contract_version = "retirement-plan-v2".to_owned();
    assert!(matches!(
        verify_plan_structure(&changed),
        Err(MemoryError::UnknownVersion(_))
    ));
    let mut changed = plan.clone();
    changed.policy_id = "other".to_owned();
    assert_eq!(
        verify_plan_structure(&changed),
        Err(MemoryError::PolicyMismatch)
    );
    let mut changed = plan.clone();
    changed.projected_state_digest = Digest32::ZERO;
    assert!(matches!(
        verify_plan_structure(&changed),
        Err(MemoryError::ReceiptRejected(
            "projected state digest mismatch"
        ))
    ));
    let mut changed = plan.clone();
    changed.decisions.swap(0, 1);
    changed.projected_post_state.source_atoms.swap(0, 1);
    changed.projected_state_digest = projected_digest(&changed)?;
    assert!(matches!(
        verify_plan_structure(&changed),
        Err(MemoryError::ReceiptRejected(
            "decisions are not strictly atom-id ordered"
        ))
    ));
    let mut changed = plan.clone();
    if let Some(semantic) = changed.semantic_atoms.first_mut() {
        semantic.body_digest = Digest32::ZERO;
    }
    if let Some(semantic) = changed.projected_post_state.semantic_atoms.first_mut() {
        semantic.header_body_digest = Digest32::ZERO;
        semantic.stored_body_digest = Some(Digest32::ZERO);
    }
    changed.projected_state_digest = projected_digest(&changed)?;
    assert!(matches!(
        verify_plan_structure(&changed),
        Err(MemoryError::ReceiptRejected(
            "semantic atom authority mismatch"
        ))
    ));
    let mut changed = plan.clone();
    if let Some(evidence) = changed.semantic_decisions.first_mut() {
        evidence.source_ids.clear();
    }
    assert!(matches!(
        verify_plan_structure(&changed),
        Err(MemoryError::ReceiptRejected(
            "semantic candidate source closure is incomplete or unordered"
        ))
    ));
    let mut changed = plan.clone();
    changed.episodic_winner_budget = 0;
    assert!(matches!(
        verify_plan_structure(&changed),
        Err(MemoryError::ReceiptRejected(
            "lottery or semantic budget mismatch"
        ))
    ));
    Ok(())
}

#[test]
fn receipt_verifier_rejects_tampered_inputs_artifacts_and_statuses() -> Result<()> {
    let policy = PolicyV1::poc_v1();
    let snapshot = snapshot()?;
    let seed = Seed32::new([5; 32]);
    let plan = plan_retirement(&snapshot, &policy, seed)?;
    let mut repository = OutcomeRepository(outcome_for(&plan));
    let receipt = apply_retirement(&mut repository, &plan)?;
    let inputs = ReceiptVerificationInputsV1 {
        policy,
        snapshot,
        seed,
        observed_state_digest: plan.projected_state_digest,
        observed_exact_artifact_digests: receipt.closure.exact_artifact_digests.clone(),
    };

    let mut changed = receipt.clone();
    changed.contract_version = "proof-receipt-v2".to_owned();
    assert!(matches!(
        verify_receipt(&changed, &inputs),
        Err(MemoryError::UnknownVersion(_))
    ));
    let mut changed = receipt.clone();
    changed.receipt_id = Digest32::ZERO;
    assert!(matches!(
        verify_receipt(&changed, &inputs),
        Err(MemoryError::ReceiptRejected("receipt id mismatch"))
    ));
    let mut changed = receipt.clone();
    changed.seed = Seed32::new([6; 32]);
    changed.receipt_id = changed.recompute_receipt_id()?;
    assert!(matches!(
        verify_receipt(&changed, &inputs),
        Err(MemoryError::ReceiptRejected(
            "input, policy, seed, decision, state, or closure mismatch"
        ))
    ));
    for mut changed in [
        {
            let mut value = receipt.clone();
            value.run_id = Digest32::ZERO;
            value
        },
        {
            let mut value = receipt.clone();
            value.memory_space_id = "other-space".to_owned();
            value
        },
        {
            let mut value = receipt.clone();
            value.cohort_day = value.cohort_day.saturating_add(1);
            value
        },
        {
            let mut value = receipt.clone();
            value.as_of_us = value.as_of_us.saturating_add(1);
            value
        },
        {
            let mut value = receipt.clone();
            value.policy_id = "other-policy".to_owned();
            value
        },
        {
            let mut value = receipt.clone();
            value.policy_digest = Digest32::ZERO;
            value
        },
    ] {
        changed.receipt_id = changed.recompute_receipt_id()?;
        assert!(matches!(
            verify_receipt(&changed, &inputs),
            Err(MemoryError::ReceiptRejected(
                "input, policy, seed, decision, state, or closure mismatch"
            ))
        ));
    }
    let mut missing_artifacts = inputs.clone();
    missing_artifacts.observed_exact_artifact_digests.clear();
    assert!(matches!(
        verify_receipt(&receipt, &missing_artifacts),
        Err(MemoryError::ReceiptRejected(
            "observed exact artifacts do not close"
        ))
    ));
    let mut changed = receipt.clone();
    changed.overall_status = OverallProofStatusV1::Failed;
    changed.receipt_id = changed.recompute_receipt_id()?;
    assert!(matches!(
        verify_receipt(&changed, &inputs),
        Err(MemoryError::ReceiptRejected(
            "receipt does not claim verified invariants"
        ))
    ));
    let mut changed = receipt;
    if let Some(status) = changed.invariants.values_mut().next() {
        *status = InvariantStatusV1::Rejected;
    }
    changed.receipt_id = changed.recompute_receipt_id()?;
    assert!(matches!(
        verify_receipt(&changed, &inputs),
        Err(MemoryError::ReceiptRejected(
            "receipt does not claim verified invariants"
        ))
    ));
    Ok(())
}

#[test]
fn semantic_consensus_counts_each_source_atom_once() -> Result<()> {
    let mut candidate_bodies = (0_u64..3)
        .map(|index| body(index, &format!("session-{index}")))
        .collect::<Vec<_>>();
    candidate_bodies[0].plan = vec!["single-source".to_owned(); 3];
    candidate_bodies[1].plan = vec!["two-sources".to_owned()];
    candidate_bodies[2].plan = vec!["two-sources".to_owned()];
    let atoms = candidate_bodies
        .into_iter()
        .map(|body| {
            let mut state = atom_state(RetentionTierV1::ShortTerm);
            state.expires_at_us =
                Some(body.interval.recorded_at_us + PolicyV1::poc_v1().stm_horizon_us);
            RetirementCandidateV1::from_present_body(body, state)
        })
        .collect::<Result<Vec<_>>>()?;
    let plan = plan_retirement(
        &RetirementSnapshotV1 {
            contract_version: "retirement-snapshot-v1".to_owned(),
            memory_space_id: "space-a".to_owned(),
            cohort_day: 7,
            as_of_us: RETIRE_AT_US,
            atoms,
        },
        &PolicyV1::poc_v1(),
        Seed32::new([71; 32]),
    )?;
    let semantic = plan
        .semantic_atoms
        .first()
        .ok_or(MemoryError::ReceiptRejected("missing semantic atom"))?;
    assert!(
        !semantic
            .body
            .plan
            .iter()
            .any(|value| value == "single-source")
    );
    assert!(
        semantic
            .body
            .plan
            .iter()
            .any(|value| value == "two-sources")
    );
    Ok(())
}

#[test]
fn complete_atom_validation_closes_observations_relations_policy_and_inline_retention() -> Result<()>
{
    let policy = PolicyV1::poc_v1();
    let (mut buffer, request) = valid_episode();
    let undeclared = Digest32::hash_prefixed(b"undeclared-artifact\0", b"bytes");
    buffer.events[0]
        .observation
        .as_mut()
        .ok_or(MemoryError::InsufficientEpisodeContent)?
        .artifact_digest = Some(undeclared);
    assert!(matches!(
        seal_episode(&buffer, &request, &policy),
        Err(MemoryError::UndeclaredArtifactReference { digest }) if digest == undeclared
    ));

    let (buffer, mut request) = valid_episode();
    let target = AtomId(Digest32::hash_prefixed(b"relation-target\0", b"target"));
    let duplicate = MemoryRelationV1 {
        kind: MemoryRelationKindV1::Supports,
        target,
    };
    request.relations = vec![duplicate.clone(), duplicate];
    assert!(matches!(
        seal_episode(&buffer, &request, &policy),
        Err(MemoryError::InvalidAtomBody(
            "relations must be unique by kind and target"
        ))
    ));

    let (buffer, request) = valid_episode();
    let mut body = seal_episode(&buffer, &request, &policy)?.body;
    body.observations[0].at_us = body.interval.ended_at_us.saturating_add(1);
    assert!(matches!(
        validate_memory_atom_body(&body, &policy),
        Err(MemoryError::InvalidAtomBody(
            "observation time lies outside the atom interval"
        ))
    ));
    body.observations[0].at_us = body.interval.started_at_us;
    body.provenance.policy_digest = Digest32::ZERO;
    assert!(matches!(
        validate_memory_atom_body(&body, &policy),
        Err(MemoryError::PolicyMismatch)
    ));

    let (buffer, mut request) = valid_episode();
    let inline = b"transient-inline".to_vec();
    request.retention_permission = RetentionPermissionV1::SemanticAndExactAllowed;
    request.artifacts = vec![ArtifactRefV1 {
        digest: Digest32::hash_prefixed(&[], &inline),
        size_bytes: u64::try_from(inline.len()).unwrap_or(u64::MAX),
        media_type: "application/octet-stream".to_owned(),
        retention_allowed: false,
        inline_payload: Some(inline),
    }];
    assert!(matches!(
        seal_episode(&buffer, &request, &policy),
        Err(MemoryError::InvalidAtomBody(
            "exact-eligible atoms cannot embed a retention-disallowed inline payload"
        ))
    ));
    Ok(())
}

#[test]
fn retirement_requires_a_nonempty_closed_utc_day_cohort() -> Result<()> {
    let policy = PolicyV1::poc_v1();
    let seed = Seed32::new([72; 32]);
    let mut early = snapshot()?;
    early.as_of_us = RETIRE_AT_US.saturating_sub(1);
    assert!(matches!(
        plan_retirement(&early, &policy, seed),
        Err(MemoryError::RetirementCohortNotClosed {
            as_of_us,
            cohort_end_us
        }) if as_of_us == RETIRE_AT_US - 1 && cohort_end_us == RETIRE_AT_US
    ));

    let mut empty = snapshot()?;
    empty.atoms.clear();
    assert!(matches!(
        plan_retirement(&empty, &policy, seed),
        Err(MemoryError::EmptyRetirementCohort)
    ));
    Ok(())
}
