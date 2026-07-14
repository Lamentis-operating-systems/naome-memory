//! Deliberately slow, test-only retirement reference model.
//!
//! This module does not call the production clustering, scoring, semantic-body,
//! lottery, decision, or post-state helpers. It favors direct scans and simple
//! data structures so a shared optimization cannot make both sides agree.

use std::collections::{BTreeMap, BTreeSet};

use naome_memory_core::{
    ActionRecordV1, AtomId, AtomStateV1, CanonicalBytes as _, ConsolidationScoreV1,
    ContentStatusV1, DecisionRecordV1, Digest32, FeedbackSignalV1, FormationSignalsV1,
    IntegrityStatusV1, InterpretationV1, MemoryAtomBodyV1, MemoryError, MemoryKindV1,
    MemoryPayloadV1, MemoryRelationKindV1, MemoryRelationV1, MemoryScopeV1, ObservationV1,
    PlannedSemanticAtomV1, PolicyV1, Ppm, ProvenanceV1, RejectedAlternativeV1,
    RetentionPermissionV1, RetentionTierV1, RetirementCandidateV1, RetirementDecisionV1,
    RetirementPlanV1, RetirementPostStateAtomV1, RetirementPostStateV1, RetirementSnapshotV1,
    SealReasonV1, Seed32, SemanticCandidateDecisionV1, SemanticCandidateDispositionV1,
    SemanticPayloadV1, TimeIntervalV1,
};
use rand_chacha::ChaCha20Rng;
use rand_core::{RngCore as _, SeedableRng as _};
use serde::Serialize;

const STATE_HASH_DOMAIN: &[u8] = b"naome-memory:projected-state:v1\0";
const POPULATION_HASH_DOMAIN: &[u8] = b"naome-memory:lottery-population:v1\0";
const INPUT_HASH_DOMAIN: &[u8] = b"naome-memory:retirement-input:v1\0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceRetirementContractV1 {
    pub full_plan: RetirementPlanV1,
    pub episodic_population_digest: Digest32,
    pub episodic_population_count: usize,
    pub episodic_winner_budget: usize,
    pub episodic_winner_bytes: u64,
    pub semantic_count_budget: usize,
    pub semantic_body_bytes: u64,
    pub semantic_decisions: Vec<SemanticCandidateDecisionV1>,
    pub semantic_atoms: Vec<PlannedSemanticAtomV1>,
    pub episodic_winners: Vec<AtomId>,
    pub decisions: Vec<RetirementDecisionV1>,
    pub projected_post_state: RetirementPostStateV1,
    pub projected_state_digest: Digest32,
}

pub fn plan_reference_retirement(
    snapshot: &RetirementSnapshotV1,
    policy: &PolicyV1,
    seed: Seed32,
) -> naome_memory_core::Result<ReferenceRetirementContractV1> {
    let mut candidates = snapshot.atoms.iter().collect::<Vec<_>>();
    candidates.sort_unstable_by_key(|candidate| candidate.atom_id);
    let semantics = reference_semantic_selection(snapshot, policy, &candidates)?;
    let lottery = reference_lottery(policy, seed, &candidates)?;
    let decisions = reference_source_decisions(&candidates, &semantics.atoms, &lottery.winners)?;

    let projected_post_state =
        reference_projected_post_state(snapshot, &candidates, &decisions, &semantics.atoms)?;
    let projected_state_digest =
        Digest32::hash_prefixed(STATE_HASH_DOMAIN, &projected_post_state.canonical_bytes()?);
    let full_plan = RetirementPlanV1 {
        contract_version: "retirement-plan-v1".to_owned(),
        policy_id: policy.policy_id.clone(),
        policy_digest: policy.digest()?,
        seed,
        memory_space_id: snapshot.memory_space_id.clone(),
        cohort_day: snapshot.cohort_day,
        as_of_us: snapshot.as_of_us,
        input_set_digest: reference_input_digest(snapshot)?,
        rng_identifier: "chacha20-rand_chacha-0.3/partial-fisher-yates-v1".to_owned(),
        episodic_population_digest: lottery.population_digest,
        episodic_population_count: lottery.population_count,
        episodic_winner_budget: lottery.winner_budget,
        episodic_winner_bytes: lottery.winner_bytes,
        semantic_count_budget: semantics.count_budget,
        semantic_body_bytes: semantics.body_bytes,
        semantic_decisions: semantics.decisions.clone(),
        semantic_atoms: semantics.atoms.clone(),
        episodic_winners: lottery.winners.clone(),
        decisions: decisions.clone(),
        projected_post_state: projected_post_state.clone(),
        projected_state_digest,
    };

    Ok(ReferenceRetirementContractV1 {
        full_plan,
        episodic_population_digest: lottery.population_digest,
        episodic_population_count: lottery.population_count,
        episodic_winner_budget: lottery.winner_budget,
        episodic_winner_bytes: lottery.winner_bytes,
        semantic_count_budget: semantics.count_budget,
        semantic_body_bytes: semantics.body_bytes,
        semantic_decisions: semantics.decisions,
        semantic_atoms: semantics.atoms,
        episodic_winners: lottery.winners,
        decisions,
        projected_post_state,
        projected_state_digest,
    })
}

