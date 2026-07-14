//! Slow test-only retrieval ranking and flashback reference.
//!
//! Inputs are assumed to have passed the production validator. This model is
//! intentionally limited to decision parity over valid candidates.

use std::collections::BTreeSet;

use naome_memory_core::{
    CanonicalBytes as _, Digest32, GeneralityV1, IntegrityStatusV1, MemoryKindV1, PolicyV1, Ppm,
    RecallReasonV1, RetrievalCandidatesV1, RetrievalHitV1, RetrievalQueryV1, RetrievalResultV1,
    RetrievalScoreV1, Seed32, SpontaneousRecallResultV1,
};
use rand_chacha::ChaCha20Rng;
use rand_core::{RngCore as _, SeedableRng as _};

const QUERY_HASH_DOMAIN: &[u8] = b"naome-memory:retrieval-query:v1\0";
const FLASHBACK_POPULATION_DOMAIN: &[u8] = b"naome-memory:flashback-population:v1\0";

pub fn rank_reference_retrieval(
    query: &RetrievalQueryV1,
    candidates: &RetrievalCandidatesV1,
    policy: &PolicyV1,
) -> naome_memory_core::Result<RetrievalResultV1> {
    let mut scored = candidates
        .ranked
        .iter()
        .filter(|candidate| {
            candidate.integrity == IntegrityStatusV1::Complete
                && candidate.body.scope.memory_space_id == query.memory_space_id
                && (query.memory_space_wide
                    || candidate.body.scope.repository_id == query.repository_id)
        })
        .map(|candidate| {
            let semantic = reference_jaccard(&query.topic_keys, &candidate.body.topic_keys);
            let entity = reference_jaccard(&query.entity_ids, &candidate.body.entity_ids);
            let utility = reference_utility(
                candidate.state.feedback_positive,
                candidate.state.feedback_negative,
            );
            let recency = reference_recency(
                query.as_of_us,
                candidate.body.interval.recorded_at_us,
                policy.retrieval_recency_horizon_us,
            );
            let total = [
                weighted(
                    candidate.lexical_score.get(),
                    policy.retrieval_lexical_weight.get(),
                ),
                weighted(semantic, policy.retrieval_semantic_weight.get()),
                weighted(entity, policy.retrieval_entity_weight.get()),
                weighted(utility, policy.retrieval_utility_weight.get()),
                weighted(recency, policy.retrieval_recency_weight.get()),
            ]
            .into_iter()
            .map(u64::from)
            .sum::<u64>()
            .min(u64::from(Ppm::SCALE));
            (
                candidate.atom_id,
                candidate.body.kind(),
                RetrievalScoreV1 {
                    lexical: candidate.lexical_score,
                    semantic: ppm(semantic),
                    entity: ppm(entity),
                    validated_utility: ppm(utility),
                    recency: ppm(recency),
                    total: ppm(u32::try_from(total).unwrap_or(Ppm::SCALE)),
                },
            )
        })
        .collect::<Vec<_>>();
    scored.sort_unstable_by(|left, right| {
        right
            .2
            .total
            .cmp(&left.2.total)
            .then_with(|| left.0.cmp(&right.0))
    });
    let hits = scored
        .into_iter()
        .take(query.k.unwrap_or(policy.retrieval_default_k))
        .map(|(atom_id, kind, score)| RetrievalHitV1 {
            atom_id,
            score: Some(score),
            recall_reason: RecallReasonV1::QueryRanked,
            generality: reference_generality(kind),
        })
        .collect::<Vec<_>>();
    let spontaneous = reference_flashbacks(query, candidates, &hits)?;
    Ok(RetrievalResultV1 {
        contract_version: "retrieval-result-v1".to_owned(),
        query_digest: Digest32::hash_prefixed(QUERY_HASH_DOMAIN, &query.canonical_bytes()?),
        hits,
        spontaneous,
    })
}

