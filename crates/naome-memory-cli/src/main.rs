#![forbid(unsafe_code)]

use clap::{Args, Parser, Subcommand};
use naome_memory_core::{
    AtomStateV1, CanonicalBytes as _, ContentStatusV1, Digest32, EpisodeBufferV1, EpisodeEventV1,
    MemoryError, PolicyV1, ProofReceiptV1, ReceiptVerificationInputsV1, RetentionTierV1,
    RetirementPlanV1, RetrievalQueryV1, SealEpisodeRequestV1, Seed32, apply_retirement,
    plan_retirement, seal_episode_chain, verify_receipt,
};
use naome_memory_lab::{LabError, OverallStatusV1, ProofProfileV1, run_proof};
use naome_memory_sqlite::{
    ArtifactDigest, ArtifactRecord, CohortKey, DraftEvent, SqliteRepository, StoreConfig,
    StoreError,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::str::FromStr as _;

const CLI_CONTRACT: &str = "naome-memory.cli-response.v1";
const GC_GRACE_US: u64 = 86_400_000_000;

#[derive(Debug, Parser)]
#[command(name = "naome-memory", version, about)]
struct Cli {
    /// Emit exactly one compact JSON response object on stdout.
    #[arg(long, global = true)]
    json: bool,
    /// Committed poc-v1 TOML projection. Runtime policy overrides are rejected.
    #[arg(long, global = true, default_value = "policies/poc-v1.toml")]
    policy: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Db {
        #[command(subcommand)]
        command: DbCommand,
    },
    Episode {
        #[command(subcommand)]
        command: EpisodeCommand,
    },
    Cycle {
        #[command(subcommand)]
        command: CycleCommand,
    },
    Retrieve(RetrieveArgs),
    Feedback {
        #[command(subcommand)]
        command: FeedbackCommand,
    },
    Verify {
        #[command(subcommand)]
        command: VerifyCommand,
    },
    Artifact {
        #[command(subcommand)]
        command: ArtifactCommand,
    },
    Lab {
        #[command(subcommand)]
        command: LabCommand,
    },
}

#[derive(Debug, Subcommand)]
enum DbCommand {
    Init(DbInitArgs),
    Check(DbCheckArgs),
}

#[derive(Debug, Subcommand)]
enum EpisodeCommand {
    Create(EpisodeCreateArgs),
    Append(EpisodeAppendArgs),
    Seal(EpisodeSealArgs),
}

#[derive(Debug, Subcommand)]
enum CycleCommand {
    Plan(CyclePlanArgs),
    Apply(CycleApplyArgs),
}

#[derive(Debug, Subcommand)]
enum FeedbackCommand {
    Add(FeedbackAddArgs),
}

#[derive(Debug, Subcommand)]
enum VerifyCommand {
    Receipt(VerifyReceiptArgs),
}

#[derive(Debug, Subcommand)]
enum ArtifactCommand {
    Gc(ArtifactGcArgs),
}

#[derive(Debug, Subcommand)]
enum LabCommand {
    Run(LabRunArgs),
}

#[derive(Debug, Clone, Args)]
struct StoreArgs {
    #[arg(long, default_value = ".naome-memory/memory.db")]
    database: PathBuf,
    #[arg(long, default_value = ".naome-memory/artifacts")]
    artifacts: PathBuf,
}

#[derive(Debug, Args)]
struct DbInitArgs {
    #[command(flatten)]
    store: StoreArgs,
    #[arg(long)]
    as_of_us: u64,
}

#[derive(Debug, Args)]
struct DbCheckArgs {
    #[command(flatten)]
    store: StoreArgs,
    #[arg(long)]
    verify_artifacts: bool,
}

#[derive(Debug, Args)]
struct EpisodeCreateArgs {
    #[command(flatten)]
    store: StoreArgs,
    #[arg(long)]
    draft_id: String,
    #[arg(long)]
    memory_space_id: String,
    #[arg(long)]
    repository_id: Option<String>,
    #[arg(long)]
    task_id: Option<String>,
    #[arg(long)]
    agent_id: Option<String>,
    #[arg(long)]
    session_id: String,
    #[arg(long)]
    created_at_us: u64,
}

#[derive(Debug, Args)]
struct EpisodeAppendArgs {
    #[command(flatten)]
    store: StoreArgs,
    #[arg(long)]
    draft_id: String,
    /// JSON projection of one `EpisodeEventV1`.
    #[arg(long)]
    event: PathBuf,
}

#[derive(Debug, Args)]
struct EpisodeSealArgs {
    #[command(flatten)]
    store: StoreArgs,
    #[arg(long)]
    draft_id: String,
    /// JSON projection of `EpisodeBufferV1`.
    #[arg(long)]
    buffer: PathBuf,
    /// JSON projection of `SealEpisodeRequestV1`, including explicit `as_of_us`.
    #[arg(long)]
    request: PathBuf,
}

#[derive(Debug, Args)]
struct CyclePlanArgs {
    #[command(flatten)]
    store: StoreArgs,
    #[arg(long)]
    memory_space_id: String,
    #[arg(long)]
    cohort_day: u64,
    #[arg(long)]
    as_of_us: u64,
    /// Exactly 64 lowercase hexadecimal characters.
    #[arg(long)]
    seed: String,
}

#[derive(Debug, Args)]
struct CycleApplyArgs {
    #[command(flatten)]
    store: StoreArgs,
    /// JSON projection of a validated `RetirementPlanV1`.
    #[arg(long)]
    plan: PathBuf,
    #[arg(long)]
    as_of_us: u64,
    #[arg(long)]
    seed: String,
}

#[derive(Debug, Args)]
struct RetrieveArgs {
    #[command(flatten)]
    store: StoreArgs,
    /// JSON projection of `RetrievalQueryV1`.
    #[arg(long)]
    query: PathBuf,
    #[arg(long)]
    as_of_us: u64,
    /// Required exactly when the query enables spontaneous recall.
    #[arg(long)]
    seed: Option<String>,
}

#[derive(Debug, Args)]
struct FeedbackAddArgs {
    #[command(flatten)]
    store: StoreArgs,
    #[arg(long)]
    feedback_id: String,
    #[arg(long)]
    atom_id: String,
    #[arg(long)]
    as_of_us: u64,
    #[arg(
        long,
        conflicts_with = "negative",
        required_unless_present = "negative"
    )]
    positive: bool,
    #[arg(
        long,
        conflicts_with = "positive",
        required_unless_present = "positive"
    )]
    negative: bool,
    #[arg(long)]
    source: String,
    #[arg(long)]
    evidence_digest: String,
}

