#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;

use naome_memory_core::{
    AtomStateV1, ContentStatusV1, Digest32, EpisodePayloadV1, FormationSignalsV1, MemoryAtomBodyV1,
    MemoryPayloadV1, MemoryScopeV1, ObservationV1, PolicyV1, RecallReasonV1, RetentionPermissionV1,
    RetentionTierV1, RetrievalQueryV1, SealReasonV1, Seed32, SpontaneousRecallRequestV1,
    TimeIntervalV1,
};
use naome_memory_sqlite::{SearchScope, SqliteRepository, StoreConfig, StoreError};
use rusqlite::{Connection, params};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one end-to-end scenario keeps lexical, structured, counter, and flashback assertions together"
)]
fn normal_structured_and_seeded_flashback_retrieval_replay_end_to_end() -> TestResult {
    let directory = tempfile::tempdir()?;
    let config = StoreConfig::new(
        directory.path().join("memory.db"),
        directory.path().join("artifacts"),
    );
    let mut repository = SqliteRepository::open(&config)?;
    let lexical = episode(
        "space-a",
        "repo-a",
        1,
        "needle deterministic memory",
        ["retrieval"],
        ["entity:alpha"],
    )?;
    let structured = episode(
        "space-a",
        "repo-a",
        2,
        "no lexical overlap",
        ["structure-only"],
        ["entity:structured"],
    )?;
    let flashback_a = episode(
        "space-a",
        "repo-a",
        3,
        "rare exact red",
        ["rare-red"],
        ["entity:red"],
    )?;
    let flashback_b = episode(
        "space-a",
        "repo-a",
        4,
        "rare exact blue",
        ["rare-blue"],
        ["entity:blue"],
    )?;
    repository.put_core_atom(&lexical, &state(&lexical, RetentionTierV1::ShortTerm))?;
    repository.put_core_atom(&structured, &state(&structured, RetentionTierV1::ShortTerm))?;
    repository.put_core_atom(
        &flashback_a,
        &state(&flashback_a, RetentionTierV1::EpisodicLongTerm),
    )?;
    repository.put_core_atom(
        &flashback_b,
        &state(&flashback_b, RetentionTierV1::EpisodicLongTerm),
    )?;

    let policy = PolicyV1::poc_v1();
    let normal_query = query("space-a", Some("repo-a"), false, ["needle"], [], [], None);
    let first = repository.retrieve(&normal_query, &policy)?;
    let second = repository.retrieve(&normal_query, &policy)?;
    assert_eq!(first, second, "retrieval counters changed logical ranking");
    assert_eq!(first.hits.len(), 1);
    assert_eq!(first.hits[0].atom_id, lexical.atom_id()?);
    assert_eq!(first.hits[0].recall_reason, RecallReasonV1::QueryRanked);
    let persisted = repository.search_candidates(
        &SearchScope {
            memory_space_id: "space-a".to_owned(),
            repository_id: Some("repo-a".to_owned()),
            memory_space_wide: false,
        },
        "needle",
        10,
    )?;
    assert_eq!(persisted[0].retrieval_count, 2);

    let structured_query = query(
        "space-a",
        Some("repo-a"),
        false,
        [],
        ["structure-only"],
        ["entity:structured"],
        None,
    );
    let structured_result = repository.retrieve(&structured_query, &policy)?;
    assert_eq!(structured_result.hits.len(), 1);
    assert_eq!(structured_result.hits[0].atom_id, structured.atom_id()?);
    let structured_candidates = repository.retrieval_candidates(&structured_query, &policy)?;
    assert_eq!(structured_candidates.ranked.len(), 1);
    assert_eq!(
        structured_candidates.ranked[0].lexical_score,
        naome_memory_core::Ppm::ZERO
    );

    let flashback_query = query(
        "space-a",
        Some("repo-a"),
        false,
        ["needle"],
        [],
        [],
        Some(SpontaneousRecallRequestV1 {
            seed: Seed32(Digest32([7; 32])),
            budget: 1,
        }),
    );
    let candidates = repository.retrieval_candidates(&flashback_query, &policy)?;
    assert_eq!(candidates.episodic_pool.len(), 2);
    assert!(
        candidates
            .episodic_pool
            .windows(2)
            .all(|pair| pair[0].atom_id < pair[1].atom_id)
    );
    let flashback_first = repository.retrieve(&flashback_query, &policy)?;
    let flashback_second = repository.retrieve(&flashback_query, &policy)?;
    assert_eq!(flashback_first, flashback_second);
    let spontaneous = flashback_first
        .spontaneous
        .as_ref()
        .ok_or("explicit flashback channel missing")?;
    assert_eq!(spontaneous.population_count, 2);
    assert_eq!(spontaneous.hits.len(), 1);
    assert_eq!(
        spontaneous.hits[0].recall_reason,
        RecallReasonV1::Spontaneous
    );
    assert_ne!(spontaneous.hits[0].atom_id, lexical.atom_id()?);
    Ok(())
}

