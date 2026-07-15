use crate::retirement::partial_fisher_yates;
use crate::{
    AtomId, AtomStateV1, CanonicalBytes as _, ContentStatusV1, Digest32, IntegrityStatusV1,
    MemoryAtomBodyV1, MemoryError, MemoryKindV1, PolicyV1, Ppm, RetentionTierV1, Seed32,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const QUERY_HASH_DOMAIN: &[u8] = b"naome-memory:retrieval-query:v1\0";
const FLASHBACK_POPULATION_DOMAIN: &[u8] = b"naome-memory:flashback-population:v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpontaneousRecallRequestV1 {
    pub seed: Seed32,
    pub budget: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalQueryV1 {
    pub contract_version: String,
    pub memory_space_id: String,
    pub repository_id: Option<String>,
    pub memory_space_wide: bool,
    pub as_of_us: u64,
    pub terms: Vec<String>,
    pub topic_keys: BTreeSet<String>,
    pub entity_ids: BTreeSet<String>,
    /// Ordinary retrieval always requires `1..=retrieval_max_k` hits and at
    /// least one non-blank term, topic key, or entity ID. Spontaneous recall
    /// is additive and does not relax that ordinary-query contract.
    pub k: Option<usize>,
    pub spontaneous: Option<SpontaneousRecallRequestV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalCandidateV1 {
    pub atom_id: AtomId,
    pub body: MemoryAtomBodyV1,
    pub state: AtomStateV1,
    /// FTS/query generation supplies a normalized fixed-point lexical score.
    pub lexical_score: Ppm,
    pub integrity: IntegrityStatusV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalCandidatesV1 {
    /// FTS5 plus structured indices must cap this collection at 512.
    pub ranked: Vec<RetrievalCandidateV1>,
    /// Complete eligible episodic LTM pool for an explicitly requested draw.
    pub episodic_pool: Vec<RetrievalCandidateV1>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalScoreV1 {
    pub lexical: Ppm,
    pub semantic: Ppm,
    pub entity: Ppm,
    pub validated_utility: Ppm,
    pub recency: Ppm,
    pub total: Ppm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallReasonV1 {
    QueryRanked,
    Spontaneous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeneralityV1 {
    Semantic,
    SingleEpisode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalHitV1 {
    pub atom_id: AtomId,
    pub score: Option<RetrievalScoreV1>,
    pub recall_reason: RecallReasonV1,
    pub generality: GeneralityV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpontaneousRecallResultV1 {
    pub seed: Seed32,
    pub budget: usize,
    pub population_digest: Digest32,
    pub population_count: usize,
    pub hits: Vec<RetrievalHitV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalResultV1 {
    pub contract_version: String,
    pub query_digest: Digest32,
    pub hits: Vec<RetrievalHitV1>,
    pub spontaneous: Option<SpontaneousRecallResultV1>,
}

pub fn rank_retrieval(
    query: &RetrievalQueryV1,
    candidates: &RetrievalCandidatesV1,
    policy: &PolicyV1,
) -> crate::Result<RetrievalResultV1> {
    policy.validate_poc_v1()?;
    if query.contract_version != "retrieval-query-v1" {
        return Err(MemoryError::UnknownVersion(query.contract_version.clone()));
    }
    if query.memory_space_id.is_empty() {
        return Err(MemoryError::EmptyField("memory_space_id"));
    }
    if !query.memory_space_wide && query.repository_id.is_none() {
        return Err(MemoryError::EmptyField("repository_id"));
    }
    if !has_meaningful_query_input(query) {
        return Err(MemoryError::EmptyField("retrieval_query"));
    }
    let k = query.k.unwrap_or(policy.retrieval_default_k);
    if k == 0 || k > policy.retrieval_max_k {
        return Err(MemoryError::RetrievalLimitExceeded {
            requested: k,
            maximum: policy.retrieval_max_k,
        });
    }
    if candidates.ranked.len() > policy.retrieval_candidate_max {
        return Err(MemoryError::CandidateLimitExceeded {
            actual: candidates.ranked.len(),
            maximum: policy.retrieval_candidate_max,
        });
    }
    validate_unique(&candidates.ranked)?;
    validate_unique(&candidates.episodic_pool)?;
    validate_candidate_times(&candidates.ranked, query.as_of_us)?;
    validate_candidate_times(&candidates.episodic_pool, query.as_of_us)?;

    let mut scored = Vec::new();
    for candidate in &candidates.ranked {
        validate_candidate(candidate)?;
        if candidate.integrity != IntegrityStatusV1::Complete
            || !in_query_scope(query, &candidate.body)
        {
            continue;
        }
        let semantic = jaccard(&query.topic_keys, &candidate.body.topic_keys);
        let entity = jaccard(&query.entity_ids, &candidate.body.entity_ids);
        let validated_utility = candidate.state.validated_utility();
        let recency = recency_score(
            query.as_of_us,
            candidate.body.interval.recorded_at_us,
            policy.retrieval_recency_horizon_us,
        );
        let total = sum_scores([
            policy
                .retrieval_lexical_weight
                .multiply(candidate.lexical_score),
            policy.retrieval_semantic_weight.multiply(semantic),
            policy.retrieval_entity_weight.multiply(entity),
            policy.retrieval_utility_weight.multiply(validated_utility),
            policy.retrieval_recency_weight.multiply(recency),
        ]);
        scored.push((
            candidate.atom_id,
            candidate.body.kind(),
            RetrievalScoreV1 {
                lexical: candidate.lexical_score,
                semantic,
                entity,
                validated_utility,
                recency,
                total,
            },
        ));
    }
    scored.sort_unstable_by(|left, right| {
        right
            .2
            .total
            .cmp(&left.2.total)
            .then_with(|| left.0.cmp(&right.0))
    });
    let hits = scored
        .into_iter()
        .take(k)
        .map(|(atom_id, kind, score)| RetrievalHitV1 {
            atom_id,
            score: Some(score),
            recall_reason: RecallReasonV1::QueryRanked,
            generality: generality(kind),
        })
        .collect::<Vec<_>>();
    let excluded = hits.iter().map(|hit| hit.atom_id).collect::<BTreeSet<_>>();

    let spontaneous = if let Some(request) = query.spontaneous {
        if request.budget == 0 || request.budget > policy.flashback_budget_max {
            return Err(MemoryError::InvalidFlashbackBudget {
                maximum: policy.flashback_budget_max,
            });
        }
        let mut pool = Vec::new();
        for candidate in &candidates.episodic_pool {
            validate_candidate(candidate)?;
            if candidate.integrity == IntegrityStatusV1::Complete
                && candidate.body.kind() == MemoryKindV1::Episode
                && candidate.state.is_episodic_ltm()
                && in_query_scope(query, &candidate.body)
                && !excluded.contains(&candidate.atom_id)
            {
                pool.push(candidate);
            }
        }
        pool.sort_unstable_by_key(|candidate| candidate.atom_id);
        let population_evidence = pool
            .iter()
            .map(|candidate| candidate.atom_id)
            .collect::<Vec<_>>();
        let population_digest = Digest32::hash_prefixed(
            FLASHBACK_POPULATION_DOMAIN,
            &population_evidence.canonical_bytes()?,
        );
        let positions = partial_fisher_yates(
            pool.len(),
            request.budget.min(pool.len()),
            *request.seed.as_bytes(),
        );
        let flashback_hits = positions
            .into_iter()
            .map(|position| RetrievalHitV1 {
                atom_id: pool[position].atom_id,
                score: None,
                recall_reason: RecallReasonV1::Spontaneous,
                generality: GeneralityV1::SingleEpisode,
            })
            .collect();
        Some(SpontaneousRecallResultV1 {
            seed: request.seed,
            budget: request.budget,
            population_digest,
            population_count: pool.len(),
            hits: flashback_hits,
        })
    } else {
        None
    };

    let query_digest = Digest32::hash_prefixed(QUERY_HASH_DOMAIN, &query.canonical_bytes()?);
    Ok(RetrievalResultV1 {
        contract_version: "retrieval-result-v1".to_owned(),
        query_digest,
        hits,
        spontaneous,
    })
}

fn has_meaningful_query_input(query: &RetrievalQueryV1) -> bool {
    query
        .terms
        .iter()
        .chain(&query.topic_keys)
        .chain(&query.entity_ids)
        .any(|value| !value.trim().is_empty())
}

fn validate_unique(candidates: &[RetrievalCandidateV1]) -> crate::Result<()> {
    let mut seen = BTreeSet::new();
    for candidate in candidates {
        if !seen.insert(candidate.atom_id) {
            return Err(MemoryError::DuplicateAtom {
                atom_id: candidate.atom_id,
            });
        }
    }
    Ok(())
}

fn validate_candidate_times(
    candidates: &[RetrievalCandidateV1],
    as_of_us: u64,
) -> crate::Result<()> {
    for candidate in candidates {
        if candidate.body.interval.recorded_at_us > as_of_us {
            return Err(MemoryError::InvalidRetrievalCandidate {
                atom_id: candidate.atom_id,
                reason: "candidate recorded_at_us is later than query as_of_us",
            });
        }
    }
    Ok(())
}

fn validate_candidate(candidate: &RetrievalCandidateV1) -> crate::Result<()> {
    if candidate.body.contract_version != "atom-v1" {
        return Err(MemoryError::UnknownVersion(
            candidate.body.contract_version.clone(),
        ));
    }
    if candidate.body.atom_id()? != candidate.atom_id {
        return Err(MemoryError::AtomDigestMismatch {
            atom_id: candidate.atom_id,
        });
    }
    if candidate.state.content_status != ContentStatusV1::Present
        || candidate.state.retention_tier == RetentionTierV1::HeaderOnly
    {
        return Err(MemoryError::InvalidRetrievalCandidate {
            atom_id: candidate.atom_id,
            reason: "retrieval requires a present body outside the header-only tier",
        });
    }
    let tier_matches_kind = match candidate.body.kind() {
        MemoryKindV1::Episode => matches!(
            candidate.state.retention_tier,
            RetentionTierV1::ShortTerm
                | RetentionTierV1::EpisodicLongTerm
                | RetentionTierV1::DualLongTerm
        ),
        MemoryKindV1::Semantic => {
            candidate.state.retention_tier == RetentionTierV1::SemanticLongTerm
        }
    };
    if !tier_matches_kind {
        return Err(MemoryError::InvalidRetrievalCandidate {
            atom_id: candidate.atom_id,
            reason: "memory kind and retention tier disagree",
        });
    }
    Ok(())
}

fn in_query_scope(query: &RetrievalQueryV1, body: &MemoryAtomBodyV1) -> bool {
    body.scope.memory_space_id == query.memory_space_id
        && (query.memory_space_wide || body.scope.repository_id == query.repository_id)
}

fn recency_score(as_of_us: u64, recorded_at_us: u64, horizon_us: u64) -> Ppm {
    if recorded_at_us > as_of_us || horizon_us == 0 {
        return Ppm::ZERO;
    }
    let age = as_of_us - recorded_at_us;
    if age >= horizon_us {
        Ppm::ZERO
    } else {
        Ppm::ratio(horizon_us - age, horizon_us)
    }
}

fn jaccard(left: &BTreeSet<String>, right: &BTreeSet<String>) -> Ppm {
    let union = left.union(right).count();
    if union == 0 {
        Ppm::ZERO
    } else {
        Ppm::ratio(
            u64::try_from(left.intersection(right).count()).unwrap_or(u64::MAX),
            u64::try_from(union).unwrap_or(u64::MAX),
        )
    }
}

fn sum_scores(values: impl IntoIterator<Item = Ppm>) -> Ppm {
    let raw = values
        .into_iter()
        .map(Ppm::get)
        .map(u64::from)
        .sum::<u64>()
        .min(u64::from(Ppm::SCALE));
    Ppm::from_raw_unchecked(u32::try_from(raw).unwrap_or(Ppm::SCALE))
}

const fn generality(kind: MemoryKindV1) -> GeneralityV1 {
    match kind {
        MemoryKindV1::Episode => GeneralityV1::SingleEpisode,
        MemoryKindV1::Semantic => GeneralityV1::Semantic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EpisodePayloadV1, FormationSignalsV1, MemoryPayloadV1, MemoryScopeV1, ObservationV1,
        ProvenanceV1, RetentionPermissionV1, SealReasonV1, TimeIntervalV1,
    };
    use std::collections::BTreeMap;

    fn episode_body(index: u64, recorded_at_us: u64) -> crate::Result<MemoryAtomBodyV1> {
        let policy = PolicyV1::poc_v1();
        Ok(MemoryAtomBodyV1 {
            contract_version: "atom-v1".to_owned(),
            scope: MemoryScopeV1 {
                memory_space_id: "space-a".to_owned(),
                repository_id: Some("repo-a".to_owned()),
                task_id: Some("task-a".to_owned()),
                agent_id: Some("agent-a".to_owned()),
                session_id: format!("session-{index}"),
            },
            interval: TimeIntervalV1 {
                started_at_us: recorded_at_us,
                ended_at_us: recorded_at_us,
                recorded_at_us,
            },
            trigger: "query-boundary".to_owned(),
            seal_reason: SealReasonV1::Checkpoint,
            goal: None,
            plan: Vec::new(),
            internal_state_before: BTreeMap::new(),
            internal_state_after: BTreeMap::new(),
            observations: vec![ObservationV1 {
                at_us: recorded_at_us,
                source: "retrieval-test".to_owned(),
                content: format!("observation-{index}"),
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
            topic_keys: BTreeSet::from(["memory".to_owned()]),
            entity_ids: BTreeSet::from(["naome".to_owned()]),
            outcome_class: None,
            goal_key: None,
            provenance: ProvenanceV1 {
                producer: "retrieval-boundary-test".to_owned(),
                source_event_digests: vec![Digest32::hash_prefixed(
                    b"retrieval-event\0",
                    &index.to_be_bytes(),
                )],
                policy_digest: policy.digest()?,
            },
            relations: Vec::new(),
            artifacts: Vec::new(),
            retention_permission: RetentionPermissionV1::SemanticAndExactAllowed,
            payload: MemoryPayloadV1::Episode(EpisodePayloadV1 {
                event_sequence_start: index,
                event_sequence_end: index,
                continues: None,
            }),
        })
    }

    fn state(tier: RetentionTierV1) -> AtomStateV1 {
        AtomStateV1 {
            retention_tier: tier,
            content_status: ContentStatusV1::Present,
            expires_at_us: None,
            superseded_by: None,
            feedback_positive: 0,
            feedback_negative: 0,
            retrieval_count: 0,
            retirement_run_id: None,
        }
    }

    fn candidate(index: u64, recorded_at_us: u64) -> crate::Result<RetrievalCandidateV1> {
        let body = episode_body(index, recorded_at_us)?;
        Ok(RetrievalCandidateV1 {
            atom_id: body.atom_id()?,
            body,
            state: state(RetentionTierV1::ShortTerm),
            lexical_score: Ppm::from_raw_unchecked(500_000),
            integrity: IntegrityStatusV1::Complete,
        })
    }

    fn query(as_of_us: u64) -> RetrievalQueryV1 {
        RetrievalQueryV1 {
            contract_version: "retrieval-query-v1".to_owned(),
            memory_space_id: "space-a".to_owned(),
            repository_id: Some("repo-a".to_owned()),
            memory_space_wide: false,
            as_of_us,
            terms: vec!["memory".to_owned()],
            topic_keys: BTreeSet::from(["memory".to_owned()]),
            entity_ids: BTreeSet::from(["naome".to_owned()]),
            k: None,
            spontaneous: None,
        }
    }

    #[test]
    fn inclusive_public_limits_are_accepted() -> crate::Result<()> {
        let policy = PolicyV1::poc_v1();
        let mut boundary_query = query(1_000);
        boundary_query.k = Some(policy.retrieval_max_k);
        let ranked = (0_u64..u64::try_from(policy.retrieval_candidate_max).unwrap_or(u64::MAX))
            .map(|index| candidate(index, index))
            .collect::<crate::Result<Vec<_>>>()?;
        let result = rank_retrieval(
            &boundary_query,
            &RetrievalCandidatesV1 {
                ranked,
                episodic_pool: Vec::new(),
            },
            &policy,
        )?;
        assert_eq!(result.hits.len(), policy.retrieval_max_k);

        boundary_query.spontaneous = Some(SpontaneousRecallRequestV1 {
            seed: Seed32::new([7; 32]),
            budget: policy.flashback_budget_max,
        });
        let result = rank_retrieval(
            &boundary_query,
            &RetrievalCandidatesV1 {
                ranked: Vec::new(),
                episodic_pool: Vec::new(),
            },
            &policy,
        )?;
        assert_eq!(
            result.spontaneous.map(|recall| recall.budget),
            Some(policy.flashback_budget_max)
        );
        Ok(())
    }

    #[test]
    fn candidate_time_equal_to_query_time_is_not_future() -> crate::Result<()> {
        let candidate = candidate(1, 42)?;
        let result = rank_retrieval(
            &query(42),
            &RetrievalCandidatesV1 {
                ranked: vec![candidate.clone()],
                episodic_pool: Vec::new(),
            },
            &PolicyV1::poc_v1(),
        )?;
        assert_eq!(result.hits[0].atom_id, candidate.atom_id);
        Ok(())
    }

    #[test]
    fn body_presence_and_header_tier_are_independent_requirements() -> crate::Result<()> {
        let mut header_only = candidate(1, 1)?;
        header_only.state.retention_tier = RetentionTierV1::HeaderOnly;
        assert!(matches!(
            validate_candidate(&header_only),
            Err(MemoryError::InvalidRetrievalCandidate { .. })
        ));

        let mut forgotten = candidate(2, 2)?;
        forgotten.state.content_status = ContentStatusV1::Forgotten;
        assert!(matches!(
            validate_candidate(&forgotten),
            Err(MemoryError::InvalidRetrievalCandidate { .. })
        ));
        Ok(())
    }

    #[test]
    fn semantic_kind_accepts_only_the_semantic_long_term_tier() -> crate::Result<()> {
        let policy = PolicyV1::poc_v1();
        let source_ids = (1_u8..=3)
            .map(|value| AtomId(Digest32([value; 32])))
            .collect::<Vec<_>>();
        let mut body = episode_body(1, 1)?;
        body.observations.clear();
        body.provenance.source_event_digests =
            source_ids.iter().map(|source| source.digest()).collect();
        body.relations = source_ids
            .iter()
            .copied()
            .map(|target| crate::MemoryRelationV1 {
                kind: crate::MemoryRelationKindV1::DerivedFrom,
                target,
            })
            .collect();
        body.retention_permission = RetentionPermissionV1::SemanticAllowed;
        body.payload = MemoryPayloadV1::Semantic(crate::SemanticPayloadV1 {
            source_body_digests: source_ids
                .iter()
                .copied()
                .map(|source| (source, source.digest()))
                .collect(),
            source_ids,
            score: crate::ConsolidationScoreV1 {
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
        crate::validate_memory_atom_body(&body, &policy)?;

        let mut semantic = RetrievalCandidateV1 {
            atom_id: body.atom_id()?,
            body,
            state: state(RetentionTierV1::SemanticLongTerm),
            lexical_score: Ppm::from_raw_unchecked(500_000),
            integrity: IntegrityStatusV1::Complete,
        };
        validate_candidate(&semantic)?;

        semantic.state.retention_tier = RetentionTierV1::ShortTerm;
        assert!(matches!(
            validate_candidate(&semantic),
            Err(MemoryError::InvalidRetrievalCandidate { .. })
        ));
        Ok(())
    }

    #[test]
    fn recency_boundaries_and_arithmetic_are_exact() {
        assert_eq!(recency_score(10, 11, 10), Ppm::ZERO);
        assert_eq!(recency_score(10, 9, 0), Ppm::ZERO);
        assert_eq!(recency_score(10, 10, 10), Ppm::ONE);
        assert_eq!(recency_score(10, 0, 10), Ppm::ZERO);
        assert_eq!(recency_score(10, 4, 10).get(), 400_000);
    }

    #[test]
    fn set_similarity_and_score_sum_preserve_nonzero_evidence() {
        assert_eq!(jaccard(&BTreeSet::new(), &BTreeSet::new()), Ppm::ZERO);
        assert_eq!(
            jaccard(
                &BTreeSet::from(["a".to_owned(), "b".to_owned()]),
                &BTreeSet::from(["b".to_owned(), "c".to_owned()]),
            )
            .get(),
            333_333
        );
        assert_eq!(
            sum_scores([
                Ppm::from_raw_unchecked(100_000),
                Ppm::from_raw_unchecked(200_000),
            ])
            .get(),
            300_000
        );
        assert_eq!(
            sum_scores([
                Ppm::from_raw_unchecked(600_000),
                Ppm::from_raw_unchecked(500_000),
            ]),
            Ppm::ONE
        );
    }
}
