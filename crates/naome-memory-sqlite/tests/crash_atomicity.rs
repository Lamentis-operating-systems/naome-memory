#![forbid(unsafe_code)]
#![cfg(feature = "lab-fault-injection")]

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use naome_memory_core::{
    AtomStateV1, ContentStatusV1, Digest32, EpisodePayloadV1, FormationSignalsV1, MemoryAtomBodyV1,
    MemoryPayloadV1, MemoryScopeV1, ObservationV1, PolicyV1, ProvenanceV1, RetentionPermissionV1,
    RetentionTierV1, SealReasonV1, TimeIntervalV1,
};
use naome_memory_sqlite::{
    ArtifactDigest, DraftEvent, GcAction, RepositoryFaultPointV1, SqliteRepository, StoreConfig,
};
use sha2::{Digest as _, Sha256};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const CHILD_DB: &str = "NAOME_MEMORY_CRASH_DB";
const CHILD_ARTIFACT_ROOT: &str = "NAOME_MEMORY_CRASH_ARTIFACT_ROOT";
const CHILD_READY: &str = "NAOME_MEMORY_CRASH_READY";
const CHILD_FAULT_POINT: &str = "NAOME_MEMORY_CRASH_FAULT_POINT";
const MICROSECONDS_PER_DAY: u64 = 86_400_000_000;
const GC_MARKED_AT_US: u64 = 200;
const FINALIZED_ORPHAN_READY: &str = "finalized_cas_object_before_sqlite_registration";
const GC_UNLINK_READY: &str = "gc_unlink_before_sqlite_commit";
const FINALIZED_ORPHAN_BYTES: &[u8] = b"finalized before SQLite registration";
const INTERRUPTED_GC_BYTES: &[u8] = b"durably unlinked before SQLite commit";

#[test]
fn typed_draft_transactions_survive_hard_child_termination() -> TestResult {
    for point in [
        RepositoryFaultPointV1::DraftAppendAfterSequenceClaim,
        RepositoryFaultPointV1::DraftAppendAfterEventInsert,
        RepositoryFaultPointV1::DraftSealAfterAtomInsert,
        RepositoryFaultPointV1::DraftSealAfterStatusUpdate,
    ] {
        exercise_crash_boundary(point)?;
    }
    Ok(())
}

#[test]
fn finalized_cas_object_survives_kill_and_enters_two_stage_orphan_gc() -> TestResult {
    let directory = tempfile::tempdir()?;
    let config = StoreConfig::new(
        directory.path().join("memory.db"),
        directory.path().join("artifacts"),
    );
    SqliteRepository::open(&config)?.install_policy(&PolicyV1::poc_v1(), 1)?;
    let ready_path = directory.path().join("orphan-child-ready");
    let mut child =
        spawn_artifact_crash_child(&config, &ready_path, "finalized_cas_orphan_child_driver")?;
    wait_until_ready(&mut child, &ready_path, FINALIZED_ORPHAN_READY)?;
    child.kill()?;
    let status = child.wait()?;
    assert!(!status.success(), "child must be hard-terminated");

    let expected_digest = ArtifactDigest(Sha256::digest(FINALIZED_ORPHAN_BYTES).into());
    let mut repository = SqliteRepository::open(&config)?;
    let first_dry_run = repository.garbage_collect(1_000, MICROSECONDS_PER_DAY, true)?;
    let second_dry_run = repository.garbage_collect(1_000, MICROSECONDS_PER_DAY, true)?;
    assert_eq!(first_dry_run, second_dry_run);
    assert_eq!(first_dry_run.entries.len(), 1);
    assert_eq!(first_dry_run.entries[0].digest, expected_digest);
    assert_eq!(first_dry_run.entries[0].action, GcAction::WouldMark);

    let mark = repository.garbage_collect(1_000, MICROSECONDS_PER_DAY, false)?;
    assert_eq!(mark.entries[0].action, GcAction::Marked);
    let delete =
        repository.garbage_collect(1_000 + MICROSECONDS_PER_DAY, MICROSECONDS_PER_DAY, false)?;
    assert_eq!(delete.entries[0].digest, expected_digest);
    assert_eq!(delete.entries[0].action, GcAction::Deleted);
    let integrity = repository.check_integrity(true)?;
    assert_eq!(integrity.sqlite_integrity, "ok");
    assert!(integrity.foreign_key_violations.is_empty());
    let recovered_state = repository.fault_injection_state_digest()?;
    drop(repository);
    assert_eq!(
        SqliteRepository::open(&config)?.fault_injection_state_digest()?,
        recovered_state
    );
    Ok(())
}

