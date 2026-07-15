use std::collections::BTreeSet;

use crate::{
    ArtifactRefV1, MemoryAtomBodyV1, MemoryError, MemoryPayloadV1, MemoryRelationKindV1,
    MemoryScopeV1, PolicyV1, Result, RetentionPermissionV1,
};

/// Validate the complete immutable `atom-v1` authority body against a committed
/// policy. Every sealing and typed persistence path uses this boundary.
pub fn validate_memory_atom_body(body: &MemoryAtomBodyV1, policy: &PolicyV1) -> Result<()> {
    policy.validate_poc_v1()?;
    if body.contract_version != "atom-v1" {
        return Err(MemoryError::UnknownVersion(body.contract_version.clone()));
    }
    validate_scope(&body.scope)?;
    if body.trigger.is_empty() {
        return Err(MemoryError::EmptyField("trigger"));
    }
    if body.provenance.producer.is_empty() {
        return Err(MemoryError::EmptyField("provenance.producer"));
    }
    if body.provenance.policy_digest != policy.digest()? {
        return Err(MemoryError::PolicyMismatch);
    }
    if body.interval.started_at_us > body.interval.ended_at_us
        || body.interval.ended_at_us > body.interval.recorded_at_us
    {
        return Err(MemoryError::InvalidTimeInterval);
    }
    if body.observations.iter().any(|observation| {
        observation.at_us < body.interval.started_at_us
            || observation.at_us > body.interval.ended_at_us
    }) {
        return Err(MemoryError::InvalidAtomBody(
            "observation time lies outside the atom interval",
        ));
    }
    if body
        .observations
        .iter()
        .any(|observation| observation.source.is_empty() || observation.content.is_empty())
    {
        return Err(MemoryError::InvalidAtomBody(
            "observation source and content must not be empty",
        ));
    }
    if let (Some(outcome), Some(outcome_class)) = (&body.outcome, &body.outcome_class)
        && outcome.class != *outcome_class
    {
        return Err(MemoryError::InvalidAtomBody(
            "outcome_class does not match the episode outcome",
        ));
    }

    validate_artifacts(&body.artifacts, policy)?;
    let declared_artifacts = body
        .artifacts
        .iter()
        .map(|artifact| artifact.digest)
        .collect::<BTreeSet<_>>();
    for digest in body
        .observations
        .iter()
        .filter_map(|observation| observation.artifact_digest)
    {
        if !declared_artifacts.contains(&digest) {
            return Err(MemoryError::UndeclaredArtifactReference { digest });
        }
    }

    let relation_keys = body
        .relations
        .iter()
        .map(|relation| (relation_kind_key(relation.kind), relation.target))
        .collect::<BTreeSet<_>>();
    if relation_keys.len() != body.relations.len() {
        return Err(MemoryError::InvalidAtomBody(
            "relations must be unique by kind and target",
        ));
    }

    match &body.payload {
        MemoryPayloadV1::Episode(payload) => validate_episode_body(body, payload)?,
        MemoryPayloadV1::Semantic(payload) => validate_semantic_body(body, payload, policy)?,
    }

    let canonical_size_bytes = body.encoded_size_bytes()?;
    if canonical_size_bytes > policy.atom_hard_max_bytes {
        return Err(MemoryError::AtomTooLarge {
            actual: canonical_size_bytes,
            maximum: policy.atom_hard_max_bytes,
        });
    }
    if body.retention_permission == RetentionPermissionV1::SemanticAndExactAllowed {
        if body
            .artifacts
            .iter()
            .any(|artifact| artifact.inline_payload.is_some() && !artifact.retention_allowed)
        {
            return Err(MemoryError::InvalidAtomBody(
                "exact-eligible atoms cannot embed a retention-disallowed inline payload",
            ));
        }
        let exact_package_bytes = body.exact_package_bytes()?;
        if exact_package_bytes > policy.exact_package_max_bytes {
            return Err(MemoryError::ExactPackageTooLarge {
                actual: exact_package_bytes,
                maximum: policy.exact_package_max_bytes,
            });
        }
    }
    Ok(())
}

