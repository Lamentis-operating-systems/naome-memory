#![allow(clippy::too_many_lines)]

use naome_memory_core::*;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

fn scope() -> MemoryScopeV1 {
    MemoryScopeV1 {
        memory_space_id: "space-a".to_owned(),
        repository_id: Some("repo-a".to_owned()),
        task_id: Some("task-a".to_owned()),
        agent_id: Some("agent-a".to_owned()),
        session_id: "session-a".to_owned(),
    }
}

fn episode_body(policy: &PolicyV1) -> Result<MemoryAtomBodyV1> {
    Ok(MemoryAtomBodyV1 {
        contract_version: "atom-v1".to_owned(),
        scope: scope(),
        interval: TimeIntervalV1 {
            started_at_us: 10,
            ended_at_us: 11,
            recorded_at_us: 12,
        },
        trigger: "checkpoint".to_owned(),
        seal_reason: SealReasonV1::Checkpoint,
        goal: Some("preserve validated facts".to_owned()),
        plan: vec!["validate".to_owned()],
        internal_state_before: BTreeMap::new(),
        internal_state_after: BTreeMap::new(),
        observations: vec![ObservationV1 {
            at_us: 10,
            source: "synthetic".to_owned(),
            content: "bounded observation".to_owned(),
            artifact_digest: None,
        }],
        interpretations: Vec::new(),
        beliefs: Vec::new(),
        decisions: Vec::new(),
        rejected_alternatives: Vec::new(),
        actions: Vec::new(),
        outcome: Some(OutcomeV1 {
            class: "success".to_owned(),
            summary: "validated".to_owned(),
            succeeded: true,
        }),
        feedback: Vec::new(),
        formation_signals: FormationSignalsV1::default(),
        topic_keys: BTreeSet::from(["validation".to_owned()]),
        entity_ids: BTreeSet::from(["naome".to_owned()]),
        outcome_class: Some("success".to_owned()),
        goal_key: Some("memory-validation".to_owned()),
        provenance: ProvenanceV1 {
            producer: "coverage-validation-contract".to_owned(),
            source_event_digests: vec![Digest32::hash_prefixed(b"event\0", b"0")],
            policy_digest: policy.digest()?,
        },
        relations: Vec::new(),
        artifacts: Vec::new(),
        retention_permission: RetentionPermissionV1::SemanticAndExactAllowed,
        payload: MemoryPayloadV1::Episode(EpisodePayloadV1 {
            event_sequence_start: 0,
            event_sequence_end: 0,
            continues: None,
        }),
    })
}

fn semantic_sources() -> Vec<AtomId> {
    (1_u8..=3)
        .map(|byte| AtomId(Digest32([byte; 32])))
        .collect()
}

fn semantic_body(policy: &PolicyV1) -> Result<MemoryAtomBodyV1> {
    let source_ids = semantic_sources();
    let source_body_digests = source_ids
        .iter()
        .copied()
        .map(|source| (source, source.digest()))
        .collect();
    Ok(MemoryAtomBodyV1 {
        contract_version: "atom-v1".to_owned(),
        scope: scope(),
        interval: TimeIntervalV1 {
            started_at_us: 10,
            ended_at_us: 20,
            recorded_at_us: 30,
        },
        trigger: "retirement".to_owned(),
        seal_reason: SealReasonV1::Timeout,
        goal: None,
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
        topic_keys: BTreeSet::from(["validation".to_owned()]),
        entity_ids: BTreeSet::from(["naome".to_owned()]),
        outcome_class: None,
        goal_key: Some("memory-validation".to_owned()),
        provenance: ProvenanceV1 {
            producer: "coverage-validation-contract".to_owned(),
            source_event_digests: source_ids.iter().map(|source| source.digest()).collect(),
            policy_digest: policy.digest()?,
        },
        relations: source_ids
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
            source_ids,
            source_body_digests,
            score: ConsolidationScoreV1 {
                support: Ppm::ONE,
                diversity: Ppm::ONE,
                utility: Ppm::ONE,
                salience: Ppm::ONE,
                cohesion: Ppm::ONE,
                novelty: Ppm::ONE,
                uncertainty: Ppm::ZERO,
                total: Ppm::ONE,
            },
            consensus_numerator: policy.consensus_numerator,
            consensus_denominator: policy.consensus_denominator,
            supersedes: None,
        }),
    })
}

fn artifact(bytes: &[u8], retention_allowed: bool) -> ArtifactRefV1 {
    ArtifactRefV1 {
        digest: Digest32::hash_prefixed(&[], bytes),
        size_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        media_type: "application/octet-stream".to_owned(),
        retention_allowed,
        inline_payload: Some(bytes.to_vec()),
    }
}

