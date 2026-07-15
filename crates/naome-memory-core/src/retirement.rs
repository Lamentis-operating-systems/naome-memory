use crate::{
    ATOM_HASH_DOMAIN, ActionRecordV1, AtomId, CanonicalBytes as _, ConsolidationScoreV1,
    ContentStatusV1, DecisionRecordV1, Digest32, FeedbackSignalV1, FormationSignalsV1,
    IntegrityStatusV1, InterpretationV1, MemoryAtomBodyV1, MemoryError, MemoryKindV1,
    MemoryPayloadV1, MemoryRelationKindV1, MemoryRelationV1, MemoryScopeV1, ObservationV1,
    PolicyV1, Ppm, ProvenanceV1, RejectedAlternativeV1, RetentionPermissionV1, RetentionTierV1,
    RetirementCandidateV1, SealReasonV1, Seed32, SemanticPayloadV1, TimeIntervalV1,
    validate_memory_atom_body,
};
use rand_chacha::ChaCha20Rng;
use rand_core::{RngCore as _, SeedableRng as _};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const INPUT_HASH_DOMAIN: &[u8] = b"naome-memory:retirement-input:v1\0";
const POPULATION_HASH_DOMAIN: &[u8] = b"naome-memory:lottery-population:v1\0";
const PLAN_HASH_DOMAIN: &[u8] = b"naome-memory:retirement-plan:v1\0";
const STATE_HASH_DOMAIN: &[u8] = b"naome-memory:projected-state:v1\0";
const MICROSECONDS_PER_DAY: u64 = 86_400_000_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetirementSnapshotV1 {
    pub contract_version: String,
    pub memory_space_id: String,
    /// UTC day index since the Unix epoch.
    pub cohort_day: u64,
    pub as_of_us: u64,
    pub atoms: Vec<RetirementCandidateV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedSemanticAtomV1 {
    pub atom_id: AtomId,
    pub body_digest: Digest32,
    pub canonical_size_bytes: u64,
    pub body: MemoryAtomBodyV1,
}

/// Auditable disposition for every consolidation-eligible cluster that met
/// the score and support gates. Entries are ordered by descending score and
/// then by the resulting semantic atom id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticCandidateDispositionV1 {
    Selected,
    SkippedAtomHardLimit,
    SkippedCountBudget,
    SkippedByteBudget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticCandidateDecisionV1 {
    pub atom_id: AtomId,
    pub source_ids: Vec<AtomId>,
    pub score: ConsolidationScoreV1,
    pub canonical_size_bytes: u64,
    pub disposition: SemanticCandidateDispositionV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetirementDecisionV1 {
    pub atom_id: AtomId,
    pub semantic_atom_ids: Vec<AtomId>,
    pub exact_retained: bool,
    pub exact_artifact_digests: Vec<Digest32>,
    pub forget_episode_body: bool,
}

/// Store-independent logical authority for one atom after a retirement plan
/// has been applied. The concrete retirement run id is represented by
/// `retirement_run_bound` to avoid a hash cycle: the run id is derived from the
/// plan digest, while this value is part of that plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetirementPostStateAtomV1 {
    pub atom_id: AtomId,
    pub atom_kind: MemoryKindV1,
    pub repository_id: Option<String>,
    pub task_id: Option<String>,
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
    pub recorded_at_us: u64,
    pub header_body_digest: Digest32,
    pub header_body_bytes: u64,
    pub retention_permission: RetentionPermissionV1,
    pub provenance_digest: Digest32,
    pub stored_body_digest: Option<Digest32>,
    pub retention_tier: RetentionTierV1,
    pub content_status: ContentStatusV1,
    pub expires_at_us: Option<u64>,
    pub superseded_by: Option<AtomId>,
    pub feedback_positive: u64,
    pub feedback_negative: u64,
    pub retrieval_count: u64,
    pub retirement_run_bound: bool,
    pub search_projection_present: bool,
    pub pinned_artifact_digests: Vec<Digest32>,
}

/// Complete logical post-state projected by a retirement plan. Source and
/// semantic atoms are separate so an implementation cannot silently exchange
/// an input member for an output atom with the same digest-shaped fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetirementPostStateV1 {
    pub memory_space_id: String,
    pub source_atoms: Vec<RetirementPostStateAtomV1>,
    pub semantic_atoms: Vec<RetirementPostStateAtomV1>,
}