#[derive(Debug, Args)]
struct VerifyReceiptArgs {
    #[arg(long)]
    receipt: PathBuf,
    #[arg(long)]
    inputs: PathBuf,
}

#[derive(Debug, Args)]
struct ArtifactGcArgs {
    #[command(flatten)]
    store: StoreArgs,
    #[arg(long)]
    as_of_us: u64,
    #[arg(long, default_value_t = GC_GRACE_US)]
    grace_period_us: u64,
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct LabRunArgs {
    #[arg(long, default_value = "pr")]
    profile: String,
}

#[derive(Debug)]
enum CliError {
    Input(String),
    Io(std::io::Error),
    Json(serde_json::Error),
    Toml(toml::de::Error),
    Core(MemoryError),
    Store(StoreError),
    Lab(LabError),
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(message) => formatter.write_str(message),
            Self::Io(error) => write!(formatter, "I/O error: {error}"),
            Self::Json(error) => write!(formatter, "JSON error: {error}"),
            Self::Toml(error) => write!(formatter, "policy TOML error: {error}"),
            Self::Core(error) => error.fmt(formatter),
            Self::Store(error) => error.fmt(formatter),
            Self::Lab(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Input(_) => None,
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Toml(error) => Some(error),
            Self::Core(error) => Some(error),
            Self::Store(error) => Some(error),
            Self::Lab(error) => Some(error),
        }
    }
}

