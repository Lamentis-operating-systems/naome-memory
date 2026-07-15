use crate::{
    ActionRecordV1, ArtifactRefV1, AtomId, BeliefV1, CanonicalBytes as _, DecisionRecordV1,
    Digest32, FeedbackSignalV1, FormationSignalsV1, InterpretationV1, MemoryAtomBodyV1,
    MemoryError, MemoryPayloadV1, MemoryRelationKindV1, MemoryRelationV1, MemoryScopeV1,
    ObservationV1, OutcomeV1, PolicyV1, ProvenanceV1, RejectedAlternativeV1, Result,
    RetentionPermissionV1, SealReasonV1, TimeIntervalV1, validate_memory_atom_body,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const EVENT_HASH_DOMAIN: &[u8] = b"naome-memory:event:v1\0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeEventV1 {
    pub sequence: u64,
    pub at_us: u64,
    pub scope: MemoryScopeV1,
    pub observation: Option<ObservationV1>,
    pub interpretation: Option<InterpretationV1>,
    pub belief: Option<BeliefV1>,
    pub decision: Option<DecisionRecordV1>,
    pub action: Option<ActionRecordV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeBufferV1 {
    pub scope: MemoryScopeV1,
    pub started_at_us: u64,
    pub events: Vec<EpisodeEventV1>,
    pub continues: Option<AtomId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealEpisodeRequestV1 {
    pub as_of_us: u64,
    pub ended_at_us: u64,
    pub trigger: String,
    pub seal_reason: SealReasonV1,
    pub goal: Option<String>,
    pub plan: Vec<String>,
    pub internal_state_before: BTreeMap<String, String>,
    pub internal_state_after: BTreeMap<String, String>,
    pub rejected_alternatives: Vec<RejectedAlternativeV1>,
    pub outcome: Option<OutcomeV1>,
    pub feedback: Vec<FeedbackSignalV1>,
    pub formation_signals: FormationSignalsV1,
    pub topic_keys: BTreeSet<String>,
    pub entity_ids: BTreeSet<String>,
    pub outcome_class: Option<String>,
    pub goal_key: Option<String>,
    pub retention_permission: RetentionPermissionV1,
    pub artifacts: Vec<ArtifactRefV1>,
    pub relations: Vec<MemoryRelationV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedEpisodeV1 {
    pub atom_id: AtomId,
    pub body: MemoryAtomBodyV1,
    pub canonical_size_bytes: u64,
    pub target_size_exceeded: bool,
}

/// Deterministically ordered episode atoms whose `continues` links preserve a
/// source episode that does not fit in one target-sized atom.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedEpisodeChainV1 {
    pub contract_version: String,
    pub source_event_count: usize,
    pub episodes: Vec<SealedEpisodeV1>,
}

pub fn seal_episode(
    buffer: &EpisodeBufferV1,
    request: &SealEpisodeRequestV1,
    policy: &PolicyV1,
) -> Result<SealedEpisodeV1> {
    policy.validate_poc_v1()?;
    validate_episode_input(buffer, request, policy)?;
    let prepared = PreparedEpisodeEvents::new(&buffer.events)?;
    let sealed = seal_episode_range(
        buffer,
        request,
        policy,
        &prepared,
        0,
        buffer.events.len(),
        buffer.continues,
    )?;
    if sealed.target_size_exceeded {
        return Err(MemoryError::EpisodeRequiresSplit {
            actual: sealed.canonical_size_bytes,
            target: policy.atom_target_bytes,
        });
    }
    Ok(sealed)
}

/// Seal an episode as the fewest deterministic target-sized atoms that can
/// preserve the complete event stream. Among equally short valid chains, the
/// earliest atom takes the longest possible prefix. A single event or shared
/// request metadata that cannot fit is rejected instead of being truncated.
pub fn seal_episode_chain(
    buffer: &EpisodeBufferV1,
    request: &SealEpisodeRequestV1,
    policy: &PolicyV1,
) -> Result<SealedEpisodeChainV1> {
    policy.validate_poc_v1()?;
    validate_episode_input(buffer, request, policy)?;
    let prepared = PreparedEpisodeEvents::new(&buffer.events)?;
    let policy_digest = policy.digest()?;
    let first_base_bytes =
        segment_base_bytes(buffer, request, policy_digest, &prepared, buffer.continues)?;
    let continued_base_bytes = segment_base_bytes(
        buffer,
        request,
        policy_digest,
        &prepared,
        Some(AtomId::default()),
    )?;
    let ranges = plan_episode_ranges(
        &prepared,
        request,
        first_base_bytes,
        continued_base_bytes,
        policy.atom_target_bytes,
    )?;

    let mut episodes = Vec::new();
    let mut previous = buffer.continues;
    for (start, end) in ranges {
        let sealed = seal_episode_range(buffer, request, policy, &prepared, start, end, previous)?;
        let base_bytes = if start == 0 {
            first_base_bytes
        } else {
            continued_base_bytes
        };
        let planned_size = base_bytes
            .checked_add(prepared.range_body_bytes(start, end)?)
            .ok_or_else(|| {
                MemoryError::CanonicalEncoding("episode segment size overflowed u64".to_owned())
            })?;
        if sealed.canonical_size_bytes != planned_size {
            return Err(MemoryError::CanonicalEncoding(
                "episode partition size model drifted".to_owned(),
            ));
        }
        if sealed.target_size_exceeded {
            return Err(MemoryError::EpisodeRequiresSplit {
                actual: sealed.canonical_size_bytes,
                target: policy.atom_target_bytes,
            });
        }
        previous = Some(sealed.atom_id);
        episodes.push(sealed);
    }

    let chain = SealedEpisodeChainV1 {
        contract_version: "sealed-episode-chain-v1".to_owned(),
        source_event_count: buffer.events.len(),
        episodes,
    };
    validate_episode_chain_closure(&chain, buffer, &prepared, policy)?;
    Ok(chain)
}

#[derive(Debug)]
struct PreparedEpisodeEvents {
    digests: Vec<Digest32>,
    cumulative_body_bytes: Vec<u64>,
    cumulative_substantive: Vec<usize>,
}

impl PreparedEpisodeEvents {
    fn new(events: &[EpisodeEventV1]) -> Result<Self> {
        let mut digests = Vec::with_capacity(events.len());
        let mut cumulative_body_bytes = Vec::with_capacity(events.len().saturating_add(1));
        let mut cumulative_substantive = Vec::with_capacity(events.len().saturating_add(1));
        cumulative_body_bytes.push(0);
        cumulative_substantive.push(0);

        for event in events {
            let digest = Digest32::hash_prefixed(EVENT_HASH_DOMAIN, &event.canonical_bytes()?);
            let mut body_bytes = framed_canonical_size(&digest)?;
            for field_bytes in [
                event
                    .observation
                    .as_ref()
                    .map(framed_canonical_size)
                    .transpose()?,
                event
                    .interpretation
                    .as_ref()
                    .map(framed_canonical_size)
                    .transpose()?,
                event
                    .belief
                    .as_ref()
                    .map(framed_canonical_size)
                    .transpose()?,
                event
                    .decision
                    .as_ref()
                    .map(framed_canonical_size)
                    .transpose()?,
                event
                    .action
                    .as_ref()
                    .map(framed_canonical_size)
                    .transpose()?,
            ] {
                body_bytes = body_bytes
                    .checked_add(field_bytes.unwrap_or(0))
                    .ok_or_else(|| {
                        MemoryError::CanonicalEncoding(
                            "episode event size overflowed u64".to_owned(),
                        )
                    })?;
            }
            let total = cumulative_body_bytes
                .last()
                .copied()
                .unwrap_or(0_u64)
                .checked_add(body_bytes)
                .ok_or_else(|| {
                    MemoryError::CanonicalEncoding("episode event bytes overflowed u64".to_owned())
                })?;
            let substantive = usize::from(
                event.observation.is_some() || event.decision.is_some() || event.action.is_some(),
            );
            let substantive_total = cumulative_substantive
                .last()
                .copied()
                .unwrap_or(0_usize)
                .checked_add(substantive)
                .ok_or_else(|| {
                    MemoryError::CanonicalEncoding(
                        "episode substantive event count overflowed usize".to_owned(),
                    )
                })?;
            digests.push(digest);
            cumulative_body_bytes.push(total);
            cumulative_substantive.push(substantive_total);
        }

        Ok(Self {
            digests,
            cumulative_body_bytes,
            cumulative_substantive,
        })
    }

    fn range_body_bytes(&self, start: usize, end: usize) -> Result<u64> {
        let start_bytes = self
            .cumulative_body_bytes
            .get(start)
            .copied()
            .ok_or_else(|| {
                MemoryError::CanonicalEncoding("episode size range start failed".to_owned())
            })?;
        let end_bytes = self
            .cumulative_body_bytes
            .get(end)
            .copied()
            .ok_or_else(|| {
                MemoryError::CanonicalEncoding("episode size range end failed".to_owned())
            })?;
        end_bytes.checked_sub(start_bytes).ok_or_else(|| {
            MemoryError::CanonicalEncoding("episode size range was inverted".to_owned())
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct RouteChoice {
    remaining_segments: usize,
    end: usize,
}

impl RouteChoice {
    fn better_than(self, other: Self) -> bool {
        self.remaining_segments < other.remaining_segments
            || (self.remaining_segments == other.remaining_segments && self.end > other.end)
    }
}

struct RouteChoices {
    leaf_count: usize,
    nodes: Vec<Option<RouteChoice>>,
}

impl RouteChoices {
    fn new(value_count: usize) -> Self {
        let leaf_count = value_count.next_power_of_two();
        Self {
            leaf_count,
            nodes: vec![None; leaf_count.saturating_mul(2)],
        }
    }

    fn insert(&mut self, index: usize, remaining_segments: usize) {
        let mut node = self.leaf_count.saturating_add(index);
        self.nodes[node] = Some(RouteChoice {
            remaining_segments,
            end: index,
        });
        while node > 1 {
            node /= 2;
            self.nodes[node] = best_route_choice(self.nodes[node * 2], self.nodes[node * 2 + 1]);
        }
    }

    fn best_in(&self, mut start: usize, mut end: usize) -> Option<RouteChoice> {
        start = start.saturating_add(self.leaf_count);
        end = end.saturating_add(self.leaf_count);
        let mut best = None;
        while start < end {
            if start % 2 == 1 {
                best = best_route_choice(best, self.nodes[start]);
                start = start.saturating_add(1);
            }
            if end % 2 == 1 {
                end = end.saturating_sub(1);
                best = best_route_choice(best, self.nodes[end]);
            }
            start /= 2;
            end /= 2;
        }
        best
    }
}

fn best_route_choice(left: Option<RouteChoice>, right: Option<RouteChoice>) -> Option<RouteChoice> {
    match (left, right) {
        (Some(left), Some(right)) => Some(if left.better_than(right) { left } else { right }),
        (Some(choice), None) | (None, Some(choice)) => Some(choice),
        (None, None) => None,
    }
}

fn plan_episode_ranges(
    prepared: &PreparedEpisodeEvents,
    request: &SealEpisodeRequestV1,
    first_base_bytes: u64,
    continued_base_bytes: u64,
    target_bytes: u64,
) -> Result<Vec<(usize, usize)>> {
    let event_count = prepared.digests.len();
    let shared_substantive = request.outcome.is_some() || !request.feedback.is_empty();
    let mut routes = RouteChoices::new(event_count.saturating_add(1));
    let mut remaining_segments = vec![None; event_count.saturating_add(1)];
    let mut next_boundary = vec![None; event_count.saturating_add(1)];
    remaining_segments[event_count] = Some(0_usize);
    routes.insert(event_count, 0);

    for start in (0..event_count).rev() {
        let base_bytes = if start == 0 {
            first_base_bytes
        } else {
            continued_base_bytes
        };
        let Some(capacity) = target_bytes.checked_sub(base_bytes) else {
            continue;
        };
        let limit = prepared.cumulative_body_bytes[start].saturating_add(capacity);
        let upper = prepared
            .cumulative_body_bytes
            .partition_point(|total| *total <= limit);
        let maximum_end = upper.saturating_sub(1).min(event_count);
        if maximum_end <= start {
            continue;
        }
        let minimum_end = if shared_substantive {
            start.saturating_add(1)
        } else {
            let substantive_before = prepared.cumulative_substantive[start];
            let first_substantive_end = prepared
                .cumulative_substantive
                .partition_point(|count| *count <= substantive_before);
            if first_substantive_end > maximum_end {
                continue;
            }
            first_substantive_end
        };
        let Some(successor) = routes.best_in(minimum_end, maximum_end.saturating_add(1)) else {
            continue;
        };
        let segment_count = successor.remaining_segments.checked_add(1).ok_or_else(|| {
            MemoryError::CanonicalEncoding("episode chain length overflowed usize".to_owned())
        })?;
        remaining_segments[start] = Some(segment_count);
        next_boundary[start] = Some(successor.end);
        routes.insert(start, segment_count);
    }

    if remaining_segments[0].is_none() {
        return Err(MemoryError::EpisodeRequiresSplit {
            actual: target_bytes.saturating_add(1),
            target: target_bytes,
        });
    }
    let mut ranges = Vec::with_capacity(remaining_segments[0].unwrap_or(0));
    let mut start = 0_usize;
    while start < event_count {
        let end = next_boundary[start].ok_or(MemoryError::EpisodeRequiresSplit {
            actual: target_bytes.saturating_add(1),
            target: target_bytes,
        })?;
        validate_partition_boundary(start, end, event_count)?;
        ranges.push((start, end));
        start = end;
    }
    Ok(ranges)
}

fn validate_partition_boundary(start: usize, end: usize, event_count: usize) -> Result<()> {
    if end <= start || end > event_count {
        return Err(MemoryError::CanonicalEncoding(
            "episode partition route was invalid".to_owned(),
        ));
    }
    Ok(())
}

fn segment_base_bytes(
    buffer: &EpisodeBufferV1,
    request: &SealEpisodeRequestV1,
    policy_digest: Digest32,
    prepared: &PreparedEpisodeEvents,
    continues: Option<AtomId>,
) -> Result<u64> {
    let body = build_episode_body(
        buffer,
        request,
        policy_digest,
        &buffer.events[..1],
        &prepared.digests[..1],
        buffer.started_at_us,
        request.ended_at_us,
        continues,
    )?;
    body.encoded_size_bytes()?
        .checked_sub(prepared.range_body_bytes(0, 1)?)
        .ok_or_else(|| {
            MemoryError::CanonicalEncoding("episode segment base size underflowed".to_owned())
        })
}

#[allow(clippy::too_many_arguments)]
fn seal_episode_range(
    buffer: &EpisodeBufferV1,
    request: &SealEpisodeRequestV1,
    policy: &PolicyV1,
    prepared: &PreparedEpisodeEvents,
    start: usize,
    end: usize,
    continues: Option<AtomId>,
) -> Result<SealedEpisodeV1> {
    let events = buffer
        .events
        .get(start..end)
        .ok_or_else(|| MemoryError::CanonicalEncoding("episode split range failed".to_owned()))?;
    let event_digests = prepared
        .digests
        .get(start..end)
        .ok_or_else(|| MemoryError::CanonicalEncoding("episode digest range failed".to_owned()))?;
    let started_at_us = if start == 0 {
        buffer.started_at_us
    } else {
        events
            .first()
            .map_or(buffer.started_at_us, |event| event.at_us)
    };
    let ended_at_us = if end == buffer.events.len() {
        request.ended_at_us
    } else {
        events
            .last()
            .map_or(request.ended_at_us, |event| event.at_us)
    };
    let body = build_episode_body(
        buffer,
        request,
        policy.digest()?,
        events,
        event_digests,
        started_at_us,
        ended_at_us,
        continues,
    )?;
    validate_memory_atom_body(&body, policy)?;
    let canonical_size_bytes = body.encoded_size_bytes()?;
    let atom_id = body.atom_id()?;
    Ok(SealedEpisodeV1 {
        atom_id,
        body,
        canonical_size_bytes,
        target_size_exceeded: canonical_size_bytes > policy.atom_target_bytes,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_episode_body(
    buffer: &EpisodeBufferV1,
    request: &SealEpisodeRequestV1,
    policy_digest: Digest32,
    events: &[EpisodeEventV1],
    event_digests: &[Digest32],
    started_at_us: u64,
    ended_at_us: u64,
    continues: Option<AtomId>,
) -> Result<MemoryAtomBodyV1> {
    let first_event = events.first().ok_or(MemoryError::EmptyEpisode)?;
    let last_event = events.last().ok_or(MemoryError::EmptyEpisode)?;

    let observations = events
        .iter()
        .filter_map(|event| event.observation.clone())
        .collect::<Vec<_>>();
    let interpretations = events
        .iter()
        .filter_map(|event| event.interpretation.clone())
        .collect::<Vec<_>>();
    let beliefs = events
        .iter()
        .filter_map(|event| event.belief.clone())
        .collect::<Vec<_>>();
    let decisions = events
        .iter()
        .filter_map(|event| event.decision.clone())
        .collect::<Vec<_>>();
    let actions = events
        .iter()
        .filter_map(|event| event.action.clone())
        .collect::<Vec<_>>();

    let mut relations = request
        .relations
        .iter()
        .filter(|relation| relation.kind != MemoryRelationKindV1::Continues)
        .cloned()
        .collect::<Vec<_>>();
    if let Some(previous) = continues {
        let relation = MemoryRelationV1 {
            kind: MemoryRelationKindV1::Continues,
            target: previous,
        };
        relations.push(relation);
    }
    relations.sort_unstable_by_key(|relation| (relation.target, relation_kind_key(relation.kind)));

    Ok(MemoryAtomBodyV1 {
        contract_version: "atom-v1".to_owned(),
        scope: buffer.scope.clone(),
        interval: TimeIntervalV1 {
            started_at_us,
            ended_at_us,
            recorded_at_us: request.as_of_us,
        },
        trigger: request.trigger.clone(),
        seal_reason: request.seal_reason,
        goal: request.goal.clone(),
        plan: request.plan.clone(),
        internal_state_before: request.internal_state_before.clone(),
        internal_state_after: request.internal_state_after.clone(),
        observations,
        interpretations,
        beliefs,
        decisions,
        rejected_alternatives: request.rejected_alternatives.clone(),
        actions,
        outcome: request.outcome.clone(),
        feedback: request.feedback.clone(),
        formation_signals: request.formation_signals,
        topic_keys: request.topic_keys.clone(),
        entity_ids: request.entity_ids.clone(),
        outcome_class: request.outcome_class.clone(),
        goal_key: request.goal_key.clone(),
        provenance: ProvenanceV1 {
            producer: "naome-memory-core/seal_episode".to_owned(),
            source_event_digests: event_digests.to_vec(),
            policy_digest,
        },
        relations,
        artifacts: request.artifacts.clone(),
        retention_permission: request.retention_permission,
        payload: MemoryPayloadV1::Episode(crate::EpisodePayloadV1 {
            event_sequence_start: first_event.sequence,
            event_sequence_end: last_event.sequence,
            continues,
        }),
    })
}

fn framed_canonical_size(value: &impl Serialize) -> Result<u64> {
    u64::try_from(value.canonical_bytes()?.len())
        .map_err(|_| MemoryError::CanonicalEncoding("canonical value exceeds u64".to_owned()))?
        .checked_add(8)
        .ok_or_else(|| {
            MemoryError::CanonicalEncoding("framed canonical size overflowed u64".to_owned())
        })
}

fn validate_episode_chain_closure(
    chain: &SealedEpisodeChainV1,
    buffer: &EpisodeBufferV1,
    prepared: &PreparedEpisodeEvents,
    policy: &PolicyV1,
) -> Result<()> {
    let mut source_cursor = 0_usize;
    let mut previous = buffer.continues;
    for episode in &chain.episodes {
        let MemoryPayloadV1::Episode(payload) = &episode.body.payload else {
            return Err(MemoryError::InvalidAtomBody(
                "episode chain contains a non-episode body",
            ));
        };
        let event_count = episode.body.provenance.source_event_digests.len();
        let source_end = source_cursor.checked_add(event_count).ok_or_else(|| {
            MemoryError::CanonicalEncoding("episode chain event count overflowed usize".to_owned())
        })?;
        let source_events =
            buffer
                .events
                .get(source_cursor..source_end)
                .ok_or(MemoryError::InvalidAtomBody(
                    "episode chain exceeds the source event stream",
                ))?;
        let expected_digests =
            prepared
                .digests
                .get(source_cursor..source_end)
                .ok_or(MemoryError::InvalidAtomBody(
                    "episode chain digest range is incomplete",
                ))?;
        let first = source_events.first().ok_or(MemoryError::InvalidAtomBody(
            "episode chain contains an empty atom",
        ))?;
        let last = source_events.last().ok_or(MemoryError::InvalidAtomBody(
            "episode chain contains an empty atom",
        ))?;
        if payload.event_sequence_start != first.sequence
            || payload.event_sequence_end != last.sequence
            || payload.continues != previous
            || episode.body.provenance.source_event_digests != expected_digests
            || episode.canonical_size_bytes != episode.body.encoded_size_bytes()?
            || episode.canonical_size_bytes > policy.atom_target_bytes
            || episode.canonical_size_bytes > policy.atom_hard_max_bytes
            || episode.atom_id != episode.body.atom_id()?
        {
            return Err(MemoryError::InvalidAtomBody(
                "episode chain does not close over its ordered source stream",
            ));
        }
        source_cursor = source_end;
        previous = Some(episode.atom_id);
    }
    if source_cursor != buffer.events.len()
        || chain.source_event_count != buffer.events.len()
        || chain.episodes.is_empty()
    {
        return Err(MemoryError::InvalidAtomBody(
            "episode chain source-event closure is incomplete",
        ));
    }
    Ok(())
}

fn validate_episode_input(
    buffer: &EpisodeBufferV1,
    request: &SealEpisodeRequestV1,
    policy: &PolicyV1,
) -> Result<()> {
    validate_scope(&buffer.scope)?;
    if request.trigger.is_empty() {
        return Err(MemoryError::EmptyField("trigger"));
    }
    if buffer.events.is_empty() {
        return Err(MemoryError::EmptyEpisode);
    }
    if buffer.started_at_us > request.ended_at_us || request.ended_at_us > request.as_of_us {
        return Err(MemoryError::InvalidTimeInterval);
    }

    let first_sequence = buffer.events[0].sequence;
    let mut expected = first_sequence;
    let mut previous_at_us = None;
    for event in &buffer.events {
        if event.sequence != expected {
            return Err(MemoryError::NonContiguousEvents {
                expected,
                found: event.sequence,
            });
        }
        if event.scope != buffer.scope {
            return Err(MemoryError::EventScopeMismatch {
                sequence: event.sequence,
            });
        }
        if event.at_us < buffer.started_at_us || event.at_us > request.ended_at_us {
            return Err(MemoryError::EventTimeOutOfRange {
                sequence: event.sequence,
            });
        }
        if let Some(previous_at_us) = previous_at_us
            && event.at_us < previous_at_us
        {
            return Err(MemoryError::NonMonotonicEventTime {
                sequence: event.sequence,
                previous_at_us,
                found_at_us: event.at_us,
            });
        }
        previous_at_us = Some(event.at_us);
        expected = expected
            .checked_add(1)
            .ok_or(MemoryError::NonContiguousEvents {
                expected,
                found: event.sequence,
            })?;
    }

    validate_artifacts(&request.artifacts, policy)?;

    if !buffer.events.iter().any(|event| {
        event.observation.is_some() || event.decision.is_some() || event.action.is_some()
    }) && request.outcome.is_none()
        && request.feedback.is_empty()
    {
        return Err(MemoryError::InsufficientEpisodeContent);
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
    for artifact in artifacts {
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
                || Digest32::hash_prefixed(&[], inline) != artifact.digest
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

    fn scope() -> MemoryScopeV1 {
        MemoryScopeV1 {
            memory_space_id: "space-a".to_owned(),
            repository_id: Some("repository-a".to_owned()),
            task_id: Some("task-a".to_owned()),
            agent_id: Some("agent-a".to_owned()),
            session_id: "session-a".to_owned(),
        }
    }

    fn event(sequence: u64, at_us: u64) -> EpisodeEventV1 {
        EpisodeEventV1 {
            sequence,
            at_us,
            scope: scope(),
            observation: Some(ObservationV1 {
                at_us,
                source: "unit-test".to_owned(),
                content: format!("event-{sequence}"),
                artifact_digest: None,
            }),
            interpretation: None,
            belief: None,
            decision: None,
            action: None,
        }
    }

    fn fixture(event_count: u64) -> (EpisodeBufferV1, SealEpisodeRequestV1) {
        let events = (0..event_count)
            .map(|offset| event(7 + offset, 11 + offset))
            .collect();
        (
            EpisodeBufferV1 {
                scope: scope(),
                started_at_us: 10,
                events,
                continues: None,
            },
            SealEpisodeRequestV1 {
                as_of_us: 30,
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

    fn synthetic_prepared(
        cumulative_body_bytes: Vec<u64>,
        cumulative_substantive: Vec<usize>,
    ) -> PreparedEpisodeEvents {
        assert_eq!(cumulative_body_bytes.len(), cumulative_substantive.len());
        let event_count = cumulative_body_bytes.len().saturating_sub(1);
        PreparedEpisodeEvents {
            digests: vec![Digest32::ZERO; event_count],
            cumulative_body_bytes,
            cumulative_substantive,
        }
    }

    fn assert_invalid_closure(
        chain: &SealedEpisodeChainV1,
        buffer: &EpisodeBufferV1,
        prepared: &PreparedEpisodeEvents,
        policy: &PolicyV1,
    ) {
        assert!(matches!(
            validate_episode_chain_closure(chain, buffer, prepared, policy),
            Err(MemoryError::InvalidAtomBody(_))
        ));
    }

    fn update_episode_identity(episode: &mut SealedEpisodeV1) -> Result<()> {
        episode.canonical_size_bytes = episode.body.encoded_size_bytes()?;
        episode.atom_id = episode.body.atom_id()?;
        Ok(())
    }

    #[test]
    fn route_choice_orders_segment_count_before_longest_prefix() {
        let fewer = RouteChoice {
            remaining_segments: 1,
            end: 1,
        };
        let more = RouteChoice {
            remaining_segments: 2,
            end: 99,
        };
        assert!(fewer.better_than(more));
        assert!(!more.better_than(fewer));

        let shorter = RouteChoice {
            remaining_segments: 2,
            end: 4,
        };
        let longer = RouteChoice {
            remaining_segments: 2,
            end: 5,
        };
        assert!(longer.better_than(shorter));
        assert!(!shorter.better_than(longer));
        assert!(!longer.better_than(longer));
    }

    #[test]
    fn route_tree_updates_only_real_nodes_and_respects_half_open_queries() -> Result<()> {
        let mut routes = RouteChoices::new(4);
        routes.insert(0, 4);
        routes.insert(1, 2);
        routes.insert(2, 2);
        routes.insert(3, 1);

        assert!(routes.nodes[0].is_none());
        for (start, end, expected_segments, expected_end) in [
            (0, 4, 1, 3),
            (0, 3, 2, 2),
            (1, 3, 2, 2),
            (1, 2, 2, 1),
            (2, 4, 1, 3),
            (3, 4, 1, 3),
        ] {
            let choice = routes.best_in(start, end).ok_or_else(|| {
                MemoryError::CanonicalEncoding("unit-test route was missing".to_owned())
            })?;
            assert_eq!(choice.remaining_segments, expected_segments);
            assert_eq!(choice.end, expected_end);
        }
        assert!(routes.best_in(0, 0).is_none());

        let mut outside_must_not_leak = RouteChoices::new(4);
        outside_must_not_leak.insert(0, 0);
        outside_must_not_leak.insert(1, 5);
        let isolated = outside_must_not_leak.best_in(1, 2).ok_or_else(|| {
            MemoryError::CanonicalEncoding("isolated unit-test route was missing".to_owned())
        })?;
        assert_eq!(isolated.remaining_segments, 5);
        assert_eq!(isolated.end, 1);
        Ok(())
    }

    #[test]
    fn shared_outcome_or_feedback_makes_each_segment_substantive() -> Result<()> {
        let prepared = synthetic_prepared(vec![0, 1, 2], vec![0, 0, 0]);
        let (_, mut request) = fixture(2);
        request.outcome = Some(OutcomeV1 {
            class: "ok".to_owned(),
            summary: "shared".to_owned(),
            succeeded: true,
        });
        assert_eq!(
            plan_episode_ranges(&prepared, &request, 0, 0, 1)?,
            vec![(0, 1), (1, 2)]
        );

        request.outcome = None;
        request.feedback.push(FeedbackSignalV1 {
            source: "unit-test".to_owned(),
            positive: true,
            note: None,
        });
        assert_eq!(
            plan_episode_ranges(&prepared, &request, 0, 0, 1)?,
            vec![(0, 1), (1, 2)]
        );
        Ok(())
    }

    #[test]
    fn substantive_boundary_exactly_at_capacity_is_a_valid_route() -> Result<()> {
        let prepared = synthetic_prepared(vec![0, 1], vec![0, 1]);
        let (_, request) = fixture(1);
        assert_eq!(
            plan_episode_ranges(&prepared, &request, 0, 0, 1)?,
            vec![(0, 1)]
        );
        Ok(())
    }

    #[test]
    fn partition_boundary_rejects_each_invalid_side_independently() {
        assert!(validate_partition_boundary(0, 1, 1).is_ok());
        assert!(validate_partition_boundary(1, 1, 2).is_err());
        assert!(validate_partition_boundary(2, 1, 3).is_err());
        assert!(validate_partition_boundary(1, 4, 3).is_err());
    }

    #[test]
    fn episode_range_uses_source_edges_only_at_chain_edges() -> Result<()> {
        let policy = PolicyV1::poc_v1();
        let (buffer, request) = fixture(3);
        let prepared = PreparedEpisodeEvents::new(&buffer.events)?;

        let prefix = seal_episode_range(&buffer, &request, &policy, &prepared, 0, 2, None)?;
        assert_eq!(prefix.body.interval.started_at_us, buffer.started_at_us);
        assert_eq!(prefix.body.interval.ended_at_us, buffer.events[1].at_us);

        let suffix = seal_episode_range(&buffer, &request, &policy, &prepared, 1, 3, None)?;
        assert_eq!(suffix.body.interval.started_at_us, buffer.events[1].at_us);
        assert_eq!(suffix.body.interval.ended_at_us, request.ended_at_us);
        Ok(())
    }

    #[test]
    fn atom_exactly_at_target_is_not_oversized() -> Result<()> {
        let policy = PolicyV1::poc_v1();
        let (buffer, mut request) = fixture(1);
        let prepared = PreparedEpisodeEvents::new(&buffer.events)?;
        request.goal = Some(String::new());
        let empty_goal = seal_episode_range(&buffer, &request, &policy, &prepared, 0, 1, None)?;
        let padding = policy
            .atom_target_bytes
            .checked_sub(empty_goal.canonical_size_bytes)
            .ok_or_else(|| {
                MemoryError::CanonicalEncoding(
                    "unit-test fixture exceeded the atom target".to_owned(),
                )
            })?;
        request.goal = Some("x".repeat(usize::try_from(padding).map_err(|_| {
            MemoryError::CanonicalEncoding("unit-test padding exceeded usize".to_owned())
        })?));
        let exact = seal_episode_range(&buffer, &request, &policy, &prepared, 0, 1, None)?;
        assert_eq!(exact.canonical_size_bytes, policy.atom_target_bytes);
        assert!(!exact.target_size_exceeded);
        Ok(())
    }

    #[test]
    fn chain_closure_checks_each_episode_authority_field_independently() -> Result<()> {
        let policy = PolicyV1::poc_v1();
        let (buffer, request) = fixture(1);
        let prepared = PreparedEpisodeEvents::new(&buffer.events)?;
        let chain = seal_episode_chain(&buffer, &request, &policy)?;
        assert!(validate_episode_chain_closure(&chain, &buffer, &prepared, &policy).is_ok());

        let mut changed = chain.clone();
        let MemoryPayloadV1::Episode(payload) = &mut changed.episodes[0].body.payload else {
            return Err(MemoryError::InvalidAtomBody(
                "unit-test fixture must be episodic",
            ));
        };
        payload.event_sequence_start = payload.event_sequence_start.saturating_add(1);
        update_episode_identity(&mut changed.episodes[0])?;
        assert_invalid_closure(&changed, &buffer, &prepared, &policy);

        let mut changed = chain.clone();
        let MemoryPayloadV1::Episode(payload) = &mut changed.episodes[0].body.payload else {
            return Err(MemoryError::InvalidAtomBody(
                "unit-test fixture must be episodic",
            ));
        };
        payload.event_sequence_end = payload.event_sequence_end.saturating_add(1);
        update_episode_identity(&mut changed.episodes[0])?;
        assert_invalid_closure(&changed, &buffer, &prepared, &policy);

        let mut changed = chain.clone();
        let MemoryPayloadV1::Episode(payload) = &mut changed.episodes[0].body.payload else {
            return Err(MemoryError::InvalidAtomBody(
                "unit-test fixture must be episodic",
            ));
        };
        payload.continues = Some(AtomId(Digest32::hash_prefixed(b"test\0", b"previous")));
        update_episode_identity(&mut changed.episodes[0])?;
        assert_invalid_closure(&changed, &buffer, &prepared, &policy);

        let mut changed = chain.clone();
        changed.episodes[0].body.provenance.source_event_digests[0] = Digest32::ZERO;
        update_episode_identity(&mut changed.episodes[0])?;
        assert_invalid_closure(&changed, &buffer, &prepared, &policy);

        let mut changed = chain.clone();
        changed.episodes[0].canonical_size_bytes =
            changed.episodes[0].canonical_size_bytes.saturating_add(1);
        assert_invalid_closure(&changed, &buffer, &prepared, &policy);

        let mut exact_target = policy.clone();
        exact_target.atom_target_bytes = chain.episodes[0].canonical_size_bytes;
        assert!(validate_episode_chain_closure(&chain, &buffer, &prepared, &exact_target).is_ok());

        let mut below_target = policy.clone();
        below_target.atom_target_bytes = chain.episodes[0].canonical_size_bytes.saturating_sub(1);
        assert_invalid_closure(&chain, &buffer, &prepared, &below_target);

        let mut exact_hard_max = policy.clone();
        exact_hard_max.atom_hard_max_bytes = chain.episodes[0].canonical_size_bytes;
        assert!(
            validate_episode_chain_closure(&chain, &buffer, &prepared, &exact_hard_max).is_ok()
        );

        let mut below_hard_max = policy.clone();
        below_hard_max.atom_hard_max_bytes =
            chain.episodes[0].canonical_size_bytes.saturating_sub(1);
        assert_invalid_closure(&chain, &buffer, &prepared, &below_hard_max);

        let mut changed = chain.clone();
        changed.episodes[0].atom_id = AtomId::default();
        assert_invalid_closure(&changed, &buffer, &prepared, &policy);
        Ok(())
    }

    #[test]
    fn chain_closure_checks_source_count_and_nonempty_chain_independently() -> Result<()> {
        let policy = PolicyV1::poc_v1();
        let (buffer, request) = fixture(1);
        let prepared = PreparedEpisodeEvents::new(&buffer.events)?;
        let chain = seal_episode_chain(&buffer, &request, &policy)?;

        let mut extended_buffer = buffer.clone();
        extended_buffer.events.push(event(8, 12));
        let extended_prepared = PreparedEpisodeEvents::new(&extended_buffer.events)?;
        let mut changed = chain.clone();
        changed.source_event_count = extended_buffer.events.len();
        assert_invalid_closure(&changed, &extended_buffer, &extended_prepared, &policy);

        let mut changed = chain.clone();
        changed.source_event_count = 0;
        assert_invalid_closure(&changed, &buffer, &prepared, &policy);

        let mut empty_buffer = buffer;
        empty_buffer.events.clear();
        let empty_prepared = PreparedEpisodeEvents::new(&empty_buffer.events)?;
        let empty_chain = SealedEpisodeChainV1 {
            contract_version: "sealed-episode-chain-v1".to_owned(),
            source_event_count: 0,
            episodes: Vec::new(),
        };
        assert_invalid_closure(&empty_chain, &empty_buffer, &empty_prepared, &policy);
        Ok(())
    }

    #[test]
    fn episode_time_boundaries_are_inclusive_but_ordered() {
        let policy = PolicyV1::poc_v1();
        let (mut buffer, mut request) = fixture(1);
        buffer.started_at_us = 20;
        buffer.events[0].at_us = 20;
        request.ended_at_us = 20;
        request.as_of_us = 20;
        assert!(validate_episode_input(&buffer, &request, &policy).is_ok());

        let (buffer, request) = fixture(1);
        assert!(validate_episode_input(&buffer, &request, &policy).is_ok());

        let mut changed = request.clone();
        changed.ended_at_us = buffer.started_at_us.saturating_sub(1);
        assert!(validate_episode_input(&buffer, &changed, &policy).is_err());

        let mut changed = request;
        changed.as_of_us = changed.ended_at_us.saturating_sub(1);
        assert!(validate_episode_input(&buffer, &changed, &policy).is_err());
    }

    #[test]
    fn event_time_boundaries_are_inclusive_and_monotonic() {
        let policy = PolicyV1::poc_v1();
        let (mut buffer, request) = fixture(1);
        buffer.events[0].at_us = request.ended_at_us;
        assert!(validate_episode_input(&buffer, &request, &policy).is_ok());

        let (mut buffer, request) = fixture(2);
        buffer.events[1].at_us = buffer.events[0].at_us;
        assert!(validate_episode_input(&buffer, &request, &policy).is_ok());

        buffer.events[1].at_us = buffer.events[0].at_us.saturating_sub(1);
        assert!(matches!(
            validate_episode_input(&buffer, &request, &policy),
            Err(MemoryError::NonMonotonicEventTime { .. })
        ));
    }

    #[test]
    fn observation_decision_or_action_each_make_an_episode_substantive() {
        let policy = PolicyV1::poc_v1();
        let (buffer, request) = fixture(1);
        assert!(validate_episode_input(&buffer, &request, &policy).is_ok());

        let mut decision_only = buffer.clone();
        decision_only.events[0].observation = None;
        decision_only.events[0].decision = Some(DecisionRecordV1 {
            decision: "keep".to_owned(),
            rationale: "authority".to_owned(),
        });
        assert!(validate_episode_input(&decision_only, &request, &policy).is_ok());

        let mut action_only = buffer;
        action_only.events[0].observation = None;
        action_only.events[0].action = Some(ActionRecordV1 {
            action: "record".to_owned(),
            result: None,
        });
        assert!(validate_episode_input(&action_only, &request, &policy).is_ok());
    }

    #[test]
    fn artifact_limits_are_inclusive_and_integrity_checks_are_independent() {
        let policy = PolicyV1::poc_v1();
        let exact_external = ArtifactRefV1 {
            digest: Digest32::ZERO,
            size_bytes: policy.artifact_max_bytes,
            media_type: "application/octet-stream".to_owned(),
            retention_allowed: true,
            inline_payload: None,
        };
        assert!(validate_artifacts(&[exact_external], &policy).is_ok());

        let inline = vec![7_u8; usize::try_from(policy.inline_payload_max_bytes).unwrap_or(0)];
        let exact_inline = ArtifactRefV1 {
            digest: Digest32::hash_prefixed(&[], &inline),
            size_bytes: policy.inline_payload_max_bytes,
            media_type: "application/octet-stream".to_owned(),
            retention_allowed: true,
            inline_payload: Some(inline.clone()),
        };
        assert!(validate_artifacts(&[exact_inline], &policy).is_ok());

        let wrong_size = ArtifactRefV1 {
            digest: Digest32::hash_prefixed(&[], &inline),
            size_bytes: policy.inline_payload_max_bytes.saturating_sub(1),
            media_type: "application/octet-stream".to_owned(),
            retention_allowed: true,
            inline_payload: Some(inline.clone()),
        };
        assert!(matches!(
            validate_artifacts(&[wrong_size], &policy),
            Err(MemoryError::ArtifactDigestMismatch { .. })
        ));

        let wrong_digest = ArtifactRefV1 {
            digest: Digest32::ZERO,
            size_bytes: policy.inline_payload_max_bytes,
            media_type: "application/octet-stream".to_owned(),
            retention_allowed: true,
            inline_payload: Some(inline),
        };
        assert!(matches!(
            validate_artifacts(&[wrong_digest], &policy),
            Err(MemoryError::ArtifactDigestMismatch { .. })
        ));

        let too_large = ArtifactRefV1 {
            digest: Digest32::ZERO,
            size_bytes: policy.artifact_max_bytes.saturating_add(1),
            media_type: "application/octet-stream".to_owned(),
            retention_allowed: true,
            inline_payload: None,
        };
        assert!(matches!(
            validate_artifacts(&[too_large], &policy),
            Err(MemoryError::ArtifactTooLarge { .. })
        ));
    }

    #[test]
    fn relation_kind_order_is_total_and_stable() {
        assert_eq!(
            [
                MemoryRelationKindV1::Continues,
                MemoryRelationKindV1::Supports,
                MemoryRelationKindV1::Contradicts,
                MemoryRelationKindV1::Supersedes,
                MemoryRelationKindV1::DerivedFrom,
            ]
            .map(relation_kind_key),
            [0, 1, 2, 3, 4]
        );
    }
}
