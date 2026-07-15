#![forbid(unsafe_code)]

mod schema_fixtures;

use anyhow::{Context as _, Result, ensure};
use clap::{Parser, Subcommand};
use naome_memory_core::{
    ATOM_HASH_DOMAIN, ApplyRetirementOutcomeV1, AtomStateV1, CanonicalBytes as _, ContentStatusV1,
    Digest32, GeneralityV1, InvariantStatusV1, MemoryAtomBodyV1, OverallProofStatusV1, PolicyV1,
    ProofReceiptV1, RecallReasonV1, ReceiptClosureV1, ReceiptVerificationInputsV1,
    RetentionPermissionV1, RetentionTierV1, RetirementPlanV1, RetirementRepositoryV1,
    RetrievalQueryV1, Seed32, SpontaneousRecallRequestV1, apply_retirement, plan_retirement,
    rank_retrieval, verify_receipt,
};
use naome_memory_lab::{
    ClaimStatusV1, CrashCampaignEvidenceV1, DecisionLedgerV1, HypothesisStatusV1, LabProofBundleV1,
    LabProofReceiptV1, OverallStatusV1, ProofProfileV1, generate_world,
    run_proof_with_crash_evidence, validate_crash_campaign_evidence, verify_lab_bundle,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::str::FromStr as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use naome_memory_sqlite::{CohortKey, MAX_ATOM_INSERT_BATCH, SqliteRepository, StoreConfig};

const PROOF_DIR: &str = "target/proofs";
const SCALE_RECEIPT_FILE: &str = "scale-receipt.json";
const SCALE_CORE_RECEIPT_FILE: &str = "scale-core-retirement-receipt.json";
const MUTATION_RECEIPT_FILE: &str = "mutation-receipt.json";
const MUTATION_LOG_STEM: &str = "mutation-naome-memory-core";
const MUTATION_OUTPUT_DIR: &str = "mutation-tool";
const FUZZ_RECEIPT_FILE: &str = "fuzz-receipt.json";
const FUZZ_ARTIFACT_DIR: &str = "fuzz-artifacts";
const FUZZ_MAX_ARTIFACT_COUNT: usize = 16;
const FUZZ_MAX_ARTIFACT_BYTES: u64 = 1024 * 1024;
const DEEP_SCALE_ATOM_COUNT: usize = 100_000;
const SCALE_MAX_WALL_MILLIS: u64 = 1_800_000;
const SCALE_MAX_RSS_BYTES: u64 = 6 * 1024 * 1024 * 1024;
const SCALE_MAX_DISK_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const MUTATION_TOOL_VERSION: &str = "27.1.0";
const FUZZ_TOOL_VERSION: &str = "0.13.2";
const FUZZ_NIGHTLY: &str = "nightly-2026-07-01";
const FUZZ_TARGETS: [(&str, &str); 4] = [
    ("decoder", "fuzz-decoder"),
    ("canonical", "fuzz-canonical"),
    ("receipt_verifier", "fuzz-receipt-verifier"),
    ("sqlite_ingest", "fuzz-sqlite-ingest"),
];
const TOOL_RECEIPT_HASH_DOMAIN: &[u8] = b"naome-memory:tool-receipt:v1\0";
const MICROSECONDS_PER_DAY: u64 = 86_400_000_000;

#[derive(Debug, Parser)]
#[command(name = "xtask", about = "NAOME Memory proof orchestration")]
struct Xtask {
    #[command(subcommand)]
    task: Task,
}

#[derive(Debug, Subcommand)]
enum Task {
    /// Generate deterministic, typed JSON instances for every public core schema.
    SchemaFixtures {
        #[arg(long)]
        output_dir: PathBuf,
    },
    Proof {
        #[arg(long)]
        profile: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// Recompute the proof and freshly re-run its complete crash campaign.
    Replay {
        #[arg(long)]
        receipt: PathBuf,
        /// Number of full proof plus 13-boundary crash reproofs to run.
        #[arg(long)]
        repetitions: usize,
    },
    PlatformDigest,
    Scale {
        #[arg(long)]
        atoms: usize,
    },
    Mutation,
    Fuzz {
        #[arg(long)]
        seconds: u64,
    },
    VerifyToolReceipt {
        #[arg(long)]
        receipt: PathBuf,
        #[arg(long)]
        proof_dir: PathBuf,
    },
    AssertClaims {
        #[arg(long)]
        receipt: PathBuf,
    },
    ReleasePreflight {
        #[arg(long)]
        version: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ScaleReceiptV1 {
    contract_version: String,
    source_commit: String,
    status: EvidenceStatusV1,
    atom_count: usize,
    p50_body_bytes: u64,
    p95_body_bytes: u64,
    maximum_body_bytes: u64,
    logical_body_bytes: u64,
    atom_stream_digest: String,
    logical_digest: String,
    input_set_digest: String,
    plan_digest: String,
    receipt_digest: String,
    retrieval_digest: String,
    semantic_atom_count: usize,
    episodic_winner_count: usize,
    normal_hit_count: usize,
    spontaneous_hit_count: usize,
    plan_replay_verified: bool,
    receipt_verified: bool,
    apply_idempotent: bool,
    observed_phase_millis: BTreeMap<String, u64>,
    observed_wall_millis: u64,
    observed_max_rss_bytes: Option<u64>,
    observed_disk_bytes: Option<u64>,
    note: String,
    evidence_digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum EvidenceStatusV1 {
    Passed,
    Failed,
    Inconclusive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ToolReceiptV1 {
    contract_version: String,
    tool: String,
    tool_version: String,
    toolchain: String,
    source_commit: String,
    status: EvidenceStatusV1,
    targets: Vec<ToolTargetEvidenceV1>,
    note: String,
    receipt_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ToolTargetEvidenceV1 {
    target: String,
    command: Vec<String>,
    requested_duration_seconds: Option<u64>,
    observed_duration_millis: u64,
    exit_code: Option<i32>,
    log_stem: String,
    stdout_digest: String,
    stderr_digest: String,
    artifacts: Vec<ToolArtifactEvidenceV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ToolArtifactEvidenceV1 {
    relative_path: String,
    byte_len: u64,
    sha256: String,
}

#[derive(Serialize)]
struct ToolReceiptClosureV1<'a> {
    contract_version: &'a str,
    tool: &'a str,
    tool_version: &'a str,
    toolchain: &'a str,
    source_commit: &'a str,
    status: EvidenceStatusV1,
    targets: &'a [ToolTargetEvidenceV1],
    note: &'a str,
}

struct ScaleLogicalV1 {
    sizes: Vec<u64>,
    logical_body_bytes: u64,
    atom_stream_digest: String,
}

#[derive(Serialize)]
struct ScaleLogicalClosureV1<'a> {
    contract_version: &'static str,
    atom_count: usize,
    p50_body_bytes: u64,
    p95_body_bytes: u64,
    maximum_body_bytes: u64,
    logical_body_bytes: u64,
    atom_stream_digest: &'a str,
    input_set_digest: Digest32,
    plan_digest: Digest32,
    receipt_digest: Digest32,
    retrieval_digest: Digest32,
}

impl ScaleReceiptV1 {
    fn recompute_evidence_digest(&self) -> Result<String> {
        let mut authority = self.clone();
        authority.evidence_digest.clear();
        Ok(Digest32::hash_prefixed(
            b"naome-memory:scale-evidence:v1\0",
            &serde_json::to_vec(&authority)?,
        )
        .to_hex())
    }
}

struct ExpectedScaleRepository(ApplyRetirementOutcomeV1);

impl RetirementRepositoryV1 for ExpectedScaleRepository {
    fn apply_plan_atomically(
        &mut self,
        _plan: &RetirementPlanV1,
    ) -> naome_memory_core::Result<ApplyRetirementOutcomeV1> {
        Ok(self.0.clone())
    }
}

fn main() -> Result<()> {
    let command = Xtask::parse();
    match command.task {
        Task::SchemaFixtures { output_dir } => schema_fixtures::generate(&output_dir),
        Task::Proof { profile, output } => proof(&profile, &output),
        Task::Replay {
            receipt,
            repetitions,
        } => replay(&receipt, repetitions),
        Task::PlatformDigest => platform_digest(),
        Task::Scale { atoms } => scale(atoms),
        Task::Mutation => mutation(),
        Task::Fuzz { seconds } => fuzz(seconds),
        Task::VerifyToolReceipt { receipt, proof_dir } => {
            verify_tool_receipt_file(&receipt, &proof_dir)
        }
        Task::AssertClaims { receipt } => assert_claims(&receipt),
        Task::ReleasePreflight { version } => release_preflight(&version),
    }
}

fn proof(profile: &str, output: &Path) -> Result<()> {
    let profile = ProofProfileV1::from_str(profile)?;
    let crash_evidence = execute_crash_harness()?;
    let bundle = run_proof_with_crash_evidence(profile, Some(crash_evidence.clone()))?;
    verify_lab_bundle(&bundle)?;
    write_json(output, &bundle.receipt)?;
    let directory = output.parent().unwrap_or_else(|| Path::new("."));
    write_json(
        &directory.join("decision-ledger.json"),
        &bundle.decision_ledger,
    )?;
    write_json(
        &directory.join("crash-campaign-evidence.json"),
        &crash_evidence,
    )?;
    copy_manifest(&directory.join("dataset-manifest-v1.json"))?;
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "contract_version": "xtask.proof-result.v1",
            "profile": profile,
            "overall_status": bundle.receipt.overall_status,
            "receipt_digest": bundle.receipt.result_digest,
        }))?
    );
    Ok(())
}

fn assert_claims(receipt_path: &Path) -> Result<()> {
    let receipt: LabProofReceiptV1 = read_json(receipt_path)?;
    let directory = receipt_path.parent().unwrap_or_else(|| Path::new("."));
    let ledger: DecisionLedgerV1 = read_json(&directory.join("decision-ledger.json"))?;
    require_manifest_copy(&directory.join("dataset-manifest-v1.json"))?;
    require_crash_evidence_copy(directory, &ledger)?;
    verify_lab_bundle(&LabProofBundleV1 {
        receipt: receipt.clone(),
        decision_ledger: ledger,
    })?;
    ensure!(
        receipt.profile == ProofProfileV1::Deep
            && receipt.overall_status == OverallStatusV1::Passed,
        "deep proof is valid but its required claims are not all supported"
    );
    ensure_required_claims(&receipt)
}

fn replay(receipt_path: &Path, repetitions: usize) -> Result<()> {
    ensure!(repetitions > 0, "repetitions must be greater than zero");
    let receipt: LabProofReceiptV1 = read_json(receipt_path)?;
    let directory = receipt_path.parent().unwrap_or_else(|| Path::new("."));
    let ledger: DecisionLedgerV1 = read_json(&directory.join("decision-ledger.json"))?;
    require_manifest_copy(&directory.join("dataset-manifest-v1.json"))?;
    let recorded_crash_evidence = require_crash_evidence_copy(directory, &ledger)?;
    let expected = LabProofBundleV1 {
        receipt,
        decision_ledger: ledger,
    };
    verify_lab_bundle(&expected)?;
    for repetition in 0..repetitions {
        let fresh_crash_evidence = execute_crash_harness()
            .with_context(|| format!("crash reproof {} failed", repetition + 1))?;
        ensure!(
            fresh_crash_evidence == recorded_crash_evidence,
            "fresh crash campaign {} differs from the supplied closure",
            repetition + 1
        );
        let replayed =
            run_proof_with_crash_evidence(expected.receipt.profile, Some(fresh_crash_evidence))
                .with_context(|| format!("proof replay {} failed", repetition + 1))?;
        verify_lab_bundle(&replayed)?;
        ensure!(
            replayed == expected,
            "proof replay {} differs from the supplied closure",
            repetition + 1
        );
    }
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "contract_version": "xtask.replay-result.v1",
            "repetitions": repetitions,
            "fresh_crash_campaigns": repetitions,
            "crash_scenarios_per_campaign": naome_memory_sqlite::RepositoryFaultPointV1::ORDERED.len(),
            "receipt_digest": expected.receipt.result_digest,
            "status": "verified",
        }))?
    );
    Ok(())
}