#[test]
fn gc_unlink_before_kill_rolls_back_metadata_and_recovers_on_reopen() -> TestResult {
    let directory = tempfile::tempdir()?;
    let config = StoreConfig::new(
        directory.path().join("memory.db"),
        directory.path().join("artifacts"),
    );
    let record = {
        let mut repository = SqliteRepository::open(&config)?;
        repository.install_policy(&PolicyV1::poc_v1(), 1)?;
        let record = repository
            .artifact_store()
            .ingest_bytes(INTERRUPTED_GC_BYTES)?;
        repository.register_artifact(&record, 100)?;
        let mark = repository.garbage_collect(GC_MARKED_AT_US, MICROSECONDS_PER_DAY, false)?;
        assert_eq!(mark.entries[0].action, GcAction::Marked);
        record
    };
    let artifact_path = config.artifact_root.join(&record.relative_path);
    let ready_path = directory.path().join("gc-child-ready");
    let mut child = spawn_artifact_crash_child(&config, &ready_path, "gc_unlink_child_driver")?;
    wait_until_ready(&mut child, &ready_path, GC_UNLINK_READY)?;
    assert!(
        !artifact_path.exists(),
        "child must reach the post-unlink, pre-commit boundary"
    );
    child.kill()?;
    let status = child.wait()?;
    assert!(!status.success(), "child must be hard-terminated");

    let mut repository = SqliteRepository::open(&config)?;
    let recovered = repository.garbage_collect(
        GC_MARKED_AT_US + MICROSECONDS_PER_DAY,
        MICROSECONDS_PER_DAY,
        false,
    )?;
    assert_eq!(recovered.entries.len(), 1);
    assert_eq!(recovered.entries[0].digest, record.digest);
    assert_eq!(recovered.entries[0].action, GcAction::Deleted);
    assert!(
        repository
            .garbage_collect(
                GC_MARKED_AT_US + MICROSECONDS_PER_DAY,
                MICROSECONDS_PER_DAY,
                false,
            )?
            .entries
            .is_empty()
    );
    let integrity = repository.check_integrity(true)?;
    assert_eq!(integrity.sqlite_integrity, "ok");
    assert!(integrity.foreign_key_violations.is_empty());
    let recovered_state = repository.fault_injection_state_digest()?;
    drop(repository);
    assert_eq!(
        SqliteRepository::open(&config)?.fault_injection_state_digest()?,
        recovered_state
    );
    Ok(())
}

fn exercise_crash_boundary(point: RepositoryFaultPointV1) -> TestResult {
    let directory = tempfile::tempdir()?;
    let config = StoreConfig::new(
        directory.path().join("memory.db"),
        directory.path().join("artifacts"),
    );
    let policy = PolicyV1::poc_v1();
    let body = sealed_body(policy.digest()?);
    {
        let mut repository = SqliteRepository::open(&config)?;
        repository.install_policy(&policy, 1)?;
        repository.create_draft(
            "draft-under-test",
            &body.scope.memory_space_id,
            body.scope.repository_id.as_deref(),
            body.scope.task_id.as_deref(),
            body.scope.agent_id.as_deref(),
            &body.scope.session_id,
            1,
        )?;
    }
    let pre_state = SqliteRepository::open(&config)?.fault_injection_state_digest()?;
    let ready_path = directory.path().join("child-ready");
    let mut child = spawn_crash_child(&config, &ready_path, point)?;
    wait_until_ready(&mut child, &ready_path, point.identifier())?;
    child.kill()?;
    let status = child.wait()?;
    assert!(!status.success(), "child must be hard-terminated");

    let mut repository = SqliteRepository::open(&config)?;
    assert_eq!(
        repository.fault_injection_state_digest()?,
        pre_state,
        "{} leaked a partial logical state",
        point.identifier()
    );
    let mut no_fault = |_point| Ok(());
    if point.identifier().starts_with("draft_append_") {
        repository.append_event_with_fault_injection(
            "draft-under-test",
            &draft_event(),
            &mut no_fault,
        )?;
        assert_eq!(repository.load_events("draft-under-test")?.len(), 1);
    } else {
        repository.seal_core_draft_with_fault_injection(
            "draft-under-test",
            &body,
            &stm_state(&body, &policy),
            &mut no_fault,
        )?;
    }
    let integrity = repository.check_integrity(true)?;
    assert_eq!(integrity.sqlite_integrity, "ok");
    assert!(integrity.foreign_key_violations.is_empty());
    Ok(())
}