struct ReferenceSemanticSelectionV1 {
    count_budget: usize,
    body_bytes: u64,
    decisions: Vec<SemanticCandidateDecisionV1>,
    atoms: Vec<PlannedSemanticAtomV1>,
}

struct ReferenceLotteryV1 {
    population_digest: Digest32,
    population_count: usize,
    winner_budget: usize,
    winner_bytes: u64,
    winners: Vec<AtomId>,
}

#[derive(Serialize)]
struct ReferenceInputAuthorityV1<'a> {
    contract_version: &'a str,
    memory_space_id: &'a str,
    cohort_day: u64,
    as_of_us: u64,
    atoms: Vec<ReferenceInputCandidateV1<'a>>,
}

#[derive(Serialize)]
struct ReferenceInputCandidateV1<'a> {
    atom_id: AtomId,
    body_digest: Digest32,
    state: &'a AtomStateV1,
    integrity: IntegrityStatusV1,
    exact_package_bytes: u64,
}

fn reference_input_digest(snapshot: &RetirementSnapshotV1) -> naome_memory_core::Result<Digest32> {
    let mut candidates = snapshot.atoms.iter().collect::<Vec<_>>();
    candidates.sort_unstable_by_key(|candidate| candidate.atom_id);
    let authority = ReferenceInputAuthorityV1 {
        contract_version: &snapshot.contract_version,
        memory_space_id: &snapshot.memory_space_id,
        cohort_day: snapshot.cohort_day,
        as_of_us: snapshot.as_of_us,
        atoms: candidates
            .into_iter()
            .map(|candidate| ReferenceInputCandidateV1 {
                atom_id: candidate.atom_id,
                body_digest: candidate.body_digest,
                state: &candidate.state,
                integrity: candidate.integrity,
                exact_package_bytes: candidate.exact_package_bytes,
            })
            .collect(),
    };
    Ok(Digest32::hash_prefixed(
        INPUT_HASH_DOMAIN,
        &authority.canonical_bytes()?,
    ))
}

fn reference_semantic_selection(
    snapshot: &RetirementSnapshotV1,
    policy: &PolicyV1,
    candidates: &[&RetirementCandidateV1],
) -> naome_memory_core::Result<ReferenceSemanticSelectionV1> {
    let scored = reference_scored_semantic_candidates(snapshot, policy, candidates)?;
    let count_budget =
        ceil_ppm(snapshot.atoms.len(), policy.semantic_rate.get()).min(policy.semantic_count_max);
    let mut atoms = Vec::new();
    let mut decisions = Vec::with_capacity(scored.len());
    let mut body_bytes = 0_u64;
    for (score, semantic) in scored {
        let MemoryPayloadV1::Semantic(payload) = &semantic.body.payload else {
            return Err(MemoryError::CanonicalEncoding(
                "reference semantic candidate has an episode payload".to_owned(),
            ));
        };
        let disposition = reference_semantic_disposition(
            semantic.canonical_size_bytes,
            atoms.len(),
            count_budget,
            &mut body_bytes,
            policy,
        )?;
        decisions.push(SemanticCandidateDecisionV1 {
            atom_id: semantic.atom_id,
            source_ids: payload.source_ids.clone(),
            score,
            canonical_size_bytes: semantic.canonical_size_bytes,
            disposition,
        });
        if disposition == SemanticCandidateDispositionV1::Selected {
            atoms.push(semantic);
        }
    }
    Ok(ReferenceSemanticSelectionV1 {
        count_budget,
        body_bytes,
        decisions,
        atoms,
    })
}