fn reference_flashbacks(
    query: &RetrievalQueryV1,
    candidates: &RetrievalCandidatesV1,
    ranked_hits: &[RetrievalHitV1],
) -> naome_memory_core::Result<Option<SpontaneousRecallResultV1>> {
    let Some(request) = query.spontaneous else {
        return Ok(None);
    };
    let excluded = ranked_hits
        .iter()
        .map(|hit| hit.atom_id)
        .collect::<BTreeSet<_>>();
    let mut pool = candidates
        .episodic_pool
        .iter()
        .filter(|candidate| {
            candidate.integrity == IntegrityStatusV1::Complete
                && candidate.body.kind() == MemoryKindV1::Episode
                && candidate.state.is_episodic_ltm()
                && candidate.body.scope.memory_space_id == query.memory_space_id
                && (query.memory_space_wide
                    || candidate.body.scope.repository_id == query.repository_id)
                && !excluded.contains(&candidate.atom_id)
        })
        .collect::<Vec<_>>();
    pool.sort_unstable_by_key(|candidate| candidate.atom_id);
    let evidence = pool
        .iter()
        .map(|candidate| candidate.atom_id)
        .collect::<Vec<_>>();
    let population_digest =
        Digest32::hash_prefixed(FLASHBACK_POPULATION_DOMAIN, &evidence.canonical_bytes()?);
    let positions =
        reference_partial_fisher_yates(pool.len(), request.budget.min(pool.len()), request.seed);
    let hits = positions
        .into_iter()
        .map(|position| RetrievalHitV1 {
            atom_id: pool[position].atom_id,
            score: None,
            recall_reason: RecallReasonV1::Spontaneous,
            generality: GeneralityV1::SingleEpisode,
        })
        .collect();
    Ok(Some(SpontaneousRecallResultV1 {
        seed: request.seed,
        budget: request.budget,
        population_digest,
        population_count: pool.len(),
        hits,
    }))
}

fn reference_partial_fisher_yates(population: usize, count: usize, seed: Seed32) -> Vec<usize> {
    let mut positions = (0..population).collect::<Vec<_>>();
    let mut rng = ChaCha20Rng::from_seed(*seed.as_bytes());
    for index in 0..count.min(population) {
        let remaining = population.saturating_sub(index);
        let offset = usize::try_from(reference_uniform_below(
            &mut rng,
            u64::try_from(remaining).unwrap_or(u64::MAX),
        ))
        .unwrap_or(0);
        positions.swap(index, index.saturating_add(offset));
    }
    positions.truncate(count.min(population));
    positions
}

fn reference_uniform_below(rng: &mut ChaCha20Rng, bound: u64) -> u64 {
    if bound <= 1 {
        return 0;
    }
    let threshold = bound.wrapping_neg() % bound;
    loop {
        let value = rng.next_u64();
        if value >= threshold {
            return value % bound;
        }
    }
}

fn reference_jaccard(left: &BTreeSet<String>, right: &BTreeSet<String>) -> u32 {
    let union = left.union(right).count();
    if union == 0 {
        0
    } else {
        ratio_ppm(
            u64::try_from(left.intersection(right).count()).unwrap_or(u64::MAX),
            u64::try_from(union).unwrap_or(u64::MAX),
        )
    }
}

fn reference_utility(positive: u64, negative: u64) -> u32 {
    ratio_ppm(
        positive.saturating_add(1),
        positive.saturating_add(negative).saturating_add(2),
    )
}

fn reference_recency(as_of_us: u64, recorded_at_us: u64, horizon_us: u64) -> u32 {
    if recorded_at_us > as_of_us || horizon_us == 0 {
        return 0;
    }
    let age = as_of_us - recorded_at_us;
    if age >= horizon_us {
        0
    } else {
        ratio_ppm(horizon_us - age, horizon_us)
    }
}

fn ratio_ppm(numerator: u64, denominator: u64) -> u32 {
    if denominator == 0 {
        return 0;
    }
    let scaled =
        u128::from(numerator).saturating_mul(u128::from(Ppm::SCALE)) / u128::from(denominator);
    u32::try_from(scaled.min(u128::from(Ppm::SCALE))).unwrap_or(Ppm::SCALE)
}

fn weighted(value: u32, weight: u32) -> u32 {
    u32::try_from(u64::from(value).saturating_mul(u64::from(weight)) / u64::from(Ppm::SCALE))
        .unwrap_or(Ppm::SCALE)
}

const fn ppm(value: u32) -> Ppm {
    Ppm::from_raw_unchecked(value)
}

const fn reference_generality(kind: MemoryKindV1) -> GeneralityV1 {
    match kind {
        MemoryKindV1::Episode => GeneralityV1::SingleEpisode,
        MemoryKindV1::Semantic => GeneralityV1::Semantic,
    }
}