fn spawn_crash_child(
    config: &StoreConfig,
    ready: &Path,
    point: RepositoryFaultPointV1,
) -> TestResult<Child> {
    let executable = std::env::current_exe()?;
    Ok(Command::new(executable)
        .arg("--ignored")
        .arg("--exact")
        .arg("crash_child_driver")
        .arg("--test-threads=1")
        .env(CHILD_DB, &config.database_path)
        .env(CHILD_ARTIFACT_ROOT, &config.artifact_root)
        .env(CHILD_READY, ready)
        .env(CHILD_FAULT_POINT, point.identifier())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?)
}

fn spawn_artifact_crash_child(
    config: &StoreConfig,
    ready: &Path,
    driver: &str,
) -> TestResult<Child> {
    let executable = std::env::current_exe()?;
    Ok(Command::new(executable)
        .arg("--ignored")
        .arg("--exact")
        .arg(driver)
        .arg("--test-threads=1")
        .env(CHILD_DB, &config.database_path)
        .env(CHILD_ARTIFACT_ROOT, &config.artifact_root)
        .env(CHILD_READY, ready)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?)
}

fn wait_until_ready(child: &mut Child, marker: &Path, point: &str) -> TestResult {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match fs::read_to_string(marker) {
            Ok(observed) if observed == point => return Ok(()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if let Some(status) = child.try_wait()? {
            return Err(
                format!("{point}: child returned before hard termination: {status}").into(),
            );
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{point}: timed out waiting for transaction boundary").into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "spawned by the parent and killed while a real repository transaction is open"]
fn crash_child_driver() -> TestResult {
    let Some(database_path) = std::env::var_os(CHILD_DB).map(PathBuf::from) else {
        return Ok(());
    };
    let artifact_root = required_path(CHILD_ARTIFACT_ROOT)?;
    let ready_path = required_path(CHILD_READY)?;
    let point_name = std::env::var(CHILD_FAULT_POINT)?;
    let point = RepositoryFaultPointV1::ORDERED
        .into_iter()
        .find(|candidate| candidate.identifier() == point_name)
        .ok_or("unknown repository fault point")?;
    let policy = PolicyV1::poc_v1();
    let body = sealed_body(policy.digest()?);
    let config = StoreConfig::new(database_path, artifact_root);
    let mut repository = SqliteRepository::open(&config)?;
    let mut observer = |observed: RepositoryFaultPointV1| -> naome_memory_sqlite::Result<()> {
        if observed == point {
            let mut marker = File::create(&ready_path)?;
            marker.write_all(observed.identifier().as_bytes())?;
            marker.sync_all()?;
            loop {
                thread::park();
            }
        }
        Ok(())
    };
    if point.identifier().starts_with("draft_append_") {
        repository.append_event_with_fault_injection(
            "draft-under-test",
            &draft_event(),
            &mut observer,
        )?;
    } else {
        repository.seal_core_draft_with_fault_injection(
            "draft-under-test",
            &body,
            &stm_state(&body, &policy),
            &mut observer,
        )?;
    }
    Err("selected repository fault point was not reached".into())
}

#[test]
#[ignore = "spawned by the parent and killed after a finalized CAS rename"]
fn finalized_cas_orphan_child_driver() -> TestResult {
    let Some(database_path) = std::env::var_os(CHILD_DB).map(PathBuf::from) else {
        return Ok(());
    };
    let artifact_root = required_path(CHILD_ARTIFACT_ROOT)?;
    let ready_path = required_path(CHILD_READY)?;
    let config = StoreConfig::new(database_path, artifact_root);
    let repository = SqliteRepository::open(&config)?;
    repository
        .artifact_store()
        .ingest_bytes(FINALIZED_ORPHAN_BYTES)?;
    let mut marker = File::create(ready_path)?;
    marker.write_all(FINALIZED_ORPHAN_READY.as_bytes())?;
    marker.sync_all()?;
    loop {
        thread::park();
    }
}

#[test]
#[ignore = "spawned by the parent and killed after GC unlink but before SQLite commit"]
fn gc_unlink_child_driver() -> TestResult {
    let Some(database_path) = std::env::var_os(CHILD_DB).map(PathBuf::from) else {
        return Ok(());
    };
    let artifact_root = required_path(CHILD_ARTIFACT_ROOT)?;
    let ready_path = required_path(CHILD_READY)?;
    let config = StoreConfig::new(database_path, artifact_root);
    let mut repository = SqliteRepository::open(&config)?;
    let mut observer = || -> naome_memory_sqlite::Result<()> {
        let mut marker = File::create(&ready_path)?;
        marker.write_all(GC_UNLINK_READY.as_bytes())?;
        marker.sync_all()?;
        loop {
            thread::park();
        }
    };
    repository.garbage_collect_with_fault_injection(
        GC_MARKED_AT_US + MICROSECONDS_PER_DAY,
        MICROSECONDS_PER_DAY,
        false,
        &mut observer,
    )?;
    Err("GC unlink fault boundary was not reached".into())
}

fn required_path(variable: &str) -> TestResult<PathBuf> {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .ok_or_else(|| format!("missing {variable}").into())
}

fn draft_event() -> DraftEvent {
    let canonical_payload = b"typed crash event".to_vec();
    DraftEvent {
        sequence: 0,
        event_at_us: 2,
        event_kind: "observation".to_owned(),
        payload_digest: Sha256::digest(&canonical_payload).into(),
        canonical_payload,
    }
}

fn sealed_body(policy_digest: Digest32) -> MemoryAtomBodyV1 {
    MemoryAtomBodyV1 {
        contract_version: "atom-v1".to_owned(),
        scope: MemoryScopeV1 {
            memory_space_id: "space-a".to_owned(),
            repository_id: Some("repo-a".to_owned()),
            task_id: Some("task-a".to_owned()),
            agent_id: Some("agent-a".to_owned()),
            session_id: "session-a".to_owned(),
        },
        interval: TimeIntervalV1 {
            started_at_us: 1,
            ended_at_us: 2,
            recorded_at_us: 3,
        },
        trigger: "typed crash fixture".to_owned(),
        seal_reason: SealReasonV1::Checkpoint,
        goal: Some("prove draft transaction atomicity".to_owned()),
        plan: Vec::new(),
        internal_state_before: BTreeMap::new(),
        internal_state_after: BTreeMap::new(),
        observations: vec![ObservationV1 {
            at_us: 1,
            source: "sqlite-crash-test".to_owned(),
            content: "typed crash fixture".to_owned(),
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
        topic_keys: BTreeSet::from(["crash".to_owned()]),
        entity_ids: BTreeSet::new(),
        outcome_class: None,
        goal_key: Some("goal:crash-proof".to_owned()),
        provenance: ProvenanceV1 {
            producer: "sqlite-crash-test".to_owned(),
            source_event_digests: vec![Digest32::hash_prefixed(
                b"naome-memory:sqlite-crash-event:v1\0",
                b"event-0",
            )],
            policy_digest,
        },
        relations: Vec::new(),
        artifacts: Vec::new(),
        retention_permission: RetentionPermissionV1::SemanticAndExactAllowed,
        payload: MemoryPayloadV1::Episode(EpisodePayloadV1 {
            event_sequence_start: 0,
            event_sequence_end: 0,
            continues: None,
        }),
    }
}

const fn stm_state(body: &MemoryAtomBodyV1, policy: &PolicyV1) -> AtomStateV1 {
    AtomStateV1 {
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
    }
}