fn reference_scored_semantic_candidates(
    snapshot: &RetirementSnapshotV1,
    policy: &PolicyV1,
    candidates: &[&RetirementCandidateV1],
) -> naome_memory_core::Result<Vec<(ConsolidationScoreV1, PlannedSemanticAtomV1)>> {
    let mut scored = Vec::new();
    for cluster in reference_complete_link_clusters(candidates, policy) {
        if !reference_cluster_is_eligible(&cluster, policy) {
            continue;
        }
        let score = reference_consolidation_score(&cluster, policy);
        if score.total < policy.consolidation_threshold {
            continue;
        }
        let body = reference_semantic_body(&cluster, score, snapshot, policy)?;
        let canonical_size_bytes = reference_body_size(&body)?;
        let atom_id = AtomId(Digest32::hash_prefixed(
            b"naome-memory:atom:v1\0",
            &body.canonical_bytes()?,
        ));
        scored.push((
            score,
            PlannedSemanticAtomV1 {
                atom_id,
                body_digest: atom_id.digest(),
                canonical_size_bytes,
                body,
            },
        ));
    }
    scored.sort_unstable_by(|left, right| {
        right
            .0
            .total
            .cmp(&left.0.total)
            .then_with(|| left.1.atom_id.cmp(&right.1.atom_id))
    });
    Ok(scored)
}

fn reference_cluster_is_eligible(cluster: &[&RetirementCandidateV1], policy: &PolicyV1) -> bool {
    cluster.len() >= policy.min_semantic_sources
        && cluster
            .iter()
            .filter_map(|candidate| candidate.body.as_ref())
            .map(|body| body.scope.session_id.as_str())
            .collect::<BTreeSet<_>>()
            .len()
            >= policy.min_semantic_sessions
}

fn reference_semantic_disposition(
    candidate_bytes: u64,
    selected_count: usize,
    count_budget: usize,
    body_bytes: &mut u64,
    policy: &PolicyV1,
) -> naome_memory_core::Result<SemanticCandidateDispositionV1> {
    if candidate_bytes > policy.atom_hard_max_bytes {
        return Ok(SemanticCandidateDispositionV1::SkippedAtomHardLimit);
    }
    if selected_count >= count_budget {
        return Ok(SemanticCandidateDispositionV1::SkippedCountBudget);
    }
    let next_bytes = body_bytes.checked_add(candidate_bytes).ok_or_else(|| {
        MemoryError::CanonicalEncoding("reference semantic byte total overflowed u64".to_owned())
    })?;
    if next_bytes > policy.semantic_body_budget_bytes {
        return Ok(SemanticCandidateDispositionV1::SkippedByteBudget);
    }
    *body_bytes = next_bytes;
    Ok(SemanticCandidateDispositionV1::Selected)
}

fn reference_lottery(
    policy: &PolicyV1,
    seed: Seed32,
    candidates: &[&RetirementCandidateV1],
) -> naome_memory_core::Result<ReferenceLotteryV1> {
    let population = reference_population(candidates);
    let evidence = population
        .iter()
        .map(|candidate| {
            (
                candidate.atom_id,
                candidate.body_digest,
                candidate.exact_package_bytes,
            )
        })
        .collect::<Vec<_>>();
    let population_digest =
        Digest32::hash_prefixed(POPULATION_HASH_DOMAIN, &evidence.canonical_bytes()?);
    let winner_budget =
        ceil_ppm(population.len(), policy.episodic_rate.get()).min(policy.episodic_count_max);
    let positions = reference_partial_fisher_yates(population.len(), winner_budget, seed);
    let winner_bytes = positions.iter().try_fold(0_u64, |total, position| {
        total
            .checked_add(population[*position].exact_package_bytes)
            .ok_or_else(|| {
                MemoryError::CanonicalEncoding(
                    "reference episodic byte total overflowed u64".to_owned(),
                )
            })
    })?;
    if winner_bytes > policy.episodic_pin_budget_bytes {
        return Err(MemoryError::EpisodicBudgetExceeded {
            actual: winner_bytes,
            maximum: policy.episodic_pin_budget_bytes,
        });
    }
    let mut winners = positions
        .into_iter()
        .map(|position| population[position].atom_id)
        .collect::<Vec<_>>();
    winners.sort_unstable();
    Ok(ReferenceLotteryV1 {
        population_digest,
        population_count: population.len(),
        winner_budget,
        winner_bytes,
        winners,
    })
}