impl RetirementPostStateV1 {
    pub fn digest(&self) -> crate::Result<Digest32> {
        Ok(Digest32::hash_prefixed(
            STATE_HASH_DOMAIN,
            &self.canonical_bytes()?,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetirementPlanV1 {
    pub contract_version: String,
    pub policy_id: String,
    pub policy_digest: Digest32,
    pub seed: Seed32,
    pub memory_space_id: String,
    pub cohort_day: u64,
    pub as_of_us: u64,
    pub input_set_digest: Digest32,
    pub rng_identifier: String,
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

impl RetirementPlanV1 {
    pub fn digest(&self) -> crate::Result<Digest32> {
        Ok(Digest32::hash_prefixed(
            PLAN_HASH_DOMAIN,
            &self.canonical_bytes()?,
        ))
    }

    pub fn decision_digest(&self) -> crate::Result<Digest32> {
        Ok(Digest32::hash_prefixed(
            b"naome-memory:retirement-decisions:v1\0",
            &self.decisions.canonical_bytes()?,
        ))
    }
}

pub fn retirement_input_digest(snapshot: &RetirementSnapshotV1) -> crate::Result<Digest32> {
    let mut candidates = snapshot.atoms.iter().collect::<Vec<_>>();
    candidates.sort_unstable_by_key(|candidate| candidate.atom_id);
    let normalized = RetirementInputAuthorityV1 {
        contract_version: &snapshot.contract_version,
        memory_space_id: &snapshot.memory_space_id,
        cohort_day: snapshot.cohort_day,
        as_of_us: snapshot.as_of_us,
        atoms: candidates
            .into_iter()
            .map(RetirementInputCandidateRefV1::from)
            .collect(),
    };
    Ok(Digest32::hash_prefixed(
        INPUT_HASH_DOMAIN,
        &normalized.canonical_bytes()?,
    ))
}

/// The input-set authority binds immutable content through its verified body
/// digest instead of embedding every large body a second time. Mutable state
/// remains fully bound. This keeps 100,000-atom planning within the `PoC` RSS
/// envelope without weakening content closure.
#[derive(Serialize)]
struct RetirementInputAuthorityV1<'a> {
    contract_version: &'a str,
    memory_space_id: &'a str,
    cohort_day: u64,
    as_of_us: u64,
    atoms: Vec<RetirementInputCandidateRefV1<'a>>,
}

#[derive(Serialize)]
struct RetirementInputCandidateRefV1<'a> {
    atom_id: AtomId,
    body_digest: Digest32,
    state: &'a crate::AtomStateV1,
    integrity: IntegrityStatusV1,
    exact_package_bytes: u64,
}

impl<'a> From<&'a RetirementCandidateV1> for RetirementInputCandidateRefV1<'a> {
    fn from(candidate: &'a RetirementCandidateV1) -> Self {
        Self {
            atom_id: candidate.atom_id,
            body_digest: candidate.body_digest,
            state: &candidate.state,
            integrity: candidate.integrity,
            exact_package_bytes: candidate.exact_package_bytes,
        }
    }
}

pub fn plan_retirement(
    snapshot: &RetirementSnapshotV1,
    policy: &PolicyV1,
    seed: Seed32,
) -> crate::Result<RetirementPlanV1> {
    policy.validate_poc_v1()?;
    if snapshot.contract_version != "retirement-snapshot-v1" {
        return Err(MemoryError::UnknownVersion(
            snapshot.contract_version.clone(),
        ));
    }
    if snapshot.memory_space_id.is_empty() {
        return Err(MemoryError::EmptyField("memory_space_id"));
    }
    validate_cohort_size(snapshot.atoms.len(), policy)?;
    if snapshot.atoms.is_empty() {
        return Err(MemoryError::EmptyRetirementCohort);
    }
    let cohort_end_us = snapshot
        .cohort_day
        .checked_add(1)
        .and_then(|day| day.checked_mul(MICROSECONDS_PER_DAY))
        .ok_or(MemoryError::InvalidAtomBody(
            "retirement cohort day upper boundary overflows u64",
        ))?;
    if snapshot.as_of_us < cohort_end_us {
        return Err(MemoryError::RetirementCohortNotClosed {
            as_of_us: snapshot.as_of_us,
            cohort_end_us,
        });
    }

    let mut candidates = snapshot.atoms.iter().collect::<Vec<_>>();
    candidates.sort_unstable_by_key(|candidate| candidate.atom_id);
    validate_candidates(&candidates, snapshot, policy)?;

    let input_set_digest = retirement_input_digest(snapshot)?;
    let policy_digest = policy.digest()?;

    let semantic_eligible_indices = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            let body = candidate.body.as_ref()?;
            (body.retention_permission != RetentionPermissionV1::TransientOnly).then_some(index)
        })
        .collect::<Vec<_>>();
    let clusters = complete_link_clusters(&candidates, semantic_eligible_indices, policy);
    let mut semantic_candidates = Vec::new();
    for cluster in clusters {
        if cluster.len() < policy.min_semantic_sources {
            continue;
        }
        let sessions = cluster
            .iter()
            .filter_map(|index| candidates[*index].body.as_ref())
            .map(|body| body.scope.session_id.as_str())
            .collect::<BTreeSet<_>>()
            .len();
        if sessions < policy.min_semantic_sessions {
            continue;
        }
        let score = consolidation_score(&candidates, &cluster, policy);
        if score.total >= policy.consolidation_threshold {
            let body = build_semantic_body(
                &candidates,
                &cluster,
                score,
                snapshot,
                policy_digest,
                policy,
            )?;
            validate_memory_atom_body(&body, policy)?;
            let canonical_size_bytes = body.encoded_size_bytes()?;
            let atom_id = body.atom_id()?;
            semantic_candidates.push((
                score,
                PlannedSemanticAtomV1 {
                    atom_id,
                    body_digest: atom_id.digest(),
                    canonical_size_bytes,
                    body,
                },
            ));
        }
    }
    semantic_candidates.sort_unstable_by(|left, right| {
        right
            .0
            .total
            .cmp(&left.0.total)
            .then_with(|| left.1.atom_id.cmp(&right.1.atom_id))
    });

