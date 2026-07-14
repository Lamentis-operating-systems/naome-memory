use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::str::FromStr;

use naome_memory_core::{
    AtomId, AtomStateV1, CanonicalBytes as _, ContentStatusV1, Digest32, GeneralityV1,
    IntegrityStatusV1, MemoryAtomBodyV1, MemoryError, MemoryKindV1, MemoryPayloadV1, PolicyV1, Ppm,
    RecallReasonV1, ReceiptVerificationInputsV1, RetentionTierV1, RetirementCandidateV1,
    RetirementPlanV1, RetirementSnapshotV1, RetrievalCandidateV1, RetrievalCandidatesV1,
    RetrievalQueryV1, Seed32, SpontaneousRecallRequestV1, apply_retirement, plan_retirement,
    rank_retrieval, verify_receipt,
};
use naome_memory_sqlite::{
    CohortKey, RepositoryFaultPointV1, SearchScope, SqliteRepository, StoreConfig,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{
    DatasetIdV1, LabError, Result, SignedPpm, bootstrap_lower_95, derive_dataset_world_seed,
    generate_world_for_dataset, ndcg_at_10, rare_event_key, rare_query_id, world_id,
};

const RECEIPT_HASH_DOMAIN: &[u8] = b"naome-memory:lab-proof-receipt:v1\0";
const LEDGER_HASH_DOMAIN: &[u8] = b"naome-memory:decision-ledger:v1\0";
const GOLDEN_HASH_DOMAIN: &[u8] = b"naome-memory:golden-fates:v1\0";
const CRASH_SCENARIO_HASH_DOMAIN: &[u8] = b"naome-memory:crash-scenario-evidence:v1\0";
const CRASH_CAMPAIGN_HASH_DOMAIN: &[u8] = b"naome-memory:crash-campaign-evidence:v1\0";
const MANIFEST_HASH_DOMAIN: &[u8] = b"naome-memory:dataset-manifest:v1\0";
const BOOTSTRAP_SEED_DOMAIN: &[u8] = b"naome-memory:dataset-bootstrap:v1\0";
const MICROSECONDS_PER_DAY: u64 = 86_400_000_000;
const WORLD_ATOM_COUNT: usize = 10;
const GOLDEN_ATOM_COUNT: usize = 20;
const GOLDEN_SCENARIO_COUNT: u8 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProofProfileV1 {
    Pr,
    Deep,
}

impl ProofProfileV1 {
    const fn dataset_id(self) -> DatasetIdV1 {
        match self {
            Self::Pr => DatasetIdV1::Calibration,
            Self::Deep => DatasetIdV1::Holdout,
        }
    }

    const fn world_count(self) -> usize {
        match self {
            Self::Pr => 40,
            Self::Deep => 100,
        }
    }

    const fn bootstrap_replicates(self) -> usize {
        match self {
            Self::Pr => 2_048,
            Self::Deep => 20_000,
        }
    }
}

impl FromStr for ProofProfileV1 {
    type Err = LabError;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "pr" => Ok(Self::Pr),
            "deep" => Ok(Self::Deep),
            _ => Err(LabError::UnknownProfile(value.to_owned())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatusV1 {
    Verified,
    Rejected,
    Inconclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HypothesisStatusV1 {
    Supported,
    Rejected,
    Inconclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverallStatusV1 {
    Passed,
    Failed,
    Inconclusive,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricComparatorV1 {
    pub comparison_id: String,
    pub left_model: String,
    pub right_model: String,
    pub shared_budget_bytes_total: u64,
    pub left_used_bytes_total: u64,
    pub right_used_bytes_total: u64,
    pub all_samples_within_shared_budget: bool,
    pub selection_rule: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabMetricV1 {
    pub estimate_ppm: SignedPpm,
    pub lower_95_ci_ppm: SignedPpm,
    pub threshold_ppm: SignedPpm,
    pub sample_count: usize,
    pub comparator: MetricComparatorV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteBudgetEvidenceV1 {
    pub semantic_vs_recency_budget_bytes: u64,
    pub hybrid_vs_semantic_budget_bytes: u64,
    pub recency_only_used_bytes: u64,
    pub semantic_only_used_bytes: u64,
    pub hybrid_used_bytes: u64,
    pub all_models_within_shared_budgets: bool,
    pub all_planned_semantic_candidates_used: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryEvaluationV1 {
    pub query_id: String,
    pub comparison_id: String,
    pub left_score_ppm: SignedPpm,
    pub right_score_ppm: SignedPpm,
    pub delta_ppm: SignedPpm,
    pub judged_relevant_atom_ids: Vec<AtomId>,
    pub judged_relevance_grades: BTreeMap<AtomId, u32>,
    pub left_hit_ids: Vec<AtomId>,
    pub right_hit_ids: Vec<AtomId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionLedgerEntryV1 {
    pub dataset_id: DatasetIdV1,
    pub world_id: String,
    pub world_index: u64,
    pub world_seed: Seed32,
    pub input_digest: Digest32,
    pub byte_budget_evidence: ByteBudgetEvidenceV1,
    pub query_evidence: Vec<QueryEvaluationV1>,
    pub result_digest: Digest32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldenFateScenarioV1 {
    pub scenario_id: String,
    pub seed: Seed32,
    pub plan_digest: Digest32,
    pub semantic_source_forgotten: usize,
    pub semantic_source_and_exact: usize,
    pub exact_only: usize,
    pub header_only: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each explicit boolean is an independently observed crash-recovery fact"
)]
pub struct CrashScenarioEvidenceV1 {
    pub fault_point: String,
    pub operation: String,
    pub child_terminated_while_transaction_open: bool,
    pub complete_pre_state_observed: bool,
    pub complete_post_state_observed: bool,
    pub recovery_replay_succeeded: bool,
    pub idempotent_replay_succeeded: Option<bool>,
    pub sqlite_integrity_ok: bool,
    pub foreign_keys_clean: bool,
    pub pre_state_digest: Digest32,
    pub post_crash_state_digest: Digest32,
    pub post_replay_state_digest: Digest32,
    pub action_digest: Digest32,
    pub result_digest: Digest32,
}

impl CrashScenarioEvidenceV1 {
    /// Recompute the typed digest for this scenario's observed recovery facts.
    ///
    /// # Errors
    ///
    /// Returns an error if canonical evidence encoding fails.
    pub fn recompute_result_digest(&self) -> Result<Digest32> {
        let mut unsigned = self.clone();
        unsigned.result_digest = Digest32::ZERO;
        Ok(Digest32::hash_prefixed(
            CRASH_SCENARIO_HASH_DOMAIN,
            &unsigned.canonical_bytes()?,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrashCampaignEvidenceV1 {
    pub contract_version: String,
    pub harness_source_digest: Digest32,
    pub repository_source_digest: Digest32,
    pub policy_digest: Digest32,
    pub scenarios: Vec<CrashScenarioEvidenceV1>,
    pub campaign_digest: Digest32,
}

impl CrashCampaignEvidenceV1 {
    /// Recompute the digest over the harness-bound ordered scenario evidence.
    ///
    /// # Errors
    ///
    /// Returns an error if canonical evidence encoding fails.
    pub fn recompute_campaign_digest(&self) -> Result<Digest32> {
        let mut unsigned = self.clone();
        unsigned.campaign_digest = Digest32::ZERO;
        Ok(Digest32::hash_prefixed(
            CRASH_CAMPAIGN_HASH_DOMAIN,
            &unsigned.canonical_bytes()?,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionLedgerV1 {
    pub contract_version: String,
    pub profile: ProofProfileV1,
    pub dataset_id: DatasetIdV1,
    pub entries: Vec<DecisionLedgerEntryV1>,
    pub golden_fate_evidence: Vec<GoldenFateScenarioV1>,
    pub crash_campaign_evidence: Option<CrashCampaignEvidenceV1>,
    pub ledger_digest: Digest32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabProofReceiptV1 {
    pub contract_version: String,
    pub profile: ProofProfileV1,
    pub dataset_id: DatasetIdV1,
    pub policy_digest: Digest32,
    pub dataset_manifest_digest: Digest32,
    pub generator_source_digest: Digest32,
    pub world_count: usize,
    pub invariant_statuses: BTreeMap<String, ClaimStatusV1>,
    pub hypothesis_statuses: BTreeMap<String, HypothesisStatusV1>,
    pub metrics: BTreeMap<String, LabMetricV1>,
    pub golden_fate_digest: Digest32,
    pub crash_campaign_digest: Option<Digest32>,
    pub decision_ledger_digest: Digest32,
    pub result_digest: Digest32,
    pub overall_status: OverallStatusV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabProofBundleV1 {
    pub receipt: LabProofReceiptV1,
    pub decision_ledger: DecisionLedgerV1,
}

#[derive(Debug, Deserialize)]
struct ManifestV1 {
    contract_version: String,
    generator_version: String,
    generator_source: ManifestGeneratorSourceV1,
    profile_datasets: BTreeMap<String, DatasetIdV1>,
    seed_derivation: ManifestSeedDerivationV1,
    datasets: BTreeMap<String, ManifestDatasetV1>,
}

#[derive(Debug, Deserialize)]
struct ManifestGeneratorSourceV1 {
    path: String,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct ManifestSeedDerivationV1 {
    algorithm: String,
    encoding: String,
    domains: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct ManifestDatasetV1 {
    world_count: Option<usize>,
    atom_count: Option<usize>,
    world_ids: Option<Vec<String>>,
    query_judgments: Option<ManifestQueryJudgmentsV1>,
    golden_seed_scenarios: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct ManifestQueryJudgmentsV1 {
    ordinary: ManifestOrdinaryJudgmentV1,
    rare: ManifestRareJudgmentV1,
}

#[derive(Debug, Deserialize)]
struct ManifestOrdinaryJudgmentV1 {
    query_id: String,
    terms: Vec<String>,
    topic_keys: Vec<String>,
    relevant_episode_atom_indices: Vec<usize>,
    relevant_semantic_topic_key: String,
    metric: String,
}

#[derive(Debug, Deserialize)]
struct ManifestRareJudgmentV1 {
    query_id_format: String,
    term_format: String,
    relevant_episode_atom_indices: Vec<usize>,
    metric: String,
}

/// Run the selected deterministic proof profile.
///
/// PR evidence is calibration-only. Deep evidence is holdout-only. Both bind
/// the committed generator source, manifest, queries, seeds, and decision
/// ledger. The atomic-apply invariant remains inconclusive until external
/// child-process crash receipts are attached; a successful `SQLite` replay is
/// necessary but is not treated as crash evidence.
///
/// # Errors
///
/// Returns an error when a committed dataset contract diverges, a synthetic
/// input is rejected, `SQLite` cannot close a real apply, or a receipt fails
/// verification.
#[allow(
    clippy::too_many_lines,
    reason = "receipt construction is intentionally linear and audit-friendly"
)]
pub fn run_proof(profile: ProofProfileV1) -> Result<LabProofBundleV1> {
    run_proof_with_crash_evidence(profile, None)
}

/// Run a proof and bind a machine-produced child-process crash campaign.
///
/// The attachment is accepted only when it covers the exact committed crash
/// boundaries, binds the current harness source and policy, and every recovery
/// fact and digest verifies. A missing attachment is represented by
/// [`run_proof`] and leaves atomic apply inconclusive.
///
/// # Errors
///
/// Returns an error for malformed, incomplete, stale-source, or negative crash
/// evidence, as well as the ordinary proof failures.
#[allow(
    clippy::too_many_lines,
    reason = "receipt construction is intentionally linear and audit-friendly"
)]
pub fn run_proof_with_crash_evidence(
    profile: ProofProfileV1,
    crash_campaign_evidence: Option<CrashCampaignEvidenceV1>,
) -> Result<LabProofBundleV1> {
    let bundle = construct_lab_bundle(profile, crash_campaign_evidence)?;
    verify_lab_bundle(&bundle)?;
    Ok(bundle)
}

#[allow(
    clippy::too_many_lines,
    reason = "receipt construction is intentionally linear and audit-friendly"
)]
fn construct_lab_bundle(
    profile: ProofProfileV1,
    crash_campaign_evidence: Option<CrashCampaignEvidenceV1>,
) -> Result<LabProofBundleV1> {
    let dataset_id = profile.dataset_id();
    let world_count = profile.world_count();
    let manifest = validate_committed_manifest(dataset_id, world_count)?;
    let query_judgments = manifest
        .datasets
        .get(dataset_id.as_str())
        .and_then(|dataset| dataset.query_judgments.as_ref())
        .ok_or_else(|| LabError::InvalidReceipt("query judgments are missing".to_owned()))?;
    let policy = PolicyV1::poc_v1();
    let mut ledger_entries = Vec::with_capacity(world_count);
    let mut semantic_deltas = Vec::with_capacity(world_count);
    let mut rare_deltas = Vec::with_capacity(world_count);
    let mut ordinary_deltas = Vec::with_capacity(world_count);
    let crash_campaign_digest = crash_campaign_evidence
        .as_ref()
        .map(validate_crash_campaign_evidence)
        .transpose()?;
    let mut invariant_statuses = required_invariant_statuses(if crash_campaign_digest.is_some() {
        ClaimStatusV1::Verified
    } else {
        ClaimStatusV1::Inconclusive
    });
    let mut comparator_totals = ComparatorTotals::default();

    for world_index in 0..world_count {
        let index = u64::try_from(world_index).unwrap_or(u64::MAX);
        let world = generate_world_for_dataset(dataset_id, index, WORLD_ATOM_COUNT)?;
        let input_digest = Digest32::hash_prefixed(
            b"naome-memory:synthetic-world-input:v1\0",
            &world.canonical_bytes()?,
        );
        let evaluation = evaluate_world(&world, &policy, query_judgments)?;
        semantic_deltas.push(evaluation.semantic_delta);
        rare_deltas.push(evaluation.rare_delta);
        ordinary_deltas.push(evaluation.ordinary_delta);
        comparator_totals.add(&evaluation.byte_budget_evidence);
        for (name, observed) in evaluation.invariants {
            aggregate_claim_status(&mut invariant_statuses, &name, observed);
        }
        ledger_entries.push(DecisionLedgerEntryV1 {
            dataset_id,
            world_id: world.world_id,
            world_index: index,
            world_seed: world.seed,
            input_digest,
            byte_budget_evidence: evaluation.byte_budget_evidence,
            query_evidence: evaluation.query_evidence,
            result_digest: evaluation.result_digest,
        });
    }

    let golden_fate_evidence = golden_fate_evidence(&policy)?;
    let golden_complete = golden_fate_evidence.iter().fold(
        (0_usize, 0_usize, 0_usize, 0_usize),
        |totals, scenario| {
            (
                totals.0.saturating_add(scenario.semantic_source_forgotten),
                totals.1.saturating_add(scenario.semantic_source_and_exact),
                totals.2.saturating_add(scenario.exact_only),
                totals.3.saturating_add(scenario.header_only),
            )
        },
    );
    aggregate_claim_status(
        &mut invariant_statuses,
        "golden_four_fates",
        if golden_complete.0 > 0
            && golden_complete.1 > 0
            && golden_complete.2 > 0
            && golden_complete.3 > 0
        {
            ClaimStatusV1::Verified
        } else {
            ClaimStatusV1::Rejected
        },
    );
    let golden_fate_digest =
        Digest32::hash_prefixed(GOLDEN_HASH_DOMAIN, &golden_fate_evidence.canonical_bytes()?);

    let mut ledger = DecisionLedgerV1 {
        contract_version: "decision-ledger-v1".to_owned(),
        profile,
        dataset_id,
        entries: ledger_entries,
        golden_fate_evidence,
        crash_campaign_evidence,
        ledger_digest: Digest32::ZERO,
    };
    ledger.ledger_digest = ledger_digest(&ledger)?;

    let bootstrap_seed = dataset_bootstrap_seed(dataset_id);
    let metrics = BTreeMap::from([
        (
            "semantic_utility".to_owned(),
            metric(
                &semantic_deltas,
                100_000,
                profile.bootstrap_replicates(),
                bootstrap_seed,
                comparator_totals.semantic_comparator(),
            ),
        ),
        (
            "episodic_rare_recovery".to_owned(),
            metric(
                &rare_deltas,
                80_000,
                profile.bootstrap_replicates(),
                bootstrap_seed,
                comparator_totals.hybrid_comparator("rare_recall_at_1"),
            ),
        ),
        (
            "ordinary_utility_preserved".to_owned(),
            metric(
                &ordinary_deltas,
                -20_000,
                profile.bootstrap_replicates(),
                bootstrap_seed,
                comparator_totals.hybrid_comparator("ordinary_ndcg_at_10"),
            ),
        ),
    ]);
    let hypothesis_statuses = metrics
        .iter()
        .map(|(name, value)| {
            let status = if profile == ProofProfileV1::Pr
                || !value.comparator.all_samples_within_shared_budget
            {
                HypothesisStatusV1::Inconclusive
            } else if value.lower_95_ci_ppm >= value.threshold_ppm {
                HypothesisStatusV1::Supported
            } else {
                HypothesisStatusV1::Rejected
            };
            (name.clone(), status)
        })
        .collect::<BTreeMap<_, _>>();

    let overall_status = overall_status(&invariant_statuses, &hypothesis_statuses);
    let generator_source_digest = generator_source_digest();
    let mut receipt = LabProofReceiptV1 {
        contract_version: "lab-proof-receipt-v1".to_owned(),
        profile,
        dataset_id,
        policy_digest: policy.digest()?,
        dataset_manifest_digest: manifest_digest(),
        generator_source_digest,
        world_count,
        invariant_statuses,
        hypothesis_statuses,
        metrics,
        golden_fate_digest,
        crash_campaign_digest,
        decision_ledger_digest: ledger.ledger_digest,
        result_digest: Digest32::ZERO,
        overall_status,
    };
    receipt.result_digest = receipt_digest(&receipt)?;
    verify_lab_receipt_with_manifest(&receipt, &manifest)?;
    Ok(LabProofBundleV1 {
        receipt,
        decision_ledger: ledger,
    })
}

#[derive(Default)]
struct ComparatorTotals {
    semantic_budget: u64,
    hybrid_budget: u64,
    recency_used: u64,
    semantic_used: u64,
    hybrid_used: u64,
    all_within: bool,
    initialized: bool,
}

impl ComparatorTotals {
    fn add(&mut self, evidence: &ByteBudgetEvidenceV1) {
        self.semantic_budget = self
            .semantic_budget
            .saturating_add(evidence.semantic_vs_recency_budget_bytes);
        self.hybrid_budget = self
            .hybrid_budget
            .saturating_add(evidence.hybrid_vs_semantic_budget_bytes);
        self.recency_used = self
            .recency_used
            .saturating_add(evidence.recency_only_used_bytes);
        self.semantic_used = self
            .semantic_used
            .saturating_add(evidence.semantic_only_used_bytes);
        self.hybrid_used = self.hybrid_used.saturating_add(evidence.hybrid_used_bytes);
        self.all_within = if self.initialized {
            self.all_within && evidence.all_models_within_shared_budgets
        } else {
            evidence.all_models_within_shared_budgets
        };
        self.initialized = true;
    }

    fn semantic_comparator(&self) -> MetricComparatorV1 {
        MetricComparatorV1 {
            comparison_id: "semantic_only_vs_recency_only".to_owned(),
            left_model: "recency_only".to_owned(),
            right_model: "semantic_only".to_owned(),
            shared_budget_bytes_total: self.semantic_budget,
            left_used_bytes_total: self.recency_used,
            right_used_bytes_total: self.semantic_used,
            all_samples_within_shared_budget: self.all_within,
            selection_rule: "both models share the per-world maximum byte cap; semantic-only contains every semantic body emitted by poc-v1 and recency-only contains source episodes newest-first".to_owned(),
        }
    }

    fn hybrid_comparator(&self, comparison_id: &str) -> MetricComparatorV1 {
        MetricComparatorV1 {
            comparison_id: comparison_id.to_owned(),
            left_model: "semantic_only".to_owned(),
            right_model: "hybrid".to_owned(),
            shared_budget_bytes_total: self.hybrid_budget,
            left_used_bytes_total: self.semantic_used,
            right_used_bytes_total: self.hybrid_used,
            all_samples_within_shared_budget: self.all_within,
            selection_rule: "both models share the per-world maximum byte cap; semantic-only exhausts all valid poc-v1 semantic outputs and hybrid adds only the lottery-selected exact package".to_owned(),
        }
    }
}

struct WorldEvaluation {
    semantic_delta: SignedPpm,
    rare_delta: SignedPpm,
    ordinary_delta: SignedPpm,
    invariants: BTreeMap<String, ClaimStatusV1>,
    byte_budget_evidence: ByteBudgetEvidenceV1,
    query_evidence: Vec<QueryEvaluationV1>,
    result_digest: Digest32,
}

#[allow(
    clippy::too_many_lines,
    reason = "one world evaluation keeps the complete evidence chain visible"
)]
fn evaluate_world(
    world: &crate::SyntheticWorldV1,
    policy: &PolicyV1,
    judgments: &ManifestQueryJudgmentsV1,
) -> Result<WorldEvaluation> {
    let states = source_states(world, policy);
    let expected_snapshot = snapshot_from_bodies(&world.atoms, &states, policy)?;
    let source_bytes = world
        .atoms
        .iter()
        .map(|body| Ok((body.atom_id()?, body.canonical_bytes()?)))
        .collect::<std::result::Result<BTreeMap<_, _>, MemoryError>>()?;
    let source_artifacts = world
        .atoms
        .iter()
        .map(|body| {
            let id = body.atom_id()?;
            let artifacts = body
                .artifacts
                .iter()
                .filter(|artifact| artifact.retention_allowed)
                .map(|artifact| {
                    (
                        artifact.digest,
                        artifact.inline_payload.clone().unwrap_or_default(),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            Ok((id, artifacts))
        })
        .collect::<std::result::Result<BTreeMap<_, _>, MemoryError>>()?;

    let directory = tempfile::tempdir()?;
    let artifact_root = directory.path().join("artifacts");
    let mut repository = SqliteRepository::open(&StoreConfig::new(
        directory.path().join("memory.db"),
        &artifact_root,
    ))?;
    repository.install_policy(policy, 0)?;
    for (body, state) in world.atoms.iter().zip(&states) {
        repository.put_core_atom(body, state)?;
    }
    let key = CohortKey {
        memory_space_id: expected_snapshot.memory_space_id.clone(),
        cohort_day: expected_snapshot.cohort_day,
        as_of_us: expected_snapshot.as_of_us,
    };
    let snapshot = repository.snapshot_core(&key)?;
    let plan = plan_retirement(&snapshot, policy, world.seed)?;
    let replay = plan_retirement(&snapshot, policy, world.seed)?;
    let deterministic_replay = plan == replay;
    let sealed_atom_immutability = snapshot.atoms.iter().all(|candidate| {
        candidate.body.as_ref().is_some_and(|body| {
            source_bytes
                .get(&candidate.atom_id)
                .is_some_and(|expected| {
                    body.canonical_bytes().is_ok_and(|bytes| bytes == *expected)
                })
        })
    });
    let budgets_respected = plan.semantic_atoms.len() <= plan.semantic_count_budget
        && plan.semantic_body_bytes <= policy.semantic_body_budget_bytes
        && plan.episodic_winners.len() <= plan.episodic_winner_budget
        && plan.episodic_winner_bytes <= policy.episodic_pin_budget_bytes;
    let semantic_provenance = semantic_provenance_closes(&plan, &snapshot, policy);

    let mut mismatched = snapshot.clone();
    "different-space".clone_into(&mut mismatched.memory_space_id);
    let memory_space_isolation = matches!(
        plan_retirement(&mismatched, policy, world.seed),
        Err(MemoryError::MemorySpaceMismatch)
    );

    let first_receipt = apply_retirement(&mut repository, &plan)?;
    let second_receipt = apply_retirement(&mut repository, &plan)?;
    let exact_artifact_map = exact_artifacts(&plan);
    let verified = verify_receipt(
        &first_receipt,
        &ReceiptVerificationInputsV1 {
            policy: policy.clone(),
            snapshot: snapshot.clone(),
            seed: world.seed,
            observed_state_digest: plan.projected_state_digest,
            observed_exact_artifact_digests: exact_artifact_map,
        },
    )?;
    let sqlite_replay_idempotent = first_receipt == second_receipt;
    let receipt_closure = verified.receipt_id == first_receipt.receipt_id;
    let mut tampered_receipt = first_receipt.clone();
    tampered_receipt.receipt_id = Digest32::ZERO;
    let tamper_fail_closed = verify_receipt(
        &tampered_receipt,
        &ReceiptVerificationInputsV1 {
            policy: policy.clone(),
            snapshot: snapshot.clone(),
            seed: world.seed,
            observed_state_digest: plan.projected_state_digest,
            observed_exact_artifact_digests: exact_artifacts(&plan),
        },
    )
    .is_err();
    let integrity = repository.check_integrity(true)?;
    let episodic_exactness = exact_winners_match_persisted_bytes(
        &repository,
        &artifact_root,
        &plan,
        &source_bytes,
        &source_artifacts,
    )? && integrity.sqlite_integrity == "ok"
        && integrity.foreign_key_violations.is_empty();

    let semantic_candidates = semantic_candidates(&plan)?;
    let hybrid_candidates = hybrid_candidates(&plan, &snapshot, &semantic_candidates)?;
    let ordinary_query = RetrievalQueryV1 {
        contract_version: "retrieval-query-v1".to_owned(),
        memory_space_id: "synthetic-space-v1".to_owned(),
        repository_id: Some("synthetic-repository".to_owned()),
        memory_space_wide: false,
        as_of_us: snapshot.as_of_us.saturating_add(1),
        terms: judgments.ordinary.terms.clone(),
        topic_keys: judgments.ordinary.topic_keys.iter().cloned().collect(),
        entity_ids: BTreeSet::new(),
        k: Some(10),
        spontaneous: None,
    };
    let semantic_ranked = rank_model(&ordinary_query, &semantic_candidates, policy)?;
    let hybrid_ranked = rank_model(&ordinary_query, &hybrid_candidates, policy)?;
    let ordinary_qrels = ordinary_qrels(world, &plan, &judgments.ordinary)?;
    let all_judged_grades = ordinary_qrels.values().copied().collect::<Vec<_>>();
    let semantic_grades = semantic_ranked
        .hits
        .iter()
        .map(|hit| ordinary_qrels.get(&hit.atom_id).copied().unwrap_or(0))
        .collect::<Vec<_>>();
    let hybrid_grades = hybrid_ranked
        .hits
        .iter()
        .map(|hit| ordinary_qrels.get(&hit.atom_id).copied().unwrap_or(0))
        .collect::<Vec<_>>();
    let mut recency = snapshot.atoms.clone();
    recency.sort_unstable_by(|left, right| {
        let left_at = left
            .body
            .as_ref()
            .map_or(0, |body| body.interval.recorded_at_us);
        let right_at = right
            .body
            .as_ref()
            .map_or(0, |body| body.interval.recorded_at_us);
        right_at
            .cmp(&left_at)
            .then_with(|| left.atom_id.cmp(&right.atom_id))
    });
    let recency_grades = recency
        .iter()
        .map(|candidate| ordinary_qrels.get(&candidate.atom_id).copied().unwrap_or(0))
        .collect::<Vec<_>>();
    let semantic_ndcg = ppm_i32(ndcg_at_10(&semantic_grades, &all_judged_grades));
    let recency_ndcg = ppm_i32(ndcg_at_10(&recency_grades, &all_judged_grades));
    let hybrid_ndcg = ppm_i32(ndcg_at_10(&hybrid_grades, &all_judged_grades));
    let semantic_delta = signed_delta(semantic_ndcg, recency_ndcg);
    let ordinary_delta = signed_delta(hybrid_ndcg, semantic_ndcg);

    let recency_used = snapshot.atoms.iter().fold(0_u64, |total, candidate| {
        total.saturating_add(candidate.exact_package_bytes)
    });
    let semantic_used = plan.semantic_body_bytes;
    let hybrid_used = semantic_used.saturating_add(plan.episodic_winner_bytes);
    let semantic_budget = recency_used.max(semantic_used);
    let hybrid_budget = semantic_used.max(hybrid_used);
    let byte_budget_evidence = ByteBudgetEvidenceV1 {
        semantic_vs_recency_budget_bytes: semantic_budget,
        hybrid_vs_semantic_budget_bytes: hybrid_budget,
        recency_only_used_bytes: recency_used,
        semantic_only_used_bytes: semantic_used,
        hybrid_used_bytes: hybrid_used,
        all_models_within_shared_budgets: recency_used <= semantic_budget
            && semantic_used <= semantic_budget
            && semantic_used <= hybrid_budget
            && hybrid_used <= hybrid_budget,
        all_planned_semantic_candidates_used: semantic_candidates.len()
            == plan.semantic_atoms.len(),
    };

    let semantic_ids = semantic_ranked
        .hits
        .iter()
        .map(|hit| hit.atom_id)
        .collect::<Vec<_>>();
    let hybrid_ids = hybrid_ranked
        .hits
        .iter()
        .map(|hit| hit.atom_id)
        .collect::<Vec<_>>();
    let recency_ids = recency.iter().map(|candidate| candidate.atom_id).collect();
    let judged_relevant_atom_ids = ordinary_qrels.keys().copied().collect::<Vec<_>>();
    let mut query_evidence = vec![
        QueryEvaluationV1 {
            query_id: judgments.ordinary.query_id.clone(),
            comparison_id: "semantic_only_vs_recency_only".to_owned(),
            left_score_ppm: SignedPpm::from_raw_unchecked(recency_ndcg),
            right_score_ppm: SignedPpm::from_raw_unchecked(semantic_ndcg),
            delta_ppm: semantic_delta,
            judged_relevant_atom_ids: judged_relevant_atom_ids.clone(),
            judged_relevance_grades: ordinary_qrels.clone(),
            left_hit_ids: recency_ids,
            right_hit_ids: semantic_ids.clone(),
        },
        QueryEvaluationV1 {
            query_id: judgments.ordinary.query_id.clone(),
            comparison_id: "ordinary_ndcg_at_10".to_owned(),
            left_score_ppm: SignedPpm::from_raw_unchecked(semantic_ndcg),
            right_score_ppm: SignedPpm::from_raw_unchecked(hybrid_ndcg),
            delta_ppm: ordinary_delta,
            judged_relevant_atom_ids,
            judged_relevance_grades: ordinary_qrels,
            left_hit_ids: semantic_ids,
            right_hit_ids: hybrid_ids,
        },
    ];

    let mut rare_semantic_sum = 0_i64;
    let mut rare_hybrid_sum = 0_i64;
    for atom_index in &judgments.rare.relevant_episode_atom_indices {
        let body = world.atoms.get(*atom_index).ok_or_else(|| {
            LabError::InvalidReceipt("rare judgment atom index is out of bounds".to_owned())
        })?;
        let relevant_id = body.atom_id()?;
        let rare_query = RetrievalQueryV1 {
            contract_version: "retrieval-query-v1".to_owned(),
            memory_space_id: "synthetic-space-v1".to_owned(),
            repository_id: Some("synthetic-repository".to_owned()),
            memory_space_wide: false,
            as_of_us: snapshot.as_of_us.saturating_add(1),
            terms: vec![rare_event_key(
                world.dataset_id,
                world.world_index,
                *atom_index,
            )],
            topic_keys: BTreeSet::new(),
            entity_ids: BTreeSet::new(),
            k: Some(1),
            spontaneous: None,
        };
        let semantic_rare = rank_model(&rare_query, &semantic_candidates, policy)?;
        let hybrid_rare = rank_model(&rare_query, &hybrid_candidates, policy)?;
        let semantic_recall = if semantic_rare
            .hits
            .iter()
            .any(|hit| hit.atom_id == relevant_id)
        {
            SignedPpm::SCALE
        } else {
            0
        };
        let hybrid_recall = if hybrid_rare
            .hits
            .iter()
            .any(|hit| hit.atom_id == relevant_id)
        {
            SignedPpm::SCALE
        } else {
            0
        };
        rare_semantic_sum = rare_semantic_sum.saturating_add(i64::from(semantic_recall));
        rare_hybrid_sum = rare_hybrid_sum.saturating_add(i64::from(hybrid_recall));
        query_evidence.push(QueryEvaluationV1 {
            query_id: rare_query_id(world.dataset_id, world.world_index, *atom_index),
            comparison_id: "rare_recall_at_1".to_owned(),
            left_score_ppm: SignedPpm::from_raw_unchecked(semantic_recall),
            right_score_ppm: SignedPpm::from_raw_unchecked(hybrid_recall),
            delta_ppm: signed_delta(hybrid_recall, semantic_recall),
            judged_relevant_atom_ids: vec![relevant_id],
            judged_relevance_grades: BTreeMap::from([(relevant_id, 1)]),
            left_hit_ids: semantic_rare
                .hits
                .into_iter()
                .map(|hit| hit.atom_id)
                .collect(),
            right_hit_ids: hybrid_rare
                .hits
                .into_iter()
                .map(|hit| hit.atom_id)
                .collect(),
        });
    }
    let rare_denominator =
        i64::try_from(judgments.rare.relevant_episode_atom_indices.len()).unwrap_or(i64::MAX);
    let rare_semantic = i32::try_from(rare_semantic_sum.div_euclid(rare_denominator)).unwrap_or(0);
    let rare_hybrid = i32::try_from(rare_hybrid_sum.div_euclid(rare_denominator)).unwrap_or(0);
    let rare_delta = signed_delta(rare_hybrid, rare_semantic);

    let separate_flashback_channel = evaluate_flashback_channel(
        &ordinary_query,
        &semantic_candidates,
        &hybrid_candidates,
        world.seed,
        policy,
    )?;
    let episode_generality_preserved = hybrid_ranked.hits.iter().all(|hit| {
        hit.recall_reason == RecallReasonV1::QueryRanked
            && (hit.generality == GeneralityV1::Semantic
                || hit.generality == GeneralityV1::SingleEpisode)
    });
    let result_digest = Digest32::hash_prefixed(
        b"naome-memory:synthetic-world-result:v1\0",
        &(
            plan.digest()?,
            &query_evidence,
            &byte_budget_evidence,
            first_receipt.receipt_id,
        )
            .canonical_bytes()?,
    );

    Ok(WorldEvaluation {
        semantic_delta,
        rare_delta,
        ordinary_delta,
        invariants: BTreeMap::from([
            (
                "apply_atomic_and_idempotent".to_owned(),
                if sqlite_replay_idempotent {
                    ClaimStatusV1::Verified
                } else {
                    ClaimStatusV1::Rejected
                },
            ),
            ("budgets_respected".to_owned(), claim(budgets_respected)),
            (
                "deterministic_replay".to_owned(),
                claim(deterministic_replay),
            ),
            (
                "episode_generality_preserved".to_owned(),
                claim(episode_generality_preserved),
            ),
            ("episodic_exactness".to_owned(), claim(episodic_exactness)),
            (
                "memory_space_isolation".to_owned(),
                claim(memory_space_isolation),
            ),
            ("receipt_closure".to_owned(), claim(receipt_closure)),
            (
                "sealed_atom_immutability".to_owned(),
                claim(sealed_atom_immutability),
            ),
            ("semantic_provenance".to_owned(), claim(semantic_provenance)),
            (
                "separate_flashback_channel".to_owned(),
                claim(separate_flashback_channel),
            ),
            ("tamper_fail_closed".to_owned(), claim(tamper_fail_closed)),
        ]),
        byte_budget_evidence,
        query_evidence,
        result_digest,
    })
}

fn ordinary_qrels(
    world: &crate::SyntheticWorldV1,
    plan: &RetirementPlanV1,
    judgment: &ManifestOrdinaryJudgmentV1,
) -> Result<BTreeMap<AtomId, u32>> {
    let mut qrels = BTreeMap::new();
    for atom_index in &judgment.relevant_episode_atom_indices {
        let body = world.atoms.get(*atom_index).ok_or_else(|| {
            LabError::InvalidReceipt("ordinary judgment atom index is out of bounds".to_owned())
        })?;
        let atom_id = body.atom_id()?;
        qrels.insert(atom_id, 1);
    }

    let semantic_matches = plan
        .semantic_atoms
        .iter()
        .filter_map(|semantic| {
            if !matches!(&semantic.body.payload, MemoryPayloadV1::Semantic(_)) {
                return None;
            }
            (semantic
                .body
                .topic_keys
                .contains(&judgment.relevant_semantic_topic_key))
            .then_some(semantic.atom_id)
        })
        .collect::<Vec<_>>();
    if semantic_matches.len() != 1 {
        return Err(LabError::InvalidReceipt(
            "committed ordinary semantic judgment did not resolve exactly once".to_owned(),
        ));
    }
    let semantic_id = semantic_matches.first().copied().ok_or_else(|| {
        LabError::InvalidReceipt(
            "committed ordinary semantic judgment is unexpectedly empty".to_owned(),
        )
    })?;
    qrels.insert(semantic_id, 1);
    Ok(qrels)
}

fn source_states(world: &crate::SyntheticWorldV1, policy: &PolicyV1) -> Vec<AtomStateV1> {
    world
        .atoms
        .iter()
        .map(|body| AtomStateV1 {
            retention_tier: RetentionTierV1::ShortTerm,
            content_status: ContentStatusV1::Present,
            expires_at_us: Some(
                body.interval
                    .recorded_at_us
                    .saturating_add(policy.stm_horizon_us),
            ),
            superseded_by: None,
            feedback_positive: 0,
            feedback_negative: 0,
            retrieval_count: 0,
            retirement_run_id: None,
        })
        .collect()
}

fn snapshot_from_bodies(
    bodies: &[MemoryAtomBodyV1],
    states: &[AtomStateV1],
    policy: &PolicyV1,
) -> Result<RetirementSnapshotV1> {
    let atoms = bodies
        .iter()
        .cloned()
        .zip(states.iter().cloned())
        .map(|(body, state)| RetirementCandidateV1::from_present_body(body, state))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let latest_expiry_us = atoms
        .iter()
        .filter_map(|candidate| candidate.state.expires_at_us)
        .max()
        .unwrap_or(policy.stm_horizon_us);
    let cohort_day = latest_expiry_us / MICROSECONDS_PER_DAY;
    let as_of_us = cohort_day
        .checked_add(1)
        .and_then(|day| day.checked_mul(MICROSECONDS_PER_DAY))
        .ok_or(MemoryError::InvalidAtomBody(
            "retirement cohort boundary overflows u64",
        ))?;
    Ok(RetirementSnapshotV1 {
        contract_version: "retirement-snapshot-v1".to_owned(),
        memory_space_id: "synthetic-space-v1".to_owned(),
        cohort_day,
        as_of_us,
        atoms,
    })
}

fn semantic_provenance_closes(
    plan: &RetirementPlanV1,
    snapshot: &RetirementSnapshotV1,
    policy: &PolicyV1,
) -> bool {
    plan.semantic_atoms.iter().all(|semantic| {
        let MemoryPayloadV1::Semantic(payload) = &semantic.body.payload else {
            return false;
        };
        payload.source_ids.len() >= policy.min_semantic_sources
            && payload.source_ids.iter().all(|source| {
                payload.source_body_digests.get(source) == Some(&source.digest())
                    && snapshot
                        .atoms
                        .iter()
                        .any(|candidate| candidate.atom_id == *source)
            })
    })
}

fn exact_artifacts(plan: &RetirementPlanV1) -> BTreeMap<AtomId, Vec<Digest32>> {
    plan.decisions
        .iter()
        .filter(|decision| decision.exact_retained)
        .map(|decision| (decision.atom_id, decision.exact_artifact_digests.clone()))
        .collect()
}

fn exact_winners_match_persisted_bytes(
    repository: &SqliteRepository,
    artifact_root: &std::path::Path,
    plan: &RetirementPlanV1,
    source_bytes: &BTreeMap<AtomId, Vec<u8>>,
    source_artifacts: &BTreeMap<AtomId, BTreeMap<Digest32, Vec<u8>>>,
) -> Result<bool> {
    let results = repository.search_candidates(
        &SearchScope {
            memory_space_id: plan.memory_space_id.clone(),
            repository_id: Some("synthetic-repository".to_owned()),
            memory_space_wide: false,
        },
        "synthetic",
        512,
    )?;
    let persisted = results
        .into_iter()
        .map(|candidate| {
            let body = serde_json::from_str::<MemoryAtomBodyV1>(&candidate.json_projection)?;
            Ok((body.atom_id()?, body))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    for winner in &plan.episodic_winners {
        let Some(body) = persisted.get(winner) else {
            return Ok(false);
        };
        let body_matches = source_bytes
            .get(winner)
            .is_some_and(|expected| body.canonical_bytes().is_ok_and(|bytes| bytes == *expected));
        if !body_matches {
            return Ok(false);
        }
        let Some(artifacts) = source_artifacts.get(winner) else {
            return Ok(false);
        };
        for (digest, expected) in artifacts {
            let hex = digest.to_hex();
            let path = artifact_root
                .join("sha256")
                .join(&hex[..2])
                .join(&hex[2..4])
                .join(&hex);
            if fs::read(path)? != *expected {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn semantic_candidates(plan: &RetirementPlanV1) -> Result<Vec<RetrievalCandidateV1>> {
    plan.semantic_atoms
        .iter()
        .map(|semantic| {
            retrieval_candidate(
                semantic.body.clone(),
                RetentionTierV1::SemanticLongTerm,
                &[],
            )
        })
        .collect()
}

fn hybrid_candidates(
    plan: &RetirementPlanV1,
    snapshot: &RetirementSnapshotV1,
    semantic: &[RetrievalCandidateV1],
) -> Result<Vec<RetrievalCandidateV1>> {
    let winners = plan
        .episodic_winners
        .iter()
        .map(|winner| {
            let body = snapshot
                .atoms
                .iter()
                .find(|candidate| candidate.atom_id == *winner)
                .and_then(|candidate| candidate.body.clone())
                .ok_or_else(|| {
                    LabError::InvalidReceipt("episodic winner body is missing".to_owned())
                })?;
            retrieval_candidate(body, RetentionTierV1::EpisodicLongTerm, &[])
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(semantic.iter().cloned().chain(winners).collect())
}

fn retrieval_candidate(
    body: MemoryAtomBodyV1,
    retention_tier: RetentionTierV1,
    terms: &[String],
) -> Result<RetrievalCandidateV1> {
    let atom_id = body.atom_id()?;
    let lexical_score = if terms.is_empty() || lexical_match(&body, terms) {
        Ppm::ONE
    } else {
        Ppm::ZERO
    };
    Ok(RetrievalCandidateV1 {
        atom_id,
        body,
        state: AtomStateV1 {
            retention_tier,
            content_status: ContentStatusV1::Present,
            expires_at_us: None,
            superseded_by: None,
            feedback_positive: 0,
            feedback_negative: 0,
            retrieval_count: 0,
            retirement_run_id: None,
        },
        lexical_score,
        integrity: IntegrityStatusV1::Complete,
    })
}

fn rank_model(
    query: &RetrievalQueryV1,
    candidates: &[RetrievalCandidateV1],
    policy: &PolicyV1,
) -> Result<naome_memory_core::RetrievalResultV1> {
    let ranked = candidates
        .iter()
        .cloned()
        .map(|mut candidate| {
            candidate.lexical_score = if lexical_match(&candidate.body, &query.terms) {
                Ppm::ONE
            } else {
                Ppm::ZERO
            };
            candidate
        })
        .collect();
    Ok(rank_retrieval(
        query,
        &RetrievalCandidatesV1 {
            ranked,
            episodic_pool: Vec::new(),
        },
        policy,
    )?)
}

fn lexical_match(body: &MemoryAtomBodyV1, terms: &[String]) -> bool {
    let mut text = body.trigger.clone();
    for topic in &body.topic_keys {
        text.push(' ');
        text.push_str(topic);
    }
    for observation in &body.observations {
        text.push(' ');
        text.push_str(&observation.content);
    }
    terms.iter().all(|term| text.contains(term))
}

fn evaluate_flashback_channel(
    ordinary_query: &RetrievalQueryV1,
    semantic_candidates: &[RetrievalCandidateV1],
    hybrid_candidates: &[RetrievalCandidateV1],
    seed: Seed32,
    policy: &PolicyV1,
) -> Result<bool> {
    let episodic_pool = hybrid_candidates
        .iter()
        .filter(|candidate| candidate.body.kind() == MemoryKindV1::Episode)
        .cloned()
        .collect::<Vec<_>>();
    let mut query = ordinary_query.clone();
    query.k = Some(10);
    query.spontaneous = Some(SpontaneousRecallRequestV1 { seed, budget: 1 });
    let result = rank_retrieval(
        &query,
        &RetrievalCandidatesV1 {
            ranked: semantic_candidates.to_vec(),
            episodic_pool,
        },
        policy,
    )?;
    let normal_ids = result
        .hits
        .iter()
        .map(|hit| hit.atom_id)
        .collect::<BTreeSet<_>>();
    Ok(result.spontaneous.as_ref().is_some_and(|channel| {
        channel.hits.iter().all(|hit| {
            hit.recall_reason == RecallReasonV1::Spontaneous
                && hit.generality == GeneralityV1::SingleEpisode
                && !normal_ids.contains(&hit.atom_id)
        })
    }))
}

fn golden_fate_evidence(policy: &PolicyV1) -> Result<Vec<GoldenFateScenarioV1>> {
    let world = generate_world_for_dataset(DatasetIdV1::Golden, 0, GOLDEN_ATOM_COUNT)?;
    let states = source_states(&world, policy);
    let snapshot = snapshot_from_bodies(&world.atoms, &states, policy)?;
    (0..GOLDEN_SCENARIO_COUNT)
        .map(|scenario| {
            let seed = Seed32::new([scenario; 32]);
            let plan = plan_retirement(&snapshot, policy, seed)?;
            let mut evidence = GoldenFateScenarioV1 {
                scenario_id: format!("golden-v1/lottery-{scenario:02}"),
                seed,
                plan_digest: plan.digest()?,
                semantic_source_forgotten: 0,
                semantic_source_and_exact: 0,
                exact_only: 0,
                header_only: 0,
            };
            for decision in &plan.decisions {
                if !decision.semantic_atom_ids.is_empty() && decision.exact_retained {
                    evidence.semantic_source_and_exact =
                        evidence.semantic_source_and_exact.saturating_add(1);
                } else if !decision.semantic_atom_ids.is_empty() {
                    evidence.semantic_source_forgotten =
                        evidence.semantic_source_forgotten.saturating_add(1);
                } else if decision.exact_retained {
                    evidence.exact_only = evidence.exact_only.saturating_add(1);
                } else if decision.forget_episode_body {
                    evidence.header_only = evidence.header_only.saturating_add(1);
                }
            }
            Ok(evidence)
        })
        .collect()
}

fn required_invariant_statuses(
    apply_atomicity_status: ClaimStatusV1,
) -> BTreeMap<String, ClaimStatusV1> {
    [
        "apply_atomic_and_idempotent",
        "budgets_respected",
        "deterministic_replay",
        "episode_generality_preserved",
        "episodic_exactness",
        "golden_four_fates",
        "memory_space_isolation",
        "receipt_closure",
        "sealed_atom_immutability",
        "semantic_provenance",
        "separate_flashback_channel",
        "tamper_fail_closed",
    ]
    .into_iter()
    .map(|name| {
        (
            name.to_owned(),
            if name == "apply_atomic_and_idempotent" {
                apply_atomicity_status
            } else {
                ClaimStatusV1::Verified
            },
        )
    })
    .collect()
}

fn aggregate_claim_status(
    statuses: &mut BTreeMap<String, ClaimStatusV1>,
    name: &str,
    observed: ClaimStatusV1,
) {
    let aggregate = statuses
        .entry(name.to_owned())
        .or_insert(ClaimStatusV1::Verified);
    *aggregate = match (*aggregate, observed) {
        (ClaimStatusV1::Rejected, _) | (_, ClaimStatusV1::Rejected) => ClaimStatusV1::Rejected,
        (ClaimStatusV1::Inconclusive, _) | (_, ClaimStatusV1::Inconclusive) => {
            ClaimStatusV1::Inconclusive
        }
        _ => ClaimStatusV1::Verified,
    };
}

const fn claim(observed: bool) -> ClaimStatusV1 {
    if observed {
        ClaimStatusV1::Verified
    } else {
        ClaimStatusV1::Rejected
    }
}

fn signed_delta(right: i32, left: i32) -> SignedPpm {
    SignedPpm::from_raw_unchecked(
        right
            .saturating_sub(left)
            .clamp(-SignedPpm::SCALE, SignedPpm::SCALE),
    )
}

fn ppm_i32(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(SignedPpm::SCALE)
}

fn dataset_bootstrap_seed(dataset_id: DatasetIdV1) -> Seed32 {
    Seed32(Digest32::hash_prefixed(
        BOOTSTRAP_SEED_DOMAIN,
        dataset_id.as_str().as_bytes(),
    ))
}

fn metric(
    values: &[SignedPpm],
    threshold: i32,
    replicates: usize,
    seed: Seed32,
    comparator: MetricComparatorV1,
) -> LabMetricV1 {
    let sum = values
        .iter()
        .map(|value| i64::from(value.get()))
        .sum::<i64>();
    let denominator = i64::try_from(values.len()).unwrap_or(i64::MAX);
    let estimate = i32::try_from(sum.div_euclid(denominator)).unwrap_or(0);
    LabMetricV1 {
        estimate_ppm: SignedPpm::from_raw_unchecked(estimate),
        lower_95_ci_ppm: bootstrap_lower_95(values, replicates, seed),
        threshold_ppm: SignedPpm::from_raw_unchecked(threshold),
        sample_count: values.len(),
        comparator,
    }
}

fn overall_status(
    invariants: &BTreeMap<String, ClaimStatusV1>,
    hypotheses: &BTreeMap<String, HypothesisStatusV1>,
) -> OverallStatusV1 {
    if invariants
        .values()
        .any(|status| *status == ClaimStatusV1::Rejected)
        || hypotheses
            .values()
            .any(|status| *status == HypothesisStatusV1::Rejected)
    {
        OverallStatusV1::Failed
    } else if invariants
        .values()
        .all(|status| *status == ClaimStatusV1::Verified)
        && hypotheses
            .values()
            .all(|status| *status == HypothesisStatusV1::Supported)
    {
        OverallStatusV1::Passed
    } else {
        OverallStatusV1::Inconclusive
    }
}

fn manifest_bytes() -> &'static [u8] {
    include_bytes!("../../../datasets/manifest-v1.json")
}

fn generator_source_digest() -> Digest32 {
    Digest32(Sha256::digest(include_bytes!("synthetic.rs")).into())
}

/// Verify a machine-produced crash campaign attachment and return its digest.
///
/// # Errors
///
/// Returns an error unless the evidence covers every ordered repository fault
/// boundary, binds the current harness/repository sources and policy, records
/// complete crash/recovery facts, and has valid scenario/campaign digests.
pub fn validate_crash_campaign_evidence(evidence: &CrashCampaignEvidenceV1) -> Result<Digest32> {
    if evidence.contract_version != "crash-campaign-evidence-v1"
        || evidence.harness_source_digest != crate::crash_harness_source_digest()
        || evidence.repository_source_digest != SqliteRepository::fault_injection_source_digest()
        || evidence.policy_digest != PolicyV1::poc_v1().digest()?
        || evidence.scenarios.len() != RepositoryFaultPointV1::ORDERED.len()
    {
        return Err(LabError::InvalidReceipt(
            "crash campaign metadata, source binding, or point count mismatch".to_owned(),
        ));
    }
    for (scenario, expected_point) in evidence
        .scenarios
        .iter()
        .zip(RepositoryFaultPointV1::ORDERED)
    {
        let expected_name = expected_point.identifier();
        let expected_operation = if expected_name.starts_with("draft_append_") {
            "draft_append"
        } else if expected_name.starts_with("draft_seal_") {
            "draft_seal"
        } else {
            "retirement_apply"
        };
        let idempotence_is_closed = if expected_operation == "retirement_apply" {
            scenario.idempotent_replay_succeeded == Some(true)
        } else {
            scenario.idempotent_replay_succeeded.is_none()
        };
        if scenario.fault_point != expected_name
            || scenario.operation != expected_operation
            || !scenario.child_terminated_while_transaction_open
            || !scenario.complete_pre_state_observed
            || !scenario.complete_post_state_observed
            || !scenario.recovery_replay_succeeded
            || !idempotence_is_closed
            || !scenario.sqlite_integrity_ok
            || !scenario.foreign_keys_clean
            || scenario.pre_state_digest != scenario.post_crash_state_digest
            || scenario.result_digest != scenario.recompute_result_digest()?
        {
            return Err(LabError::InvalidReceipt(format!(
                "crash fault point {expected_name} did not close"
            )));
        }
    }
    let observed = evidence.recompute_campaign_digest()?;
    if evidence.campaign_digest != observed {
        return Err(LabError::InvalidReceipt(
            "crash campaign digest mismatch".to_owned(),
        ));
    }
    Ok(observed)
}

fn manifest_digest() -> Digest32 {
    Digest32::hash_prefixed(MANIFEST_HASH_DOMAIN, manifest_bytes())
}

fn validate_committed_manifest(
    dataset_id: DatasetIdV1,
    expected_world_count: usize,
) -> Result<ManifestV1> {
    validate_manifest_bytes(
        manifest_bytes(),
        include_bytes!("synthetic.rs"),
        dataset_id,
        expected_world_count,
    )
}

fn validate_manifest_bytes(
    bytes: &[u8],
    generator_source: &[u8],
    dataset_id: DatasetIdV1,
    expected_world_count: usize,
) -> Result<ManifestV1> {
    let manifest: ManifestV1 = serde_json::from_slice(bytes)?;
    if manifest.contract_version != "dataset-manifest-v1"
        || manifest.generator_version != "synthetic-world-v1"
        || manifest.generator_source.path != "crates/naome-memory-lab/src/synthetic.rs"
        || manifest.seed_derivation.algorithm != "sha256"
        || manifest.seed_derivation.encoding != "domain || u64_be(world_index)"
    {
        return Err(LabError::InvalidReceipt(
            "dataset manifest contract metadata diverged".to_owned(),
        ));
    }
    let expected_source = Digest32(Sha256::digest(generator_source).into());
    let declared_source = manifest
        .generator_source
        .sha256
        .parse::<Digest32>()
        .map_err(|error| LabError::InvalidReceipt(error.to_string()))?;
    if declared_source != expected_source {
        return Err(LabError::InvalidReceipt(
            "generator source digest mismatch".to_owned(),
        ));
    }
    let profile_key = match dataset_id {
        DatasetIdV1::Calibration => "pr",
        DatasetIdV1::Holdout => "deep",
        _ => {
            return Err(LabError::InvalidReceipt(
                "proof profiles may use only calibration-v1 or holdout-v1".to_owned(),
            ));
        }
    };
    if manifest.profile_datasets.get(profile_key) != Some(&dataset_id) {
        return Err(LabError::InvalidReceipt(
            "profile-to-dataset mapping mismatch".to_owned(),
        ));
    }
    let Some(dataset) = manifest.datasets.get(dataset_id.as_str()) else {
        return Err(LabError::InvalidReceipt(
            "dataset entry is missing".to_owned(),
        ));
    };
    if dataset.world_count != Some(expected_world_count)
        || dataset.atom_count != Some(WORLD_ATOM_COUNT)
    {
        return Err(LabError::InvalidReceipt(
            "dataset world or atom count mismatch".to_owned(),
        ));
    }
    let expected_ids = (0..expected_world_count)
        .map(|index| world_id(dataset_id, u64::try_from(index).unwrap_or(u64::MAX)))
        .collect::<Vec<_>>();
    if dataset.world_ids.as_ref() != Some(&expected_ids) {
        return Err(LabError::InvalidReceipt(
            "explicit world IDs are missing, reordered, or divergent".to_owned(),
        ));
    }
    let expected_domain = String::from_utf8(dataset_id.seed_domain().to_vec())
        .map_err(|error| LabError::InvalidReceipt(error.to_string()))?;
    if manifest.seed_derivation.domains.get(dataset_id.as_str()) != Some(&expected_domain) {
        return Err(LabError::InvalidReceipt(
            "dataset seed domain mismatch".to_owned(),
        ));
    }
    validate_query_judgments(dataset_id, dataset.query_judgments.as_ref())?;
    validate_golden_manifest(&manifest)?;
    Ok(manifest)
}

fn validate_query_judgments(
    dataset_id: DatasetIdV1,
    judgments: Option<&ManifestQueryJudgmentsV1>,
) -> Result<()> {
    let Some(judgments) = judgments else {
        return Err(LabError::InvalidReceipt(
            "query judgments are missing".to_owned(),
        ));
    };
    let ordinary = &judgments.ordinary;
    let rare = &judgments.rare;
    if ordinary.query_id != "ordinary-build-proof-v1"
        || ordinary.terms != ["build", "proof"]
        || ordinary.topic_keys != ["build-proof"]
        || ordinary.relevant_episode_atom_indices != [0, 1, 2]
        || ordinary.relevant_semantic_topic_key != "build-proof"
        || ordinary.metric != "ndcg_at_10"
        || rare.query_id_format != "rare-query:{dataset_id}:{world_index:04}:{atom_index:02}"
        || rare.term_format != "rare:{dataset_id}:{world_index:04}:{atom_index:02}"
        || rare.relevant_episode_atom_indices != (0..WORLD_ATOM_COUNT).collect::<Vec<_>>()
        || rare.metric != "recall_at_1"
    {
        return Err(LabError::InvalidReceipt(format!(
            "query judgments for {} diverged",
            dataset_id.as_str()
        )));
    }
    Ok(())
}

fn validate_golden_manifest(manifest: &ManifestV1) -> Result<()> {
    let Some(golden) = manifest.datasets.get(DatasetIdV1::Golden.as_str()) else {
        return Err(LabError::InvalidReceipt(
            "golden dataset entry is missing".to_owned(),
        ));
    };
    let expected = (0..GOLDEN_SCENARIO_COUNT)
        .map(|value| hex::encode([value; 32]))
        .collect::<Vec<_>>();
    if golden.atom_count != Some(GOLDEN_ATOM_COUNT)
        || golden.golden_seed_scenarios.as_ref() != Some(&expected)
    {
        return Err(LabError::InvalidReceipt(
            "golden fate seed scenarios diverged".to_owned(),
        ));
    }
    Ok(())
}

/// Verify the self-contained lab experiment envelope.
///
/// This is distinct from the core `proof-receipt-v1` retirement receipt.
///
/// # Errors
///
/// Returns an error for an unknown contract, a policy, manifest, generator,
/// dataset/profile mismatch, or a tampered result digest.
pub fn verify_lab_receipt(receipt: &LabProofReceiptV1) -> Result<()> {
    let manifest = validate_committed_manifest(receipt.dataset_id, receipt.world_count)?;
    verify_lab_receipt_with_manifest(receipt, &manifest)
}

fn verify_lab_receipt_with_manifest(
    receipt: &LabProofReceiptV1,
    _manifest: &ManifestV1,
) -> Result<()> {
    if receipt.contract_version != "lab-proof-receipt-v1" {
        return Err(LabError::InvalidReceipt(
            "unknown lab receipt contract version".to_owned(),
        ));
    }
    if receipt.dataset_id != receipt.profile.dataset_id()
        || receipt.world_count != receipt.profile.world_count()
    {
        return Err(LabError::InvalidReceipt(
            "profile, dataset, and world count do not match".to_owned(),
        ));
    }
    if receipt.policy_digest != PolicyV1::poc_v1().digest()? {
        return Err(LabError::InvalidReceipt(
            "policy digest mismatch".to_owned(),
        ));
    }
    if receipt.dataset_manifest_digest != manifest_digest()
        || receipt.generator_source_digest != generator_source_digest()
    {
        return Err(LabError::InvalidReceipt(
            "dataset manifest or generator source digest mismatch".to_owned(),
        ));
    }
    if receipt.result_digest != receipt_digest(receipt)? {
        return Err(LabError::InvalidReceipt(
            "result digest mismatch".to_owned(),
        ));
    }
    Ok(())
}

/// Verify the full lab closure by replaying the committed experiment.
///
/// Digest closure is checked first, then every world is regenerated from the
/// committed manifest and generator, persisted and evaluated again. The
/// verifier independently recomputes world inputs/results, complete qrels,
/// byte-budget evidence, golden fates, metrics, bootstrap confidence bounds,
/// statuses, the ledger, and the overall outcome. Rehashing altered evidence
/// therefore cannot make it authoritative.
///
/// # Errors
///
/// Returns an error when the receipt fails verification or an attachment is
/// missing, reordered, dataset-mismatched, or digest-inconsistent.
pub fn verify_lab_bundle(bundle: &LabProofBundleV1) -> Result<()> {
    verify_lab_receipt(&bundle.receipt)?;
    let ledger = &bundle.decision_ledger;
    if ledger.contract_version != "decision-ledger-v1"
        || ledger.profile != bundle.receipt.profile
        || ledger.dataset_id != bundle.receipt.dataset_id
        || ledger.entries.len() != bundle.receipt.world_count
    {
        return Err(LabError::InvalidReceipt(
            "decision ledger metadata does not close".to_owned(),
        ));
    }
    for (expected_index, entry) in ledger.entries.iter().enumerate() {
        let index = u64::try_from(expected_index).unwrap_or(u64::MAX);
        if entry.dataset_id != ledger.dataset_id
            || entry.world_index != index
            || entry.world_id != world_id(ledger.dataset_id, index)
            || entry.world_seed != derive_dataset_world_seed(ledger.dataset_id, index)
        {
            return Err(LabError::InvalidReceipt(
                "decision ledger world mapping is not canonical".to_owned(),
            ));
        }
    }
    let observed_ledger = ledger_digest(ledger)?;
    let observed_golden = Digest32::hash_prefixed(
        GOLDEN_HASH_DOMAIN,
        &ledger.golden_fate_evidence.canonical_bytes()?,
    );
    let observed_crash = ledger
        .crash_campaign_evidence
        .as_ref()
        .map(validate_crash_campaign_evidence)
        .transpose()?;
    if observed_ledger != ledger.ledger_digest
        || observed_ledger != bundle.receipt.decision_ledger_digest
        || observed_golden != bundle.receipt.golden_fate_digest
        || observed_crash != bundle.receipt.crash_campaign_digest
    {
        return Err(LabError::InvalidReceipt(
            "decision ledger or golden fate digest mismatch".to_owned(),
        ));
    }
    let apply_status = bundle
        .receipt
        .invariant_statuses
        .get("apply_atomic_and_idempotent");
    if (observed_crash.is_some() && apply_status != Some(&ClaimStatusV1::Verified))
        || (observed_crash.is_none() && apply_status == Some(&ClaimStatusV1::Verified))
    {
        return Err(LabError::InvalidReceipt(
            "atomicity status is not justified by crash evidence".to_owned(),
        ));
    }
    let expected = construct_lab_bundle(
        bundle.receipt.profile,
        ledger.crash_campaign_evidence.clone(),
    )?;
    if bundle != &expected {
        return Err(LabError::InvalidReceipt(
            "lab bundle diverges from independent committed experiment replay".to_owned(),
        ));
    }
    Ok(())
}

fn receipt_digest(receipt: &LabProofReceiptV1) -> Result<Digest32> {
    let mut unsigned = receipt.clone();
    unsigned.result_digest = Digest32::ZERO;
    Ok(Digest32::hash_prefixed(
        RECEIPT_HASH_DOMAIN,
        &unsigned.canonical_bytes()?,
    ))
}

fn ledger_digest(ledger: &DecisionLedgerV1) -> Result<Digest32> {
    let mut unsigned = ledger.clone();
    unsigned.ledger_digest = Digest32::ZERO;
    Ok(Digest32::hash_prefixed(
        LEDGER_HASH_DOMAIN,
        &unsigned.canonical_bytes()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        DatasetIdV1, HypothesisStatusV1, OverallStatusV1, ProofProfileV1, SignedPpm,
        construct_lab_bundle, evaluate_world, generator_source_digest, ledger_digest,
        manifest_bytes, receipt_digest, run_proof, validate_committed_manifest,
        validate_manifest_bytes, verify_lab_bundle, verify_lab_receipt,
    };

    fn rehash_bundle(bundle: &mut super::LabProofBundleV1) -> crate::Result<()> {
        bundle.decision_ledger.ledger_digest = ledger_digest(&bundle.decision_ledger)?;
        bundle.receipt.decision_ledger_digest = bundle.decision_ledger.ledger_digest;
        bundle.receipt.result_digest = receipt_digest(&bundle.receipt)?;
        Ok(())
    }

    #[test]
    fn proof_replays_and_tampering_fails_closed() -> crate::Result<()> {
        let bundle = run_proof(ProofProfileV1::Pr)?;
        assert!(verify_lab_bundle(&bundle).is_ok());
        assert!(verify_lab_receipt(&bundle.receipt).is_ok());
        assert_eq!(bundle.receipt.dataset_id, DatasetIdV1::Calibration);
        assert_eq!(bundle.receipt.overall_status, OverallStatusV1::Inconclusive);
        assert!(
            bundle
                .receipt
                .hypothesis_statuses
                .values()
                .all(|status| *status == HypothesisStatusV1::Inconclusive)
        );
        assert!(
            bundle
                .receipt
                .metrics
                .values()
                .all(|metric| metric.sample_count == 40)
        );
        assert_eq!(
            bundle.receipt.generator_source_digest,
            generator_source_digest()
        );
        let mut tampered = bundle.receipt;
        tampered.world_count = tampered.world_count.saturating_add(1);
        assert!(verify_lab_receipt(&tampered).is_err());
        Ok(())
    }

    #[test]
    fn generator_source_divergence_fails_closed() {
        let mut divergent_source = include_bytes!("synthetic.rs").to_vec();
        divergent_source.push(0);
        assert!(
            validate_manifest_bytes(
                manifest_bytes(),
                &divergent_source,
                DatasetIdV1::Calibration,
                40,
            )
            .is_err()
        );
    }

    #[test]
    fn first_world_matches_hand_calculated_model_scores() -> crate::Result<()> {
        let manifest = validate_committed_manifest(DatasetIdV1::Calibration, 40)?;
        let judgments = manifest
            .datasets
            .get(DatasetIdV1::Calibration.as_str())
            .and_then(|dataset| dataset.query_judgments.as_ref())
            .ok_or_else(|| {
                crate::LabError::InvalidReceipt("validated query judgments missing".to_owned())
            })?;
        let world = crate::generate_world_for_dataset(DatasetIdV1::Calibration, 0, 10)?;
        let evaluation = evaluate_world(&world, &naome_memory_core::PolicyV1::poc_v1(), judgments)?;
        let semantic = evaluation.query_evidence.first().ok_or_else(|| {
            crate::LabError::InvalidReceipt("semantic comparison evidence missing".to_owned())
        })?;
        let hybrid = evaluation.query_evidence.get(1).ok_or_else(|| {
            crate::LabError::InvalidReceipt("hybrid comparison evidence missing".to_owned())
        })?;
        let mut grades = semantic
            .judged_relevance_grades
            .values()
            .copied()
            .collect::<Vec<_>>();
        grades.sort_unstable();
        assert_eq!(grades, [1, 1, 1, 1]);
        assert_eq!(semantic.left_score_ppm.get(), 353_511);
        assert_eq!(semantic.right_score_ppm.get(), 390_380);
        assert_eq!(semantic.delta_ppm.get(), 36_869);
        assert_eq!(hybrid.left_score_ppm.get(), 390_380);
        assert_eq!(hybrid.right_score_ppm.get(), 636_682);
        assert_eq!(hybrid.delta_ppm.get(), 246_302);
        Ok(())
    }

    #[test]
    fn independently_replayed_evidence_rejects_rehashed_semantic_tampering() -> crate::Result<()> {
        let expected = construct_lab_bundle(ProofProfileV1::Pr, None)?;

        let mut changed_bound = expected.clone();
        let metric = changed_bound
            .receipt
            .metrics
            .get_mut("semantic_utility")
            .ok_or_else(|| crate::LabError::InvalidReceipt("required metric missing".to_owned()))?;
        metric.lower_95_ci_ppm = SignedPpm::from_raw_unchecked(
            metric
                .lower_95_ci_ppm
                .get()
                .saturating_sub(1)
                .max(-SignedPpm::SCALE),
        );
        changed_bound.receipt.result_digest = receipt_digest(&changed_bound.receipt)?;
        assert!(verify_lab_receipt(&changed_bound.receipt).is_ok());
        assert!(verify_lab_bundle(&changed_bound).is_err());

        let mut changed_status = expected.clone();
        changed_status
            .receipt
            .hypothesis_statuses
            .insert("semantic_utility".to_owned(), HypothesisStatusV1::Supported);
        changed_status.receipt.overall_status = OverallStatusV1::Passed;
        changed_status.receipt.result_digest = receipt_digest(&changed_status.receipt)?;
        assert!(verify_lab_receipt(&changed_status.receipt).is_ok());
        assert!(verify_lab_bundle(&changed_status).is_err());

        let mut changed_world_result = expected.clone();
        let world_result = changed_world_result
            .decision_ledger
            .entries
            .first_mut()
            .ok_or_else(|| crate::LabError::InvalidReceipt("world result missing".to_owned()))?;
        let query_result = world_result
            .query_evidence
            .first_mut()
            .ok_or_else(|| crate::LabError::InvalidReceipt("query result missing".to_owned()))?;
        query_result.right_score_ppm = SignedPpm::ZERO;
        let grade = query_result
            .judged_relevance_grades
            .values_mut()
            .next()
            .ok_or_else(|| crate::LabError::InvalidReceipt("query qrels missing".to_owned()))?;
        *grade = 2;
        world_result.result_digest = naome_memory_core::Digest32::ZERO;
        rehash_bundle(&mut changed_world_result)?;
        assert!(verify_lab_bundle(&changed_world_result).is_err());

        let mut changed_budget = expected;
        let budget = &mut changed_budget
            .decision_ledger
            .entries
            .first_mut()
            .ok_or_else(|| crate::LabError::InvalidReceipt("world budget missing".to_owned()))?
            .byte_budget_evidence;
        budget.semantic_only_used_bytes = budget.semantic_only_used_bytes.saturating_add(1);
        rehash_bundle(&mut changed_budget)?;
        assert!(verify_lab_bundle(&changed_budget).is_err());
        Ok(())
    }

    #[test]
    fn frozen_holdout_reports_binary_qrel_non_support() -> crate::Result<()> {
        let bundle = run_proof(ProofProfileV1::Deep)?;
        assert_eq!(bundle.receipt.dataset_id, DatasetIdV1::Holdout);
        assert_eq!(bundle.receipt.world_count, 100);
        assert_eq!(bundle.receipt.overall_status, OverallStatusV1::Failed);
        assert_eq!(
            bundle.receipt.hypothesis_statuses.get("semantic_utility"),
            Some(&super::HypothesisStatusV1::Rejected)
        );
        assert_eq!(
            bundle
                .receipt
                .hypothesis_statuses
                .get("episodic_rare_recovery"),
            Some(&super::HypothesisStatusV1::Supported)
        );
        assert_eq!(
            bundle
                .receipt
                .hypothesis_statuses
                .get("ordinary_utility_preserved"),
            Some(&super::HypothesisStatusV1::Supported)
        );
        assert_eq!(
            bundle
                .receipt
                .invariant_statuses
                .get("apply_atomic_and_idempotent"),
            Some(&super::ClaimStatusV1::Inconclusive)
        );
        assert!(verify_lab_bundle(&bundle).is_ok());
        Ok(())
    }
}