#[test]
fn retrieval_is_hard_isolated_by_space_and_repository() -> TestResult {
    let directory = tempfile::tempdir()?;
    let config = StoreConfig::new(
        directory.path().join("memory.db"),
        directory.path().join("artifacts"),
    );
    let mut repository = SqliteRepository::open(&config)?;
    let local = episode(
        "space-a",
        "repo-a",
        10,
        "shared retrieval token",
        ["shared"],
        ["entity:shared"],
    )?;
    let other_repository = episode(
        "space-a",
        "repo-b",
        11,
        "shared retrieval token",
        ["shared"],
        ["entity:shared"],
    )?;
    let other_space = episode(
        "space-b",
        "repo-a",
        12,
        "shared retrieval token",
        ["shared"],
        ["entity:shared"],
    )?;
    for body in [&local, &other_repository, &other_space] {
        repository.put_core_atom(body, &state(body, RetentionTierV1::ShortTerm))?;
    }
    let policy = PolicyV1::poc_v1();
    let local_result = repository.retrieve(
        &query(
            "space-a",
            Some("repo-a"),
            false,
            ["shared"],
            ["shared"],
            [],
            None,
        ),
        &policy,
    )?;
    assert_eq!(local_result.hits.len(), 1);
    assert_eq!(local_result.hits[0].atom_id, local.atom_id()?);

    let wide_result = repository.retrieve(
        &query("space-a", None, true, ["shared"], ["shared"], [], None),
        &policy,
    )?;
    let wide_ids = wide_result
        .hits
        .iter()
        .map(|hit| hit.atom_id)
        .collect::<BTreeSet<_>>();
    assert_eq!(wide_ids.len(), 2);
    assert!(wide_ids.contains(&local.atom_id()?));
    assert!(wide_ids.contains(&other_repository.atom_id()?));
    assert!(!wide_ids.contains(&other_space.atom_id()?));
    Ok(())
}

#[test]
fn candidate_generation_is_deterministic_and_hard_capped_at_512() -> TestResult {
    let directory = tempfile::tempdir()?;
    let config = StoreConfig::new(
        directory.path().join("memory.db"),
        directory.path().join("artifacts"),
    );
    let mut repository = SqliteRepository::open(&config)?;
    let mut records = Vec::new();
    let mut all_ids = Vec::new();
    for discriminator in 1..=520 {
        let body = episode(
            "space-a",
            "repo-a",
            discriminator,
            "cap-token",
            ["cap-topic"],
            ["entity:cap"],
        )?;
        all_ids.push(body.atom_id()?);
        let state = state(&body, RetentionTierV1::ShortTerm);
        records.push((body, state));
    }
    let outcome = repository.put_core_atoms_batch(&records)?;
    assert_eq!(outcome.inserted, 520);
    all_ids.sort_unstable();

    let query = query(
        "space-a",
        Some("repo-a"),
        false,
        ["cap-token"],
        ["cap-topic"],
        ["entity:cap"],
        None,
    );
    let policy = PolicyV1::poc_v1();
    let first = repository.retrieval_candidates(&query, &policy)?;
    let second = repository.retrieval_candidates(&query, &policy)?;
    assert_eq!(first, second);
    assert_eq!(first.ranked.len(), 512);
    let observed = first
        .ranked
        .iter()
        .map(|candidate| candidate.atom_id)
        .collect::<Vec<_>>();
    assert_eq!(observed, all_ids[..512]);
    Ok(())
}

