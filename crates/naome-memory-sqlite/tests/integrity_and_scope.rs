#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;

use naome_memory_core::{
    AtomStateV1, ContentStatusV1, Digest32, EpisodePayloadV1, FormationSignalsV1, MemoryAtomBodyV1,
    MemoryPayloadV1, MemoryScopeV1, ObservationV1, PolicyV1, ProvenanceV1, RetentionPermissionV1,
    RetentionTierV1, SealReasonV1, TimeIntervalV1,
};
use naome_memory_sqlite::{
    ArtifactStore, CohortKey, DraftEvent, SearchScope, SqliteRepository, StoreConfig, StoreError,
};
use rusqlite::{Connection, params};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const MICROSECONDS_PER_DAY: u64 = 86_400_000_000;

#[test]
fn empty_draft_payload_is_integral_after_required_policy_install() -> TestResult {
    let directory = tempfile::tempdir()?;
    let mut repository = SqliteRepository::open(&StoreConfig::new(
        directory.path().join("memory.db"),
        directory.path().join("artifacts"),
    ))?;
    repository.install_policy(&PolicyV1::poc_v1(), 1)?;
    repository.create_draft(
        "empty-payload-draft",
        "test-space",
        Some("test-repository"),
        Some("test-task"),
        Some("test-agent"),
        "test-session",
        1,
    )?;
    let event = DraftEvent {
        sequence: 0,
        event_at_us: 2,
        event_kind: "empty-payload".to_owned(),
        canonical_payload: Vec::new(),
        payload_digest: Digest32::hash_prefixed(&[], &[]).0,
    };

    repository.append_event("empty-payload-draft", &event)?;
    assert_eq!(repository.load_events("empty-payload-draft")?, vec![event]);
    repository.check_integrity(false)?;
    Ok(())
}

#[test]
fn fts_candidates_are_hard_isolated_by_memory_space_and_repository() -> TestResult {
    let directory = tempfile::tempdir()?;
    let config = StoreConfig::new(
        directory.path().join("memory.db"),
        directory.path().join("artifacts"),
    );
    let mut repository = SqliteRepository::open(&config)?;
    let first = episode("space-a", "repo-a", "session-a", 1)?;
    let second = episode("space-a", "repo-b", "session-b", 2)?;
    let other_space = episode("space-b", "repo-a", "session-c", 3)?;
    repository.put_core_atom(&first, &stm_state(&first))?;
    repository.put_core_atom(&second, &stm_state(&second))?;
    repository.put_core_atom(&other_space, &stm_state(&other_space))?;

    let local = repository.search_candidates(
        &SearchScope {
            memory_space_id: "space-a".to_owned(),
            repository_id: Some("repo-a".to_owned()),
            memory_space_wide: false,
        },
        "sharedtoken",
        512,
    )?;
    assert_eq!(local.len(), 1, "repository-local retrieval leaked scope");
    assert_eq!(local[0].atom_id, first.atom_id()?.digest().0);

    let space_wide = repository.search_candidates(
        &SearchScope {
            memory_space_id: "space-a".to_owned(),
            repository_id: None,
            memory_space_wide: true,
        },
        "sharedtoken",
        512,
    )?;
    assert_eq!(space_wide.len(), 2, "explicit space-wide retrieval");
    let other_space_id = other_space.atom_id()?.digest().0;
    assert!(
        space_wide
            .iter()
            .all(|candidate| candidate.atom_id != other_space_id),
        "space-wide retrieval crossed the memory-space boundary"
    );

    let isolated = repository.search_candidates(
        &SearchScope {
            memory_space_id: "space-b".to_owned(),
            repository_id: Some("repo-a".to_owned()),
            memory_space_wide: false,
        },
        "sharedtoken",
        512,
    )?;
    assert_eq!(isolated.len(), 1, "second memory space result");
    assert_eq!(isolated[0].atom_id, other_space.atom_id()?.digest().0);
    Ok(())
}