fn reference_source_decisions(
    candidates: &[&RetirementCandidateV1],
    semantic_atoms: &[PlannedSemanticAtomV1],
    episodic_winners: &[AtomId],
) -> naome_memory_core::Result<Vec<RetirementDecisionV1>> {
    let winner_set = episodic_winners.iter().copied().collect::<BTreeSet<_>>();
    let mut semantics_by_source = BTreeMap::<AtomId, Vec<AtomId>>::new();
    for semantic in semantic_atoms {
        let MemoryPayloadV1::Semantic(payload) = &semantic.body.payload else {
            return Err(MemoryError::CanonicalEncoding(
                "reference selected semantic atom has an episode payload".to_owned(),
            ));
        };
        for source in &payload.source_ids {
            semantics_by_source
                .entry(*source)
                .or_default()
                .push(semantic.atom_id);
        }
    }
    Ok(candidates
        .iter()
        .map(|candidate| {
            reference_source_decision(candidate, &winner_set, &mut semantics_by_source)
        })
        .collect())
}

fn reference_source_decision(
    candidate: &RetirementCandidateV1,
    winner_set: &BTreeSet<AtomId>,
    semantics_by_source: &mut BTreeMap<AtomId, Vec<AtomId>>,
) -> RetirementDecisionV1 {
    let exact_retained = winner_set.contains(&candidate.atom_id);
    let mut semantic_atom_ids = semantics_by_source
        .remove(&candidate.atom_id)
        .unwrap_or_default();
    semantic_atom_ids.sort_unstable();
    let exact_artifact_digests = if exact_retained {
        candidate
            .body
            .as_ref()
            .map_or_else(Vec::new, reference_retained_artifact_digests)
    } else {
        Vec::new()
    };
    RetirementDecisionV1 {
        atom_id: candidate.atom_id,
        semantic_atom_ids,
        exact_retained,
        exact_artifact_digests,
        forget_episode_body: candidate
            .body
            .as_ref()
            .is_some_and(|body| body.kind() == MemoryKindV1::Episode)
            && !exact_retained,
    }
}

fn reference_complete_link_clusters<'a>(
    candidates: &[&'a RetirementCandidateV1],
    policy: &PolicyV1,
) -> Vec<Vec<&'a RetirementCandidateV1>> {
    let eligible = candidates
        .iter()
        .copied()
        .filter(|candidate| {
            candidate.integrity == IntegrityStatusV1::Complete
                && candidate.body.as_ref().is_some_and(|body| {
                    body.kind() == MemoryKindV1::Episode
                        && body.retention_permission != RetentionPermissionV1::TransientOnly
                })
        })
        .collect::<Vec<_>>();
    let mut clusters: Vec<Vec<&RetirementCandidateV1>> = Vec::new();
    for candidate in eligible {
        let destination = clusters.iter().position(|cluster| {
            cluster.iter().all(|member| {
                reference_similarity(candidate, member, policy)
                    >= policy.cluster_similarity_threshold.get()
            })
        });
        if let Some(index) = destination {
            clusters[index].push(candidate);
        } else {
            clusters.push(vec![candidate]);
        }
    }
    clusters
}

fn reference_population<'a>(
    candidates: &[&'a RetirementCandidateV1],
) -> Vec<&'a RetirementCandidateV1> {
    candidates
        .iter()
        .copied()
        .filter(|candidate| {
            candidate.integrity == IntegrityStatusV1::Complete
                && candidate.body.as_ref().is_some_and(|body| {
                    body.kind() == MemoryKindV1::Episode
                        && body.retention_permission
                            == RetentionPermissionV1::SemanticAndExactAllowed
                })
        })
        .collect()
}