fn validate_episode_body(body: &MemoryAtomBodyV1, payload: &crate::EpisodePayloadV1) -> Result<()> {
    if payload.event_sequence_start > payload.event_sequence_end {
        return Err(MemoryError::InvalidAtomBody(
            "episode event sequence is inverted",
        ));
    }
    let event_count = payload
        .event_sequence_end
        .checked_sub(payload.event_sequence_start)
        .and_then(|difference| difference.checked_add(1))
        .ok_or(MemoryError::InvalidAtomBody(
            "episode event sequence count overflows u64",
        ))?;
    if u64::try_from(body.provenance.source_event_digests.len()).ok() != Some(event_count) {
        return Err(MemoryError::InvalidAtomBody(
            "episode provenance must contain one digest per event",
        ));
    }
    let unique_event_digests = body
        .provenance
        .source_event_digests
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if unique_event_digests.len() != body.provenance.source_event_digests.len() {
        return Err(MemoryError::InvalidAtomBody(
            "episode source event digests must be unique",
        ));
    }
    if body.observations.is_empty()
        && body.decisions.is_empty()
        && body.actions.is_empty()
        && body.outcome.is_none()
        && body.feedback.is_empty()
    {
        return Err(MemoryError::InsufficientEpisodeContent);
    }
    let continues = body
        .relations
        .iter()
        .filter_map(|relation| {
            (relation.kind == MemoryRelationKindV1::Continues).then_some(relation.target)
        })
        .collect::<Vec<_>>();
    if continues != payload.continues.into_iter().collect::<Vec<_>>() {
        return Err(MemoryError::InvalidAtomBody(
            "episode payload and continues relation must match exactly",
        ));
    }
    Ok(())
}

fn validate_semantic_body(
    body: &MemoryAtomBodyV1,
    payload: &crate::SemanticPayloadV1,
    policy: &PolicyV1,
) -> Result<()> {
    if body.retention_permission != RetentionPermissionV1::SemanticAllowed {
        return Err(MemoryError::InvalidAtomBody(
            "semantic atoms require semantic_allowed retention",
        ));
    }
    if !body.observations.is_empty()
        || !body.interpretations.is_empty()
        || !body.beliefs.is_empty()
        || !body.decisions.is_empty()
        || !body.rejected_alternatives.is_empty()
        || !body.actions.is_empty()
        || body.outcome.is_some()
        || !body.feedback.is_empty()
        || !body.artifacts.is_empty()
    {
        return Err(MemoryError::InvalidAtomBody(
            "semantic atoms may contain only deterministic structured consolidation fields",
        ));
    }
    if payload.source_ids.len() < policy.min_semantic_sources {
        return Err(MemoryError::InvalidAtomBody(
            "semantic atom has too few sources",
        ));
    }
    if !payload.source_ids.windows(2).all(|pair| pair[0] < pair[1]) {
        return Err(MemoryError::InvalidAtomBody(
            "semantic source IDs must be unique and sorted",
        ));
    }
    if payload.source_body_digests.len() != payload.source_ids.len()
        || payload
            .source_ids
            .iter()
            .any(|source| payload.source_body_digests.get(source) != Some(&source.digest()))
    {
        return Err(MemoryError::InvalidAtomBody(
            "semantic source body digest closure is incomplete",
        ));
    }
    if body.provenance.source_event_digests
        != payload
            .source_ids
            .iter()
            .map(|source| source.digest())
            .collect::<Vec<_>>()
    {
        return Err(MemoryError::InvalidAtomBody(
            "semantic provenance digest order does not match source IDs",
        ));
    }
    if payload.consensus_numerator != policy.consensus_numerator
        || payload.consensus_denominator != policy.consensus_denominator
    {
        return Err(MemoryError::InvalidAtomBody(
            "semantic consensus ratio does not match policy",
        ));
    }
    let derived_from = body
        .relations
        .iter()
        .filter_map(|relation| {
            (relation.kind == MemoryRelationKindV1::DerivedFrom).then_some(relation.target)
        })
        .collect::<Vec<_>>();
    if derived_from != payload.source_ids {
        return Err(MemoryError::InvalidAtomBody(
            "semantic DerivedFrom relations do not match source IDs",
        ));
    }
    let supersedes = body
        .relations
        .iter()
        .filter_map(|relation| {
            (relation.kind == MemoryRelationKindV1::Supersedes).then_some(relation.target)
        })
        .collect::<Vec<_>>();
    if supersedes != payload.supersedes.into_iter().collect::<Vec<_>>()
        || body.relations.len() != derived_from.len() + supersedes.len()
    {
        return Err(MemoryError::InvalidAtomBody(
            "semantic relations must be exactly DerivedFrom plus optional Supersedes",
        ));
    }
    Ok(())
}