#[test]
fn artifact_and_atom_authority_tampering_is_detected() -> TestResult {
    let directory = tempfile::tempdir()?;
    let artifact_root = directory.path().join("standalone-artifacts");
    let artifacts = ArtifactStore::open(&artifact_root)?;
    let record = artifacts.ingest_bytes(b"authority bytes")?;
    assert!(artifacts.verify(&record).is_ok());
    fs::write(
        artifact_root.join(&record.relative_path),
        b"tampered-bytes!",
    )?;
    assert!(matches!(
        artifacts.verify(&record),
        Err(StoreError::ArtifactDigestMismatch { .. })
    ));

    let database_path = directory.path().join("memory.db");
    let config = StoreConfig::new(
        &database_path,
        directory.path().join("repository-artifacts"),
    );
    let body = episode("space-a", "repo-a", "session-a", 9)?;
    let atom_id = body.atom_id()?.digest().0;
    {
        let mut repository = SqliteRepository::open(&config)?;
        repository.put_core_atom(&body, &stm_state(&body))?;
    }
    let connection = Connection::open(&database_path)?;
    connection.execute(
        "UPDATE atom_bodies SET canonical_body = x'00' WHERE atom_id = ?1",
        params![&atom_id[..]],
    )?;
    drop(connection);

    let repository = SqliteRepository::open(&config)?;
    let snapshot = repository.snapshot(&CohortKey {
        memory_space_id: "space-a".to_owned(),
        cohort_day: 7,
        as_of_us: 8 * MICROSECONDS_PER_DAY,
    });
    assert!(
        matches!(snapshot, Err(StoreError::Integrity(_))),
        "cohort read accepted a body whose authority digest was changed"
    );
    Ok(())
}

fn episode(
    memory_space_id: &str,
    repository_id: &str,
    session_id: &str,
    discriminator: u64,
) -> naome_memory_core::Result<MemoryAtomBodyV1> {
    let policy_digest = PolicyV1::poc_v1().digest()?;
    Ok(MemoryAtomBodyV1 {
        contract_version: "atom-v1".to_owned(),
        scope: MemoryScopeV1 {
            memory_space_id: memory_space_id.to_owned(),
            repository_id: Some(repository_id.to_owned()),
            task_id: Some(format!("task-{discriminator}")),
            agent_id: Some("agent-a".to_owned()),
            session_id: session_id.to_owned(),
        },
        interval: TimeIntervalV1 {
            started_at_us: discriminator,
            ended_at_us: discriminator.saturating_add(1),
            recorded_at_us: discriminator.saturating_add(2),
        },
        trigger: "store-integration-test".to_owned(),
        seal_reason: SealReasonV1::Checkpoint,
        goal: Some("prove storage isolation".to_owned()),
        plan: vec!["persist".to_owned(), "retrieve".to_owned()],
        internal_state_before: BTreeMap::new(),
        internal_state_after: BTreeMap::new(),
        observations: vec![ObservationV1 {
            at_us: discriminator,
            source: "synthetic-test".to_owned(),
            content: format!("sharedtoken observation {discriminator}"),
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
        topic_keys: BTreeSet::from(["sharedtopic".to_owned()]),
        entity_ids: BTreeSet::new(),
        outcome_class: None,
        goal_key: Some("goal:store-proof".to_owned()),
        provenance: ProvenanceV1 {
            producer: "naome-memory-sqlite-integration-test".to_owned(),
            source_event_digests: vec![Digest32::hash_prefixed(
                b"naome-memory:test-event:v1\0",
                &discriminator.to_be_bytes(),
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
    })
}

fn stm_state(body: &MemoryAtomBodyV1) -> AtomStateV1 {
    AtomStateV1 {
        retention_tier: RetentionTierV1::ShortTerm,
        content_status: ContentStatusV1::Present,
        expires_at_us: Some(body.interval.recorded_at_us + PolicyV1::poc_v1().stm_horizon_us),
        superseded_by: None,
        feedback_positive: 0,
        feedback_negative: 0,
        retrieval_count: 0,
        retirement_run_id: None,
    }
}
