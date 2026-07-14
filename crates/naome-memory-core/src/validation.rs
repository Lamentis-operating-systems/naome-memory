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