fn platform_digest() -> Result<()> {
    let crash_evidence = execute_crash_harness()?;
    let bundle = run_proof_with_crash_evidence(ProofProfileV1::Pr, Some(crash_evidence))?;
    verify_lab_bundle(&bundle)?;
    println!("{}", bundle.receipt.result_digest);
    Ok(())
}

fn scale(atoms: usize) -> Result<()> {
    scale_with_output(atoms, &workspace_root().join(PROOF_DIR))
}

#[allow(
    clippy::too_many_lines,
    reason = "the scale proof keeps its ordered persist-plan-verify-apply-retrieve evidence chain explicit"
)]
fn scale_with_output(atoms: usize, proof_directory: &Path) -> Result<()> {
    ensure!(
        atoms >= 100,
        "representative scale proof requires at least 100 atoms"
    );
    ensure!(
        atoms <= DEEP_SCALE_ATOM_COUNT,
        "atom count exceeds the 100,000-atom PoC envelope"
    );
    clear_scale_evidence(proof_directory)?;
    let source_commit_at_start = current_clean_source_commit();
    let started = Instant::now();
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("scale.db");
    let artifact_root = temporary.path().join("artifacts");
    let mut repository = SqliteRepository::open(&StoreConfig::new(&database, &artifact_root))?;
    let policy = PolicyV1::poc_v1();
    repository.install_policy(&policy, 1)?;
    let rss_sampler = RssSampler::start();
    let mut phases = BTreeMap::new();

    let phase_started = Instant::now();
    let mut logical = persist_scale_atoms(&mut repository, atoms, &policy)?;
    phases.insert("persist".to_owned(), elapsed_millis(phase_started));

    let expires_at_us = 100_u64
        .checked_add(policy.stm_horizon_us)
        .context("scale STM expiry overflow")?;
    let cohort_day = expires_at_us / MICROSECONDS_PER_DAY;
    let retirement_as_of_us = cohort_day
        .checked_add(1)
        .and_then(|day| day.checked_mul(MICROSECONDS_PER_DAY))
        .context("scale retirement cohort boundary overflow")?;
    let key = CohortKey {
        memory_space_id: "synthetic-space-v1".to_owned(),
        cohort_day,
        as_of_us: retirement_as_of_us,
    };

    let phase_started = Instant::now();
    let snapshot = repository.snapshot_core(&key)?;
    phases.insert("snapshot".to_owned(), elapsed_millis(phase_started));
    ensure!(
        snapshot.atoms.len() == atoms,
        "persisted cohort count differs from generated count"
    );

    let seed = Seed32::new([0x5a; 32]);
    let phase_started = Instant::now();
    let plan = plan_retirement(&snapshot, &policy, seed)?;
    phases.insert("plan".to_owned(), elapsed_millis(phase_started));
    ensure!(
        !plan.semantic_atoms.is_empty(),
        "representative scale cohort produced no semantic atom"
    );
    ensure!(
        !plan.episodic_winners.is_empty(),
        "representative scale cohort produced no episodic winner"
    );

    let phase_started = Instant::now();
    let replayed_plan = plan_retirement(&snapshot, &policy, seed)?;
    ensure!(replayed_plan == plan, "scale plan replay diverged");
    ensure!(
        replayed_plan.digest()? == plan.digest()?,
        "scale plan digest replay diverged"
    );
    phases.insert("plan_replay".to_owned(), elapsed_millis(phase_started));
    drop(replayed_plan);

    let phase_started = Instant::now();
    let closure = ReceiptClosureV1::from_plan(&plan);
    let expected_outcome = ApplyRetirementOutcomeV1 {
        observed_input_digest: plan.input_set_digest,
        state_digest: plan.projected_state_digest,
        applied_decision_count: plan.decisions.len(),
        semantic_atom_ids: closure.semantic_atom_ids.clone(),
        episodic_atom_ids: closure.exact_episode_ids.clone(),
        exact_artifact_digests: closure.exact_artifact_digests.clone(),
        already_applied: false,
    };
    let expected_receipt = apply_retirement(&mut ExpectedScaleRepository(expected_outcome), &plan)?;
    let verification_inputs = ReceiptVerificationInputsV1 {
        policy: policy.clone(),
        snapshot,
        seed,
        observed_state_digest: plan.projected_state_digest,
        observed_exact_artifact_digests: closure.exact_artifact_digests,
    };
    let verified = verify_receipt(&expected_receipt, &verification_inputs)?;
    ensure!(
        verified.receipt_id == expected_receipt.receipt_id,
        "scale receipt verification returned a different receipt id"
    );
    phases.insert("receipt_verify".to_owned(), elapsed_millis(phase_started));

    // The large pre-apply snapshot is no longer needed. The SQLite adapter
    // performs its own stale read inside the atomic apply boundary.
    drop(verification_inputs);

    let phase_started = Instant::now();
    let applied_receipt = apply_retirement(&mut repository, &plan)?;
    ensure!(
        applied_receipt == expected_receipt,
        "real SQLite apply produced a receipt different from the verified closure"
    );
    phases.insert("apply".to_owned(), elapsed_millis(phase_started));

    let phase_started = Instant::now();
    let replayed_receipt = apply_retirement(&mut repository, &plan)?;
    ensure!(
        replayed_receipt == applied_receipt,
        "idempotent SQLite apply replay changed the receipt"
    );
    phases.insert("apply_replay".to_owned(), elapsed_millis(phase_started));

    let phase_started = Instant::now();
    let query = RetrievalQueryV1 {
        contract_version: "retrieval-query-v1".to_owned(),
        memory_space_id: "synthetic-space-v1".to_owned(),
        repository_id: Some("synthetic-repository".to_owned()),
        memory_space_wide: false,
        as_of_us: retirement_as_of_us,
        terms: vec!["build-proof".to_owned()],
        topic_keys: BTreeSet::from(["build-proof".to_owned()]),
        entity_ids: BTreeSet::from(["entity:naome-memory".to_owned()]),
        k: Some(1),
        spontaneous: Some(SpontaneousRecallRequestV1 {
            seed: Seed32::new([0xa5; 32]),
            budget: 1,
        }),
    };
    let candidates = repository.retrieval_candidates(&query, &policy)?;
    let retrieval = rank_retrieval(&query, &candidates, &policy)?;
    ensure!(
        retrieval.hits.iter().any(|hit| {
            hit.recall_reason == RecallReasonV1::QueryRanked
                && hit.generality == GeneralityV1::Semantic
        }),
        "scale retrieval produced no ordinary semantic hit"
    );
    let spontaneous_hit_count = retrieval
        .spontaneous
        .as_ref()
        .map_or(0, |result| result.hits.len());
    ensure!(
        retrieval.spontaneous.as_ref().is_some_and(|result| {
            result.hits.len() == 1
                && result.hits[0].recall_reason == RecallReasonV1::Spontaneous
                && result.hits[0].generality == GeneralityV1::SingleEpisode
        }),
        "scale retrieval produced no explicit single-episode flashback"
    );
    let retrieval_digest = Digest32::hash_prefixed(
        b"naome-memory:scale-retrieval:v1\0",
        &retrieval.canonical_bytes()?,
    );
    phases.insert("retrieval".to_owned(), elapsed_millis(phase_started));

    let phase_started = Instant::now();
    repository.check_integrity(false)?;
    phases.insert("integrity".to_owned(), elapsed_millis(phase_started));
    drop(repository);

    let observed_disk_bytes = directory_bytes(temporary.path()).ok();
    let observed_max_rss_bytes = rss_sampler.finish();
    let observed_wall_millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let source_commit_at_publication = current_clean_source_commit();
    let source_commit = bound_clean_source_commit(
        source_commit_at_start.as_deref(),
        source_commit_at_publication.as_deref(),
    );
    let source_binding_complete = source_commit.is_some();
    let source_commit = source_commit.unwrap_or_else(|| "uncommitted".to_owned());
    logical.sizes.sort_unstable();
    let p50 = percentile(&logical.sizes, 50)?;
    let p95 = percentile(&logical.sizes, 95)?;
    let maximum = *logical.sizes.last().context("scale size set is empty")?;
    let distribution_matches = p50 == 16 * 1024 && p95 == 64 * 1024 && maximum == 256 * 1024;
    let evidence_status = classify_scale_evidence(
        atoms,
        distribution_matches,
        observed_wall_millis,
        observed_max_rss_bytes,
        observed_disk_bytes,
        source_binding_complete,
    );
    let plan_digest = plan.digest()?;
    let logical_closure = ScaleLogicalClosureV1 {
        contract_version: "scale-logical-closure-v1",
        atom_count: atoms,
        p50_body_bytes: p50,
        p95_body_bytes: p95,
        maximum_body_bytes: maximum,
        logical_body_bytes: logical.logical_body_bytes,
        atom_stream_digest: &logical.atom_stream_digest,
        input_set_digest: plan.input_set_digest,
        plan_digest,
        receipt_digest: applied_receipt.receipt_id,
        retrieval_digest,
    };
    let logical_digest = Digest32::hash_prefixed(
        b"naome-memory:scale-logical-closure:v1\0",
        &logical_closure.canonical_bytes()?,
    );
    let mut report = ScaleReceiptV1 {
        contract_version: "scale-receipt-v1".to_owned(),
        source_commit,
        status: evidence_status,
        atom_count: atoms,
        p50_body_bytes: p50,
        p95_body_bytes: p95,
        maximum_body_bytes: maximum,
        logical_body_bytes: logical.logical_body_bytes,
        atom_stream_digest: logical.atom_stream_digest,
        logical_digest: logical_digest.to_hex(),
        input_set_digest: plan.input_set_digest.to_hex(),
        plan_digest: plan_digest.to_hex(),
        receipt_digest: applied_receipt.receipt_id.to_hex(),
        retrieval_digest: retrieval_digest.to_hex(),
        semantic_atom_count: plan.semantic_atoms.len(),
        episodic_winner_count: plan.episodic_winners.len(),
        normal_hit_count: retrieval.hits.len(),
        spontaneous_hit_count,
        plan_replay_verified: true,
        receipt_verified: true,
        apply_idempotent: true,
        observed_phase_millis: phases,
        observed_wall_millis,
        observed_max_rss_bytes,
        observed_disk_bytes,
        note: format!(
            "Phase timing, sampled peak RSS, persisted temporary-directory bytes, and source-binding stability are observational fields excluded from logical_digest. Missing required 100,000-atom resource measurements or source binding make evidence inconclusive; observed threshold breaches make it failed. source_binding_complete={source_binding_complete}."
        ),
        evidence_digest: String::new(),
    };
    report.evidence_digest = report.recompute_evidence_digest()?;
    publish_and_verify_scale_evidence(proof_directory, &applied_receipt, &report, atoms)
}

