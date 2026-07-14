#![forbid(unsafe_code)]

mod schema_fixtures;

use anyhow::{Context as _, Result, ensure};
use clap::{Parser, Subcommand};
use naome_memory_core::{
    ATOM_HASH_DOMAIN, ApplyRetirementOutcomeV1, AtomStateV1, CanonicalBytes as _, ContentStatusV1,
    Digest32, GeneralityV1, MemoryAtomBodyV1, PolicyV1, ProofReceiptV1, RecallReasonV1,
    ReceiptClosureV1, ReceiptVerificationInputsV1, RetentionPermissionV1, RetentionTierV1,
    RetirementPlanV1, RetirementRepositoryV1, RetrievalQueryV1, Seed32, SpontaneousRecallRequestV1,
    apply_retirement, plan_retirement, rank_retrieval, verify_receipt,
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
const MUTATION_TOOL_VERSION: &str = "27.1.0";
const FUZZ_TOOL_VERSION: &str = "0.13.2";
const FUZZ_NIGHTLY: &str = "nightly-2026-07-01";
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
        atoms <= 100_000,
        "atom count exceeds the 100,000-atom PoC envelope"
    );
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

    let observed_wall_millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let observed_max_rss_bytes = rss_sampler.finish();
    let observed_disk_bytes = directory_bytes(temporary.path())?;
    logical.sizes.sort_unstable();
    let p50 = percentile(&logical.sizes, 50)?;
    let p95 = percentile(&logical.sizes, 95)?;
    let maximum = *logical.sizes.last().context("scale size set is empty")?;
    let distribution_matches = p50 == 16 * 1024 && p95 == 64 * 1024 && maximum == 256 * 1024;
    let within_resource_envelope = atoms != 100_000
        || (observed_wall_millis <= 1_800_000
            && observed_max_rss_bytes.is_some_and(|value| value <= 6 * 1024 * 1024 * 1024)
            && observed_disk_bytes <= 10 * 1024 * 1024 * 1024);
    let within_envelope = distribution_matches
        && within_resource_envelope
        && observed_wall_millis <= 1_800_000
        && (atoms != 100_000 || observed_max_rss_bytes.is_some());
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
        source_commit: current_source_commit().unwrap_or_else(|_| "uncommitted".to_owned()),
        status: if within_envelope {
            EvidenceStatusV1::Passed
        } else {
            EvidenceStatusV1::Failed
        },
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
        observed_disk_bytes: Some(observed_disk_bytes),
        note: "Phase timing, sampled peak RSS, and persisted temporary-directory bytes are observational fields excluded from logical_digest.".to_owned(),
        evidence_digest: String::new(),
    };
    report.evidence_digest = report.recompute_evidence_digest()?;
    verify_scale_receipt(&report, atoms)?;
    write_json(
        &proof_directory.join("scale-core-retirement-receipt.json"),
        &applied_receipt,
    )?;
    let output = proof_directory.join("scale-receipt.json");
    write_json(&output, &report)?;
    println!("{}", serde_json::to_string(&report)?);
    ensure!(
        within_envelope,
        "scale proof failed its logical, distribution, or 100,000-atom resource envelope"
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
    let source_commit = current_source_commit().unwrap_or_else(|_| "uncommitted".to_owned());
    let toolchain = "1.95.0";
    let tool_version = observed_cargo_subcommand_version(toolchain, "mutants")
        .unwrap_or_else(|_| "unavailable".to_owned());
    let command = vec![
        "cargo".to_owned(),
        format!("+{toolchain}"),
        "mutants".to_owned(),
        "--package".to_owned(),
        "naome-memory-core".to_owned(),
        "--timeout".to_owned(),
        "120".to_owned(),
    ];
    let target = capture_tool_target(
        &proof_dir,
        "mutation-naome-memory-core",
        "naome-memory-core",
        command,
        None,
    )?;
    let evidence_status = if target.exit_code == Some(0)
        && tool_version.contains(MUTATION_TOOL_VERSION)
        && valid_git_commit(&source_commit)
    {
        EvidenceStatusV1::Passed
    } else if target.exit_code.is_some() {
        EvidenceStatusV1::Failed
    } else {
        EvidenceStatusV1::Inconclusive
    };
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
    let output = proof_dir.join("mutation-receipt.json");
    write_json(&output, &receipt)?;
    println!("{}", serde_json::to_string(&receipt)?);
    ensure!(
        evidence_status == EvidenceStatusV1::Passed,
        "mutation campaign did not pass"
    );
    Ok(())
}