#[test]
fn selected_candidate_search_projection_tampering_fails_closed() -> TestResult {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("memory.db");
    let config = StoreConfig::new(&database, directory.path().join("artifacts"));
    let body = episode(
        "space-a",
        "repo-a",
        99,
        "tamper-token",
        ["tamper-topic"],
        ["entity:tamper"],
    )?;
    let atom_id = body.atom_id()?;
    {
        let mut repository = SqliteRepository::open(&config)?;
        repository.put_core_atom(&body, &state(&body, RetentionTierV1::ShortTerm))?;
    }
    let connection = Connection::open(&database)?;
    connection.execute(
        "UPDATE search_projection SET searchable_text = 'tampered' WHERE atom_id = ?1",
        params![atom_id.0.as_bytes()],
    )?;
    drop(connection);
    let repository = SqliteRepository::open(&config)?;
    let result = repository.retrieval_candidates(
        &query(
            "space-a",
            Some("repo-a"),
            false,
            [],
            ["tamper-topic"],
            [],
            None,
        ),
        &PolicyV1::poc_v1(),
    );
    assert!(matches!(result, Err(StoreError::Integrity(_))));
    Ok(())
}

fn query<const T: usize, const K: usize, const E: usize>(
    memory_space_id: &str,
    repository_id: Option<&str>,
    memory_space_wide: bool,
    terms: [&str; T],
    topic_keys: [&str; K],
    entity_ids: [&str; E],
    spontaneous: Option<SpontaneousRecallRequestV1>,
) -> RetrievalQueryV1 {
    RetrievalQueryV1 {
        contract_version: "retrieval-query-v1".to_owned(),
        memory_space_id: memory_space_id.to_owned(),
        repository_id: repository_id.map(str::to_owned),
        memory_space_wide,
        as_of_us: 100_000,
        terms: terms.into_iter().map(str::to_owned).collect(),
        topic_keys: topic_keys.into_iter().map(str::to_owned).collect(),
        entity_ids: entity_ids.into_iter().map(str::to_owned).collect(),
        k: Some(10),
        spontaneous,
    }
}

fn episode<const T: usize, const E: usize>(
    memory_space_id: &str,
    repository_id: &str,
    discriminator: u64,
    observation: &str,
    topic_keys: [&str; T],
    entity_ids: [&str; E],
) -> naome_memory_core::Result<MemoryAtomBodyV1> {
    Ok(MemoryAtomBodyV1 {
        contract_version: "atom-v1".to_owned(),
        scope: MemoryScopeV1 {
            memory_space_id: memory_space_id.to_owned(),
            repository_id: Some(repository_id.to_owned()),
            task_id: Some(format!("task-{discriminator}")),
            agent_id: Some("agent-a".to_owned()),
            session_id: format!("session-{discriminator}"),
        },
        interval: TimeIntervalV1 {
            started_at_us: discriminator,
            ended_at_us: discriminator.saturating_add(1),
            recorded_at_us: discriminator.saturating_add(2),
        },
        trigger: "retrieval-e2e".to_owned(),
        seal_reason: SealReasonV1::Checkpoint,
        goal: Some("prove retrieval closure".to_owned()),
        plan: vec!["persist".to_owned(), "retrieve".to_owned()],
        internal_state_before: BTreeMap::new(),
        internal_state_after: BTreeMap::new(),
        observations: vec![ObservationV1 {
            at_us: discriminator,
            source: "synthetic-test".to_owned(),
            content: observation.to_owned(),
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
        topic_keys: topic_keys.into_iter().map(str::to_owned).collect(),
        entity_ids: entity_ids.into_iter().map(str::to_owned).collect(),
        outcome_class: None,
        goal_key: Some("goal:retrieval-e2e".to_owned()),
        provenance: naome_memory_core::ProvenanceV1 {
            producer: "naome-memory-sqlite-retrieval-e2e".to_owned(),
            source_event_digests: vec![Digest32::hash_prefixed(
                b"naome-memory:retrieval-e2e-event:v1\0",
                &discriminator.to_be_bytes(),
            )],
            policy_digest: PolicyV1::poc_v1().digest()?,
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

fn state(body: &MemoryAtomBodyV1, retention_tier: RetentionTierV1) -> AtomStateV1 {
    AtomStateV1 {
        retention_tier,
        content_status: ContentStatusV1::Present,
        expires_at_us: (retention_tier == RetentionTierV1::ShortTerm)
            .then_some(body.interval.recorded_at_us + PolicyV1::poc_v1().stm_horizon_us),
        superseded_by: None,
        feedback_positive: 0,
        feedback_negative: 0,
        retrieval_count: 0,
        retirement_run_id: None,
    }
}