fn classify_scale_evidence(
    atoms: usize,
    distribution_matches: bool,
    observed_wall_millis: u64,
    observed_max_rss_bytes: Option<u64>,
    observed_disk_bytes: Option<u64>,
    source_binding_complete: bool,
) -> EvidenceStatusV1 {
    let known_threshold_breach = !distribution_matches
        || observed_wall_millis > SCALE_MAX_WALL_MILLIS
        || (atoms == DEEP_SCALE_ATOM_COUNT
            && (observed_max_rss_bytes.is_some_and(|bytes| bytes > SCALE_MAX_RSS_BYTES)
                || observed_disk_bytes.is_some_and(|bytes| bytes > SCALE_MAX_DISK_BYTES)));
    if known_threshold_breach {
        return EvidenceStatusV1::Failed;
    }
    if atoms == DEEP_SCALE_ATOM_COUNT
        && (observed_max_rss_bytes.is_none()
            || observed_disk_bytes.is_none()
            || !source_binding_complete)
    {
        return EvidenceStatusV1::Inconclusive;
    }
    EvidenceStatusV1::Passed
}

fn clear_scale_evidence(proof_directory: &Path) -> Result<()> {
    for file_name in [SCALE_CORE_RECEIPT_FILE, SCALE_RECEIPT_FILE] {
        let path = proof_directory.join(file_name);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("could not remove stale {}", path.display()));
            }
        }
    }
    Ok(())
}

fn publish_and_verify_scale_evidence(
    proof_directory: &Path,
    core_receipt: &ProofReceiptV1,
    report: &ScaleReceiptV1,
    expected_atoms: usize,
) -> Result<()> {
    verify_scale_core_binding(core_receipt, report)?;
    verify_scale_receipt_structure(report, expected_atoms)?;
    write_json(&proof_directory.join(SCALE_CORE_RECEIPT_FILE), core_receipt)?;
    write_json(&proof_directory.join(SCALE_RECEIPT_FILE), report)?;
    println!("{}", serde_json::to_string(report)?);
    verify_scale_receipt(report, expected_atoms)
}

fn verify_scale_core_binding(core_receipt: &ProofReceiptV1, report: &ScaleReceiptV1) -> Result<()> {
    ensure!(
        core_receipt.contract_version == "proof-receipt-v1"
            && core_receipt.receipt_id == core_receipt.recompute_receipt_id()?,
        "scale core retirement receipt id does not close"
    );
    let report_receipt_digest = Digest32::from_str(&report.receipt_digest)?;
    let report_input_set_digest = Digest32::from_str(&report.input_set_digest)?;
    let report_plan_digest = Digest32::from_str(&report.plan_digest)?;
    ensure!(
        report_receipt_digest == core_receipt.receipt_id
            && report_input_set_digest == core_receipt.input_set_digest
            && report_plan_digest == core_receipt.plan_digest,
        "scale report is not bound to the attached core retirement receipt"
    );
    ensure!(
        core_receipt.closure.source_atom_ids.len() == report.atom_count
            && core_receipt.closure.semantic_atom_ids.len() == report.semantic_atom_count
            && core_receipt.closure.exact_episode_ids.len() == report.episodic_winner_count,
        "scale report counts do not match the attached core retirement closure"
    );
    let required_invariants = BTreeMap::from([
        (
            "budgets_structurally_bounded".to_owned(),
            InvariantStatusV1::Verified,
        ),
        (
            "plan_structure_verified".to_owned(),
            InvariantStatusV1::Verified,
        ),
        (
            "repository_outcome_closure_verified".to_owned(),
            InvariantStatusV1::Verified,
        ),
    ]);
    ensure!(
        core_receipt.overall_status == OverallProofStatusV1::Passed
            && core_receipt.invariants == required_invariants,
        "scale core retirement receipt does not claim the required verified invariants"
    );
    Ok(())
}

fn persist_scale_atoms(
    repository: &mut SqliteRepository,
    atoms: usize,
    policy: &PolicyV1,
) -> Result<ScaleLogicalV1> {
    let mut sizes = Vec::with_capacity(atoms);
    let mut logical_body_bytes = 0_u64;
    let mut stream = Sha256::new();
    stream.update(b"naome-memory:scale-atom-stream:v1\0");
    let mut batch = Vec::with_capacity(MAX_ATOM_INSERT_BATCH);
    for index in 0..atoms {
        let mut world = generate_world(u64::try_from(index).unwrap_or(u64::MAX), 1)?;
        let mut body = world
            .atoms
            .pop()
            .context("synthetic scale world did not contain its atom")?;
        body.artifacts.clear();
        for observation in &mut body.observations {
            observation.artifact_digest = None;
        }
        body.retention_permission = if index < 100 {
            RetentionPermissionV1::SemanticAndExactAllowed
        } else {
            RetentionPermissionV1::TransientOnly
        };
        body.interval.started_at_us = 90;
        body.interval.ended_at_us = 95;
        body.interval.recorded_at_us = 100;
        for observation in &mut body.observations {
            observation.at_us = 92;
        }
        body.internal_state_before
            .insert("scale_payload".to_owned(), String::new());
        let target = scale_target(index, atoms);
        let fixed_bytes = u64::try_from(body.canonical_bytes()?.len())?;
        ensure!(
            fixed_bytes <= target,
            "scale atom framing exceeds target size"
        );
        let padding = usize::try_from(target - fixed_bytes)?;
        body.internal_state_before
            .insert("scale_payload".to_owned(), "x".repeat(padding));
        let canonical_body = body.canonical_bytes()?;
        let actual = u64::try_from(canonical_body.len())?;
        ensure!(
            actual == target,
            "scale atom did not reach its exact target size"
        );
        let atom_id = Digest32::hash_prefixed(ATOM_HASH_DOMAIN, &canonical_body);
        stream.update(atom_id.as_bytes());
        stream.update(actual.to_be_bytes());
        logical_body_bytes = logical_body_bytes
            .checked_add(actual)
            .context("scale byte total overflow")?;
        let state = AtomStateV1 {
            retention_tier: RetentionTierV1::ShortTerm,
            content_status: ContentStatusV1::Present,
            expires_at_us: Some(100_u64.saturating_add(policy.stm_horizon_us)),
            superseded_by: None,
            feedback_positive: 0,
            feedback_negative: 0,
            retrieval_count: 0,
            retirement_run_id: None,
        };
        batch.push((body, state));
        if batch.len() == MAX_ATOM_INSERT_BATCH {
            persist_scale_batch(repository, &batch)?;
            batch.clear();
        }
        sizes.push(actual);
    }
    if !batch.is_empty() {
        persist_scale_batch(repository, &batch)?;
    }
    Ok(ScaleLogicalV1 {
        sizes,
        logical_body_bytes,
        atom_stream_digest: Digest32(stream.finalize().into()).to_hex(),
    })
}

fn persist_scale_batch(
    repository: &mut SqliteRepository,
    batch: &[(MemoryAtomBodyV1, AtomStateV1)],
) -> Result<()> {
    let outcome = repository.put_core_atoms_batch(batch)?;
    ensure!(
        outcome.requested == batch.len()
            && outcome.inserted == batch.len()
            && outcome.already_present == 0,
        "scale batch was not inserted exactly once"
    );
    Ok(())
}

fn mutation() -> Result<()> {
    let proof_dir = workspace_root().join(PROOF_DIR);
    fs::create_dir_all(&proof_dir)?;
    clear_mutation_evidence(&proof_dir)?;
    let source_commit_at_start = current_clean_source_commit();
    let toolchain = "1.95.0";
    let tool_version = observed_cargo_subcommand_version(toolchain, "mutants")
        .unwrap_or_else(|_| "unavailable".to_owned());
    let command = mutation_target_command(toolchain);
    let target = capture_tool_target(
        &proof_dir,
        MUTATION_LOG_STEM,
        "naome-memory-core",
        command,
        None,
    )?;
    let source_commit_at_publication = current_clean_source_commit();
    let source_commit = bound_clean_source_commit(
        source_commit_at_start.as_deref(),
        source_commit_at_publication.as_deref(),
    );
    let evidence_status = classify_tool_evidence(
        std::slice::from_ref(&target),
        1,
        tool_version.contains(MUTATION_TOOL_VERSION),
        source_commit.is_some(),
    );
    let source_commit = source_commit.unwrap_or_else(|| "uncommitted".to_owned());
    let mut receipt = ToolReceiptV1 {
        contract_version: "mutation-receipt-v1".to_owned(),
        tool: "cargo-mutants".to_owned(),
        tool_version,
        toolchain: toolchain.to_owned(),
        source_commit,
        status: evidence_status,
        targets: vec![target],
        note: "Mutation evidence covers the decision core and receipt verifier.".to_owned(),
        receipt_digest: String::new(),
    };
    receipt.receipt_digest = receipt.recompute_digest()?;
    let output = proof_dir.join(MUTATION_RECEIPT_FILE);
    write_json(&output, &receipt)?;
    println!("{}", serde_json::to_string(&receipt)?);
    ensure!(
        evidence_status == EvidenceStatusV1::Passed,
        "mutation campaign did not pass"
    );
    Ok(())
}

fn mutation_target_command(toolchain: &str) -> Vec<String> {
    vec![
        "cargo".to_owned(),
        format!("+{toolchain}"),
        "mutants".to_owned(),
        "--package".to_owned(),
        "naome-memory-core".to_owned(),
        "--timeout".to_owned(),
        "120".to_owned(),
        "--output".to_owned(),
        format!("{PROOF_DIR}/{MUTATION_OUTPUT_DIR}"),
    ]
}

fn fuzz(seconds: u64) -> Result<()> {
    ensure!(seconds > 0, "fuzz duration must be greater than zero");
    let fuzz_manifest = workspace_root().join("fuzz/Cargo.toml");
    let fuzz_artifacts = workspace_root().join("fuzz/artifacts");
    let proof_dir = workspace_root().join(PROOF_DIR);
    fs::create_dir_all(&proof_dir)?;
    clear_fuzz_evidence(&proof_dir, &fuzz_artifacts)?;
    let source_commit_at_start = current_clean_source_commit();
    let tool_version = observed_cargo_subcommand_version(FUZZ_NIGHTLY, "fuzz")
        .unwrap_or_else(|_| "unavailable".to_owned());
    let mut targets = Vec::with_capacity(FUZZ_TARGETS.len());
    if fuzz_manifest.is_file() {
        for (target, log_stem) in FUZZ_TARGETS {
            let command = fuzz_target_command(target, seconds);
            let mut evidence =
                capture_tool_target(&proof_dir, log_stem, target, command, Some(seconds))?;
            evidence.artifacts = preserve_fuzz_artifacts(
                &fuzz_artifacts,
                &proof_dir.join(FUZZ_ARTIFACT_DIR),
                target,
            )?;
            targets.push(evidence);
        }
    }
    let source_commit_at_publication = current_clean_source_commit();
    let source_commit = bound_clean_source_commit(
        source_commit_at_start.as_deref(),
        source_commit_at_publication.as_deref(),
    );
    let status = classify_tool_evidence(
        &targets,
        FUZZ_TARGETS.len(),
        tool_version.contains(FUZZ_TOOL_VERSION),
        source_commit.is_some(),
    );
    let source_commit = source_commit.unwrap_or_else(|| "uncommitted".to_owned());
    let mut receipt = ToolReceiptV1 {
        contract_version: "fuzz-receipt-v1".to_owned(),
        tool: "cargo-fuzz/libFuzzer".to_owned(),
        tool_version,
        toolchain: FUZZ_NIGHTLY.to_owned(),
        source_commit,
        status,
        targets,
        note: format!("Each of four fuzz targets requested {seconds} seconds."),
        receipt_digest: String::new(),
    };
    receipt.receipt_digest = receipt.recompute_digest()?;
    let output = proof_dir.join(FUZZ_RECEIPT_FILE);
    write_json(&output, &receipt)?;
    println!("{}", serde_json::to_string(&receipt)?);
    ensure!(
        status == EvidenceStatusV1::Passed,
        "fuzz campaign did not pass"
    );
    Ok(())
}