fn validate_scope(scope: &MemoryScopeV1) -> Result<()> {
    if scope.memory_space_id.is_empty() {
        return Err(MemoryError::EmptyField("memory_space_id"));
    }
    if scope.session_id.is_empty() {
        return Err(MemoryError::EmptyField("session_id"));
    }
    for (name, value) in [
        ("repository_id", scope.repository_id.as_deref()),
        ("task_id", scope.task_id.as_deref()),
        ("agent_id", scope.agent_id.as_deref()),
    ] {
        if value == Some("") {
            return Err(MemoryError::EmptyField(name));
        }
    }
    Ok(())
}

fn validate_artifacts(artifacts: &[ArtifactRefV1], policy: &PolicyV1) -> Result<()> {
    let mut digests = BTreeSet::new();
    for artifact in artifacts {
        if !digests.insert(artifact.digest) {
            return Err(MemoryError::InvalidAtomBody(
                "artifact digests must be unique within an atom",
            ));
        }
        if artifact.media_type.is_empty() {
            return Err(MemoryError::EmptyField("artifact.media_type"));
        }
        if artifact.size_bytes > policy.artifact_max_bytes {
            return Err(MemoryError::ArtifactTooLarge {
                actual: artifact.size_bytes,
                maximum: policy.artifact_max_bytes,
            });
        }
        if let Some(inline) = &artifact.inline_payload {
            let inline_size = u64::try_from(inline.len()).map_err(|_| {
                MemoryError::CanonicalEncoding("inline payload exceeds u64 length".to_owned())
            })?;
            if inline_size > policy.inline_payload_max_bytes {
                return Err(MemoryError::InlinePayloadTooLarge {
                    actual: inline_size,
                    maximum: policy.inline_payload_max_bytes,
                });
            }
            if inline_size != artifact.size_bytes
                || crate::Digest32::hash_prefixed(&[], inline) != artifact.digest
            {
                return Err(MemoryError::ArtifactDigestMismatch {
                    digest: artifact.digest,
                });
            }
        }
    }
    Ok(())
}