#[test]
fn canonical_authority_rejects_floats_and_non_string_digests() -> Result<()> {
    let authority = json!({
        "negative": -7,
        "positive": 7_u64,
        "sequence": [null, false, true, "exact UTF-8 ☃"],
    });
    assert!(!canonical::canonical_binary(&authority)?.is_empty());
    assert!(matches!(
        canonical::canonical_binary(&1.5_f64),
        Err(MemoryError::CanonicalEncoding(message))
            if message == "floating point values are forbidden"
    ));

    // This deliberately exercises the deserializer's type expectation rather
    // than accepting an integer-shaped digest authority.
    assert!(serde_json::from_value::<Digest32>(json!(7)).is_err());
    Ok(())
}

#[test]
fn model_helpers_preserve_kind_and_exact_artifact_accounting() -> Result<()> {
    let policy = PolicyV1::poc_v1();
    let episode = episode_body(&policy)?;
    let semantic = semantic_body(&policy)?;
    assert_eq!(episode.kind(), MemoryKindV1::Episode);
    assert_eq!(semantic.kind(), MemoryKindV1::Semantic);
    assert_ne!(episode.atom_id()?, semantic.atom_id()?);

    let retained = Digest32([1; 32]);
    let ignored = Digest32([2; 32]);
    let mut with_artifacts = episode;
    with_artifacts.artifacts = vec![
        ArtifactRefV1 {
            digest: retained,
            size_bytes: 1,
            media_type: "application/octet-stream".to_owned(),
            retention_allowed: true,
            inline_payload: None,
        },
        ArtifactRefV1 {
            digest: ignored,
            size_bytes: 1,
            media_type: "application/octet-stream".to_owned(),
            retention_allowed: false,
            inline_payload: None,
        },
        ArtifactRefV1 {
            digest: retained,
            size_bytes: 1,
            media_type: "application/octet-stream".to_owned(),
            retention_allowed: true,
            inline_payload: None,
        },
    ];
    assert_eq!(with_artifacts.retained_artifact_digests(), vec![retained]);

    with_artifacts.artifacts = vec![
        ArtifactRefV1 {
            digest: Digest32([3; 32]),
            size_bytes: u64::MAX,
            media_type: "application/octet-stream".to_owned(),
            retention_allowed: true,
            inline_payload: None,
        },
        ArtifactRefV1 {
            digest: Digest32([4; 32]),
            size_bytes: 1,
            media_type: "application/octet-stream".to_owned(),
            retention_allowed: true,
            inline_payload: None,
        },
    ];
    assert!(matches!(
        with_artifacts.exact_package_bytes(),
        Err(MemoryError::CanonicalEncoding(message))
            if message == "artifact byte total overflowed u64"
    ));
    Ok(())
}

#[test]
fn episode_authority_fails_closed_for_header_scope_time_and_content() -> Result<()> {
    let policy = PolicyV1::poc_v1();
    let baseline = episode_body(&policy)?;
    validate_memory_atom_body(&baseline, &policy)?;

    let mut changed = baseline.clone();
    changed.contract_version = "atom-v2".to_owned();
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::UnknownVersion("atom-v2".to_owned()))
    );

    let mut changed = baseline.clone();
    changed.trigger.clear();
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::EmptyField("trigger"))
    );
    let mut changed = baseline.clone();
    changed.provenance.producer.clear();
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::EmptyField("provenance.producer"))
    );

    for (field, mutate) in [
        ("memory_space_id", 0_u8),
        ("session_id", 1),
        ("repository_id", 2),
        ("task_id", 3),
        ("agent_id", 4),
    ] {
        let mut changed = baseline.clone();
        match mutate {
            0 => changed.scope.memory_space_id.clear(),
            1 => changed.scope.session_id.clear(),
            2 => changed.scope.repository_id = Some(String::new()),
            3 => changed.scope.task_id = Some(String::new()),
            _ => changed.scope.agent_id = Some(String::new()),
        }
        assert_eq!(
            validate_memory_atom_body(&changed, &policy),
            Err(MemoryError::EmptyField(field))
        );
    }

    let mut changed = baseline.clone();
    changed.interval.started_at_us = changed.interval.ended_at_us + 1;
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::InvalidTimeInterval)
    );

    for clear_source in [true, false] {
        let mut changed = baseline.clone();
        if clear_source {
            changed.observations[0].source.clear();
        } else {
            changed.observations[0].content.clear();
        }
        assert_eq!(
            validate_memory_atom_body(&changed, &policy),
            Err(MemoryError::InvalidAtomBody(
                "observation source and content must not be empty"
            ))
        );
    }

    let mut changed = baseline.clone();
    changed.outcome_class = Some("failure".to_owned());
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::InvalidAtomBody(
            "outcome_class does not match the episode outcome"
        ))
    );
    Ok(())
}