fn clear_mutation_evidence(proof_dir: &Path) -> Result<()> {
    let logs = proof_dir.join("tool-logs");
    for path in [
        proof_dir.join(MUTATION_RECEIPT_FILE),
        logs.join(format!("{MUTATION_LOG_STEM}.stdout.log")),
        logs.join(format!("{MUTATION_LOG_STEM}.stderr.log")),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("could not remove stale {}", path.display()));
            }
        }
    }
    let output = proof_dir.join(MUTATION_OUTPUT_DIR);
    match fs::remove_dir_all(&output) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("could not remove stale {}", output.display()));
        }
    }
    Ok(())
}

fn fuzz_target_command(target: &str, seconds: u64) -> Vec<String> {
    vec![
        "cargo".to_owned(),
        format!("+{FUZZ_NIGHTLY}"),
        "fuzz".to_owned(),
        "run".to_owned(),
        target.to_owned(),
        "--".to_owned(),
        format!("-max_total_time={seconds}"),
    ]
}

fn clear_fuzz_evidence(proof_dir: &Path, fuzz_artifacts: &Path) -> Result<()> {
    let mut paths = vec![proof_dir.join(FUZZ_RECEIPT_FILE)];
    let logs = proof_dir.join("tool-logs");
    for (_, log_stem) in FUZZ_TARGETS {
        paths.push(logs.join(format!("{log_stem}.stdout.log")));
        paths.push(logs.join(format!("{log_stem}.stderr.log")));
    }
    for path in paths {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("could not remove stale {}", path.display()));
            }
        }
    }
    for directory in [proof_dir.join(FUZZ_ARTIFACT_DIR), fuzz_artifacts.to_owned()] {
        match fs::remove_dir_all(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("could not remove stale {}", directory.display()));
            }
        }
    }
    Ok(())
}

fn preserve_fuzz_artifacts(
    source_root: &Path,
    proof_root: &Path,
    target: &str,
) -> Result<Vec<ToolArtifactEvidenceV1>> {
    let source = source_root.join(target);
    if !source.exists() {
        return Ok(Vec::new());
    }
    let mut entries = fs::read_dir(&source)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_unstable_by_key(std::fs::DirEntry::file_name);
    ensure!(
        entries.len() <= FUZZ_MAX_ARTIFACT_COUNT,
        "fuzz target produced too many crash artifacts"
    );
    let destination = proof_root.join(target);
    let mut artifacts = Vec::with_capacity(entries.len());
    for entry in entries {
        ensure!(
            entry.file_type()?.is_file(),
            "fuzz crash artifact is not a regular file"
        );
        let file_name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("fuzz crash artifact name is not UTF-8"))?;
        ensure!(
            safe_artifact_file_name(&file_name),
            "unsafe fuzz crash artifact name"
        );
        let bytes = fs::read(entry.path())?;
        let byte_len = u64::try_from(bytes.len())?;
        ensure!(
            byte_len <= FUZZ_MAX_ARTIFACT_BYTES,
            "fuzz crash artifact exceeds the bounded evidence limit"
        );
        fs::create_dir_all(&destination)?;
        fs::write(destination.join(&file_name), &bytes)?;
        artifacts.push(ToolArtifactEvidenceV1 {
            relative_path: format!("{FUZZ_ARTIFACT_DIR}/{target}/{file_name}"),
            byte_len,
            sha256: sha256_hex(&bytes),
        });
    }
    Ok(artifacts)
}

fn safe_artifact_file_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn classify_tool_evidence(
    targets: &[ToolTargetEvidenceV1],
    expected_target_count: usize,
    supported_tool_version: bool,
    source_binding_complete: bool,
) -> EvidenceStatusV1 {
    if targets
        .iter()
        .any(|target| target.exit_code.is_some_and(|code| code != 0))
    {
        return EvidenceStatusV1::Failed;
    }
    if targets.len() == expected_target_count
        && targets.iter().all(|target| target.exit_code == Some(0))
        && supported_tool_version
        && source_binding_complete
    {
        return EvidenceStatusV1::Passed;
    }
    EvidenceStatusV1::Inconclusive
}

impl ToolReceiptV1 {
    fn recompute_digest(&self) -> Result<String> {
        let closure = ToolReceiptClosureV1 {
            contract_version: &self.contract_version,
            tool: &self.tool,
            tool_version: &self.tool_version,
            toolchain: &self.toolchain,
            source_commit: &self.source_commit,
            status: self.status,
            targets: &self.targets,
            note: &self.note,
        };
        let encoded = serde_json::to_vec(&closure)?;
        Ok(Digest32::hash_prefixed(TOOL_RECEIPT_HASH_DOMAIN, &encoded).to_hex())
    }
}

fn capture_tool_target(
    proof_dir: &Path,
    log_stem: &str,
    target: &str,
    command: Vec<String>,
    requested_duration_seconds: Option<u64>,
) -> Result<ToolTargetEvidenceV1> {
    ensure!(safe_log_stem(log_stem), "unsafe tool log stem");
    let started = Instant::now();
    let (exit_code, stdout, stderr) = match command.split_first() {
        Some((program, arguments)) => match ProcessCommand::new(program)
            .current_dir(workspace_root())
            .args(arguments)
            .output()
        {
            Ok(output) => (output.status.code(), output.stdout, output.stderr),
            Err(error) => (None, Vec::new(), error.to_string().into_bytes()),
        },
        None => (None, Vec::new(), b"empty command".to_vec()),
    };
    let observed_duration_millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let logs = proof_dir.join("tool-logs");
    fs::create_dir_all(&logs)?;
    fs::write(logs.join(format!("{log_stem}.stdout.log")), &stdout)?;
    fs::write(logs.join(format!("{log_stem}.stderr.log")), &stderr)?;
    Ok(ToolTargetEvidenceV1 {
        target: target.to_owned(),
        command,
        requested_duration_seconds,
        observed_duration_millis,
        exit_code,
        log_stem: log_stem.to_owned(),
        stdout_digest: sha256_hex(&stdout),
        stderr_digest: sha256_hex(&stderr),
        artifacts: Vec::new(),
    })
}

fn observed_cargo_subcommand_version(toolchain: &str, subcommand: &str) -> Result<String> {
    let output = ProcessCommand::new("cargo")
        .current_dir(workspace_root())
        .args([
            format!("+{toolchain}"),
            subcommand.to_owned(),
            "--version".to_owned(),
        ])
        .output()?;
    ensure!(
        output.status.success(),
        "could not query {subcommand} version"
    );
    let mut bytes = output.stdout;
    bytes.extend_from_slice(&output.stderr);
    Ok(String::from_utf8(bytes)?.trim().to_owned())
}

fn current_source_commit() -> Result<String> {
    let output = ProcessCommand::new("git")
        .current_dir(workspace_root())
        .args(["rev-parse", "--verify", "HEAD"])
        .output()?;
    ensure!(output.status.success(), "could not resolve source commit");
    let commit = String::from_utf8(output.stdout)?.trim().to_owned();
    ensure!(
        valid_git_commit(&commit),
        "source commit is not a full Git object ID"
    );
    Ok(commit)
}

fn current_clean_source_commit() -> Option<String> {
    let commit = current_source_commit().ok()?;
    let status = ProcessCommand::new("git")
        .current_dir(workspace_root())
        .args(["status", "--porcelain", "--untracked-files=normal"])
        .output()
        .ok()?;
    (status.status.success() && status.stdout.is_empty()).then_some(commit)
}

fn bound_clean_source_commit(start: Option<&str>, publication: Option<&str>) -> Option<String> {
    match (start, publication) {
        (Some(start), Some(publication)) if start == publication && valid_git_commit(start) => {
            Some(start.to_owned())
        }
        _ => None,
    }
}

fn valid_git_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn safe_log_stem(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn sha256_hex(bytes: &[u8]) -> String {
    Digest32(Sha256::digest(bytes).into()).to_hex()
}

struct ToolReceiptExpectationV1<'a> {
    contract: &'a str,
    tool: &'a str,
    version: &'a str,
    toolchain: &'a str,
    target_count: usize,
}

fn verify_tool_receipt_common(
    receipt: &ToolReceiptV1,
    proof_dir: &Path,
    expected: &ToolReceiptExpectationV1<'_>,
    expected_commit: &str,
) -> Result<()> {
    let expected_status =
        classify_tool_evidence(&receipt.targets, expected.target_count, true, true);
    ensure!(
        receipt.contract_version == expected.contract
            && receipt.tool == expected.tool
            && receipt.tool_version.contains(expected.version)
            && receipt.toolchain == expected.toolchain
            && receipt.source_commit == expected_commit
            && receipt.status == expected_status
            && receipt.receipt_digest == receipt.recompute_digest()?,
        "tool receipt metadata, source, status, or digest does not close"
    );
    for target in &receipt.targets {
        ensure!(
            safe_log_stem(&target.log_stem),
            "tool target log path is outside the bounded contract"
        );
        let logs = proof_dir.join("tool-logs");
        let stdout = fs::read(logs.join(format!("{}.stdout.log", target.log_stem)))?;
        let stderr = fs::read(logs.join(format!("{}.stderr.log", target.log_stem)))?;
        ensure!(
            !stdout.is_empty() || !stderr.is_empty(),
            "tool target retained no process output"
        );
        ensure!(
            target.stdout_digest == sha256_hex(&stdout)
                && target.stderr_digest == sha256_hex(&stderr),
            "tool target output digest does not match its retained log"
        );
        ensure!(
            target.artifacts.len() <= FUZZ_MAX_ARTIFACT_COUNT,
            "tool target retained too many bounded artifacts"
        );
        for artifact in &target.artifacts {
            ensure!(
                safe_proof_relative_path(&artifact.relative_path)
                    && artifact.byte_len <= FUZZ_MAX_ARTIFACT_BYTES,
                "tool target artifact path or size is outside the bounded contract"
            );
            let artifact_path = proof_dir.join(&artifact.relative_path);
            let metadata = fs::symlink_metadata(&artifact_path)?;
            ensure!(
                metadata.file_type().is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.len() == artifact.byte_len
                    && metadata.len() <= FUZZ_MAX_ARTIFACT_BYTES,
                "tool target artifact is not a bounded self-contained file"
            );
            let bytes = fs::read(&artifact_path)?;
            ensure!(
                artifact.byte_len == u64::try_from(bytes.len())?
                    && artifact.sha256 == sha256_hex(&bytes),
                "tool target artifact digest or size does not match retained evidence"
            );
        }
    }
    Ok(())
}

fn safe_proof_relative_path(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && !value.contains('\\')
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn retained_fuzz_artifact_paths(proof_dir: &Path) -> Result<BTreeSet<String>> {
    let root = proof_dir.join(FUZZ_ARTIFACT_DIR);
    let root_metadata = match fs::symlink_metadata(&root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BTreeSet::new());
        }
        Err(error) => return Err(error.into()),
    };
    ensure!(
        root_metadata.file_type().is_dir() && !root_metadata.file_type().is_symlink(),
        "fuzz artifact root is not a self-contained directory"
    );
    let expected_targets = FUZZ_TARGETS
        .iter()
        .map(|(target, _)| *target)
        .collect::<BTreeSet<_>>();
    let mut retained = BTreeSet::new();
    for target_entry in fs::read_dir(&root)? {
        let target_entry = target_entry?;
        ensure!(
            target_entry.file_type()?.is_dir(),
            "fuzz artifact root contains a non-directory entry"
        );
        let target = target_entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("fuzz artifact target is not UTF-8"))?;
        ensure!(
            expected_targets.contains(target.as_str()),
            "fuzz artifact belongs to an unknown target"
        );
        let mut target_count = 0_usize;
        for artifact_entry in fs::read_dir(target_entry.path())? {
            let artifact_entry = artifact_entry?;
            ensure!(
                artifact_entry.file_type()?.is_file(),
                "fuzz artifact target contains a non-file entry"
            );
            let file_name = artifact_entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("fuzz artifact name is not UTF-8"))?;
            ensure!(
                safe_artifact_file_name(&file_name),
                "fuzz artifact name is unsafe"
            );
            ensure!(
                artifact_entry.metadata()?.len() <= FUZZ_MAX_ARTIFACT_BYTES,
                "fuzz artifact exceeds the bounded evidence limit"
            );
            target_count = target_count
                .checked_add(1)
                .context("fuzz artifact count overflow")?;
            ensure!(
                target_count <= FUZZ_MAX_ARTIFACT_COUNT,
                "fuzz target retained too many crash artifacts"
            );
            ensure!(
                retained.insert(format!("{FUZZ_ARTIFACT_DIR}/{target}/{file_name}")),
                "duplicate fuzz artifact path"
            );
        }
    }
    Ok(retained)
}