    let semantic_count_budget = policy
        .semantic_rate
        .ceil_count(candidates.len())
        .min(policy.semantic_count_max);
    let mut semantic_atoms = Vec::new();
    let mut semantic_decisions = Vec::with_capacity(semantic_candidates.len());
    let mut semantic_body_bytes = 0_u64;
    for (score, semantic) in semantic_candidates {
        let source_ids = match &semantic.body.payload {
            MemoryPayloadV1::Semantic(payload) => payload.source_ids.clone(),
            MemoryPayloadV1::Episode(_) => {
                return Err(MemoryError::CanonicalEncoding(
                    "planned semantic candidate has episode payload".to_owned(),
                ));
            }
        };
        let (disposition, next_semantic_body_bytes) = semantic_candidate_disposition(
            semantic.canonical_size_bytes,
            semantic_atoms.len(),
            semantic_count_budget,
            semantic_body_bytes,
            policy,
        )?;
        semantic_body_bytes = next_semantic_body_bytes;
        semantic_decisions.push(SemanticCandidateDecisionV1 {
            atom_id: semantic.atom_id,
            source_ids,
            score,
            canonical_size_bytes: semantic.canonical_size_bytes,
            disposition,
        });
        if disposition == SemanticCandidateDispositionV1::Selected {
            semantic_atoms.push(semantic);
        }
    }
    let exact_population = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            let body = candidate.body.as_ref()?;
            (body.retention_permission == RetentionPermissionV1::SemanticAndExactAllowed)
                .then_some(index)
        })
        .collect::<Vec<_>>();
    let population_evidence = exact_population
        .iter()
        .map(|index| {
            let candidate = &candidates[*index];
            (
                candidate.atom_id,
                candidate.body_digest,
                candidate.exact_package_bytes,
            )
        })
        .collect::<Vec<_>>();
    let episodic_population_digest = Digest32::hash_prefixed(
        POPULATION_HASH_DOMAIN,
        &population_evidence.canonical_bytes()?,
    );
    let episodic_winner_budget = policy
        .episodic_rate
        .ceil_count(exact_population.len())
        .min(policy.episodic_count_max);
    let selected_positions = partial_fisher_yates(
        exact_population.len(),
        episodic_winner_budget,
        *seed.as_bytes(),
    );
    let mut episodic_winners = selected_positions
        .iter()
        .map(|position| candidates[exact_population[*position]].atom_id)
        .collect::<Vec<_>>();
    let episodic_winner_bytes = selected_positions.iter().try_fold(0_u64, |sum, position| {
        sum.checked_add(candidates[exact_population[*position]].exact_package_bytes)
            .ok_or_else(|| {
                MemoryError::CanonicalEncoding("episodic byte total overflowed u64".to_owned())
            })
    })?;
    validate_episodic_budget(episodic_winner_bytes, policy)?;
    episodic_winners.sort_unstable();

    let winner_set = episodic_winners.iter().copied().collect::<BTreeSet<_>>();
    let mut semantic_sources = BTreeMap::<AtomId, Vec<AtomId>>::new();
    for semantic in &semantic_atoms {
        let MemoryPayloadV1::Semantic(payload) = &semantic.body.payload else {
            return Err(MemoryError::CanonicalEncoding(
                "planned semantic atom has episode payload".to_owned(),
            ));
        };
        for source in &payload.source_ids {
            semantic_sources
                .entry(*source)
                .or_default()
                .push(semantic.atom_id);
        }
    }

    let mut decisions = Vec::with_capacity(candidates.len());
    for candidate in &candidates {
        let exact_retained = winner_set.contains(&candidate.atom_id);
        let mut semantic_atom_ids = semantic_sources
            .remove(&candidate.atom_id)
            .unwrap_or_default();
        semantic_atom_ids.sort_unstable();
        let exact_artifact_digests = if exact_retained {
            candidate
                .body
                .as_ref()
                .map_or_else(Vec::new, MemoryAtomBodyV1::retained_artifact_digests)
        } else {
            Vec::new()
        };
        let forget_episode_body = candidate
            .body
            .as_ref()
            .is_some_and(|body| body.kind() == MemoryKindV1::Episode)
            && !exact_retained;
        decisions.push(RetirementDecisionV1 {
            atom_id: candidate.atom_id,
            semantic_atom_ids,
            exact_retained,
            exact_artifact_digests,
            forget_episode_body,
        });
    }

    let projected_post_state = projected_post_state(
        &snapshot.memory_space_id,
        &candidates,
        &decisions,
        &semantic_atoms,
    )?;
    let projected_state_digest = projected_post_state.digest()?;
    Ok(RetirementPlanV1 {
        contract_version: "retirement-plan-v1".to_owned(),
        policy_id: policy.policy_id.clone(),
        policy_digest,
        seed,
        memory_space_id: snapshot.memory_space_id.clone(),
        cohort_day: snapshot.cohort_day,
        as_of_us: snapshot.as_of_us,
        input_set_digest,
        rng_identifier: "chacha20-rand_chacha-0.3/partial-fisher-yates-v1".to_owned(),
        episodic_population_digest,
        episodic_population_count: exact_population.len(),
        episodic_winner_budget,
        episodic_winner_bytes,
        semantic_count_budget,
        semantic_body_bytes,
        semantic_decisions,
        semantic_atoms,
        episodic_winners,
        decisions,
        projected_post_state,
        projected_state_digest,
    })
}

fn semantic_candidate_disposition(
    candidate_bytes: u64,
    selected_count: usize,
    count_budget: usize,
    selected_bytes: u64,
    policy: &PolicyV1,
) -> crate::Result<(SemanticCandidateDispositionV1, u64)> {
    if candidate_bytes > policy.atom_hard_max_bytes {
        return Ok((
            SemanticCandidateDispositionV1::SkippedAtomHardLimit,
            selected_bytes,
        ));
    }
    if selected_count >= count_budget {
        return Ok((
            SemanticCandidateDispositionV1::SkippedCountBudget,
            selected_bytes,
        ));
    }
    let new_total = selected_bytes.checked_add(candidate_bytes).ok_or_else(|| {
        MemoryError::CanonicalEncoding("semantic byte total overflowed u64".to_owned())
    })?;
    if new_total > policy.semantic_body_budget_bytes {
        return Ok((
            SemanticCandidateDispositionV1::SkippedByteBudget,
            selected_bytes,
        ));
    }
    Ok((SemanticCandidateDispositionV1::Selected, new_total))
}

fn validate_cohort_size(atom_count: usize, policy: &PolicyV1) -> crate::Result<()> {
    if atom_count > policy.max_cohort_atoms {
        return Err(MemoryError::CohortTooLarge {
            actual: atom_count,
            maximum: policy.max_cohort_atoms,
        });
    }
    Ok(())
}

fn validate_episodic_budget(episodic_winner_bytes: u64, policy: &PolicyV1) -> crate::Result<()> {
    if episodic_winner_bytes > policy.episodic_pin_budget_bytes {
        return Err(MemoryError::EpisodicBudgetExceeded {
            actual: episodic_winner_bytes,
            maximum: policy.episodic_pin_budget_bytes,
        });
    }
    Ok(())
}