#[test]
fn episode_sequence_and_relation_closure_cannot_be_forged() -> Result<()> {
    let policy = PolicyV1::poc_v1();
    let baseline = episode_body(&policy)?;

    let mut changed = baseline.clone();
    let MemoryPayloadV1::Episode(payload) = &mut changed.payload else {
        return Err(MemoryError::InvalidAtomBody("expected episode payload"));
    };
    payload.event_sequence_start = 2;
    payload.event_sequence_end = 1;
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::InvalidAtomBody(
            "episode event sequence is inverted"
        ))
    );

    let mut changed = baseline.clone();
    let MemoryPayloadV1::Episode(payload) = &mut changed.payload else {
        return Err(MemoryError::InvalidAtomBody("expected episode payload"));
    };
    payload.event_sequence_end = 1;
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::InvalidAtomBody(
            "episode provenance must contain one digest per event"
        ))
    );

    let mut changed = baseline.clone();
    let event_digest = changed.provenance.source_event_digests[0];
    changed.provenance.source_event_digests.push(event_digest);
    let MemoryPayloadV1::Episode(payload) = &mut changed.payload else {
        return Err(MemoryError::InvalidAtomBody("expected episode payload"));
    };
    payload.event_sequence_end = 1;
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::InvalidAtomBody(
            "episode source event digests must be unique"
        ))
    );

    let mut changed = baseline.clone();
    changed.observations.clear();
    changed.outcome = None;
    changed.outcome_class = None;
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::InsufficientEpisodeContent)
    );

    let continuation = AtomId(Digest32([9; 32]));
    let mut changed = baseline.clone();
    let MemoryPayloadV1::Episode(payload) = &mut changed.payload else {
        return Err(MemoryError::InvalidAtomBody("expected episode payload"));
    };
    payload.continues = Some(continuation);
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::InvalidAtomBody(
            "episode payload and continues relation must match exactly"
        ))
    );

    // Non-continuation edges are valid provenance edges and exercise every
    // relation-kind discriminator without weakening the continuation closure.
    let mut changed = baseline;
    changed.relations = [
        MemoryRelationKindV1::Supports,
        MemoryRelationKindV1::Contradicts,
        MemoryRelationKindV1::Supersedes,
        MemoryRelationKindV1::DerivedFrom,
    ]
    .into_iter()
    .enumerate()
    .map(|(index, kind)| MemoryRelationV1 {
        kind,
        target: AtomId(Digest32([u8::try_from(index + 1).unwrap_or(u8::MAX); 32])),
    })
    .collect();
    validate_memory_atom_body(&changed, &policy)?;
    Ok(())
}

#[test]
fn artifact_authority_closes_references_digests_and_byte_budgets() -> Result<()> {
    let policy = PolicyV1::poc_v1();
    let baseline = episode_body(&policy)?;

    let mut changed = baseline.clone();
    let undeclared = Digest32([7; 32]);
    changed.observations[0].artifact_digest = Some(undeclared);
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::UndeclaredArtifactReference { digest: undeclared })
    );

    let valid = artifact(b"proof bytes", true);
    let mut changed = baseline.clone();
    changed.observations[0].artifact_digest = Some(valid.digest);
    changed.artifacts.push(valid.clone());
    validate_memory_atom_body(&changed, &policy)?;

    let mut changed = baseline.clone();
    changed.artifacts = vec![valid.clone(), valid.clone()];
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::InvalidAtomBody(
            "artifact digests must be unique within an atom"
        ))
    );

    let mut changed = baseline.clone();
    let mut invalid = valid.clone();
    invalid.media_type.clear();
    changed.artifacts = vec![invalid];
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::EmptyField("artifact.media_type"))
    );

    let mut changed = baseline.clone();
    let mut invalid = valid.clone();
    invalid.inline_payload = None;
    invalid.size_bytes = policy.artifact_max_bytes + 1;
    changed.artifacts = vec![invalid];
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::ArtifactTooLarge {
            actual: policy.artifact_max_bytes + 1,
            maximum: policy.artifact_max_bytes,
        })
    );

    let oversized_inline = vec![0_u8; 4_097];
    let mut changed = baseline.clone();
    changed.artifacts = vec![artifact(&oversized_inline, true)];
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::InlinePayloadTooLarge {
            actual: 4_097,
            maximum: policy.inline_payload_max_bytes,
        })
    );

    let mut changed = baseline.clone();
    let mut invalid = artifact(b"proof bytes", true);
    invalid.digest = Digest32::ZERO;
    let invalid_digest = invalid.digest;
    changed.artifacts = vec![invalid];
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::ArtifactDigestMismatch {
            digest: invalid_digest,
        })
    );

    let mut changed = baseline.clone();
    changed.artifacts = vec![artifact(b"transient inline", false)];
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::InvalidAtomBody(
            "exact-eligible atoms cannot embed a retention-disallowed inline payload"
        ))
    );

    let mut changed = baseline;
    changed.artifacts = vec![ArtifactRefV1 {
        digest: Digest32([8; 32]),
        size_bytes: policy.artifact_max_bytes,
        media_type: "application/octet-stream".to_owned(),
        retention_allowed: true,
        inline_payload: None,
    }];
    let actual = changed.exact_package_bytes()?;
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::ExactPackageTooLarge {
            actual,
            maximum: policy.exact_package_max_bytes,
        })
    );
    Ok(())
}