impl From<std::io::Error> for CliError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for CliError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<toml::de::Error> for CliError {
    fn from(error: toml::de::Error) -> Self {
        Self::Toml(error)
    }
}

impl From<MemoryError> for CliError {
    fn from(error: MemoryError) -> Self {
        Self::Core(error)
    }
}

impl From<StoreError> for CliError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<LabError> for CliError {
    fn from(error: LabError) -> Self {
        Self::Lab(error)
    }
}

impl CliError {
    fn exit_code(&self) -> u8 {
        match self {
            Self::Core(MemoryError::UnknownVersion(_))
            | Self::Store(StoreError::UnsupportedSchema { .. }) => 5,
            Self::Core(MemoryError::StalePlan { .. })
            | Self::Store(
                StoreError::StalePlan { .. }
                | StoreError::RetirementConflict
                | StoreError::DraftConflict(_)
                | StoreError::ImmutableConflict(_),
            ) => 4,
            Self::Core(
                MemoryError::AtomDigestMismatch { .. }
                | MemoryError::ArtifactDigestMismatch { .. }
                | MemoryError::InvalidApplyClosure(_)
                | MemoryError::ReceiptRejected(_)
                | MemoryError::RepositoryRejected(_),
            )
            | Self::Store(
                StoreError::Sqlite(_)
                | StoreError::Io(_)
                | StoreError::Integrity(_)
                | StoreError::ArtifactDigestMismatch { .. }
                | StoreError::ArtifactMissing { .. }
                | StoreError::InvalidArtifactPath(_)
                | StoreError::InvalidDigestLength { .. }
                | StoreError::InvalidRetirementPlan(_),
            )
            | Self::Lab(LabError::InvalidReceipt(_)) => 3,
            Self::Lab(LabError::Core(error)) | Self::Store(StoreError::Core(error)) => {
                Self::Core(error.clone()).exit_code()
            }
            _ => 2,
        }
    }
}

#[derive(Debug)]
struct Execution {
    command: &'static str,
    result: Value,
    exit_code: u8,
}

#[derive(Serialize)]
struct SuccessEnvelope<'a> {
    contract_version: &'static str,
    command: &'a str,
    status: &'static str,
    result: &'a Value,
}

#[derive(Serialize)]
struct ErrorEnvelope<'a> {
    contract_version: &'static str,
    status: &'static str,
    exit_code: u8,
    error: &'a str,
}

#[derive(Serialize)]
struct ClapEnvelope<'a> {
    contract_version: &'static str,
    status: &'static str,
    exit_code: u8,
    output: &'a str,
}

fn main() -> ExitCode {
    let json_requested = std::env::args_os().any(|value| value == "--json");
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let exit_code = u8::try_from(error.exit_code()).unwrap_or(2);
            if json_requested {
                let output = error.to_string();
                if exit_code == 0 {
                    emit_clap_json(exit_code, &output);
                } else {
                    emit_error_json(exit_code, &output);
                }
            } else {
                let _ = error.print();
            }
            return ExitCode::from(exit_code);
        }
    };
    let policy = match load_policy(&cli.policy) {
        Ok(policy) => policy,
        Err(error) => return emit_failure(cli.json, &error),
    };
    match execute(cli.command, &policy) {
        Ok(execution) => {
            emit_success(cli.json, &execution);
            ExitCode::from(execution.exit_code)
        }
        Err(error) => emit_failure(cli.json, &error),
    }
}

fn execute(command: Command, policy: &PolicyV1) -> Result<Execution, CliError> {
    match command {
        Command::Db { command } => execute_db(command, policy),
        Command::Episode { command } => execute_episode(command, policy),
        Command::Cycle { command } => execute_cycle(command, policy),
        Command::Retrieve(args) => execute_retrieve(&args, policy),
        Command::Feedback { command } => execute_feedback(command),
        Command::Verify { command } => execute_verify(command, policy),
        Command::Artifact { command } => execute_artifact(command),
        Command::Lab { command } => execute_lab(command),
    }
}

