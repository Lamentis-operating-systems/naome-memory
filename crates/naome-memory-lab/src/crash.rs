use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use naome_memory_core::{
    AtomStateV1, ContentStatusV1, Digest32, MemoryAtomBodyV1, PolicyV1, RetentionTierV1,
    RetirementPlanV1, plan_retirement,
};
use naome_memory_sqlite::{
    CohortKey, DraftEvent, RepositoryFaultPointV1, SqliteRepository, StoreConfig,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{
    CrashCampaignEvidenceV1, CrashScenarioEvidenceV1, DatasetIdV1, LabError, Result,
    generate_world_for_dataset,
};

const CHILD_DB: &str = "NAOME_MEMORY_CRASH_DB";
const CHILD_ARTIFACT_ROOT: &str = "NAOME_MEMORY_CRASH_ARTIFACT_ROOT";
const CHILD_READY: &str = "NAOME_MEMORY_CRASH_READY";
const CHILD_ACTION: &str = "NAOME_MEMORY_CRASH_ACTION";
const CHILD_FAULT_POINT: &str = "NAOME_MEMORY_CRASH_FAULT_POINT";
const MICROSECONDS_PER_DAY: u64 = 86_400_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OperationKind {
    DraftAppend,
    DraftSeal,
    RetirementApply,
}

impl OperationKind {
    const fn for_point(point: RepositoryFaultPointV1) -> Self {
        match point {
            RepositoryFaultPointV1::DraftAppendAfterSequenceClaim
            | RepositoryFaultPointV1::DraftAppendAfterEventInsert => Self::DraftAppend,
            RepositoryFaultPointV1::DraftSealAfterAtomInsert
            | RepositoryFaultPointV1::DraftSealAfterStatusUpdate => Self::DraftSeal,
            RepositoryFaultPointV1::RetirementAfterRunCreation
            | RepositoryFaultPointV1::RetirementAfterSemanticInsertion
            | RepositoryFaultPointV1::RetirementAfterDecisionPersistence
            | RepositoryFaultPointV1::RetirementAfterStateMutation
            | RepositoryFaultPointV1::RetirementAfterForgetting
            | RepositoryFaultPointV1::RetirementAfterArtifactPins
            | RepositoryFaultPointV1::RetirementAfterReceiptInsertion
            | RepositoryFaultPointV1::RetirementAfterRunCompletion
            | RepositoryFaultPointV1::RetirementBeforeCommit => Self::RetirementApply,
        }
    }

    const fn identifier(self) -> &'static str {
        match self {
            Self::DraftAppend => "draft_append",
            Self::DraftSeal => "draft_seal",
            Self::RetirementApply => "retirement_apply",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum CrashActionV1 {
    DraftAppend {
        event: DraftEvent,
    },
    DraftSeal {
        body: Box<MemoryAtomBodyV1>,
        state: AtomStateV1,
    },
    RetirementApply {
        plan: Box<RetirementPlanV1>,
    },
}

impl CrashActionV1 {
    const fn operation_kind(&self) -> OperationKind {
        match self {
            Self::DraftAppend { .. } => OperationKind::DraftAppend,
            Self::DraftSeal { .. } => OperationKind::DraftSeal,
            Self::RetirementApply { .. } => OperationKind::RetirementApply,
        }
    }

    fn digest(&self) -> Result<Digest32> {
        Ok(Digest32::hash_prefixed(
            b"naome-memory:crash-action:v1\0",
            &serde_json::to_vec(self)?,
        ))
    }
}

/// Digest the exact shared parent/child runner and thin executable sources.
///
/// The repository transaction implementation is bound separately in every
/// campaign, so changing either side invalidates prior crash evidence.
#[must_use]
pub fn crash_harness_source_digest() -> Digest32 {
    let mut hasher = Sha256::new();
    hasher.update(b"naome-memory:crash-harness-source:v1\0");
    for source in [
        include_bytes!("crash.rs").as_slice(),
        include_bytes!("bin/naome-memory-crash-harness.rs").as_slice(),
    ] {
        hasher.update(
            u64::try_from(source.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        hasher.update(source);
    }
    Digest32(hasher.finalize().into())
}

/// Run every explicit repository fault boundary in a real child process.
///
/// For each point the parent constructs identical test and control stores. It
/// kills the child while the production `BEGIN IMMEDIATE` transaction and call
/// stack are still live, proves byte-independent logical rollback to the exact
/// pre-state, retries the real operation, compares it with the independently
/// committed control post-state, checks SQLite/FK integrity, and (for
/// retirement) verifies an idempotent second replay.
///
/// # Errors
///
/// Returns an error if setup diverges, the child returns instead of reaching a
/// boundary, hard termination fails, or any closure check is incomplete.
pub fn run_crash_campaign(driver_executable: impl AsRef<Path>) -> Result<CrashCampaignEvidenceV1> {
    let mut scenarios = Vec::with_capacity(RepositoryFaultPointV1::ORDERED.len());
    for point in RepositoryFaultPointV1::ORDERED {
        scenarios.push(run_scenario(driver_executable.as_ref(), point)?);
    }
    let policy = PolicyV1::poc_v1();
    let mut evidence = CrashCampaignEvidenceV1 {
        contract_version: "crash-campaign-evidence-v1".to_owned(),
        harness_source_digest: crash_harness_source_digest(),
        repository_source_digest: SqliteRepository::fault_injection_source_digest(),
        policy_digest: policy.digest()?,
        scenarios,
        campaign_digest: Digest32::ZERO,
    };
    evidence.campaign_digest = evidence.recompute_campaign_digest()?;
    Ok(evidence)
}

fn run_scenario(
    driver: &Path,
    fault_point: RepositoryFaultPointV1,
) -> Result<CrashScenarioEvidenceV1> {
    let directory = tempfile::tempdir()?;
    let case_root = directory.path().join("case");
    let control_root = directory.path().join("control");
    let operation = OperationKind::for_point(fault_point);
    let (case_config, action) = prepare_fixture(&case_root, operation)?;
    let (control_config, control_action) = prepare_fixture(&control_root, operation)?;
    if action != control_action {
        return Err(LabError::InvalidReceipt(format!(
            "{} fixture action is not deterministic",
            fault_point.identifier()
        )));
    }
    let action_digest = action.digest()?;
    let action_path = case_root.join("crash-action.json");
    fs::write(&action_path, serde_json::to_vec(&action)?)?;

    let pre_state_digest = repository_state_digest(&case_config)?;
    if repository_state_digest(&control_config)? != pre_state_digest {
        return Err(LabError::InvalidReceipt(format!(
            "{} fixture pre-state is not deterministic",
            fault_point.identifier()
        )));
    }
    let control_first = replay_action(&control_config, &control_action)?;
    if control_first == Some(true) {
        return Err(LabError::InvalidReceipt(
            "control operation was unexpectedly already applied".to_owned(),
        ));
    }
    let expected_post_state_digest = repository_state_digest(&control_config)?;
    if operation == OperationKind::RetirementApply {
        let control_second = replay_action(&control_config, &control_action)?;
        if control_second != Some(true)
            || repository_state_digest(&control_config)? != expected_post_state_digest
        {
            return Err(LabError::InvalidReceipt(
                "control retirement replay was not idempotent".to_owned(),
            ));
        }
    }

    let ready_path = case_root.join("child-ready");
    let mut child = spawn_child(driver, &case_config, &action_path, &ready_path, fault_point)?;
    wait_until_ready(&mut child, &ready_path, fault_point.identifier())?;
    child.kill()?;
    let status = child.wait()?;
    let child_terminated_while_transaction_open = !status.success()
        && read_ready_point(&ready_path)?.as_deref() == Some(fault_point.identifier());

    let post_crash_state_digest = repository_state_digest(&case_config)?;
    let complete_pre_state_observed = post_crash_state_digest == pre_state_digest;
    let after_crash_integrity = repository_integrity(&case_config)?;
    let recovery_first = replay_action(&case_config, &action)?;
    let recovery_replay_succeeded = recovery_first != Some(true);
    let post_replay_state_digest = repository_state_digest(&case_config)?;
    let complete_post_state_observed = post_replay_state_digest == expected_post_state_digest;
    let idempotent_replay_succeeded = if operation == OperationKind::RetirementApply {
        let replay = replay_action(&case_config, &action)?;
        Some(
            replay == Some(true)
                && repository_state_digest(&case_config)? == post_replay_state_digest,
        )
    } else {
        None
    };
    let after_replay_integrity = repository_integrity(&case_config)?;
    let sqlite_integrity_ok = after_crash_integrity.0 && after_replay_integrity.0;
    let foreign_keys_clean = after_crash_integrity.1 && after_replay_integrity.1;

    let mut evidence = CrashScenarioEvidenceV1 {
        fault_point: fault_point.identifier().to_owned(),
        operation: operation.identifier().to_owned(),
        child_terminated_while_transaction_open,
        complete_pre_state_observed,
        complete_post_state_observed,
        recovery_replay_succeeded,
        idempotent_replay_succeeded,
        sqlite_integrity_ok,
        foreign_keys_clean,
        pre_state_digest,
        post_crash_state_digest,
        post_replay_state_digest,
        action_digest,
        result_digest: Digest32::ZERO,
    };
    evidence.result_digest = evidence.recompute_result_digest()?;
    Ok(evidence)
}

fn prepare_fixture(root: &Path, operation: OperationKind) -> Result<(StoreConfig, CrashActionV1)> {
    fs::create_dir_all(root)?;
    let config = StoreConfig::new(root.join("memory.db"), root.join("artifacts"));
    let policy = PolicyV1::poc_v1();
    let mut repository = SqliteRepository::open(&config)?;
    repository.install_policy(&policy, 1)?;
    let action = match operation {
        OperationKind::DraftAppend => {
            repository.create_draft(
                "draft-under-test",
                "space-a",
                Some("repo-a"),
                Some("task-a"),
                Some("agent-a"),
                "session-a",
                1,
            )?;
            let payload = b"crash recovery event".to_vec();
            CrashActionV1::DraftAppend {
                event: DraftEvent {
                    sequence: 0,
                    event_at_us: 2,
                    event_kind: "observation".to_owned(),
                    payload_digest: Sha256::digest(&payload).into(),
                    canonical_payload: payload,
                },
            }
        }
        OperationKind::DraftSeal => {
            let world = generate_world_for_dataset(DatasetIdV1::Golden, 0, 1)?;
            let mut body = world.atoms.into_iter().next().ok_or_else(|| {
                LabError::InvalidReceipt("seal fixture did not generate an atom".to_owned())
            })?;
            body.artifacts.clear();
            for observation in &mut body.observations {
                observation.artifact_digest = None;
            }
            repository.create_draft(
                "draft-under-test",
                &body.scope.memory_space_id,
                body.scope.repository_id.as_deref(),
                body.scope.task_id.as_deref(),
                body.scope.agent_id.as_deref(),
                &body.scope.session_id,
                1,
            )?;
            CrashActionV1::DraftSeal {
                state: stm_state(
                    body.interval
                        .recorded_at_us
                        .saturating_add(policy.stm_horizon_us),
                ),
                body: Box::new(body),
            }
        }
        OperationKind::RetirementApply => {
            let world = generate_world_for_dataset(DatasetIdV1::Golden, 0, 20)?;
            for body in &world.atoms {
                repository.put_core_atom(
                    body,
                    &stm_state(
                        body.interval
                            .recorded_at_us
                            .saturating_add(policy.stm_horizon_us),
                    ),
                )?;
            }
            let key = CohortKey {
                memory_space_id: "synthetic-space-v1".to_owned(),
                cohort_day: 7,
                as_of_us: MICROSECONDS_PER_DAY.saturating_mul(8),
            };
            let snapshot = repository.snapshot_core(&key)?;
            let plan = plan_retirement(&snapshot, &policy, world.seed)?;
            if plan.semantic_atoms.is_empty()
                || plan.episodic_winners.is_empty()
                || !plan
                    .decisions
                    .iter()
                    .any(|decision| decision.forget_episode_body)
                || !plan
                    .decisions
                    .iter()
                    .any(|decision| !decision.exact_artifact_digests.is_empty())
            {
                return Err(LabError::InvalidReceipt(
                    "retirement crash fixture does not exercise semantic, forgetting, and pin paths"
                        .to_owned(),
                ));
            }
            CrashActionV1::RetirementApply {
                plan: Box::new(plan),
            }
        }
    };
    drop(repository);
    Ok((config, action))
}

const fn stm_state(expires_at_us: u64) -> AtomStateV1 {
    AtomStateV1 {
        retention_tier: RetentionTierV1::ShortTerm,
        content_status: ContentStatusV1::Present,
        expires_at_us: Some(expires_at_us),
        superseded_by: None,
        feedback_positive: 0,
        feedback_negative: 0,
        retrieval_count: 0,
        retirement_run_id: None,
    }
}

fn replay_action(config: &StoreConfig, action: &CrashActionV1) -> Result<Option<bool>> {
    let mut repository = SqliteRepository::open(config)?;
    let mut observer = |_point| Ok(());
    match action {
        CrashActionV1::DraftAppend { event } => {
            repository.append_event_with_fault_injection(
                "draft-under-test",
                event,
                &mut observer,
            )?;
            Ok(None)
        }
        CrashActionV1::DraftSeal { body, state } => {
            repository.seal_core_draft_with_fault_injection(
                "draft-under-test",
                body,
                state,
                &mut observer,
            )?;
            Ok(None)
        }
        CrashActionV1::RetirementApply { plan } => {
            let outcome = repository.apply_core_plan_with_fault_injection(plan, &mut observer)?;
            Ok(Some(outcome.already_applied))
        }
    }
}

fn repository_state_digest(config: &StoreConfig) -> Result<Digest32> {
    Ok(SqliteRepository::open(config)?.fault_injection_state_digest()?)
}

fn repository_integrity(config: &StoreConfig) -> Result<(bool, bool)> {
    let report = SqliteRepository::open(config)?.check_integrity(true)?;
    Ok((
        report.sqlite_integrity == "ok",
        report.foreign_key_violations.is_empty(),
    ))
}

fn spawn_child(
    driver: &Path,
    config: &StoreConfig,
    action: &Path,
    ready: &Path,
    fault_point: RepositoryFaultPointV1,
) -> Result<Child> {
    Ok(Command::new(driver)
        .env(CHILD_DB, &config.database_path)
        .env(CHILD_ARTIFACT_ROOT, &config.artifact_root)
        .env(CHILD_ACTION, action)
        .env(CHILD_READY, ready)
        .env(CHILD_FAULT_POINT, fault_point.identifier())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?)
}

fn wait_until_ready(child: &mut Child, marker: &Path, fault_point: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if read_ready_point(marker)?.as_deref() == Some(fault_point) {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            return Err(LabError::InvalidReceipt(format!(
                "crash driver for {fault_point} returned before hard termination: {status}"
            )));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(LabError::InvalidReceipt(format!(
                "crash driver for {fault_point} timed out"
            )));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn read_ready_point(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Whether this process was spawned as the crash child.
#[must_use]
pub fn crash_child_requested() -> bool {
    std::env::var_os(CHILD_DB).is_some()
}

/// Execute the child half of the crash harness through repository APIs.
///
/// At the selected point the observer writes and syncs a ready marker, then
/// parks forever. Only the parent can end it via hard process termination; the
/// transaction can never return an error or run a Rust rollback destructor.
///
/// # Errors
///
/// Returns an error for malformed explicit child inputs or when the selected
/// boundary was not reached. A correctly selected boundary never returns.
pub fn run_crash_child() -> Result<()> {
    let database_path = required_path(CHILD_DB, "database path")?;
    let artifact_root = required_path(CHILD_ARTIFACT_ROOT, "artifact root")?;
    let action_path = required_path(CHILD_ACTION, "action path")?;
    let ready_path = required_path(CHILD_READY, "ready path")?;
    let fault_point_name = std::env::var(CHILD_FAULT_POINT)
        .map_err(|error| LabError::InvalidReceipt(error.to_string()))?;
    let fault_point = RepositoryFaultPointV1::ORDERED
        .into_iter()
        .find(|point| point.identifier() == fault_point_name)
        .ok_or_else(|| {
            LabError::InvalidReceipt(format!("unknown crash fault point: {fault_point_name}"))
        })?;
    let action: CrashActionV1 = serde_json::from_slice(&fs::read(action_path)?)?;
    if action.operation_kind() != OperationKind::for_point(fault_point) {
        return Err(LabError::InvalidReceipt(
            "crash action does not match selected fault point".to_owned(),
        ));
    }

    let config = StoreConfig::new(database_path, artifact_root);
    let mut repository = SqliteRepository::open(&config)?;
    let mut observer = |observed: RepositoryFaultPointV1| -> naome_memory_sqlite::Result<()> {
        if observed == fault_point {
            write_ready_marker(&ready_path, observed.identifier())?;
            loop {
                thread::park();
            }
        }
        Ok(())
    };
    match action {
        CrashActionV1::DraftAppend { event } => repository.append_event_with_fault_injection(
            "draft-under-test",
            &event,
            &mut observer,
        )?,
        CrashActionV1::DraftSeal { body, state } => repository
            .seal_core_draft_with_fault_injection(
                "draft-under-test",
                &body,
                &state,
                &mut observer,
            )?,
        CrashActionV1::RetirementApply { plan } => {
            repository.apply_core_plan_with_fault_injection(&plan, &mut observer)?;
        }
    }
    Err(LabError::InvalidReceipt(format!(
        "repository returned without reaching {}",
        fault_point.identifier()
    )))
}

fn required_path(variable: &str, description: &str) -> Result<PathBuf> {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .ok_or_else(|| LabError::InvalidReceipt(format!("missing crash child {description}")))
}

fn write_ready_marker(path: &Path, fault_point: &str) -> naome_memory_sqlite::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(fault_point.as_bytes())?;
    file.sync_all()?;
    Ok(())
}