#[test]
fn semantic_authority_requires_complete_sorted_source_closure() -> Result<()> {
    let policy = PolicyV1::poc_v1();
    let baseline = semantic_body(&policy)?;
    validate_memory_atom_body(&baseline, &policy)?;

    let mut changed = baseline.clone();
    changed.retention_permission = RetentionPermissionV1::SemanticAndExactAllowed;
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::InvalidAtomBody(
            "semantic atoms require semantic_allowed retention"
        ))
    );

    let mut changed = baseline.clone();
    changed.observations.push(ObservationV1 {
        at_us: 10,
        source: "forbidden".to_owned(),
        content: "semantic atoms do not carry episodes".to_owned(),
        artifact_digest: None,
    });
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::InvalidAtomBody(
            "semantic atoms may contain only deterministic structured consolidation fields"
        ))
    );

    let mut changed = baseline.clone();
    let MemoryPayloadV1::Semantic(payload) = &mut changed.payload else {
        return Err(MemoryError::InvalidAtomBody("expected semantic payload"));
    };
    payload.source_ids.truncate(2);
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::InvalidAtomBody(
            "semantic atom has too few sources"
        ))
    );

    let mut changed = baseline.clone();
    let MemoryPayloadV1::Semantic(payload) = &mut changed.payload else {
        return Err(MemoryError::InvalidAtomBody("expected semantic payload"));
    };
    payload.source_ids.swap(0, 1);
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::InvalidAtomBody(
            "semantic source IDs must be unique and sorted"
        ))
    );

    let mut changed = baseline.clone();
    let MemoryPayloadV1::Semantic(payload) = &mut changed.payload else {
        return Err(MemoryError::InvalidAtomBody("expected semantic payload"));
    };
    payload
        .source_body_digests
        .insert(payload.source_ids[0], Digest32::ZERO);
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::InvalidAtomBody(
            "semantic source body digest closure is incomplete"
        ))
    );

    let mut changed = baseline.clone();
    changed.provenance.source_event_digests.swap(0, 1);
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::InvalidAtomBody(
            "semantic provenance digest order does not match source IDs"
        ))
    );

    let mut changed = baseline.clone();
    let MemoryPayloadV1::Semantic(payload) = &mut changed.payload else {
        return Err(MemoryError::InvalidAtomBody("expected semantic payload"));
    };
    payload.consensus_numerator += 1;
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::InvalidAtomBody(
            "semantic consensus ratio does not match policy"
        ))
    );

    let mut changed = baseline.clone();
    changed.relations.pop();
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::InvalidAtomBody(
            "semantic DerivedFrom relations do not match source IDs"
        ))
    );

    let mut changed = baseline.clone();
    let superseded = AtomId(Digest32([9; 32]));
    let MemoryPayloadV1::Semantic(payload) = &mut changed.payload else {
        return Err(MemoryError::InvalidAtomBody("expected semantic payload"));
    };
    payload.supersedes = Some(superseded);
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::InvalidAtomBody(
            "semantic relations must be exactly DerivedFrom plus optional Supersedes"
        ))
    );

    let mut changed = baseline;
    changed.relations.push(MemoryRelationV1 {
        kind: MemoryRelationKindV1::Supports,
        target: AtomId(Digest32([10; 32])),
    });
    assert_eq!(
        validate_memory_atom_body(&changed, &policy),
        Err(MemoryError::InvalidAtomBody(
            "semantic relations must be exactly DerivedFrom plus optional Supersedes"
        ))
    );
    Ok(())
}