fn verify_mutation_receipt_evidence(
    receipt: &ToolReceiptV1,
    proof_dir: &Path,
    expected_commit: &str,
) -> Result<()> {
    verify_tool_receipt_common(
        receipt,
        proof_dir,
        &ToolReceiptExpectationV1 {
            contract: "mutation-receipt-v1",
            tool: "cargo-mutants",
            version: MUTATION_TOOL_VERSION,
            toolchain: "1.95.0",
            target_count: 1,
        },
        expected_commit,
    )?;
    let expected_command = vec![
        "cargo".to_owned(),
        "+1.95.0".to_owned(),
        "mutants".to_owned(),
        "--package".to_owned(),
        "naome-memory-core".to_owned(),
        "--timeout".to_owned(),
        "120".to_owned(),
        "--output".to_owned(),
        "target/proofs/mutation-tool".to_owned(),
    ];
    ensure!(
        receipt.targets.len() == 1
            && receipt.targets.first().is_some_and(|target| {
                target.target == "naome-memory-core"
                    && target.log_stem == "mutation-naome-memory-core"
                    && target.requested_duration_seconds.is_none()
                    && target.command == expected_command
                    && target.artifacts.is_empty()
            }),
        "mutation receipt target or command differs from the release gate"
    );
    Ok(())
}

fn verify_fuzz_receipt_evidence(
    receipt: &ToolReceiptV1,
    proof_dir: &Path,
    expected_commit: &str,
) -> Result<()> {
    let retained_artifacts = retained_fuzz_artifact_paths(proof_dir)?;
    verify_tool_receipt_common(
        receipt,
        proof_dir,
        &ToolReceiptExpectationV1 {
            contract: "fuzz-receipt-v1",
            tool: "cargo-fuzz/libFuzzer",
            version: FUZZ_TOOL_VERSION,
            toolchain: FUZZ_NIGHTLY,
            target_count: FUZZ_TARGETS.len(),
        },
        expected_commit,
    )?;
    let expected_targets = [
        ("decoder", "fuzz-decoder"),
        ("canonical", "fuzz-canonical"),
        ("receipt_verifier", "fuzz-receipt-verifier"),
        ("sqlite_ingest", "fuzz-sqlite-ingest"),
    ];
    ensure!(
        receipt.targets.len() == expected_targets.len(),
        "fuzz receipt target count differs from the release gate"
    );
    let mut recorded_artifacts = BTreeSet::new();
    for (target, (expected, expected_log_stem)) in receipt.targets.iter().zip(expected_targets) {
        let expected_command = vec![
            "cargo".to_owned(),
            format!("+{FUZZ_NIGHTLY}"),
            "fuzz".to_owned(),
            "run".to_owned(),
            expected.to_owned(),
            "--".to_owned(),
            "-max_total_time=120".to_owned(),
        ];
        ensure!(
            target.target == expected
                && target.log_stem == expected_log_stem
                && target.requested_duration_seconds == Some(120)
                && target.command == expected_command,
            "fuzz receipt target or command differs from the release gate"
        );
        let expected_prefix = format!("{FUZZ_ARTIFACT_DIR}/{expected}/");
        for artifact in &target.artifacts {
            ensure!(
                artifact.relative_path.starts_with(&expected_prefix)
                    && artifact
                        .relative_path
                        .strip_prefix(&expected_prefix)
                        .is_some_and(safe_artifact_file_name),
                "fuzz artifact is not bound to its producing target"
            );
            ensure!(
                recorded_artifacts.insert(artifact.relative_path.clone()),
                "duplicate fuzz artifact evidence"
            );
        }
    }
    ensure!(
        recorded_artifacts == retained_artifacts,
        "fuzz artifact directory does not exactly match the receipt closure"
    );
    Ok(())
}

fn require_passed_tool_receipt(receipt: &ToolReceiptV1) -> Result<()> {
    ensure!(
        receipt.status == EvidenceStatusV1::Passed
            && receipt.targets.iter().all(|target| {
                target.exit_code == Some(0) && target.observed_duration_millis > 0
            }),
        "tool receipt is closed but did not pass its bounded campaign"
    );
    Ok(())
}

fn verify_mutation_receipt(
    receipt: &ToolReceiptV1,
    proof_dir: &Path,
    expected_commit: &str,
) -> Result<()> {
    verify_mutation_receipt_evidence(receipt, proof_dir, expected_commit)?;
    require_passed_tool_receipt(receipt)
}

fn verify_fuzz_receipt(
    receipt: &ToolReceiptV1,
    proof_dir: &Path,
    expected_commit: &str,
) -> Result<()> {
    verify_fuzz_receipt_evidence(receipt, proof_dir, expected_commit)?;
    require_passed_tool_receipt(receipt)?;
    ensure!(
        receipt
            .targets
            .iter()
            .all(|target| target.artifacts.is_empty()),
        "a passed fuzz campaign must not retain crash reproducers"
    );
    Ok(())
}

fn verify_tool_receipt_file(receipt_path: &Path, proof_dir: &Path) -> Result<()> {
    let receipt: ToolReceiptV1 = read_json(receipt_path)?;
    let expected_commit = current_clean_source_commit()
        .context("tool receipt verification requires the exact clean source commit")?;
    match receipt.contract_version.as_str() {
        "mutation-receipt-v1" => {
            verify_mutation_receipt_evidence(&receipt, proof_dir, &expected_commit)?;
        }
        "fuzz-receipt-v1" => {
            verify_fuzz_receipt_evidence(&receipt, proof_dir, &expected_commit)?;
        }
        _ => anyhow::bail!("unknown tool receipt contract version"),
    }
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "contract_version": "tool-receipt-verification-v1",
            "receipt_contract_version": receipt.contract_version,
            "source_commit": expected_commit,
            "evidence_status": receipt.status,
            "verification_status": "verified",
            "receipt_digest": receipt.receipt_digest,
        }))?
    );
    Ok(())
}

fn release_preflight(version: &str) -> Result<()> {
    ensure!(!version.is_empty(), "version must not be empty");
    let root = workspace_root();
    let cargo: toml::Value = toml::from_str(&fs::read_to_string(root.join("Cargo.toml"))?)?;
    let workspace_version = cargo
        .get("workspace")
        .and_then(|value| value.get("package"))
        .and_then(|value| value.get("version"))
        .and_then(toml::Value::as_str)
        .context("workspace.package.version is missing")?;
    ensure!(
        workspace_version == version,
        "tag version does not match Cargo workspace version"
    );

    let git_status = ProcessCommand::new("git")
        .current_dir(&root)
        .args(["status", "--porcelain", "--untracked-files=normal"])
        .output()
        .context("could not inspect Git worktree")?;
    ensure!(git_status.status.success(), "git status failed");
    ensure!(
        git_status.stdout.is_empty(),
        "release worktree is not clean"
    );

    let proof_dir = root.join(PROOF_DIR);
    let receipt_path = proof_dir.join("release-receipt.json");
    let receipt: LabProofReceiptV1 = read_json(&receipt_path)?;
    let ledger: DecisionLedgerV1 = read_json(&proof_dir.join("decision-ledger.json"))?;
    require_manifest_copy(&proof_dir.join("dataset-manifest-v1.json"))?;
    require_crash_evidence_copy(&proof_dir, &ledger)?;
    let bundle = LabProofBundleV1 {
        receipt,
        decision_ledger: ledger,
    };
    verify_lab_bundle(&bundle)?;
    ensure!(
        bundle.receipt.profile == ProofProfileV1::Deep,
        "release receipt is not deep profile"
    );
    ensure!(
        bundle.receipt.overall_status == OverallStatusV1::Passed,
        "release proof is not passed"
    );
    ensure_required_claims(&bundle.receipt)?;

    let scale: ScaleReceiptV1 = read_json(&proof_dir.join("scale-receipt.json"))?;
    let scale_core: ProofReceiptV1 =
        read_json(&proof_dir.join("scale-core-retirement-receipt.json"))?;
    let mutation: ToolReceiptV1 = read_json(&proof_dir.join("mutation-receipt.json"))?;
    let fuzz: ToolReceiptV1 = read_json(&proof_dir.join("fuzz-receipt.json"))?;
    verify_scale_receipt(&scale, 100_000)?;
    verify_scale_core_binding(&scale_core, &scale)?;
    let source_commit = current_source_commit()?;
    ensure!(
        scale.source_commit == source_commit,
        "scale evidence belongs to a different source commit"
    );
    verify_mutation_receipt(&mutation, &proof_dir, &source_commit)?;
    verify_fuzz_receipt(&fuzz, &proof_dir, &source_commit)?;
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "contract_version": "release-preflight-v1",
            "version": version,
            "receipt_digest": bundle.receipt.result_digest,
            "status": "passed",
        }))?
    );
    Ok(())
}

fn ensure_required_claims(receipt: &LabProofReceiptV1) -> Result<()> {
    let required_invariants = BTreeSet::from([
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
    ]);
    let observed = receipt
        .invariant_statuses
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    ensure!(
        observed == required_invariants,
        "release invariant set is incomplete or unexpected"
    );
    ensure!(
        receipt
            .invariant_statuses
            .values()
            .all(|status| *status == ClaimStatusV1::Verified),
        "not every release invariant is verified"
    );
    let required_hypotheses = BTreeSet::from([
        "episodic_rare_recovery",
        "ordinary_utility_preserved",
        "semantic_utility",
    ]);
    let observed = receipt
        .hypothesis_statuses
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    ensure!(
        observed == required_hypotheses,
        "release hypothesis set is incomplete or unexpected"
    );
    ensure!(
        receipt
            .hypothesis_statuses
            .values()
            .all(|status| *status == HypothesisStatusV1::Supported),
        "not every release hypothesis is supported"
    );
    Ok(())
}

fn verify_scale_receipt(receipt: &ScaleReceiptV1, expected_atoms: usize) -> Result<()> {
    verify_scale_receipt_structure(receipt, expected_atoms)?;
    ensure!(
        receipt.status == EvidenceStatusV1::Passed,
        "scale evidence status {:?} is not passed (source={}, wall={}ms, rss={:?}, disk={:?})",
        receipt.status,
        receipt.source_commit,
        receipt.observed_wall_millis,
        receipt.observed_max_rss_bytes,
        receipt.observed_disk_bytes
    );
    Ok(())
}