fn projected_post_state(
    memory_space_id: &str,
    candidates: &[&RetirementCandidateV1],
    decisions: &[RetirementDecisionV1],
    semantic_atoms: &[PlannedSemanticAtomV1],
) -> crate::Result<RetirementPostStateV1> {
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
            let header_body_bytes = u64::try_from(body.canonical_bytes()?.len()).map_err(|_| {
                MemoryError::CanonicalEncoding("atom body exceeds u64 length".to_owned())
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
                header_body_bytes,
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
        .collect::<crate::Result<Vec<_>>>()?;
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
        .collect::<crate::Result<Vec<_>>>()?;
    Ok(RetirementPostStateV1 {
        memory_space_id: memory_space_id.to_owned(),
        source_atoms,
        semantic_atoms,
    })
}

fn validate_candidates(
    candidates: &[&RetirementCandidateV1],
    snapshot: &RetirementSnapshotV1,
    policy: &PolicyV1,
) -> crate::Result<()> {
    let mut previous = None;
    for candidate in candidates {
        if previous == Some(candidate.atom_id) {
            return Err(MemoryError::DuplicateAtom {
                atom_id: candidate.atom_id,
            });
        }
        previous = Some(candidate.atom_id);
        if candidate.state.retention_tier != RetentionTierV1::ShortTerm
            || candidate.state.content_status != ContentStatusV1::Present
            || candidate.state.retirement_run_id.is_some()
            || candidate.state.superseded_by.is_some()
        {
            return Err(MemoryError::InvalidRetirementCandidate {
                atom_id: candidate.atom_id,
                reason: "candidate is not an unretired present STM atom",
            });
        }
        let expires_at_us =
            candidate
                .state
                .expires_at_us
                .ok_or(MemoryError::InvalidRetirementCandidate {
                    atom_id: candidate.atom_id,
                    reason: "STM candidate has no expiry",
                })?;
        if expires_at_us / MICROSECONDS_PER_DAY != snapshot.cohort_day {
            return Err(MemoryError::InvalidRetirementCandidate {
                atom_id: candidate.atom_id,
                reason: "candidate expiry does not belong to the requested UTC cohort day",
            });
        }
        if candidate.integrity != IntegrityStatusV1::Complete {
            return Err(MemoryError::InvalidRetirementCandidate {
                atom_id: candidate.atom_id,
                reason: "candidate integrity is incomplete",
            });
        }
        if candidate.body_digest != candidate.atom_id.digest() {
            return Err(MemoryError::AtomDigestMismatch {
                atom_id: candidate.atom_id,
            });
        }
        let Some(body) = &candidate.body else {
            return Err(MemoryError::AtomDigestMismatch {
                atom_id: candidate.atom_id,
            });
        };
        // Complete atom validation is the single authority for scope shape,
        // interval/sequence closure, artifacts, body size, and exact-package
        // limits. Retirement adds only cohort, lifecycle, and digest closure.
        validate_memory_atom_body(body, policy)?;
        let expected_expires_at_us = body
            .interval
            .recorded_at_us
            .checked_add(policy.stm_horizon_us)
            .ok_or(MemoryError::InvalidRetirementCandidate {
                atom_id: candidate.atom_id,
                reason: "recorded time plus STM horizon overflows u64",
            })?;
        if expires_at_us != expected_expires_at_us {
            return Err(MemoryError::InvalidRetirementCandidate {
                atom_id: candidate.atom_id,
                reason: "STM expiry does not equal recorded time plus policy horizon",
            });
        }
        let MemoryPayloadV1::Episode(_) = &body.payload else {
            return Err(MemoryError::InvalidRetirementCandidate {
                atom_id: candidate.atom_id,
                reason: "STM retirement cohort contains a non-episode atom",
            });
        };
        validate_candidate_scope(body, &snapshot.memory_space_id)?;
        let canonical_body = body.canonical_bytes()?;
        let canonical_size = u64::try_from(canonical_body.len()).map_err(|_| {
            MemoryError::CanonicalEncoding("atom body exceeds u64 length".to_owned())
        })?;
        let artifact_bytes = body
            .artifacts
            .iter()
            .filter(|artifact| artifact.retention_allowed)
            .try_fold(0_u64, |sum, artifact| {
                sum.checked_add(artifact.size_bytes).ok_or_else(|| {
                    MemoryError::CanonicalEncoding("artifact byte total overflowed u64".to_owned())
                })
            })?;
        let exact_package_bytes = canonical_size.checked_add(artifact_bytes).ok_or_else(|| {
            MemoryError::CanonicalEncoding("exact package byte total overflowed u64".to_owned())
        })?;
        let computed_atom_id = AtomId(Digest32::hash_prefixed(ATOM_HASH_DOMAIN, &canonical_body));
        if exact_package_bytes != candidate.exact_package_bytes
            || computed_atom_id != candidate.atom_id
        {
            return Err(MemoryError::AtomDigestMismatch {
                atom_id: candidate.atom_id,
            });
        }
    }
    Ok(())
}

fn validate_candidate_scope(body: &MemoryAtomBodyV1, memory_space_id: &str) -> crate::Result<()> {
    if body.scope.memory_space_id != memory_space_id {
        return Err(MemoryError::MemorySpaceMismatch);
    }
    Ok(())
}

fn complete_link_clusters(
    candidates: &[&RetirementCandidateV1],
    ordered_indices: Vec<usize>,
    policy: &PolicyV1,
) -> Vec<Vec<usize>> {
    let mut clusters: Vec<Vec<usize>> = Vec::new();
    for index in ordered_indices {
        let mut destination = None;
        for (cluster_index, cluster) in clusters.iter().enumerate() {
            let compatible = cluster.iter().all(|member| {
                let left = candidates[index].body.as_ref();
                let right = candidates[*member].body.as_ref();
                match (left, right) {
                    (Some(left), Some(right)) => {
                        semantic_similarity(left, right, policy)
                            >= policy.cluster_similarity_threshold
                    }
                    _ => false,
                }
            });
            if compatible {
                destination = Some(cluster_index);
                break;
            }
        }
        if let Some(cluster_index) = destination {
            clusters[cluster_index].push(index);
        } else {
            clusters.push(vec![index]);
        }
    }
    clusters
}

pub fn semantic_similarity(
    left: &MemoryAtomBodyV1,
    right: &MemoryAtomBodyV1,
    policy: &PolicyV1,
) -> Ppm {
    let topic = jaccard(&left.topic_keys, &right.topic_keys);
    let entity = jaccard(&left.entity_ids, &right.entity_ids);
    let outcome = if left
        .outcome_class
        .as_deref()
        .is_some_and(|value| !value.is_empty())
        && left.outcome_class == right.outcome_class
    {
        Ppm::ONE
    } else {
        Ppm::ZERO
    };
    sum_positive([
        policy.similarity_topic_weight.multiply(topic),
        policy.similarity_entity_weight.multiply(entity),
        policy.similarity_outcome_weight.multiply(outcome),
    ])
}

fn jaccard(left: &BTreeSet<String>, right: &BTreeSet<String>) -> Ppm {
    let union = left.union(right).count();
    if union == 0 {
        return Ppm::ZERO;
    }
    Ppm::ratio(
        u64::try_from(left.intersection(right).count()).unwrap_or(u64::MAX),
        u64::try_from(union).unwrap_or(u64::MAX),
    )
}

fn consolidation_score(
    candidates: &[&RetirementCandidateV1],
    cluster: &[usize],
    policy: &PolicyV1,
) -> ConsolidationScoreV1 {
    let bodies = cluster
        .iter()
        .filter_map(|index| candidates[*index].body.as_ref())
        .collect::<Vec<_>>();
    let support = Ppm::ratio(
        u64::try_from(bodies.len()).unwrap_or(u64::MAX),
        u64::try_from(policy.support_saturation_atoms).unwrap_or(u64::MAX),
    );
    let sessions = bodies
        .iter()
        .map(|body| body.scope.session_id.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let diversity = Ppm::ratio(
        u64::try_from(sessions).unwrap_or(u64::MAX),
        u64::try_from(bodies.len()).unwrap_or(u64::MAX),
    );
    let utility = Ppm::mean(
        cluster
            .iter()
            .map(|index| candidates[*index].state.validated_utility()),
    );
    let salience = Ppm::mean(bodies.iter().map(|body| body.formation_signals.salience));
    let novelty = Ppm::mean(bodies.iter().map(|body| body.formation_signals.novelty));
    let uncertainty = Ppm::mean(bodies.iter().map(|body| body.formation_signals.uncertainty));
    let mut cohesion = Ppm::ONE;
    let mut remaining_bodies = bodies.as_slice();
    while let Some((left, tail)) = remaining_bodies.split_first() {
        for right in tail {
            cohesion = cohesion.min(semantic_similarity(left, right, policy));
        }
        remaining_bodies = tail;
    }
    let positive = u64::from(policy.consolidation_support_weight.multiply(support).get())
        + u64::from(
            policy
                .consolidation_diversity_weight
                .multiply(diversity)
                .get(),
        )
        + u64::from(policy.consolidation_utility_weight.multiply(utility).get())
        + u64::from(
            policy
                .consolidation_salience_weight
                .multiply(salience)
                .get(),
        )
        + u64::from(
            policy
                .consolidation_cohesion_weight
                .multiply(cohesion)
                .get(),
        )
        + u64::from(policy.consolidation_novelty_weight.multiply(novelty).get());
    let negative = u64::from(
        policy
            .consolidation_uncertainty_weight
            .multiply(uncertainty)
            .get(),
    );
    let bounded = positive.saturating_sub(negative).min(u64::from(Ppm::SCALE));
    let total = Ppm::from_raw_unchecked(u32::try_from(bounded).unwrap_or(Ppm::SCALE));
    ConsolidationScoreV1 {
        support,
        diversity,
        utility,
        salience,
        cohesion,
        novelty,
        uncertainty,
        total,
    }
}

fn build_semantic_body(
    candidates: &[&RetirementCandidateV1],
    cluster: &[usize],
    score: ConsolidationScoreV1,
    snapshot: &RetirementSnapshotV1,
    policy_digest: Digest32,
    policy: &PolicyV1,
) -> crate::Result<MemoryAtomBodyV1> {
    let mut bodies = cluster
        .iter()
        .map(|index| {
            candidates[*index].body.as_ref().ok_or_else(|| {
                MemoryError::CanonicalEncoding("semantic cluster contains no body".to_owned())
            })
        })
        .collect::<crate::Result<Vec<_>>>()?;
    bodies.sort_unstable_by_key(|body| body.atom_id().unwrap_or_default());
    let required = consensus_required(bodies.len(), policy);
    let source_ids = bodies
        .iter()
        .map(|body| body.atom_id())
        .collect::<crate::Result<Vec<_>>>()?;
    let source_body_digests = source_ids
        .iter()
        .map(|source| (*source, source.digest()))
        .collect::<BTreeMap<_, _>>();
    let source_digests = source_ids
        .iter()
        .map(|source| source.digest())
        .collect::<Vec<_>>();

    let repository_id = consensus_optional(
        bodies
            .iter()
            .filter_map(|body| body.scope.repository_id.as_ref()),
        required,
    );
    let task_id = consensus_optional(
        bodies.iter().filter_map(|body| body.scope.task_id.as_ref()),
        required,
    );
    let agent_id = consensus_optional(
        bodies
            .iter()
            .filter_map(|body| body.scope.agent_id.as_ref()),
        required,
    );
    let goal = consensus_optional(
        bodies.iter().filter_map(|body| body.goal.as_ref()),
        required,
    );
    let goal_key = consensus_optional(
        bodies.iter().filter_map(|body| body.goal_key.as_ref()),
        required,
    );
    let outcome_class = consensus_optional(
        bodies.iter().filter_map(|body| body.outcome_class.as_ref()),
        required,
    );
    let plan = consensus_body_strings(&bodies, required, |body| &body.plan);
    let internal_state_before = consensus_map(&bodies, required, false);
    let internal_state_after = consensus_map(&bodies, required, true);
    let topic_keys = consensus_set(
        bodies.iter().flat_map(|body| body.topic_keys.iter()),
        required,
    );
    let entity_ids = consensus_set(
        bodies.iter().flat_map(|body| body.entity_ids.iter()),
        required,
    );
    let started_at_us = bodies
        .iter()
        .map(|body| body.interval.started_at_us)
        .min()
        .unwrap_or(snapshot.as_of_us);
    let ended_at_us = bodies
        .iter()
        .map(|body| body.interval.ended_at_us)
        .max()
        .unwrap_or(snapshot.as_of_us);
    let relations = source_ids
        .iter()
        .map(|source| MemoryRelationV1 {
            kind: MemoryRelationKindV1::DerivedFrom,
            target: *source,
        })
        .collect();
    Ok(MemoryAtomBodyV1 {
        contract_version: "atom-v1".to_owned(),
        scope: MemoryScopeV1 {
            memory_space_id: snapshot.memory_space_id.clone(),
            repository_id,
            task_id,
            agent_id,
            session_id: format!("semantic:{}", snapshot.cohort_day),
        },
        interval: TimeIntervalV1 {
            started_at_us,
            ended_at_us,
            recorded_at_us: snapshot.as_of_us,
        },
        trigger: "poc-v1-retirement".to_owned(),
        seal_reason: SealReasonV1::Checkpoint,
        goal,
        plan,
        internal_state_before,
        internal_state_after,
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
        topic_keys,
        entity_ids,
        outcome_class,
        goal_key,
        provenance: ProvenanceV1 {
            producer: "naome-memory-core/plan_retirement".to_owned(),
            source_event_digests: source_digests,
            policy_digest,
        },
        relations,
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

fn consensus_required(count: usize, policy: &PolicyV1) -> usize {
    let numerator = (count as u128) * u128::from(policy.consensus_numerator);
    let value = numerator.div_ceil(u128::from(policy.consensus_denominator));
    usize::try_from(value).unwrap_or(usize::MAX)
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

fn consensus_strings<'a>(values: impl Iterator<Item = &'a String>, required: usize) -> Vec<String> {
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

fn consensus_body_strings(
    bodies: &[&MemoryAtomBodyV1],
    required: usize,
    values: impl Fn(&MemoryAtomBodyV1) -> &[String],
) -> Vec<String> {
    let mut counts = BTreeMap::<&str, usize>::new();
    for body in bodies {
        let distinct_for_source = values(body)
            .iter()
            .filter(|value| !value.is_empty())
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        for value in distinct_for_source {
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
    consensus_strings(values, required).into_iter().collect()
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

fn sum_positive(values: impl IntoIterator<Item = Ppm>) -> Ppm {
    let sum = values
        .into_iter()
        .map(Ppm::get)
        .map(u64::from)
        .sum::<u64>()
        .min(u64::from(Ppm::SCALE));
    Ppm::from_raw_unchecked(u32::try_from(sum).unwrap_or(Ppm::SCALE))
}

/// Uniform selection without replacement. Rejection sampling avoids modulo
/// bias even when the remaining population does not divide 2^64.
pub(crate) fn partial_fisher_yates(population: usize, count: usize, seed: [u8; 32]) -> Vec<usize> {
    let mut positions = (0..population).collect::<Vec<_>>();
    let mut rng = ChaCha20Rng::from_seed(seed);
    let selected = count.min(population);
    for index in 0..selected {
        let Some(remaining) = population.checked_sub(index) else {
            return Vec::new();
        };
        let bound = u64::try_from(remaining).unwrap_or(u64::MAX);
        let offset = usize::try_from(uniform_below(&mut rng, bound)).unwrap_or(0);
        let Some(swap_index) = index.checked_add(offset) else {
            return Vec::new();
        };
        positions.swap(index, swap_index);
    }
    positions.truncate(selected);
    positions
}

fn uniform_below(rng: &mut ChaCha20Rng, bound: u64) -> u64 {
    match bound {
        0 | 1 => return 0,
        _ => {}
    }
    let threshold = bound.wrapping_neg() % bound;
    loop {
        let value = rng.next_u64();
        if value.checked_sub(threshold).is_some() {
            return value % bound;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AtomStateV1, EpisodePayloadV1, OutcomeV1};

    const RETIRE_AT_US: u64 = 8 * MICROSECONDS_PER_DAY;

    fn body(index: u64, session_id: &str, permission: RetentionPermissionV1) -> MemoryAtomBodyV1 {
        MemoryAtomBodyV1 {
            contract_version: "atom-v1".to_owned(),
            scope: MemoryScopeV1 {
                memory_space_id: "space-a".to_owned(),
                repository_id: Some("repo-a".to_owned()),
                task_id: Some("task-a".to_owned()),
                agent_id: Some("agent-a".to_owned()),
                session_id: session_id.to_owned(),
            },
            interval: TimeIntervalV1 {
                started_at_us: index * 10,
                ended_at_us: index * 10 + 1,
                recorded_at_us: index * 10 + 2,
            },
            trigger: "synthetic-event".to_owned(),
            seal_reason: SealReasonV1::GoalCompleted,
            goal: Some("prove deterministic retirement".to_owned()),
            plan: vec!["observe".to_owned(), "verify".to_owned()],
            internal_state_before: BTreeMap::from([("phase".to_owned(), "open".to_owned())]),
            internal_state_after: BTreeMap::from([("phase".to_owned(), "closed".to_owned())]),
            observations: vec![ObservationV1 {
                at_us: index * 10,
                source: "synthetic".to_owned(),
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
                producer: "retirement-unit-test".to_owned(),
                source_event_digests: vec![Digest32::hash_prefixed(
                    b"retirement-unit-event\0",
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

    fn candidate(
        index: u64,
        session_id: &str,
        permission: RetentionPermissionV1,
    ) -> crate::Result<RetirementCandidateV1> {
        let body = body(index, session_id, permission);
        let state = AtomStateV1 {
            retention_tier: RetentionTierV1::ShortTerm,
            content_status: ContentStatusV1::Present,
            expires_at_us: Some(
                body.interval
                    .recorded_at_us
                    .saturating_add(PolicyV1::poc_v1().stm_horizon_us),
            ),
            superseded_by: None,
            feedback_positive: 9,
            feedback_negative: 0,
            retrieval_count: 17,
            retirement_run_id: None,
        };
        RetirementCandidateV1::from_present_body(body, state)
    }

    fn snapshot_with_permission(
        permission: RetentionPermissionV1,
    ) -> crate::Result<RetirementSnapshotV1> {
        Ok(RetirementSnapshotV1 {
            contract_version: "retirement-snapshot-v1".to_owned(),
            memory_space_id: "space-a".to_owned(),
            cohort_day: 7,
            as_of_us: RETIRE_AT_US,
            atoms: (0_u64..3)
                .map(|index| candidate(index, &format!("session-{index}"), permission))
                .collect::<crate::Result<Vec<_>>>()?,
        })
    }

    #[test]
    fn count_and_byte_budgets_accept_exact_boundaries_and_reject_only_excess() {
        let policy = PolicyV1::poc_v1();
        assert!(validate_cohort_size(policy.max_cohort_atoms, &policy).is_ok());
        assert!(matches!(
            validate_cohort_size(policy.max_cohort_atoms + 1, &policy),
            Err(MemoryError::CohortTooLarge { .. })
        ));

        assert_eq!(
            semantic_candidate_disposition(policy.atom_hard_max_bytes, 0, 1, 0, &policy),
            Ok((
                SemanticCandidateDispositionV1::Selected,
                policy.atom_hard_max_bytes
            ))
        );
        assert_eq!(
            semantic_candidate_disposition(policy.atom_hard_max_bytes + 1, 0, 1, 7, &policy),
            Ok((SemanticCandidateDispositionV1::SkippedAtomHardLimit, 7))
        );
        assert_eq!(
            semantic_candidate_disposition(1, 1, 1, 7, &policy),
            Ok((SemanticCandidateDispositionV1::SkippedCountBudget, 7))
        );
        assert_eq!(
            semantic_candidate_disposition(1, 0, 1, policy.semantic_body_budget_bytes - 1, &policy,),
            Ok((
                SemanticCandidateDispositionV1::Selected,
                policy.semantic_body_budget_bytes
            ))
        );
        assert_eq!(
            semantic_candidate_disposition(1, 0, 1, policy.semantic_body_budget_bytes, &policy,),
            Ok((
                SemanticCandidateDispositionV1::SkippedByteBudget,
                policy.semantic_body_budget_bytes
            ))
        );
        assert!(matches!(
            semantic_candidate_disposition(1, 0, 1, u64::MAX, &policy),
            Err(MemoryError::CanonicalEncoding(_))
        ));

        assert!(validate_episodic_budget(policy.episodic_pin_budget_bytes - 1, &policy).is_ok());
        assert!(validate_episodic_budget(policy.episodic_pin_budget_bytes, &policy).is_ok());
        assert!(matches!(
            validate_episodic_budget(policy.episodic_pin_budget_bytes + 1, &policy),
            Err(MemoryError::EpisodicBudgetExceeded { .. })
        ));
    }

    #[test]
    fn retention_permissions_independently_gate_semantic_and_exact_paths() -> crate::Result<()> {
        let policy = PolicyV1::poc_v1();
        let transient = plan_retirement(
            &snapshot_with_permission(RetentionPermissionV1::TransientOnly)?,
            &policy,
            Seed32::new([11; 32]),
        )?;
        assert!(transient.semantic_atoms.is_empty());
        assert!(transient.semantic_decisions.is_empty());
        assert_eq!(transient.episodic_population_count, 0);
        assert!(transient.episodic_winners.is_empty());
        assert_ne!(transient.digest()?, Digest32::ZERO);
        assert_ne!(transient.decision_digest()?, Digest32::ZERO);

        let semantic_only = plan_retirement(
            &snapshot_with_permission(RetentionPermissionV1::SemanticAllowed)?,
            &policy,
            Seed32::new([12; 32]),
        )?;
        assert_eq!(semantic_only.semantic_atoms.len(), 1);
        assert_eq!(semantic_only.episodic_population_count, 0);
        assert!(semantic_only.episodic_winners.is_empty());
        Ok(())
    }

    #[test]
    fn outcome_similarity_requires_the_same_nonempty_class() {
        let policy = PolicyV1::poc_v1();
        let mut left = body(1, "left", RetentionPermissionV1::SemanticAllowed);
        let mut right = body(2, "right", RetentionPermissionV1::SemanticAllowed);
        left.topic_keys.clear();
        right.topic_keys.clear();
        left.entity_ids.clear();
        right.entity_ids.clear();

        left.outcome_class = Some("success".to_owned());
        right.outcome_class = Some("success".to_owned());
        assert_eq!(
            semantic_similarity(&left, &right, &policy),
            policy.similarity_outcome_weight
        );

        right.outcome_class = Some("failure".to_owned());
        assert_eq!(semantic_similarity(&left, &right, &policy), Ppm::ZERO);

        left.outcome_class = Some(String::new());
        right.outcome_class = Some(String::new());
        assert_eq!(semantic_similarity(&left, &right, &policy), Ppm::ZERO);
    }

    #[test]
    fn consolidation_score_binds_every_weight_and_distinct_pair_cohesion() -> crate::Result<()> {
        let policy = PolicyV1::poc_v1();
        let mut candidates = (0_u64..3)
            .map(|index| {
                candidate(
                    index,
                    &format!("session-{index}"),
                    RetentionPermissionV1::SemanticAllowed,
                )
            })
            .collect::<crate::Result<Vec<_>>>()?;
        for candidate in &mut candidates {
            candidate.state.feedback_positive = 0;
            candidate.state.feedback_negative = 0;
            let body = candidate
                .body
                .as_mut()
                .ok_or(MemoryError::AtomDigestMismatch {
                    atom_id: candidate.atom_id,
                })?;
            body.formation_signals.salience = Ppm::from_raw_unchecked(400_000);
            body.formation_signals.novelty = Ppm::from_raw_unchecked(300_000);
            body.formation_signals.uncertainty = Ppm::from_raw_unchecked(200_000);
        }
        let refs = candidates.iter().collect::<Vec<_>>();
        let score = consolidation_score(&refs, &[0, 1, 2], &policy);
        assert_eq!(score.support, Ppm::from_raw_unchecked(300_000));
        assert_eq!(score.diversity, Ppm::ONE);
        assert_eq!(score.utility, Ppm::from_raw_unchecked(500_000));
        assert_eq!(score.salience, Ppm::from_raw_unchecked(400_000));
        assert_eq!(score.cohesion, Ppm::ONE);
        assert_eq!(score.novelty, Ppm::from_raw_unchecked(300_000));
        assert_eq!(score.uncertainty, Ppm::from_raw_unchecked(200_000));
        assert_eq!(score.total, Ppm::from_raw_unchecked(535_000));

        let only = candidates
            .first_mut()
            .ok_or(MemoryError::EmptyRetirementCohort)?;
        let only_body = only.body.as_mut().ok_or(MemoryError::AtomDigestMismatch {
            atom_id: only.atom_id,
        })?;
        only_body.topic_keys.clear();
        only_body.entity_ids.clear();
        only_body.outcome_class = None;
        let refs = candidates.iter().collect::<Vec<_>>();
        assert_eq!(consolidation_score(&refs, &[0], &policy).cohesion, Ppm::ONE);
        Ok(())
    }

    #[test]
    fn consensus_helpers_enforce_thresholds_filter_empty_values_and_choose_the_requested_map() {
        let policy = PolicyV1::poc_v1();
        assert_eq!(consensus_required(1, &policy), 1);
        assert_eq!(consensus_required(4, &policy), 3);

        let alpha = "alpha".to_owned();
        let beta = "beta".to_owned();
        let empty = String::new();
        let values = [&alpha, &alpha, &beta, &empty];
        assert_eq!(
            consensus_optional(values.into_iter(), 2),
            Some(alpha.clone())
        );
        assert_eq!(consensus_optional([&beta, &empty].into_iter(), 2), None);
        assert_eq!(
            consensus_strings(values.into_iter(), 2),
            vec![alpha.clone()]
        );
        assert_eq!(
            consensus_set(values.into_iter(), 2),
            BTreeSet::from([alpha.clone()])
        );

        let mut bodies = [
            body(10, "one", RetentionPermissionV1::SemanticAllowed),
            body(11, "two", RetentionPermissionV1::SemanticAllowed),
            body(12, "three", RetentionPermissionV1::SemanticAllowed),
        ];
        bodies[2].internal_state_before =
            BTreeMap::from([("phase".to_owned(), "different".to_owned())]);
        bodies[2].internal_state_after = BTreeMap::from([("phase".to_owned(), "other".to_owned())]);
        let refs = bodies.iter().collect::<Vec<_>>();
        assert_eq!(
            consensus_map(&refs, 2, false),
            BTreeMap::from([("phase".to_owned(), "open".to_owned())])
        );
        assert_eq!(
            consensus_map(&refs, 2, true),
            BTreeMap::from([("phase".to_owned(), "closed".to_owned())])
        );
    }

    #[test]
    fn lottery_sampling_is_bounded_deterministic_and_does_not_consume_trivial_bounds() {
        assert!(partial_fisher_yates(0, 1, [1; 32]).is_empty());

        let seeds_choose_nonzero = (0_u8..=32).any(|byte| {
            partial_fisher_yates(16, 1, [byte; 32])
                .first()
                .is_some_and(|position| *position != 0)
        });
        assert!(seeds_choose_nonzero);

        let mut permutation = partial_fisher_yates(8, 12, [9; 32]);
        assert_eq!(permutation.len(), 8);
        permutation.sort_unstable();
        assert_eq!(permutation, (0..8).collect::<Vec<_>>());
        assert_eq!(
            partial_fisher_yates(12, 5, [17; 32]),
            partial_fisher_yates(12, 5, [17; 32])
        );

        let mut trivial = ChaCha20Rng::from_seed([23; 32]);
        assert_eq!(uniform_below(&mut trivial, 0), 0);
        assert_eq!(uniform_below(&mut trivial, 1), 0);
        let after_trivial = trivial.next_u64();
        let mut untouched = ChaCha20Rng::from_seed([23; 32]);
        assert_eq!(after_trivial, untouched.next_u64());

        let samples = (0_u8..=32)
            .map(|byte| {
                let mut rng = ChaCha20Rng::from_seed([byte; 32]);
                uniform_below(&mut rng, 10)
            })
            .collect::<Vec<_>>();
        assert!(samples.iter().all(|sample| *sample < 10));
        assert!(samples.iter().any(|sample| *sample != 0));

        let acceptance_floor = 10_u64.wrapping_neg() % 10;
        let mut exercised_low_accepted_draw = false;
        for byte in 0_u8..=u8::MAX {
            let seed = [byte; 32];
            let mut reference = ChaCha20Rng::from_seed(seed);
            let first_draw = reference.next_u64();
            let second_draw = reference.next_u64();
            if (acceptance_floor..u64::MAX / 10).contains(&first_draw) {
                let mut observed_rng = ChaCha20Rng::from_seed(seed);
                assert_eq!(uniform_below(&mut observed_rng, 10), first_draw % 10);
                assert_eq!(observed_rng.next_u64(), second_draw);
                exercised_low_accepted_draw = true;
                break;
            }
        }
        assert!(exercised_low_accepted_draw);
    }

    #[test]
    fn uniform_sampling_rejects_a_below_threshold_draw_before_accepting_the_next() {
        let bound = (1_u64 << 63) + 1;
        let rejection_threshold = bound.wrapping_neg() % bound;
        let mut exercised_rejection = false;

        for byte in 0_u8..=u8::MAX {
            let seed = [byte; 32];
            let mut reference = ChaCha20Rng::from_seed(seed);
            let first_draw = reference.next_u64();
            let second_draw = reference.next_u64();
            let third_draw = reference.next_u64();
            if first_draw < rejection_threshold && second_draw >= rejection_threshold {
                let mut observed_rng = ChaCha20Rng::from_seed(seed);
                assert_eq!(uniform_below(&mut observed_rng, bound), second_draw % bound);
                assert_eq!(observed_rng.next_u64(), third_draw);
                exercised_rejection = true;
                break;
            }
        }

        assert!(exercised_rejection);
    }
}
