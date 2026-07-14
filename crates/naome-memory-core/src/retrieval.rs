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