fn verify_scale_receipt_structure(receipt: &ScaleReceiptV1, expected_atoms: usize) -> Result<()> {
    ensure!(
        receipt.contract_version == "scale-receipt-v1"
            && receipt.evidence_digest == receipt.recompute_evidence_digest()?,
        "unknown or tampered scale receipt"
    );
    ensure!(
        receipt.atom_count == expected_atoms,
        "scale atom count {} differs from expected {expected_atoms}",
        receipt.atom_count
    );
    ensure!(
        (100..=DEEP_SCALE_ATOM_COUNT).contains(&receipt.atom_count),
        "scale atom count is outside the supported proof envelope"
    );
    ensure!(
        receipt.plan_replay_verified
            && receipt.receipt_verified
            && receipt.apply_idempotent
            && receipt.semantic_atom_count > 0
            && receipt.episodic_winner_count > 0
            && receipt.normal_hit_count > 0
            && receipt.spontaneous_hit_count > 0,
        "scale full-path closure is incomplete"
    );
    let _atom_stream_digest = Digest32::from_str(&receipt.atom_stream_digest)?;
    let expected_logical_digest = recompute_scale_logical_digest(receipt)?;
    ensure!(
        Digest32::from_str(&receipt.logical_digest)? == expected_logical_digest,
        "scale logical digest does not close"
    );
    let distribution_matches = receipt.p50_body_bytes == 16 * 1024
        && receipt.p95_body_bytes == 64 * 1024
        && receipt.maximum_body_bytes == 256 * 1024;
    let expected_status = classify_scale_evidence(
        receipt.atom_count,
        distribution_matches,
        receipt.observed_wall_millis,
        receipt.observed_max_rss_bytes,
        receipt.observed_disk_bytes,
        valid_git_commit(&receipt.source_commit),
    );
    ensure!(
        receipt.status == expected_status,
        "scale evidence status {:?} is inconsistent with recomputed status {:?}",
        receipt.status,
        expected_status
    );
    Ok(())
}

fn recompute_scale_logical_digest(receipt: &ScaleReceiptV1) -> Result<Digest32> {
    let input_set_digest = Digest32::from_str(&receipt.input_set_digest)?;
    let plan_digest = Digest32::from_str(&receipt.plan_digest)?;
    let proof_receipt_digest = Digest32::from_str(&receipt.receipt_digest)?;
    let retrieval_digest = Digest32::from_str(&receipt.retrieval_digest)?;
    let authority = ScaleLogicalClosureV1 {
        contract_version: "scale-logical-closure-v1",
        atom_count: receipt.atom_count,
        p50_body_bytes: receipt.p50_body_bytes,
        p95_body_bytes: receipt.p95_body_bytes,
        maximum_body_bytes: receipt.maximum_body_bytes,
        logical_body_bytes: receipt.logical_body_bytes,
        atom_stream_digest: &receipt.atom_stream_digest,
        input_set_digest,
        plan_digest,
        receipt_digest: proof_receipt_digest,
        retrieval_digest,
    };
    Ok(Digest32::hash_prefixed(
        b"naome-memory:scale-logical-closure:v1\0",
        &authority.canonical_bytes()?,
    ))
}

fn scale_target(index: usize, atom_count: usize) -> u64 {
    let p95_index = atom_count.saturating_sub(1).saturating_mul(95) / 100;
    if index < p95_index {
        16 * 1024
    } else if index < atom_count.saturating_sub(1) {
        64 * 1024
    } else {
        256 * 1024
    }
}