fn execute_db(command: DbCommand, policy: &PolicyV1) -> Result<Execution, CliError> {
    match command {
        DbCommand::Init(args) => {
            let mut repository = open_store(&args.store)?;
            repository.install_policy(policy, args.as_of_us)?;
            Ok(execution(
                "db.init",
                json!({"schema_version": naome_memory_sqlite::SCHEMA_VERSION, "policy_id": &policy.policy_id, "policy_digest": policy.digest()?}),
            ))
        }
        DbCommand::Check(args) => {
            let repository = open_store(&args.store)?;
            let report = repository.check_integrity(args.verify_artifacts)?;
            Ok(execution("db.check", serde_json::to_value(report)?))
        }
    }
}

fn execute_episode(command: EpisodeCommand, policy: &PolicyV1) -> Result<Execution, CliError> {
    match command {
        EpisodeCommand::Create(args) => {
            let mut repository = open_store(&args.store)?;
            repository.create_draft(
                &args.draft_id,
                &args.memory_space_id,
                args.repository_id.as_deref(),
                args.task_id.as_deref(),
                args.agent_id.as_deref(),
                &args.session_id,
                args.created_at_us,
            )?;
            Ok(execution(
                "episode.create",
                json!({"draft_id": args.draft_id, "status": "open"}),
            ))
        }
        EpisodeCommand::Append(args) => {
            let event: EpisodeEventV1 = read_json(&args.event)?;
            let canonical_payload = event.canonical_bytes()?;
            let payload_digest = sha256(&canonical_payload);
            let stored = DraftEvent {
                sequence: event.sequence,
                event_at_us: event.at_us,
                event_kind: "episode_event_v1".to_owned(),
                canonical_payload,
                payload_digest,
            };
            let mut repository = open_store(&args.store)?;
            repository.append_event(&args.draft_id, &stored)?;
            Ok(execution(
                "episode.append",
                json!({"draft_id": args.draft_id, "sequence": event.sequence, "event_digest": hex::encode(payload_digest)}),
            ))
        }
        EpisodeCommand::Seal(args) => seal_stored_episode(&args, policy),
    }
}

fn seal_stored_episode(args: &EpisodeSealArgs, policy: &PolicyV1) -> Result<Execution, CliError> {
    let buffer: EpisodeBufferV1 = read_json(&args.buffer)?;
    let request: SealEpisodeRequestV1 = read_json(&args.request)?;
    let mut repository = open_store(&args.store)?;
    let stored_events = repository.load_events(&args.draft_id)?;
    if stored_events.len() != buffer.events.len() {
        return Err(CliError::Input(
            "buffer event count does not match persisted draft".to_owned(),
        ));
    }
    for (stored, event) in stored_events.iter().zip(&buffer.events) {
        let canonical = event.canonical_bytes()?;
        if stored.sequence != event.sequence
            || stored.event_at_us != event.at_us
            || stored.canonical_payload != canonical
            || stored.payload_digest != sha256(&canonical)
        {
            return Err(CliError::Input(
                "buffer events do not match persisted draft authority".to_owned(),
            ));
        }
    }
    let chain = seal_episode_chain(&buffer, &request, policy)?;
    let mut atoms = Vec::with_capacity(chain.episodes.len());
    for episode in &chain.episodes {
        let expires_at_us = episode
            .body
            .interval
            .recorded_at_us
            .checked_add(policy.stm_horizon_us)
            .ok_or_else(|| CliError::Input("STM expiry overflows u64".to_owned()))?;
        let state = AtomStateV1 {
            retention_tier: RetentionTierV1::ShortTerm,
            content_status: ContentStatusV1::Present,
            expires_at_us: Some(expires_at_us),
            superseded_by: None,
            feedback_positive: 0,
            feedback_negative: 0,
            retrieval_count: 0,
            retirement_run_id: None,
        };
        for artifact in &episode.body.artifacts {
            if artifact.inline_payload.is_none() {
                let hex_digest = artifact.digest.to_hex();
                let record = ArtifactRecord {
                    digest: ArtifactDigest(artifact.digest.0),
                    byte_len: artifact.size_bytes,
                    relative_path: format!(
                        "sha256/{}/{}/{}",
                        &hex_digest[..2],
                        &hex_digest[2..4],
                        hex_digest
                    ),
                };
                repository.register_artifact(&record, request.as_of_us)?;
            }
        }
        atoms.push((episode.body.clone(), state));
    }
    repository.seal_core_draft_chain(&args.draft_id, &atoms)?;
    Ok(execution("episode.seal", serde_json::to_value(chain)?))
}