const fn relation_kind_key(kind: MemoryRelationKindV1) -> u8 {
    match kind {
        MemoryRelationKindV1::Continues => 0,
        MemoryRelationKindV1::Supports => 1,
        MemoryRelationKindV1::Contradicts => 2,
        MemoryRelationKindV1::Supersedes => 3,
        MemoryRelationKindV1::DerivedFrom => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ActionRecordV1, AtomId, BeliefV1, ConsolidationScoreV1, DecisionRecordV1, Digest32,
        EpisodePayloadV1, FeedbackSignalV1, FormationSignalsV1, InterpretationV1, MemoryPayloadV1,
        MemoryRelationV1, ObservationV1, OutcomeV1, Ppm, ProvenanceV1, RejectedAlternativeV1,
        SealReasonV1, SemanticPayloadV1, TimeIntervalV1,
    };
    use std::collections::BTreeMap;

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
                ended_at_us: 10,
                recorded_at_us: 11,
            },
            trigger: "validation-boundary".to_owned(),
            seal_reason: SealReasonV1::Checkpoint,
            goal: None,
            plan: Vec::new(),
            internal_state_before: BTreeMap::new(),
            internal_state_after: BTreeMap::new(),
            observations: vec![ObservationV1 {
                at_us: 10,
                source: "validation-test".to_owned(),
                content: "x".to_owned(),
                artifact_digest: None,
            }],
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
                producer: "validation-boundary-test".to_owned(),
                source_event_digests: vec![Digest32::hash_prefixed(b"event\0", b"0")],
                policy_digest: policy.digest()?,
            },
            relations: Vec::new(),
            artifacts: Vec::new(),
            retention_permission: RetentionPermissionV1::TransientOnly,
            payload: MemoryPayloadV1::Episode(EpisodePayloadV1 {
                event_sequence_start: 0,
                event_sequence_end: 0,
                continues: None,
            }),
        })
    }

    fn semantic_body(policy: &PolicyV1) -> Result<MemoryAtomBodyV1> {
        let source_ids = (1_u8..=3)
            .map(|value| AtomId(Digest32([value; 32])))
            .collect::<Vec<_>>();
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
            topic_keys: BTreeSet::from(["memory".to_owned()]),
            entity_ids: BTreeSet::from(["naome".to_owned()]),
            outcome_class: None,
            goal_key: None,
            provenance: ProvenanceV1 {
                producer: "validation-boundary-test".to_owned(),
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
                source_body_digests: source_ids
                    .iter()
                    .copied()
                    .map(|source| (source, source.digest()))
                    .collect(),
                source_ids,
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

    #[test]
    fn inclusive_time_and_byte_limits_are_accepted() -> Result<()> {
        let policy = PolicyV1::poc_v1();
        let time_boundary = episode_body(&policy)?;
        validate_memory_atom_body(&time_boundary, &policy)?;

        let mut body_boundary = episode_body(&policy)?;
        let initial_size = body_boundary.encoded_size_bytes()?;
        let padding = policy.atom_hard_max_bytes.checked_sub(initial_size).ok_or(
            MemoryError::InvalidAtomBody("test atom unexpectedly exceeds hard maximum"),
        )?;
        body_boundary.observations[0].content.push_str(&"x".repeat(
            usize::try_from(padding).map_err(|_| {
                MemoryError::CanonicalEncoding("test padding exceeds usize".to_owned())
            })?,
        ));
        assert_eq!(
            body_boundary.encoded_size_bytes()?,
            policy.atom_hard_max_bytes
        );
        validate_memory_atom_body(&body_boundary, &policy)?;

        let mut exact_package_boundary = episode_body(&policy)?;
        exact_package_boundary.retention_permission =
            RetentionPermissionV1::SemanticAndExactAllowed;
        exact_package_boundary.artifacts.push(ArtifactRefV1 {
            digest: Digest32([7; 32]),
            size_bytes: 0,
            media_type: "application/octet-stream".to_owned(),
            retention_allowed: true,
            inline_payload: None,
        });
        let encoded_size = exact_package_boundary.encoded_size_bytes()?;
        exact_package_boundary.artifacts[0].size_bytes = policy
            .exact_package_max_bytes
            .checked_sub(encoded_size)
            .ok_or(MemoryError::InvalidAtomBody(
                "test atom unexpectedly exceeds exact-package maximum",
            ))?;
        assert_eq!(
            exact_package_boundary.exact_package_bytes()?,
            policy.exact_package_max_bytes
        );
        validate_memory_atom_body(&exact_package_boundary, &policy)?;
        Ok(())
    }

    #[test]
    fn artifact_and_inline_limits_are_inclusive() -> Result<()> {
        let policy = PolicyV1::poc_v1();
        let mut artifact_boundary = episode_body(&policy)?;
        artifact_boundary.artifacts.push(ArtifactRefV1 {
            digest: Digest32([8; 32]),
            size_bytes: policy.artifact_max_bytes,
            media_type: "application/octet-stream".to_owned(),
            retention_allowed: false,
            inline_payload: None,
        });
        validate_memory_atom_body(&artifact_boundary, &policy)?;

        let inline = vec![
            0_u8;
            usize::try_from(policy.inline_payload_max_bytes).map_err(|_| {
                MemoryError::CanonicalEncoding("inline maximum exceeds usize".to_owned())
            })?
        ];
        let mut inline_boundary = episode_body(&policy)?;
        inline_boundary.artifacts.push(ArtifactRefV1 {
            digest: Digest32::hash_prefixed(&[], &inline),
            size_bytes: policy.inline_payload_max_bytes,
            media_type: "application/octet-stream".to_owned(),
            retention_allowed: false,
            inline_payload: Some(inline),
        });
        validate_memory_atom_body(&inline_boundary, &policy)?;
        Ok(())
    }

    #[test]
    fn every_forbidden_semantic_episode_field_is_independently_rejected() -> Result<()> {
        let policy = PolicyV1::poc_v1();
        let baseline = semantic_body(&policy)?;
        let mutations: [fn(&mut MemoryAtomBodyV1); 9] = [
            |body| {
                body.observations.push(ObservationV1 {
                    at_us: 10,
                    source: "forbidden".to_owned(),
                    content: "observation".to_owned(),
                    artifact_digest: None,
                });
            },
            |body| {
                body.interpretations.push(InterpretationV1 {
                    content: "interpretation".to_owned(),
                    confidence: Ppm::ONE,
                    evidence: Vec::new(),
                });
            },
            |body| {
                body.beliefs.push(BeliefV1 {
                    proposition: "belief".to_owned(),
                    confidence: Ppm::ONE,
                    evidence: Vec::new(),
                });
            },
            |body| {
                body.decisions.push(DecisionRecordV1 {
                    decision: "decision".to_owned(),
                    rationale: "rationale".to_owned(),
                });
            },
            |body| {
                body.rejected_alternatives.push(RejectedAlternativeV1 {
                    alternative: "alternative".to_owned(),
                    reason: "reason".to_owned(),
                });
            },
            |body| {
                body.actions.push(ActionRecordV1 {
                    action: "action".to_owned(),
                    result: None,
                });
            },
            |body| {
                body.outcome = Some(OutcomeV1 {
                    class: "success".to_owned(),
                    summary: "outcome".to_owned(),
                    succeeded: true,
                });
            },
            |body| {
                body.feedback.push(FeedbackSignalV1 {
                    source: "feedback".to_owned(),
                    positive: true,
                    note: None,
                });
            },
            |body| {
                body.artifacts.push(ArtifactRefV1 {
                    digest: Digest32([9; 32]),
                    size_bytes: 1,
                    media_type: "application/octet-stream".to_owned(),
                    retention_allowed: false,
                    inline_payload: None,
                });
            },
        ];

        for mutate in mutations {
            let mut changed = baseline.clone();
            mutate(&mut changed);
            assert_eq!(
                validate_memory_atom_body(&changed, &policy),
                Err(MemoryError::InvalidAtomBody(
                    "semantic atoms may contain only deterministic structured consolidation fields"
                ))
            );
        }
        Ok(())
    }

    #[test]
    fn semantic_source_order_is_strict_and_supersession_count_is_additive() -> Result<()> {
        let policy = PolicyV1::poc_v1();
        let baseline = semantic_body(&policy)?;

        let mut duplicate = baseline.clone();
        let MemoryPayloadV1::Semantic(payload) = &mut duplicate.payload else {
            return Err(MemoryError::InvalidAtomBody("expected semantic payload"));
        };
        payload.source_ids[1] = payload.source_ids[0];
        assert_eq!(
            validate_memory_atom_body(&duplicate, &policy),
            Err(MemoryError::InvalidAtomBody(
                "semantic source IDs must be unique and sorted"
            ))
        );

        let superseded = AtomId(Digest32([10; 32]));
        let mut with_supersession = baseline;
        let MemoryPayloadV1::Semantic(payload) = &mut with_supersession.payload else {
            return Err(MemoryError::InvalidAtomBody("expected semantic payload"));
        };
        payload.supersedes = Some(superseded);
        with_supersession.relations.push(MemoryRelationV1 {
            kind: MemoryRelationKindV1::Supersedes,
            target: superseded,
        });
        validate_memory_atom_body(&with_supersession, &policy)?;
        Ok(())
    }
}