fn percentile(sorted: &[u64], percentile: usize) -> Result<u64> {
    let last = sorted
        .len()
        .checked_sub(1)
        .context("empty percentile input")?;
    let index = last.saturating_mul(percentile) / 100;
    sorted
        .get(index)
        .copied()
        .context("percentile index is outside input")
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn execute_crash_harness() -> Result<CrashCampaignEvidenceV1> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = ProcessCommand::new(cargo)
        .current_dir(workspace_root())
        .args([
            "run",
            "--package",
            "naome-memory-lab",
            "--bin",
            "naome-memory-crash-harness",
            "--locked",
        ])
        .output()
        .context("could not execute the shared child-process crash harness")?;
    ensure!(
        output.status.success(),
        "shared crash harness failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let evidence: CrashCampaignEvidenceV1 = serde_json::from_slice(&output.stdout)
        .context("shared crash harness stdout was not one typed evidence object")?;
    validate_crash_campaign_evidence(&evidence)?;
    Ok(evidence)
}

fn require_crash_evidence_copy(
    directory: &Path,
    ledger: &DecisionLedgerV1,
) -> Result<CrashCampaignEvidenceV1> {
    let evidence: CrashCampaignEvidenceV1 =
        read_json(&directory.join("crash-campaign-evidence.json"))?;
    validate_crash_campaign_evidence(&evidence)?;
    ensure!(
        ledger.crash_campaign_evidence.as_ref() == Some(&evidence),
        "crash campaign attachment does not match the decision ledger"
    );
    Ok(evidence)
}

fn copy_manifest(destination: &Path) -> Result<()> {
    let source = workspace_root().join("datasets/manifest-v1.json");
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&source, destination).with_context(|| {
        format!(
            "could not copy dataset manifest from {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

fn require_manifest_copy(path: &Path) -> Result<()> {
    let expected = fs::read(workspace_root().join("datasets/manifest-v1.json"))?;
    let observed = fs::read(path)
        .with_context(|| format!("required dataset manifest {} is missing", path.display()))?;
    ensure!(
        observed == expected,
        "dataset manifest attachment does not match committed authority"
    );
    Ok(())
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(&temporary, bytes)?;
    fs::rename(&temporary, path)?;
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_slice(&fs::read(path)?)
        .with_context(|| format!("could not decode JSON authority from {}", path.display()))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

struct RssSampler {
    stop: Arc<AtomicBool>,
    maximum: Arc<AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl RssSampler {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let maximum = Arc::new(AtomicU64::new(sample_rss_bytes().unwrap_or(0)));
        let thread_stop = Arc::clone(&stop);
        let thread_maximum = Arc::clone(&maximum);
        let thread = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                if let Some(value) = sample_rss_bytes() {
                    thread_maximum.fetch_max(value, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        Self {
            stop,
            maximum,
            thread: Some(thread),
        }
    }

    fn finish(mut self) -> Option<u64> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let maximum = self.maximum.load(Ordering::Relaxed);
        (maximum > 0).then_some(maximum)
    }
}

impl Drop for RssSampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn sample_rss_bytes() -> Option<u64> {
    if let Ok(status) = fs::read_to_string("/proc/self/status") {
        let kilobytes = status.lines().find_map(|line| {
            line.strip_prefix("VmRSS:")?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
        })?;
        return kilobytes.checked_mul(1024);
    }
    let output = ProcessCommand::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let kilobytes = String::from_utf8(output.stdout)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?;
    kilobytes.checked_mul(1024)
}

fn directory_bytes(path: &Path) -> Result<u64> {
    let mut total = 0_u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            total = total
                .checked_add(directory_bytes(&entry.path())?)
                .context("directory byte count overflow")?;
        } else if metadata.is_file() {
            total = total
                .checked_add(metadata.len())
                .context("directory byte count overflow")?;
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::{
        DEEP_SCALE_ATOM_COUNT, EvidenceStatusV1, FUZZ_ARTIFACT_DIR, FUZZ_NIGHTLY,
        FUZZ_RECEIPT_FILE, FUZZ_TARGETS, MUTATION_LOG_STEM, MUTATION_OUTPUT_DIR,
        MUTATION_RECEIPT_FILE, SCALE_CORE_RECEIPT_FILE, SCALE_MAX_DISK_BYTES, SCALE_MAX_RSS_BYTES,
        SCALE_MAX_WALL_MILLIS, SCALE_RECEIPT_FILE, ScaleReceiptV1, ToolArtifactEvidenceV1,
        ToolReceiptV1, ToolTargetEvidenceV1, bound_clean_source_commit, classify_scale_evidence,
        classify_tool_evidence, clear_fuzz_evidence, clear_mutation_evidence, clear_scale_evidence,
        fuzz_target_command, mutation_target_command, preserve_fuzz_artifacts,
        publish_and_verify_scale_evidence, read_json, recompute_scale_logical_digest,
        safe_artifact_file_name, safe_log_stem, scale_with_output, sha256_hex, verify_fuzz_receipt,
        verify_fuzz_receipt_evidence, verify_mutation_receipt, verify_scale_receipt,
        verify_scale_receipt_structure,
    };
    use anyhow::Context as _;
    use naome_memory_core::{Digest32, OverallProofStatusV1, ProofReceiptV1};

    #[test]
    fn source_binding_requires_the_same_clean_full_commit() {
        let first = "a".repeat(40);
        let second = "b".repeat(40);
        assert_eq!(
            bound_clean_source_commit(Some(first.as_str()), Some(first.as_str())),
            Some(first.clone())
        );
        assert_eq!(
            bound_clean_source_commit(Some(first.as_str()), Some(second.as_str())),
            None
        );
        assert_eq!(bound_clean_source_commit(Some(first.as_str()), None), None);
        assert_eq!(bound_clean_source_commit(None, Some(first.as_str())), None);
        assert_eq!(
            bound_clean_source_commit(Some("uncommitted"), Some("uncommitted")),
            None
        );
    }

    #[test]
    fn deep_scale_status_distinguishes_pass_failure_and_missing_evidence() {
        assert_eq!(
            classify_scale_evidence(
                DEEP_SCALE_ATOM_COUNT,
                true,
                SCALE_MAX_WALL_MILLIS,
                Some(SCALE_MAX_RSS_BYTES),
                Some(SCALE_MAX_DISK_BYTES),
                true,
            ),
            EvidenceStatusV1::Passed
        );
        assert_eq!(
            classify_scale_evidence(
                DEEP_SCALE_ATOM_COUNT,
                true,
                SCALE_MAX_WALL_MILLIS,
                None,
                Some(SCALE_MAX_DISK_BYTES),
                true,
            ),
            EvidenceStatusV1::Inconclusive
        );
        assert_eq!(
            classify_scale_evidence(
                DEEP_SCALE_ATOM_COUNT,
                true,
                SCALE_MAX_WALL_MILLIS,
                Some(SCALE_MAX_RSS_BYTES),
                None,
                true,
            ),
            EvidenceStatusV1::Inconclusive
        );

        for (wall, rss, disk, distribution_matches) in [
            (
                SCALE_MAX_WALL_MILLIS + 1,
                Some(SCALE_MAX_RSS_BYTES),
                Some(SCALE_MAX_DISK_BYTES),
                true,
            ),
            (
                SCALE_MAX_WALL_MILLIS,
                Some(SCALE_MAX_RSS_BYTES + 1),
                Some(SCALE_MAX_DISK_BYTES),
                true,
            ),
            (
                SCALE_MAX_WALL_MILLIS,
                Some(SCALE_MAX_RSS_BYTES),
                Some(SCALE_MAX_DISK_BYTES + 1),
                true,
            ),
            (
                SCALE_MAX_WALL_MILLIS,
                Some(SCALE_MAX_RSS_BYTES),
                Some(SCALE_MAX_DISK_BYTES),
                false,
            ),
            (SCALE_MAX_WALL_MILLIS + 1, None, None, true),
        ] {
            assert_eq!(
                classify_scale_evidence(
                    DEEP_SCALE_ATOM_COUNT,
                    distribution_matches,
                    wall,
                    rss,
                    disk,
                    true,
                ),
                EvidenceStatusV1::Failed
            );
        }

        assert_eq!(
            classify_scale_evidence(5_000, true, SCALE_MAX_WALL_MILLIS, None, None, false),
            EvidenceStatusV1::Passed
        );
        assert_eq!(
            classify_scale_evidence(
                DEEP_SCALE_ATOM_COUNT,
                true,
                SCALE_MAX_WALL_MILLIS,
                Some(SCALE_MAX_RSS_BYTES),
                Some(SCALE_MAX_DISK_BYTES),
                false,
            ),
            EvidenceStatusV1::Inconclusive
        );
    }

    #[test]
    fn clearing_scale_evidence_removes_both_stale_receipts_and_is_idempotent() -> anyhow::Result<()>
    {
        let output = tempfile::tempdir()?;
        std::fs::write(output.path().join(SCALE_RECEIPT_FILE), b"stale scale")?;
        std::fs::write(output.path().join(SCALE_CORE_RECEIPT_FILE), b"stale core")?;

        clear_scale_evidence(output.path())?;
        assert!(!output.path().join(SCALE_RECEIPT_FILE).exists());
        assert!(!output.path().join(SCALE_CORE_RECEIPT_FILE).exists());
        clear_scale_evidence(output.path())?;
        Ok(())
    }

    fn scale_fixture() -> anyhow::Result<(tempfile::TempDir, ScaleReceiptV1, ProofReceiptV1)> {
        let output = tempfile::tempdir()?;
        scale_with_output(100, output.path())?;
        let report: ScaleReceiptV1 = read_json(&output.path().join(SCALE_RECEIPT_FILE))?;
        let core_receipt: ProofReceiptV1 = read_json(&output.path().join(SCALE_CORE_RECEIPT_FILE))?;
        Ok((output, report, core_receipt))
    }

    #[test]
    fn representative_scale_smoke_runs_the_full_path() -> anyhow::Result<()> {
        let (_output, report, _core_receipt) = scale_fixture()?;
        assert_eq!(report.status, EvidenceStatusV1::Passed);
        assert_eq!(report.atom_count, 100);
        assert!(report.plan_replay_verified);
        assert!(report.receipt_verified);
        assert!(report.apply_idempotent);
        assert!(report.semantic_atom_count > 0);
        assert!(report.episodic_winner_count > 0);
        assert!(report.normal_hit_count > 0);
        assert!(report.spontaneous_hit_count > 0);
        let mut tampered = report.clone();
        tampered.observed_wall_millis = tampered.observed_wall_millis.saturating_add(1);
        let error = verify_scale_receipt(&tampered, 100)
            .err()
            .context("tampered scale receipt unexpectedly verified")?;
        assert!(error.to_string().contains("unknown or tampered"));

        let error = verify_scale_receipt(&report, 101)
            .err()
            .context("wrong scale atom count unexpectedly verified")?;
        assert!(error.to_string().contains("atom count"));

        let mut mislabeled = report.clone();
        mislabeled.observed_wall_millis = SCALE_MAX_WALL_MILLIS + 1;
        mislabeled.evidence_digest = mislabeled.recompute_evidence_digest()?;
        let error = verify_scale_receipt(&mislabeled, 100)
            .err()
            .context("mislabeled scale receipt unexpectedly verified")?;
        assert!(error.to_string().contains("inconsistent"));

        let mut inconclusive = report.clone();
        inconclusive.atom_count = DEEP_SCALE_ATOM_COUNT;
        inconclusive.source_commit = "uncommitted".to_owned();
        inconclusive.status = EvidenceStatusV1::Inconclusive;
        inconclusive.observed_wall_millis = SCALE_MAX_WALL_MILLIS;
        inconclusive.observed_max_rss_bytes = None;
        inconclusive.observed_disk_bytes = Some(SCALE_MAX_DISK_BYTES);
        inconclusive.logical_digest = recompute_scale_logical_digest(&inconclusive)?.to_hex();
        inconclusive.evidence_digest = inconclusive.recompute_evidence_digest()?;
        verify_scale_receipt_structure(&inconclusive, DEEP_SCALE_ATOM_COUNT)?;
        let error = verify_scale_receipt(&inconclusive, DEEP_SCALE_ATOM_COUNT)
            .err()
            .context("inconclusive scale receipt unexpectedly passed")?;
        assert!(error.to_string().contains("status Inconclusive"));
        Ok(())
    }

    #[test]
    fn rejected_scale_evidence_publishes_only_after_structural_validation() -> anyhow::Result<()> {
        let (_output, report, core_receipt) = scale_fixture()?;
        let mut failed = report.clone();
        failed.status = EvidenceStatusV1::Failed;
        failed.observed_wall_millis = SCALE_MAX_WALL_MILLIS + 1;
        failed.evidence_digest = failed.recompute_evidence_digest()?;
        verify_scale_receipt_structure(&failed, 100)?;
        let error = verify_scale_receipt(&failed, 100)
            .err()
            .context("failed scale receipt unexpectedly verified")?;
        assert!(error.to_string().contains("status Failed"));

        let failed_output = tempfile::tempdir()?;
        let error =
            publish_and_verify_scale_evidence(failed_output.path(), &core_receipt, &failed, 100)
                .err()
                .context("failed scale publication unexpectedly verified")?;
        assert!(error.to_string().contains("status Failed"));
        let published_report: ScaleReceiptV1 =
            read_json(&failed_output.path().join(SCALE_RECEIPT_FILE))?;
        let published_core: ProofReceiptV1 =
            read_json(&failed_output.path().join(SCALE_CORE_RECEIPT_FILE))?;
        assert_eq!(published_report, failed);
        assert_eq!(published_core, core_receipt);

        let mut invalid_logical = failed.clone();
        invalid_logical.logical_digest =
            Digest32::hash_prefixed(b"naome-memory:test:invalid-scale\0", b"invalid").to_hex();
        invalid_logical.evidence_digest = invalid_logical.recompute_evidence_digest()?;
        let invalid_output = tempfile::tempdir()?;
        let error = publish_and_verify_scale_evidence(
            invalid_output.path(),
            &core_receipt,
            &invalid_logical,
            100,
        )
        .err()
        .context("invalid logical scale receipt was published")?;
        assert!(error.to_string().contains("logical digest"));
        assert!(!invalid_output.path().join(SCALE_RECEIPT_FILE).exists());
        assert!(!invalid_output.path().join(SCALE_CORE_RECEIPT_FILE).exists());
        Ok(())
    }

    #[test]
    fn scale_core_binding_rejects_mismatched_or_nonpassed_receipts() -> anyhow::Result<()> {
        let (_output, report, core_receipt) = scale_fixture()?;
        let mut invalid_core = core_receipt.clone();
        invalid_core.receipt_id =
            Digest32::hash_prefixed(b"naome-memory:test:invalid-core\0", b"invalid");
        let invalid_output = tempfile::tempdir()?;
        let error =
            publish_and_verify_scale_evidence(invalid_output.path(), &invalid_core, &report, 100)
                .err()
                .context("invalid core receipt was published")?;
        assert!(error.to_string().contains("core retirement receipt id"));
        assert!(!invalid_output.path().join(SCALE_RECEIPT_FILE).exists());
        assert!(!invalid_output.path().join(SCALE_CORE_RECEIPT_FILE).exists());

        let mut mismatched_report = report.clone();
        mismatched_report.input_set_digest =
            Digest32::hash_prefixed(b"naome-memory:test:mismatched-input\0", b"input").to_hex();
        mismatched_report.logical_digest =
            recompute_scale_logical_digest(&mismatched_report)?.to_hex();
        mismatched_report.evidence_digest = mismatched_report.recompute_evidence_digest()?;
        let invalid_output = tempfile::tempdir()?;
        let error = publish_and_verify_scale_evidence(
            invalid_output.path(),
            &core_receipt,
            &mismatched_report,
            100,
        )
        .err()
        .context("mismatched input-set report was published")?;
        assert!(error.to_string().contains("not bound"));
        assert!(!invalid_output.path().join(SCALE_RECEIPT_FILE).exists());
        assert!(!invalid_output.path().join(SCALE_CORE_RECEIPT_FILE).exists());

        let mut rejected_core = core_receipt.clone();
        rejected_core.overall_status = OverallProofStatusV1::Invalid;
        rejected_core.receipt_id = rejected_core.recompute_receipt_id()?;
        let mut rejected_core_report = report.clone();
        rejected_core_report.receipt_digest = rejected_core.receipt_id.to_hex();
        rejected_core_report.logical_digest =
            recompute_scale_logical_digest(&rejected_core_report)?.to_hex();
        rejected_core_report.evidence_digest = rejected_core_report.recompute_evidence_digest()?;
        let invalid_output = tempfile::tempdir()?;
        let error = publish_and_verify_scale_evidence(
            invalid_output.path(),
            &rejected_core,
            &rejected_core_report,
            100,
        )
        .err()
        .context("non-passed core receipt was published")?;
        assert!(error.to_string().contains("required verified invariants"));
        assert!(!invalid_output.path().join(SCALE_RECEIPT_FILE).exists());
        assert!(!invalid_output.path().join(SCALE_CORE_RECEIPT_FILE).exists());
        Ok(())
    }

    #[test]
    fn status_only_tool_receipt_is_not_a_valid_contract() {
        let fabricated = serde_json::json!({
            "contract_version": "mutation-receipt-v1",
            "tool": "cargo-mutants",
            "status": "passed"
        });
        assert!(serde_json::from_value::<ToolReceiptV1>(fabricated).is_err());
    }

    #[test]
    fn fuzz_commands_and_log_stems_match_the_pinned_cli_contract() {
        for (target, log_stem) in FUZZ_TARGETS {
            assert_eq!(
                fuzz_target_command(target, 120),
                vec![
                    "cargo".to_owned(),
                    format!("+{FUZZ_NIGHTLY}"),
                    "fuzz".to_owned(),
                    "run".to_owned(),
                    target.to_owned(),
                    "--".to_owned(),
                    "-max_total_time=120".to_owned(),
                ]
            );
            assert!(safe_log_stem(log_stem));
        }
        for unsafe_stem in ["", ".", "../fuzz", "fuzz/log", "fuzz_receipt"] {
            assert!(!safe_log_stem(unsafe_stem));
        }
        for safe_artifact in ["crash-a", "oom-123", "slow.unit"] {
            assert!(safe_artifact_file_name(safe_artifact));
        }
        for unsafe_artifact in ["", ".", "..", ".crash-a", "crash/a", "crash\\a"] {
            assert!(!safe_artifact_file_name(unsafe_artifact));
        }
    }

    #[test]
    fn tool_evidence_distinguishes_execution_failure_from_missing_closure() {
        let mut target = ToolTargetEvidenceV1 {
            target: "target".to_owned(),
            command: vec!["tool".to_owned()],
            requested_duration_seconds: None,
            observed_duration_millis: 1,
            exit_code: Some(0),
            log_stem: "tool-target".to_owned(),
            stdout_digest: sha256_hex(&[]),
            stderr_digest: sha256_hex(&[]),
            artifacts: Vec::new(),
        };
        assert_eq!(
            classify_tool_evidence(std::slice::from_ref(&target), 1, true, true),
            EvidenceStatusV1::Passed
        );
        assert_eq!(
            classify_tool_evidence(std::slice::from_ref(&target), 1, false, true),
            EvidenceStatusV1::Inconclusive
        );
        assert_eq!(
            classify_tool_evidence(std::slice::from_ref(&target), 1, true, false),
            EvidenceStatusV1::Inconclusive
        );
        assert_eq!(
            classify_tool_evidence(&[], 1, true, true),
            EvidenceStatusV1::Inconclusive
        );
        target.exit_code = None;
        assert_eq!(
            classify_tool_evidence(std::slice::from_ref(&target), 1, true, true),
            EvidenceStatusV1::Inconclusive
        );
        target.exit_code = Some(2);
        assert_eq!(
            classify_tool_evidence(std::slice::from_ref(&target), 1, false, false),
            EvidenceStatusV1::Failed
        );
    }

    #[test]
    fn mutation_command_routes_tool_output_under_ignored_proof_storage() {
        assert_eq!(
            mutation_target_command("1.95.0"),
            vec![
                "cargo".to_owned(),
                "+1.95.0".to_owned(),
                "mutants".to_owned(),
                "--package".to_owned(),
                "naome-memory-core".to_owned(),
                "--timeout".to_owned(),
                "120".to_owned(),
                "--output".to_owned(),
                "target/proofs/mutation-tool".to_owned(),
            ]
        );
    }

    #[test]
    fn clearing_mutation_evidence_removes_receipt_logs_and_tool_output() -> anyhow::Result<()> {
        let output = tempfile::tempdir()?;
        let logs = output.path().join("tool-logs");
        let tool_output = output.path().join(MUTATION_OUTPUT_DIR).join("mutants.out");
        std::fs::create_dir_all(&logs)?;
        std::fs::create_dir_all(&tool_output)?;
        std::fs::write(output.path().join(MUTATION_RECEIPT_FILE), b"stale receipt")?;
        std::fs::write(
            logs.join(format!("{MUTATION_LOG_STEM}.stdout.log")),
            b"stale",
        )?;
        std::fs::write(
            logs.join(format!("{MUTATION_LOG_STEM}.stderr.log")),
            b"stale",
        )?;
        std::fs::write(tool_output.join("outcomes.json"), b"stale")?;

        clear_mutation_evidence(output.path())?;
        assert!(!output.path().join(MUTATION_RECEIPT_FILE).exists());
        assert!(
            !logs
                .join(format!("{MUTATION_LOG_STEM}.stdout.log"))
                .exists()
        );
        assert!(
            !logs
                .join(format!("{MUTATION_LOG_STEM}.stderr.log"))
                .exists()
        );
        assert!(!output.path().join(MUTATION_OUTPUT_DIR).exists());
        clear_mutation_evidence(output.path())?;
        Ok(())
    }

    #[test]
    fn clearing_fuzz_evidence_removes_receipt_and_all_bound_logs() -> anyhow::Result<()> {
        let output = tempfile::tempdir()?;
        let source_artifacts = tempfile::tempdir()?;
        let logs = output.path().join("tool-logs");
        let retained_artifacts = output.path().join(FUZZ_ARTIFACT_DIR).join("decoder");
        let generated_artifacts = source_artifacts.path().join("decoder");
        std::fs::create_dir_all(&logs)?;
        std::fs::create_dir_all(&retained_artifacts)?;
        std::fs::create_dir_all(&generated_artifacts)?;
        std::fs::write(output.path().join(FUZZ_RECEIPT_FILE), b"stale receipt")?;
        std::fs::write(retained_artifacts.join("crash-stale"), b"stale")?;
        std::fs::write(generated_artifacts.join("crash-stale"), b"stale")?;
        for (_, log_stem) in FUZZ_TARGETS {
            std::fs::write(logs.join(format!("{log_stem}.stdout.log")), b"stale")?;
            std::fs::write(logs.join(format!("{log_stem}.stderr.log")), b"stale")?;
        }

        clear_fuzz_evidence(output.path(), source_artifacts.path())?;
        assert!(!output.path().join(FUZZ_RECEIPT_FILE).exists());
        assert!(!output.path().join(FUZZ_ARTIFACT_DIR).exists());
        assert!(!source_artifacts.path().exists());
        for (_, log_stem) in FUZZ_TARGETS {
            assert!(!logs.join(format!("{log_stem}.stdout.log")).exists());
            assert!(!logs.join(format!("{log_stem}.stderr.log")).exists());
        }
        clear_fuzz_evidence(output.path(), source_artifacts.path())?;
        Ok(())
    }

    #[test]
    fn fuzz_receipt_binds_the_exact_supported_command_and_all_logs() -> anyhow::Result<()> {
        let proof_dir = tempfile::tempdir()?;
        let logs = proof_dir.path().join("tool-logs");
        std::fs::create_dir_all(&logs)?;
        let commit = "a".repeat(40);
        let mut targets = Vec::new();
        for (target, log_stem) in FUZZ_TARGETS {
            let stdout = format!("{target} stdout").into_bytes();
            let stderr = format!("{target} stderr").into_bytes();
            std::fs::write(logs.join(format!("{log_stem}.stdout.log")), &stdout)?;
            std::fs::write(logs.join(format!("{log_stem}.stderr.log")), &stderr)?;
            targets.push(ToolTargetEvidenceV1 {
                target: target.to_owned(),
                command: fuzz_target_command(target, 120),
                requested_duration_seconds: Some(120),
                observed_duration_millis: 1,
                exit_code: Some(0),
                log_stem: log_stem.to_owned(),
                stdout_digest: sha256_hex(&stdout),
                stderr_digest: sha256_hex(&stderr),
                artifacts: Vec::new(),
            });
        }
        let mut receipt = ToolReceiptV1 {
            contract_version: "fuzz-receipt-v1".to_owned(),
            tool: "cargo-fuzz/libFuzzer".to_owned(),
            tool_version: "cargo-fuzz 0.13.2".to_owned(),
            toolchain: FUZZ_NIGHTLY.to_owned(),
            source_commit: commit.clone(),
            status: EvidenceStatusV1::Passed,
            targets,
            note: "Each of four fuzz targets requested 120 seconds.".to_owned(),
            receipt_digest: String::new(),
        };
        receipt.receipt_digest = receipt.recompute_digest()?;
        verify_fuzz_receipt(&receipt, proof_dir.path(), &commit)?;

        receipt.targets[0].command.insert(5, "--locked".to_owned());
        receipt.receipt_digest = receipt.recompute_digest()?;
        let error = verify_fuzz_receipt(&receipt, proof_dir.path(), &commit)
            .err()
            .context("obsolete --locked fuzz command unexpectedly verified")?;
        assert!(error.to_string().contains("target or command"));
        Ok(())
    }

    #[test]
    fn fuzz_receipt_binds_zero_byte_crash_artifacts_and_exact_directory_closure()
    -> anyhow::Result<()> {
        let proof_dir = tempfile::tempdir()?;
        let source_artifacts = tempfile::tempdir()?;
        let logs = proof_dir.path().join("tool-logs");
        let sqlite_source = source_artifacts.path().join("sqlite_ingest");
        std::fs::create_dir_all(&logs)?;
        std::fs::create_dir_all(&sqlite_source)?;
        std::fs::write(sqlite_source.join("crash-empty"), [])?;
        let artifacts = preserve_fuzz_artifacts(
            source_artifacts.path(),
            &proof_dir.path().join(FUZZ_ARTIFACT_DIR),
            "sqlite_ingest",
        )?;
        assert_eq!(
            artifacts,
            vec![ToolArtifactEvidenceV1 {
                relative_path: "fuzz-artifacts/sqlite_ingest/crash-empty".to_owned(),
                byte_len: 0,
                sha256: sha256_hex(&[]),
            }]
        );

        let commit = "a".repeat(40);
        let mut targets = Vec::new();
        for (target, log_stem) in FUZZ_TARGETS {
            let stdout = format!("{target} stdout").into_bytes();
            let stderr = format!("{target} stderr").into_bytes();
            std::fs::write(logs.join(format!("{log_stem}.stdout.log")), &stdout)?;
            std::fs::write(logs.join(format!("{log_stem}.stderr.log")), &stderr)?;
            targets.push(ToolTargetEvidenceV1 {
                target: target.to_owned(),
                command: fuzz_target_command(target, 120),
                requested_duration_seconds: Some(120),
                observed_duration_millis: 1,
                exit_code: Some(if target == "sqlite_ingest" { 77 } else { 0 }),
                log_stem: log_stem.to_owned(),
                stdout_digest: sha256_hex(&stdout),
                stderr_digest: sha256_hex(&stderr),
                artifacts: if target == "sqlite_ingest" {
                    artifacts.clone()
                } else {
                    Vec::new()
                },
            });
        }
        let mut receipt = ToolReceiptV1 {
            contract_version: "fuzz-receipt-v1".to_owned(),
            tool: "cargo-fuzz/libFuzzer".to_owned(),
            tool_version: "cargo-fuzz 0.13.2".to_owned(),
            toolchain: FUZZ_NIGHTLY.to_owned(),
            source_commit: commit.clone(),
            status: EvidenceStatusV1::Failed,
            targets,
            note: "Each of four fuzz targets requested 120 seconds.".to_owned(),
            receipt_digest: String::new(),
        };
        receipt.receipt_digest = receipt.recompute_digest()?;
        verify_fuzz_receipt_evidence(&receipt, proof_dir.path(), &commit)?;
        let release_error = verify_fuzz_receipt(&receipt, proof_dir.path(), &commit)
            .err()
            .context("failed crash receipt unexpectedly passed the release gate")?;
        assert!(release_error.to_string().contains("did not pass"));

        let retained = proof_dir
            .path()
            .join("fuzz-artifacts/sqlite_ingest/crash-empty");
        std::fs::write(&retained, b"tampered")?;
        assert!(verify_fuzz_receipt_evidence(&receipt, proof_dir.path(), &commit).is_err());
        std::fs::write(&retained, [])?;
        std::fs::write(
            proof_dir
                .path()
                .join("fuzz-artifacts/sqlite_ingest/crash-unbound"),
            b"unbound",
        )?;
        let error = verify_fuzz_receipt_evidence(&receipt, proof_dir.path(), &commit)
            .err()
            .context("unbound fuzz artifact unexpectedly verified")?;
        assert!(error.to_string().contains("exactly match"));

        #[cfg(unix)]
        {
            let retained_root = proof_dir.path().join(FUZZ_ARTIFACT_DIR);
            std::fs::remove_dir_all(&retained_root)?;
            std::os::unix::fs::symlink(source_artifacts.path(), &retained_root)?;
            let error = verify_fuzz_receipt_evidence(&receipt, proof_dir.path(), &commit)
                .err()
                .context("symlinked fuzz artifact root unexpectedly verified")?;
            assert!(error.to_string().contains("self-contained directory"));
        }
        Ok(())
    }

    #[test]
    fn rehashed_tool_receipt_without_bound_logs_is_rejected() -> anyhow::Result<()> {
        let proof_dir = tempfile::tempdir()?;
        let commit = "a".repeat(40);
        let empty_digest = sha256_hex(&[]);
        let mut receipt = ToolReceiptV1 {
            contract_version: "mutation-receipt-v1".to_owned(),
            tool: "cargo-mutants".to_owned(),
            tool_version: "cargo-mutants 27.1.0".to_owned(),
            toolchain: "1.95.0".to_owned(),
            source_commit: commit.clone(),
            status: EvidenceStatusV1::Passed,
            targets: vec![ToolTargetEvidenceV1 {
                target: "naome-memory-core".to_owned(),
                command: vec![
                    "cargo".to_owned(),
                    "+1.95.0".to_owned(),
                    "mutants".to_owned(),
                    "--package".to_owned(),
                    "naome-memory-core".to_owned(),
                    "--timeout".to_owned(),
                    "120".to_owned(),
                    "--output".to_owned(),
                    "target/proofs/mutation-tool".to_owned(),
                ],
                requested_duration_seconds: None,
                observed_duration_millis: 1,
                exit_code: Some(0),
                log_stem: "mutation-naome-memory-core".to_owned(),
                stdout_digest: empty_digest.clone(),
                stderr_digest: empty_digest,
                artifacts: Vec::new(),
            }],
            note: "Mutation evidence covers the decision core and receipt verifier.".to_owned(),
            receipt_digest: String::new(),
        };
        receipt.receipt_digest = receipt.recompute_digest()?;
        assert!(verify_mutation_receipt(&receipt, proof_dir.path(), &commit).is_err());
        Ok(())
    }
}