fn reference_similarity(
    left: &RetirementCandidateV1,
    right: &RetirementCandidateV1,
    policy: &PolicyV1,
) -> u32 {
    let Some(left_body) = left.body.as_ref() else {
        return 0;
    };
    let Some(right_body) = right.body.as_ref() else {
        return 0;
    };
    let topic = reference_jaccard(&left_body.topic_keys, &right_body.topic_keys);
    let entity = reference_jaccard(&left_body.entity_ids, &right_body.entity_ids);
    let outcome = if left_body
        .outcome_class
        .as_deref()
        .is_some_and(|value| !value.is_empty())
        && left_body.outcome_class == right_body.outcome_class
    {
        Ppm::SCALE
    } else {
        0
    };
    weighted(topic, policy.similarity_topic_weight.get())
        .saturating_add(weighted(entity, policy.similarity_entity_weight.get()))
        .saturating_add(weighted(outcome, policy.similarity_outcome_weight.get()))
        .min(Ppm::SCALE)
}

fn reference_jaccard(left: &BTreeSet<String>, right: &BTreeSet<String>) -> u32 {
    let union = left.union(right).count();
    if union == 0 {
        return 0;
    }
    ratio_ppm(left.intersection(right).count(), union)
}

fn reference_consolidation_score(
    cluster: &[&RetirementCandidateV1],
    policy: &PolicyV1,
) -> ConsolidationScoreV1 {
    let bodies = cluster
        .iter()
        .filter_map(|candidate| candidate.body.as_ref())
        .collect::<Vec<_>>();
    let support = ratio_ppm(bodies.len(), policy.support_saturation_atoms);
    let session_count = bodies
        .iter()
        .map(|body| body.scope.session_id.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let diversity = ratio_ppm(session_count, bodies.len());
    let utility = reference_mean(cluster.iter().map(|candidate| {
        let positive = candidate.state.feedback_positive.saturating_add(1);
        let denominator = candidate
            .state
            .feedback_positive
            .saturating_add(candidate.state.feedback_negative)
            .saturating_add(2);
        ratio_ppm_u64(positive, denominator)
    }));
    let salience = reference_mean(
        bodies
            .iter()
            .map(|body| body.formation_signals.salience.get()),
    );
    let novelty = reference_mean(
        bodies
            .iter()
            .map(|body| body.formation_signals.novelty.get()),
    );
    let uncertainty = reference_mean(
        bodies
            .iter()
            .map(|body| body.formation_signals.uncertainty.get()),
    );
    let mut cohesion = Ppm::SCALE;
    for (position, left) in cluster.iter().enumerate() {
        for right in cluster.iter().skip(position.saturating_add(1)) {
            cohesion = cohesion.min(reference_similarity(left, right, policy));
        }
    }
    let positive = [
        (support, policy.consolidation_support_weight.get()),
        (diversity, policy.consolidation_diversity_weight.get()),
        (utility, policy.consolidation_utility_weight.get()),
        (salience, policy.consolidation_salience_weight.get()),
        (cohesion, policy.consolidation_cohesion_weight.get()),
        (novelty, policy.consolidation_novelty_weight.get()),
    ]
    .into_iter()
    .map(|(value, weight)| u64::from(weighted(value, weight)))
    .sum::<u64>();
    let negative = u64::from(weighted(
        uncertainty,
        policy.consolidation_uncertainty_weight.get(),
    ));
    let total = u32::try_from(positive.saturating_sub(negative).min(u64::from(Ppm::SCALE)))
        .unwrap_or(Ppm::SCALE);
    ConsolidationScoreV1 {
        support: ppm(support),
        diversity: ppm(diversity),
        utility: ppm(utility),
        salience: ppm(salience),
        cohesion: ppm(cohesion),
        novelty: ppm(novelty),
        uncertainty: ppm(uncertainty),
        total: ppm(total),
    }
}

fn reference_semantic_body(
    cluster: &[&RetirementCandidateV1],
    score: ConsolidationScoreV1,
    snapshot: &RetirementSnapshotV1,
    policy: &PolicyV1,
) -> naome_memory_core::Result<MemoryAtomBodyV1> {
    let mut sources = cluster.to_vec();
    sources.sort_unstable_by_key(|candidate| candidate.atom_id);
    let bodies = sources
        .iter()
        .map(|candidate| {
            candidate.body.as_ref().ok_or_else(|| {
                MemoryError::CanonicalEncoding(
                    "reference semantic source is missing its body".to_owned(),
                )
            })
        })
        .collect::<naome_memory_core::Result<Vec<_>>>()?;
    let required = consensus_required(bodies.len(), policy);
    let source_ids = sources
        .iter()
        .map(|candidate| candidate.atom_id)
        .collect::<Vec<_>>();
    let source_body_digests = source_ids
        .iter()
        .map(|source| (*source, source.digest()))
        .collect::<BTreeMap<_, _>>();
    let output_scope = reference_semantic_scope(&bodies, snapshot, required);
    let interval = reference_semantic_interval(&bodies, snapshot.as_of_us);

    Ok(MemoryAtomBodyV1 {
        contract_version: "atom-v1".to_owned(),
        scope: output_scope,
        interval,
        trigger: "poc-v1-retirement".to_owned(),
        seal_reason: SealReasonV1::Checkpoint,
        goal: consensus_optional(
            bodies.iter().filter_map(|body| body.goal.as_ref()),
            required,
        ),
        plan: consensus_body_strings(&bodies, required, |body| &body.plan),
        internal_state_before: consensus_map(&bodies, required, false),
        internal_state_after: consensus_map(&bodies, required, true),
        observations: Vec::<ObservationV1>::new(),
        interpretations: Vec::<InterpretationV1>::new(),
        beliefs: Vec::new(),
        decisions: Vec::<DecisionRecordV1>::new(),
        rejected_alternatives: Vec::<RejectedAlternativeV1>::new(),
        actions: Vec::<ActionRecordV1>::new(),
        outcome: None,
        feedback: Vec::<FeedbackSignalV1>::new(),
        formation_signals: FormationSignalsV1 {
            utility: score.utility,
            salience: score.salience,
            novelty: score.novelty,
            uncertainty: score.uncertainty,
        },
        topic_keys: consensus_set(
            bodies.iter().flat_map(|body| body.topic_keys.iter()),
            required,
        ),
        entity_ids: consensus_set(
            bodies.iter().flat_map(|body| body.entity_ids.iter()),
            required,
        ),
        outcome_class: consensus_optional(
            bodies.iter().filter_map(|body| body.outcome_class.as_ref()),
            required,
        ),
        goal_key: consensus_optional(
            bodies.iter().filter_map(|body| body.goal_key.as_ref()),
            required,
        ),
        provenance: ProvenanceV1 {
            producer: "naome-memory-core/plan_retirement".to_owned(),
            source_event_digests: source_ids.iter().map(|source| source.digest()).collect(),
            policy_digest: policy.digest()?,
        },
        relations: source_ids
            .iter()
            .map(|source| MemoryRelationV1 {
                kind: MemoryRelationKindV1::DerivedFrom,
                target: *source,
            })
            .collect(),
        artifacts: Vec::new(),
        retention_permission: RetentionPermissionV1::SemanticAllowed,
        payload: MemoryPayloadV1::Semantic(SemanticPayloadV1 {
            source_ids,
            source_body_digests,
            score,
            consensus_numerator: policy.consensus_numerator,
            consensus_denominator: policy.consensus_denominator,
            supersedes: None,
        }),
    })
}

fn reference_semantic_scope(
    bodies: &[&MemoryAtomBodyV1],
    snapshot: &RetirementSnapshotV1,
    required: usize,
) -> MemoryScopeV1 {
    MemoryScopeV1 {
        memory_space_id: snapshot.memory_space_id.clone(),
        repository_id: consensus_optional(
            bodies
                .iter()
                .filter_map(|body| body.scope.repository_id.as_ref()),
            required,
        ),
        task_id: consensus_optional(
            bodies.iter().filter_map(|body| body.scope.task_id.as_ref()),
            required,
        ),
        agent_id: consensus_optional(
            bodies
                .iter()
                .filter_map(|body| body.scope.agent_id.as_ref()),
            required,
        ),
        session_id: format!("semantic:{}", snapshot.cohort_day),
    }
}

fn reference_semantic_interval(
    bodies: &[&MemoryAtomBodyV1],
    recorded_at_us: u64,
) -> TimeIntervalV1 {
    TimeIntervalV1 {
        started_at_us: bodies
            .iter()
            .map(|body| body.interval.started_at_us)
            .min()
            .unwrap_or(recorded_at_us),
        ended_at_us: bodies
            .iter()
            .map(|body| body.interval.ended_at_us)
            .max()
            .unwrap_or(recorded_at_us),
        recorded_at_us,
    }
}

fn reference_projected_post_state(
    snapshot: &RetirementSnapshotV1,
    candidates: &[&RetirementCandidateV1],
    decisions: &[RetirementDecisionV1],
    semantic_atoms: &[PlannedSemanticAtomV1],
) -> naome_memory_core::Result<RetirementPostStateV1> {
    let source_atoms = candidates
        .iter()
        .zip(decisions)
        .map(|(candidate, decision)| {
            let body = candidate
                .body
                .as_ref()
                .ok_or(MemoryError::AtomDigestMismatch {
                    atom_id: candidate.atom_id,
                })?;
            let (retention_tier, content_status, stored_body_digest, search_projection_present) =
                if decision.exact_retained {
                    (
                        RetentionTierV1::EpisodicLongTerm,
                        ContentStatusV1::Present,
                        Some(candidate.body_digest),
                        true,
                    )
                } else {
                    (
                        RetentionTierV1::HeaderOnly,
                        ContentStatusV1::Forgotten,
                        None,
                        false,
                    )
                };
            Ok(RetirementPostStateAtomV1 {
                atom_id: candidate.atom_id,
                atom_kind: MemoryKindV1::Episode,
                repository_id: body.scope.repository_id.clone(),
                task_id: body.scope.task_id.clone(),
                agent_id: body.scope.agent_id.clone(),
                session_id: Some(body.scope.session_id.clone()),
                recorded_at_us: body.interval.recorded_at_us,
                header_body_digest: candidate.body_digest,
                header_body_bytes: reference_body_size(body)?,
                retention_permission: body.retention_permission,
                provenance_digest: Digest32::hash_prefixed(
                    b"naome-memory:provenance:v1\0",
                    &body.provenance.canonical_bytes()?,
                ),
                stored_body_digest,
                retention_tier,
                content_status,
                expires_at_us: None,
                superseded_by: None,
                feedback_positive: candidate.state.feedback_positive,
                feedback_negative: candidate.state.feedback_negative,
                retrieval_count: candidate.state.retrieval_count,
                retirement_run_bound: true,
                search_projection_present,
                pinned_artifact_digests: decision.exact_artifact_digests.clone(),
            })
        })
        .collect::<naome_memory_core::Result<Vec<_>>>()?;
    let semantic_atoms = semantic_atoms
        .iter()
        .map(|semantic| {
            Ok(RetirementPostStateAtomV1 {
                atom_id: semantic.atom_id,
                atom_kind: MemoryKindV1::Semantic,
                repository_id: semantic.body.scope.repository_id.clone(),
                task_id: semantic.body.scope.task_id.clone(),
                agent_id: semantic.body.scope.agent_id.clone(),
                session_id: Some(semantic.body.scope.session_id.clone()),
                recorded_at_us: semantic.body.interval.recorded_at_us,
                header_body_digest: semantic.body_digest,
                header_body_bytes: semantic.canonical_size_bytes,
                retention_permission: semantic.body.retention_permission,
                provenance_digest: Digest32::hash_prefixed(
                    b"naome-memory:provenance:v1\0",
                    &semantic.body.provenance.canonical_bytes()?,
                ),
                stored_body_digest: Some(semantic.body_digest),
                retention_tier: RetentionTierV1::SemanticLongTerm,
                content_status: ContentStatusV1::Present,
                expires_at_us: None,
                superseded_by: None,
                feedback_positive: 0,
                feedback_negative: 0,
                retrieval_count: 0,
                retirement_run_bound: false,
                search_projection_present: true,
                pinned_artifact_digests: Vec::new(),
            })
        })
        .collect::<naome_memory_core::Result<Vec<_>>>()?;
    Ok(RetirementPostStateV1 {
        memory_space_id: snapshot.memory_space_id.clone(),
        source_atoms,
        semantic_atoms,
    })
}

fn reference_retained_artifact_digests(body: &MemoryAtomBodyV1) -> Vec<Digest32> {
    let mut digests = body
        .artifacts
        .iter()
        .filter(|artifact| artifact.retention_allowed)
        .map(|artifact| artifact.digest)
        .collect::<Vec<_>>();
    digests.sort_unstable();
    digests.dedup();
    digests
}

fn reference_body_size(body: &MemoryAtomBodyV1) -> naome_memory_core::Result<u64> {
    u64::try_from(body.canonical_bytes()?.len())
        .map_err(|_| MemoryError::CanonicalEncoding("reference body exceeds u64 length".to_owned()))
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

fn consensus_required(count: usize, policy: &PolicyV1) -> usize {
    let numerator = (count as u128).saturating_mul(u128::from(policy.consensus_numerator));
    usize::try_from(numerator.div_ceil(u128::from(policy.consensus_denominator)))
        .unwrap_or(usize::MAX)
}

fn consensus_optional<'a>(
    values: impl Iterator<Item = &'a String>,
    required: usize,
) -> Option<String> {
    let mut counts = BTreeMap::<&str, usize>::new();
    for value in values.filter(|value| !value.is_empty()) {
        *counts.entry(value.as_str()).or_default() += 1;
    }
    counts
        .into_iter()
        .find_map(|(value, count)| (count >= required).then(|| value.to_owned()))
}

fn consensus_body_strings(
    bodies: &[&MemoryAtomBodyV1],
    required: usize,
    values: impl Fn(&MemoryAtomBodyV1) -> &[String],
) -> Vec<String> {
    let mut counts = BTreeMap::<&str, usize>::new();
    for body in bodies {
        let distinct = values(body)
            .iter()
            .filter(|value| !value.is_empty())
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        for value in distinct {
            *counts.entry(value).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .filter(|(_, count)| *count >= required)
        .map(|(value, _)| value.to_owned())
        .collect()
}

fn consensus_set<'a>(
    values: impl Iterator<Item = &'a String>,
    required: usize,
) -> BTreeSet<String> {
    let mut counts = BTreeMap::<&str, usize>::new();
    for value in values.filter(|value| !value.is_empty()) {
        *counts.entry(value.as_str()).or_default() += 1;
    }
    counts
        .into_iter()
        .filter(|(_, count)| *count >= required)
        .map(|(value, _)| value.to_owned())
        .collect()
}

fn consensus_map(
    bodies: &[&MemoryAtomBodyV1],
    required: usize,
    after: bool,
) -> BTreeMap<String, String> {
    let mut counts = BTreeMap::<(&str, &str), usize>::new();
    for body in bodies {
        let map = if after {
            &body.internal_state_after
        } else {
            &body.internal_state_before
        };
        for (key, value) in map {
            *counts.entry((key.as_str(), value.as_str())).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .filter(|(_, count)| *count >= required)
        .map(|((key, value), _)| (key.to_owned(), value.to_owned()))
        .collect()
}

fn weighted(value: u32, weight: u32) -> u32 {
    u32::try_from(u64::from(value).saturating_mul(u64::from(weight)) / u64::from(Ppm::SCALE))
        .unwrap_or(Ppm::SCALE)
}

fn reference_mean(values: impl IntoIterator<Item = u32>) -> u32 {
    let mut count = 0_u64;
    let mut sum = 0_u64;
    for value in values {
        count = count.saturating_add(1);
        sum = sum.saturating_add(u64::from(value));
    }
    let rounded = sum
        .saturating_add(count / 2)
        .checked_div(count)
        .unwrap_or(0);
    u32::try_from(rounded).unwrap_or(Ppm::SCALE)
}

fn ratio_ppm(numerator: usize, denominator: usize) -> u32 {
    ratio_ppm_u64(
        u64::try_from(numerator).unwrap_or(u64::MAX),
        u64::try_from(denominator).unwrap_or(u64::MAX),
    )
}

fn ratio_ppm_u64(numerator: u64, denominator: u64) -> u32 {
    if denominator == 0 {
        return 0;
    }
    let scaled =
        u128::from(numerator).saturating_mul(u128::from(Ppm::SCALE)) / u128::from(denominator);
    u32::try_from(scaled.min(u128::from(Ppm::SCALE))).unwrap_or(Ppm::SCALE)
}

fn ceil_ppm(population: usize, rate: u32) -> usize {
    if population == 0 || rate == 0 {
        return 0;
    }
    let numerator = (population as u128).saturating_mul(u128::from(rate));
    usize::try_from(numerator.div_ceil(u128::from(Ppm::SCALE))).unwrap_or(usize::MAX)
}

const fn ppm(value: u32) -> Ppm {
    Ppm::from_raw_unchecked(value)
}