fn execute_cycle(command: CycleCommand, policy: &PolicyV1) -> Result<Execution, CliError> {
    match command {
        CycleCommand::Plan(args) => {
            let repository = open_store(&args.store)?;
            let snapshot = repository.snapshot_core(&CohortKey {
                memory_space_id: args.memory_space_id,
                cohort_day: args.cohort_day,
                as_of_us: args.as_of_us,
            })?;
            let plan = plan_retirement(&snapshot, policy, parse_seed(&args.seed)?)?;
            Ok(execution("cycle.plan", serde_json::to_value(plan)?))
        }
        CycleCommand::Apply(args) => {
            let plan: RetirementPlanV1 = read_json(&args.plan)?;
            let seed = parse_seed(&args.seed)?;
            if plan.as_of_us != args.as_of_us || plan.seed != seed {
                return Err(CliError::Input(
                    "explicit time or seed does not match retirement plan".to_owned(),
                ));
            }
            if plan.policy_id != policy.policy_id || plan.policy_digest != policy.digest()? {
                return Err(CliError::Core(MemoryError::PolicyMismatch));
            }
            let mut repository = open_store(&args.store)?;
            let receipt = apply_retirement(&mut repository, &plan)?;
            Ok(execution("cycle.apply", serde_json::to_value(receipt)?))
        }
    }
}

fn execute_retrieve(args: &RetrieveArgs, policy: &PolicyV1) -> Result<Execution, CliError> {
    let query: RetrievalQueryV1 = read_json(&args.query)?;
    if query.as_of_us != args.as_of_us {
        return Err(CliError::Input(
            "--as-of-us does not match query.as_of_us".to_owned(),
        ));
    }
    match (query.spontaneous, args.seed.as_deref()) {
        (Some(request), Some(seed)) if request.seed == parse_seed(seed)? => {}
        (None, None) => {}
        _ => {
            return Err(CliError::Input(
                "--seed must exactly match an enabled spontaneous recall request".to_owned(),
            ));
        }
    }
    let mut repository = open_store(&args.store)?;
    let result = repository.retrieve(&query, policy)?;
    Ok(execution("retrieve", serde_json::to_value(result)?))
}

fn execute_feedback(command: FeedbackCommand) -> Result<Execution, CliError> {
    match command {
        FeedbackCommand::Add(args) => {
            let mut repository = open_store(&args.store)?;
            repository.add_feedback(
                parse_digest_array(&args.feedback_id)?,
                parse_digest_array(&args.atom_id)?,
                args.as_of_us,
                args.positive && !args.negative,
                &args.source,
                parse_digest_array(&args.evidence_digest)?,
            )?;
            Ok(execution(
                "feedback.add",
                json!({"feedback_id": args.feedback_id, "atom_id": args.atom_id, "positive": args.positive}),
            ))
        }
    }
}

fn execute_verify(command: VerifyCommand, policy: &PolicyV1) -> Result<Execution, CliError> {
    match command {
        VerifyCommand::Receipt(args) => {
            let receipt: ProofReceiptV1 = read_json(&args.receipt)?;
            let inputs: ReceiptVerificationInputsV1 = read_json(&args.inputs)?;
            if inputs.policy != *policy {
                return Err(CliError::Core(MemoryError::PolicyMismatch));
            }
            let verified = verify_receipt(&receipt, &inputs)?;
            Ok(execution("verify.receipt", serde_json::to_value(verified)?))
        }
    }
}