fn fuzz(seconds: u64) -> Result<()> {
    ensure!(seconds > 0, "fuzz duration must be greater than zero");
    let target_names = ["decoder", "canonical", "receipt_verifier", "sqlite_ingest"];
    let fuzz_manifest = workspace_root().join("fuzz/Cargo.toml");
    let proof_dir = workspace_root().join(PROOF_DIR);
    fs::create_dir_all(&proof_dir)?;
    let source_commit = current_source_commit().unwrap_or_else(|_| "uncommitted".to_owned());
    let tool_version = observed_cargo_subcommand_version(FUZZ_NIGHTLY, "fuzz")
        .unwrap_or_else(|_| "unavailable".to_owned());
    let mut targets = Vec::with_capacity(target_names.len());
    if fuzz_manifest.is_file() {
        for target in target_names {
            let command = vec![
                "cargo".to_owned(),
                format!("+{FUZZ_NIGHTLY}"),
                "fuzz".to_owned(),
                "run".to_owned(),
                target.to_owned(),
                "--locked".to_owned(),
                "--".to_owned(),
                format!("-max_total_time={seconds}"),
            ];
            targets.push(capture_tool_target(
                &proof_dir,
                &format!("fuzz-{target}"),
                target,
                command,
                Some(seconds),
            )?);
        }
    }
    let all_passed = targets.len() == target_names.len()
        && targets.iter().all(|target| target.exit_code == Some(0))
        && tool_version.contains(FUZZ_TOOL_VERSION)
        && valid_git_commit(&source_commit);
    let status = if all_passed {
        EvidenceStatusV1::Passed
    } else if targets.iter().any(|target| target.exit_code.is_some()) {
        EvidenceStatusV1::Failed
    } else {
        EvidenceStatusV1::Inconclusive
    };
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
    let output = proof_dir.join("fuzz-receipt.json");
    write_json(&output, &receipt)?;
    println!("{}", serde_json::to_string(&receipt)?);
    ensure!(
        status == EvidenceStatusV1::Passed,
        "fuzz campaign is inconclusive"
    );
    Ok(())
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

fn verify_tool_receipt_common(
    receipt: &ToolReceiptV1,
    proof_dir: &Path,
    expected_contract: &str,
    expected_tool: &str,
    expected_version: &str,
    expected_toolchain: &str,
    expected_commit: &str,
) -> Result<()> {
    ensure!(
        receipt.contract_version == expected_contract
            && receipt.tool == expected_tool
            && receipt.tool_version.contains(expected_version)
            && receipt.toolchain == expected_toolchain
            && receipt.source_commit == expected_commit
            && receipt.status == EvidenceStatusV1::Passed
            && receipt.receipt_digest == receipt.recompute_digest()?,
        "tool receipt metadata, source, status, or digest does not close"
    );
    for target in &receipt.targets {
        ensure!(
            safe_log_stem(&target.log_stem)
                && target.exit_code == Some(0)
                && target.observed_duration_millis > 0,
            "tool target did not record a successful bounded execution"
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
    }
    Ok(())
}

fn verify_mutation_receipt(
    receipt: &ToolReceiptV1,
    proof_dir: &Path,
    expected_commit: &str,
) -> Result<()> {
    verify_tool_receipt_common(
        receipt,
        proof_dir,
        "mutation-receipt-v1",
        "cargo-mutants",
        MUTATION_TOOL_VERSION,
        "1.95.0",
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
    ];
    ensure!(
        receipt.targets.len() == 1
            && receipt.targets.first().is_some_and(|target| {
                target.target == "naome-memory-core"
                    && target.log_stem == "mutation-naome-memory-core"
                    && target.requested_duration_seconds.is_none()
                    && target.command == expected_command
            }),
        "mutation receipt target or command differs from the release gate"
    );
    Ok(())
}

fn verify_fuzz_receipt(
    receipt: &ToolReceiptV1,
    proof_dir: &Path,
    expected_commit: &str,
) -> Result<()> {
    verify_tool_receipt_common(
        receipt,
        proof_dir,
        "fuzz-receipt-v1",
        "cargo-fuzz/libFuzzer",
        FUZZ_TOOL_VERSION,
        FUZZ_NIGHTLY,
        expected_commit,
    )?;
    let expected_targets = ["decoder", "canonical", "receipt_verifier", "sqlite_ingest"];
    ensure!(
        receipt.targets.len() == expected_targets.len(),
        "fuzz receipt target count differs from the release gate"
    );
    for (target, expected) in receipt.targets.iter().zip(expected_targets) {
        let expected_command = vec![
            "cargo".to_owned(),
            format!("+{FUZZ_NIGHTLY}"),
            "fuzz".to_owned(),
            "run".to_owned(),
            expected.to_owned(),
            "--locked".to_owned(),
            "--".to_owned(),
            "-max_total_time=120".to_owned(),
        ];
        ensure!(
            target.target == expected
                && target.log_stem == format!("fuzz-{expected}")
                && target.requested_duration_seconds == Some(120)
                && target.command == expected_command,
            "fuzz receipt target or command differs from the release gate"
        );
    }
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
    ensure!(
        scale_core.receipt_id == scale_core.recompute_receipt_id()?
            && scale_core.receipt_id.to_hex() == scale.receipt_digest,
        "scale receipt is not bound to the attached core retirement receipt"
    );
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
    ensure!(
        receipt.contract_version == "scale-receipt-v1"
            && receipt.evidence_digest == receipt.recompute_evidence_digest()?,
        "unknown or tampered scale receipt"
    );
    ensure!(
        receipt.status == EvidenceStatusV1::Passed && receipt.atom_count == expected_atoms,
        "scale evidence is absent, failed, or has the wrong atom count"
    );
    ensure!(
        receipt.p50_body_bytes == 16 * 1024
            && receipt.p95_body_bytes == 64 * 1024
            && receipt.maximum_body_bytes == 256 * 1024,
        "scale body distribution differs from scale-v1"
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
    if expected_atoms == 100_000 {
        ensure!(
            valid_git_commit(&receipt.source_commit)
                && receipt.observed_wall_millis <= 1_800_000
                && receipt
                    .observed_max_rss_bytes
                    .is_some_and(|bytes| bytes <= 6 * 1024 * 1024 * 1024)
                && receipt
                    .observed_disk_bytes
                    .is_some_and(|bytes| bytes <= 10 * 1024 * 1024 * 1024),
            "100,000-atom scale receipt exceeds its time, RSS, or disk envelope"
        );
    }
    let _atom_stream_digest = Digest32::from_str(&receipt.atom_stream_digest)?;
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
    let expected_logical_digest = Digest32::hash_prefixed(
        b"naome-memory:scale-logical-closure:v1\0",
        &authority.canonical_bytes()?,
    );
    ensure!(
        Digest32::from_str(&receipt.logical_digest)? == expected_logical_digest,
        "scale logical digest does not close"
    );
    Ok(())
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
        EvidenceStatusV1, ScaleReceiptV1, ToolReceiptV1, ToolTargetEvidenceV1, read_json,
        scale_with_output, sha256_hex, verify_mutation_receipt, verify_scale_receipt,
    };

    #[test]
    fn representative_scale_smoke_runs_the_full_path() -> anyhow::Result<()> {
        let output = tempfile::tempdir()?;
        scale_with_output(100, output.path())?;
        let report: ScaleReceiptV1 = read_json(&output.path().join("scale-receipt.json"))?;
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
        assert!(verify_scale_receipt(&tampered, 100).is_err());
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
                ],
                requested_duration_seconds: None,
                observed_duration_millis: 1,
                exit_code: Some(0),
                log_stem: "mutation-naome-memory-core".to_owned(),
                stdout_digest: empty_digest.clone(),
                stderr_digest: empty_digest,
            }],
            note: "Mutation evidence covers the decision core and receipt verifier.".to_owned(),
            receipt_digest: String::new(),
        };
        receipt.receipt_digest = receipt.recompute_digest()?;
        assert!(verify_mutation_receipt(&receipt, proof_dir.path(), &commit).is_err());
        Ok(())
    }
}