fn execute_artifact(command: ArtifactCommand) -> Result<Execution, CliError> {
    match command {
        ArtifactCommand::Gc(args) => {
            let mut repository = open_store(&args.store)?;
            let report =
                repository.garbage_collect(args.as_of_us, args.grace_period_us, args.dry_run)?;
            Ok(execution("artifact.gc", serde_json::to_value(report)?))
        }
    }
}

fn execute_lab(command: LabCommand) -> Result<Execution, CliError> {
    match command {
        LabCommand::Run(args) => {
            let profile = ProofProfileV1::from_str(&args.profile)?;
            let bundle = run_proof(profile)?;
            let exit_code = if bundle.receipt.overall_status == OverallStatusV1::Failed {
                6
            } else {
                0
            };
            Ok(Execution {
                command: "lab.run",
                result: serde_json::to_value(bundle)?,
                exit_code,
            })
        }
    }
}

fn load_policy(path: &Path) -> Result<PolicyV1, CliError> {
    let projection = fs::read_to_string(path)?;
    let policy: PolicyV1 = toml::from_str(&projection)?;
    policy.validate_poc_v1()?;
    Ok(policy)
}

fn open_store(args: &StoreArgs) -> Result<SqliteRepository, CliError> {
    Ok(SqliteRepository::open(&StoreConfig::new(
        &args.database,
        &args.artifacts,
    ))?)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, CliError> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn parse_seed(value: &str) -> Result<Seed32, CliError> {
    Ok(Seed32(Digest32::from_str(value)?))
}

fn parse_digest_array(value: &str) -> Result<[u8; 32], CliError> {
    Ok(Digest32::from_str(value)?.0)
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn execution(command: &'static str, result: Value) -> Execution {
    Execution {
        command,
        result,
        exit_code: 0,
    }
}

fn emit_success(compact: bool, execution: &Execution) {
    let envelope = SuccessEnvelope {
        contract_version: CLI_CONTRACT,
        command: execution.command,
        status: if execution.exit_code == 0 {
            "succeeded"
        } else {
            "rejected"
        },
        result: &execution.result,
    };
    let rendered = if compact {
        serde_json::to_string(&envelope)
    } else {
        serde_json::to_string_pretty(&envelope)
    };
    match rendered {
        Ok(rendered) => println!("{rendered}"),
        Err(error) => eprintln!("failed to encode CLI response: {error}"),
    }
}

fn emit_failure(json_requested: bool, error: &CliError) -> ExitCode {
    let exit_code = error.exit_code();
    if json_requested {
        emit_error_json(exit_code, &error.to_string());
    } else {
        eprintln!("error: {error}");
    }
    ExitCode::from(exit_code)
}

fn emit_error_json(exit_code: u8, message: &str) {
    let envelope = ErrorEnvelope {
        contract_version: CLI_CONTRACT,
        status: "error",
        exit_code,
        error: message,
    };
    match serde_json::to_string(&envelope) {
        Ok(rendered) => println!("{rendered}"),
        Err(error) => eprintln!("failed to encode CLI error: {error}"),
    }
}

fn emit_clap_json(exit_code: u8, output: &str) {
    let envelope = ClapEnvelope {
        contract_version: CLI_CONTRACT,
        status: "help",
        exit_code,
        output,
    };
    match serde_json::to_string(&envelope) {
        Ok(rendered) => println!("{rendered}"),
        Err(error) => eprintln!("failed to encode CLI response: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use naome_memory_core::{
        FormationSignalsV1, MemoryPayloadV1, MemoryScopeV1, ObservationV1, RetentionPermissionV1,
        SealReasonV1, SealedEpisodeChainV1,
    };
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn committed_policy_projection_matches_typed_authority()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../policies/poc-v1.toml");
        assert_eq!(load_policy(&path)?, PolicyV1::poc_v1());
        Ok(())
    }

    #[test]
    fn json_help_envelope_matches_the_committed_schema_discriminator()
    -> Result<(), serde_json::Error> {
        let value = serde_json::to_value(ClapEnvelope {
            contract_version: CLI_CONTRACT,
            status: "help",
            exit_code: 0,
            output: "usage",
        })?;
        assert_eq!(value["status"], "help");
        assert_eq!(value["exit_code"], 0);
        Ok(())
    }

    #[test]
    fn cycle_plan_reads_the_store_snapshot_instead_of_external_candidates()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let policy = PolicyV1::poc_v1();
        let world = naome_memory_lab::generate_world_for_dataset(
            naome_memory_lab::DatasetIdV1::Calibration,
            0,
            1,
        )?;
        let body = world.atoms[0].clone();
        let expires_at_us = body
            .interval
            .recorded_at_us
            .checked_add(policy.stm_horizon_us)
            .ok_or("expiry overflow")?;
        let cohort_day = expires_at_us / 86_400_000_000;
        let as_of_us = cohort_day
            .checked_add(1)
            .and_then(|day| day.checked_mul(86_400_000_000))
            .ok_or("cohort boundary overflow")?;
        let args = CyclePlanArgs {
            store: StoreArgs {
                database: directory.path().join("memory.db"),
                artifacts: directory.path().join("artifacts"),
            },
            memory_space_id: body.scope.memory_space_id.clone(),
            cohort_day,
            as_of_us,
            seed: "00".repeat(32),
        };
        let mut repository = open_store(&args.store)?;
        repository.put_core_atom(
            &body,
            &AtomStateV1 {
                retention_tier: RetentionTierV1::ShortTerm,
                content_status: ContentStatusV1::Present,
                expires_at_us: Some(expires_at_us),
                superseded_by: None,
                feedback_positive: 0,
                feedback_negative: 0,
                retrieval_count: 0,
                retirement_run_id: None,
            },
        )?;
        drop(repository);
        let execution = execute_cycle(CycleCommand::Plan(args), &policy)?;
        assert_eq!(execution.command, "cycle.plan");
        assert_eq!(
            execution.result["memory_space_id"],
            Value::String(body.scope.memory_space_id)
        );
        assert_eq!(execution.result["cohort_day"], Value::from(cohort_day));
        Ok(())
    }

    #[test]
    fn retrieve_builds_candidates_from_the_configured_store()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let query_path = directory.path().join("query.json");
        let query = RetrievalQueryV1 {
            contract_version: "retrieval-query-v1".to_owned(),
            memory_space_id: "space-a".to_owned(),
            repository_id: Some("repo-a".to_owned()),
            memory_space_wide: false,
            as_of_us: 1,
            terms: vec!["nothing-persisted".to_owned()],
            topic_keys: std::collections::BTreeSet::new(),
            entity_ids: std::collections::BTreeSet::new(),
            k: Some(10),
            spontaneous: None,
        };
        fs::write(&query_path, serde_json::to_vec(&query)?)?;
        let execution = execute_retrieve(
            &RetrieveArgs {
                store: StoreArgs {
                    database: directory.path().join("memory.db"),
                    artifacts: directory.path().join("artifacts"),
                },
                query: query_path,
                as_of_us: 1,
                seed: None,
            },
            &PolicyV1::poc_v1(),
        )?;
        assert_eq!(execution.command, "retrieve");
        assert_eq!(
            execution.result["hits"],
            Value::Array(Vec::new()),
            "empty store must produce an empty ranked result"
        );
        Ok(())
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the end-to-end chain test keeps CLI, SQLite, and relation closure assertions together"
    )]
    fn oversized_stored_episode_seals_as_one_persisted_continuation_chain()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let store = StoreArgs {
            database: directory.path().join("memory.db"),
            artifacts: directory.path().join("artifacts"),
        };
        let scope = MemoryScopeV1 {
            memory_space_id: "space-chain".to_owned(),
            repository_id: Some("repo-chain".to_owned()),
            task_id: Some("task-chain".to_owned()),
            agent_id: Some("agent-chain".to_owned()),
            session_id: "session-chain".to_owned(),
        };
        let events = (0_u64..6)
            .map(|sequence| EpisodeEventV1 {
                sequence,
                at_us: 10 + sequence,
                scope: scope.clone(),
                observation: Some(ObservationV1 {
                    at_us: 10 + sequence,
                    source: "synthetic-cli-chain-test".to_owned(),
                    content: format!("{sequence}:{}", "x".repeat(40_000)),
                    artifact_digest: None,
                }),
                interpretation: None,
                belief: None,
                decision: None,
                action: None,
            })
            .collect::<Vec<_>>();
        let buffer = EpisodeBufferV1 {
            scope: scope.clone(),
            started_at_us: 10,
            events,
            continues: None,
        };
        let request = SealEpisodeRequestV1 {
            as_of_us: 30,
            ended_at_us: 20,
            trigger: "checkpoint".to_owned(),
            seal_reason: SealReasonV1::Checkpoint,
            goal: Some("persist an oversized synthetic episode".to_owned()),
            plan: Vec::new(),
            internal_state_before: BTreeMap::new(),
            internal_state_after: BTreeMap::new(),
            rejected_alternatives: Vec::new(),
            outcome: None,
            feedback: Vec::new(),
            formation_signals: FormationSignalsV1::default(),
            topic_keys: BTreeSet::from(["chain".to_owned()]),
            entity_ids: BTreeSet::new(),
            outcome_class: None,
            goal_key: Some("goal:chain".to_owned()),
            retention_permission: RetentionPermissionV1::SemanticAndExactAllowed,
            artifacts: Vec::new(),
            relations: Vec::new(),
        };
        {
            let mut repository = open_store(&store)?;
            repository.create_draft(
                "draft-chain",
                &scope.memory_space_id,
                scope.repository_id.as_deref(),
                scope.task_id.as_deref(),
                scope.agent_id.as_deref(),
                &scope.session_id,
                1,
            )?;
            for event in &buffer.events {
                let canonical_payload = event.canonical_bytes()?;
                repository.append_event(
                    "draft-chain",
                    &DraftEvent {
                        sequence: event.sequence,
                        event_at_us: event.at_us,
                        event_kind: "episode_event_v1".to_owned(),
                        payload_digest: sha256(&canonical_payload),
                        canonical_payload,
                    },
                )?;
            }
        }
        let buffer_path = directory.path().join("buffer.json");
        let request_path = directory.path().join("request.json");
        fs::write(&buffer_path, serde_json::to_vec(&buffer)?)?;
        fs::write(&request_path, serde_json::to_vec(&request)?)?;
        let execution = seal_stored_episode(
            &EpisodeSealArgs {
                store: store.clone(),
                draft_id: "draft-chain".to_owned(),
                buffer: buffer_path,
                request: request_path,
            },
            &PolicyV1::poc_v1(),
        )?;
        let chain: SealedEpisodeChainV1 = serde_json::from_value(execution.result)?;
        assert!(chain.episodes.len() > 1);

        let expires_at_us = request
            .as_of_us
            .checked_add(PolicyV1::poc_v1().stm_horizon_us)
            .ok_or("expiry overflow")?;
        let repository = open_store(&store)?;
        let cohort_day = expires_at_us / 86_400_000_000;
        let snapshot = repository.snapshot_core(&CohortKey {
            memory_space_id: scope.memory_space_id,
            cohort_day,
            as_of_us: (cohort_day + 1) * 86_400_000_000,
        })?;
        let expected_ids = chain
            .episodes
            .iter()
            .map(|episode| episode.atom_id)
            .collect::<BTreeSet<_>>();
        let persisted_ids = snapshot
            .atoms
            .iter()
            .map(|candidate| candidate.atom_id)
            .collect::<BTreeSet<_>>();
        assert_eq!(persisted_ids, expected_ids);
        for pair in chain.episodes.windows(2) {
            let MemoryPayloadV1::Episode(payload) = &pair[1].body.payload else {
                return Err("episode chain contained semantic payload".into());
            };
            assert_eq!(payload.continues, Some(pair[0].atom_id));
        }
        Ok(())
    }
}
