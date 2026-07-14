use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use naome_memory_core::{
    ATOM_HASH_DOMAIN, ApplyRetirementOutcomeV1, AtomId, AtomStateV1, CanonicalBytes as _,
    ContentStatusV1, Digest32, IntegrityStatusV1, MemoryAtomBodyV1, MemoryKindV1, MemoryPayloadV1,
    MemoryRelationKindV1, PlannedSemanticAtomV1, PolicyV1, Ppm, ProofReceiptV1, ProvenanceV1,
    ReceiptClosureV1, RetentionPermissionV1, RetentionTierV1, RetirementCandidateV1,
    RetirementPlanV1, RetirementPostStateAtomV1, RetirementPostStateV1, RetirementRepositoryV1,
    RetirementSnapshotV1, RetrievalCandidateV1, RetrievalCandidatesV1, RetrievalQueryV1,
    RetrievalResultV1, plan_retirement, rank_retrieval, validate_memory_atom_body,
};

use crate::artifact::GcEntry;
use crate::migration::migrate;
use crate::{
    ArtifactDigest, ArtifactRecord, ArtifactStore, GcAction, GcReport, Result, SCHEMA_VERSION,
    StoreError,
};

const MAX_ATOM_BYTES: usize = 256 * 1024;
const MAX_COHORT_ATOMS: usize = 100_000;
const MAX_EXACT_PACKAGE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PINNED_BYTES_PER_COHORT: u64 = 256 * 1024 * 1024;
const MICROSECONDS_PER_DAY: u64 = 86_400_000_000;
pub const MAX_ATOM_INSERT_BATCH: usize = 1_000;

#[derive(Clone, Debug)]
pub struct StoreConfig {
    pub database_path: PathBuf,
    pub artifact_root: PathBuf,
    pub busy_timeout: Duration,
}

impl StoreConfig {
    #[must_use]
    pub fn new(database_path: impl Into<PathBuf>, artifact_root: impl Into<PathBuf>) -> Self {
        Self {
            database_path: database_path.into(),
            artifact_root: artifact_root.into(),
            busy_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AtomRecord {
    pub atom_id: [u8; 32],
    pub atom_kind: String,
    pub memory_space_id: String,
    pub repository_id: Option<String>,
    pub task_id: Option<String>,
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
    pub recorded_at_us: u64,
    pub body_digest: [u8; 32],
    pub canonical_body: Vec<u8>,
    pub json_projection: String,
    pub retention_permission: String,
    pub provenance_digest: [u8; 32],
    pub provenance_producer: String,
    pub provenance_policy_digest: [u8; 32],
    pub canonical_provenance: Vec<u8>,
    pub provenance_json_projection: String,
    pub provenance_source_digests: Vec<[u8; 32]>,
    pub retention_tier: String,
    pub content_status: String,
    pub expires_at_us: Option<u64>,
    pub searchable_text: String,
    pub topic_keys_json: String,
    pub entity_ids_json: String,
    pub outcome_class: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AtomArtifactLink {
    pub artifact_digest: ArtifactDigest,
    pub role: String,
    pub retention_allowed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AtomRelationLink {
    pub target_atom_id: [u8; 32],
    pub relation_kind: String,
    pub canonical_relation: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AtomBatchInsertOutcome {
    pub requested: usize,
    pub inserted: usize,
    pub already_present: usize,
}

impl AtomRecord {
    pub fn from_core(
        body: &MemoryAtomBodyV1,
        state: &naome_memory_core::AtomStateV1,
    ) -> Result<Self> {
        validate_memory_atom_body(body, &PolicyV1::poc_v1())?;
        let canonical_body = body.canonical_bytes()?;
        let atom_id = body.atom_id()?.digest().0;
        let provenance_bytes = body.provenance.canonical_bytes()?;
        let provenance_digest =
            Digest32::hash_prefixed(b"naome-memory:provenance:v1\0", &provenance_bytes).0;
        let atom_kind = match body.payload {
            MemoryPayloadV1::Episode(_) => "episode",
            MemoryPayloadV1::Semantic(_) => "semantic",
        };
        let retention_permission = match body.retention_permission {
            RetentionPermissionV1::TransientOnly => "transient_only",
            RetentionPermissionV1::SemanticAllowed => "semantic_allowed",
            RetentionPermissionV1::SemanticAndExactAllowed => "semantic_and_exact_allowed",
        };
        let retention_tier = match state.retention_tier {
            RetentionTierV1::ShortTerm => "stm",
            RetentionTierV1::SemanticLongTerm => "ltm_semantic",
            RetentionTierV1::EpisodicLongTerm => "ltm_episodic",
            RetentionTierV1::DualLongTerm => "ltm_dual",
            RetentionTierV1::HeaderOnly => "forgotten",
        };
        let content_status = match state.content_status {
            ContentStatusV1::Present => "present",
            ContentStatusV1::Forgotten => "forgotten",
            ContentStatusV1::Corrupt => "quarantined",
        };
        let topic_keys_json = serde_json::to_string(&body.topic_keys)?;
        let entity_ids_json = serde_json::to_string(&body.entity_ids)?;
        let json_projection = serde_json::to_string(body)?;
        Ok(Self {
            atom_id,
            atom_kind: atom_kind.into(),
            memory_space_id: body.scope.memory_space_id.clone(),
            repository_id: body.scope.repository_id.clone(),
            task_id: body.scope.task_id.clone(),
            agent_id: body.scope.agent_id.clone(),
            session_id: Some(body.scope.session_id.clone()),
            recorded_at_us: body.interval.recorded_at_us,
            body_digest: atom_id,
            canonical_body,
            json_projection,
            retention_permission: retention_permission.into(),
            provenance_digest,
            provenance_producer: body.provenance.producer.clone(),
            provenance_policy_digest: body.provenance.policy_digest.0,
            canonical_provenance: provenance_bytes,
            provenance_json_projection: serde_json::to_string(&body.provenance)?,
            provenance_source_digests: body
                .provenance
                .source_event_digests
                .iter()
                .map(|digest| digest.0)
                .collect(),
            retention_tier: retention_tier.into(),
            content_status: content_status.into(),
            expires_at_us: state.expires_at_us,
            searchable_text: searchable_text(body),
            topic_keys_json,
            entity_ids_json,
            outcome_class: body.outcome_class.clone(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DraftEvent {
    pub sequence: u64,
    pub event_at_us: u64,
    pub event_kind: String,
    pub canonical_payload: Vec<u8>,
    pub payload_digest: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SearchScope {
    pub memory_space_id: String,
    pub repository_id: Option<String>,
    pub memory_space_wide: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SearchCandidate {
    pub atom_id: [u8; 32],
    pub atom_kind: String,
    pub json_projection: String,
    pub feedback_positive: u64,
    pub feedback_negative: u64,
    pub retrieval_count: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RetrievalGenerationSignals {
    lexical: u64,
    topics: u64,
    entities: u64,
}

#[derive(Debug)]
struct StoredRetrievalAtom {
    atom_kind: String,
    memory_space_id: String,
    repository_id: Option<String>,
    task_id: Option<String>,
    agent_id: Option<String>,
    session_id: Option<String>,
    recorded_at_us: i64,
    body_digest: Vec<u8>,
    body_bytes: i64,
    retention_permission: String,
    provenance_digest: Vec<u8>,
    canonical_body: Vec<u8>,
    json_projection: String,
    retention_tier: String,
    content_status: String,
    expires_at_us: Option<i64>,
    superseded_by: Option<Vec<u8>>,
    feedback_positive: i64,
    feedback_negative: i64,
    retrieval_count: i64,
    retirement_run_id: Option<Vec<u8>>,
    searchable_text: String,
    topic_keys_json: String,
    entity_ids_json: String,
    outcome_class: Option<String>,
    projection_digest: Vec<u8>,
}

#[derive(Debug)]
struct StoredAppliedRun {
    memory_space_id: String,
    cohort_day: i64,
    as_of_us: i64,
    policy_id: String,
    policy_digest: Vec<u8>,
    seed: Vec<u8>,
    rng_identifier: String,
    input_set_digest: Vec<u8>,
    population_digest: Vec<u8>,
    plan_digest: Vec<u8>,
    projected_state_digest: Vec<u8>,
    status: String,
    receipt_digest: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
struct StoredIntegrityAtom {
    atom_id: Vec<u8>,
    atom_kind: String,
    memory_space_id: String,
    repository_id: Option<String>,
    task_id: Option<String>,
    agent_id: Option<String>,
    session_id: Option<String>,
    recorded_at_us: i64,
    body_digest: Vec<u8>,
    body_bytes: i64,
    retention_permission: String,
    provenance_digest: Vec<u8>,
    canonical_body: Option<Vec<u8>>,
    json_projection: Option<String>,
    retention_tier: Option<String>,
    content_status: Option<String>,
    expires_at_us: Option<i64>,
    superseded_by: Option<Vec<u8>>,
    feedback_positive: Option<i64>,
    feedback_negative: Option<i64>,
    retrieval_count: Option<i64>,
    retirement_run_id: Option<Vec<u8>>,
    observed_feedback_positive: i64,
    observed_feedback_negative: i64,
    search_atom_id: Option<Vec<u8>>,
    search_memory_space_id: Option<String>,
    search_repository_id: Option<String>,
    searchable_text: Option<String>,
    topic_keys_json: Option<String>,
    entity_ids_json: Option<String>,
    outcome_class: Option<String>,
    projection_digest: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
struct HeaderAuthority {
    memory_space_id: String,
    atom_kind: String,
    body_digest: [u8; 32],
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ArtifactLinkAuthority {
    digest: ArtifactDigest,
    role: String,
    retention_allowed: bool,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RelationAuthority {
    target_atom_id: [u8; 32],
    relation_kind: String,
    relation_digest: [u8; 32],
}

#[derive(Clone, Copy, Debug)]
struct FtsAuthority {
    row_count: u64,
    searchable_text_digest: [u8; 32],
    has_conflicting_text: bool,
}

#[derive(Clone, Debug)]
struct ArtifactAuthority {
    record: ArtifactRecord,
    integrity_status: String,
}

#[derive(Clone, Debug)]
struct RetirementRunAuthority {
    memory_space_id: String,
    status: String,
}

#[derive(Clone, Debug)]
struct ProvenanceAuthority {
    producer: String,
    policy_digest: [u8; 32],
    canonical_provenance: Vec<u8>,
    json_projection: String,
    provenance_digest: [u8; 32],
    source_event_digests: Vec<[u8; 32]>,
}

type ArtifactPinsByAtom = BTreeMap<[u8; 32], Vec<(ArtifactDigest, [u8; 32])>>;
type RetirementDecisionAuthorities = BTreeMap<([u8; 32], [u8; 32]), String>;

type PersistedDecisionAuthority = ([u8; 32], String, Vec<u8>, [u8; 32]);

#[derive(Debug)]
struct StoredPostStateAtom {
    atom_id: Vec<u8>,
    atom_kind: String,
    memory_space_id: String,
    repository_id: Option<String>,
    task_id: Option<String>,
    agent_id: Option<String>,
    session_id: Option<String>,
    recorded_at_us: i64,
    body_digest: Vec<u8>,
    body_bytes: i64,
    retention_permission: String,
    provenance_digest: Vec<u8>,
    canonical_body: Option<Vec<u8>>,
    json_projection: Option<String>,
    retention_tier: String,
    content_status: String,
    expires_at_us: Option<i64>,
    superseded_by: Option<Vec<u8>>,
    feedback_positive: i64,
    feedback_negative: i64,
    retrieval_count: i64,
    retirement_run_id: Option<Vec<u8>>,
    search_atom_id: Option<Vec<u8>>,
    searchable_text: Option<String>,
    topic_keys_json: Option<String>,
    entity_ids_json: Option<String>,
    outcome_class: Option<String>,
    projection_digest: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CohortKey {
    pub memory_space_id: String,
    pub cohort_day: u64,
    pub as_of_us: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SnapshotAtom {
    pub atom_id: [u8; 32],
    pub body_digest: [u8; 32],
    pub state_digest: [u8; 32],
    pub body_bytes: u64,
    pub artifact_bytes: u64,
    pub retention_permission: String,
    pub session_id: Option<String>,
}

impl SnapshotAtom {
    #[must_use]
    pub const fn exact_package_bytes(&self) -> u64 {
        self.body_bytes.saturating_add(self.artifact_bytes)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CohortSnapshot {
    pub key: CohortKey,
    pub atoms: Vec<SnapshotAtom>,
    pub input_set_digest: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct PersistedDecision {
    pub atom_id: [u8; 32],
    pub pre_state_digest: [u8; 32],
    pub decision: String,
    pub canonical_details: Vec<u8>,
    pub details_digest: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct ArtifactPin {
    pub atom_id: [u8; 32],
    pub artifact_digest: ArtifactDigest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct PersistedReceipt {
    pub receipt_digest: [u8; 32],
    pub canonical_receipt: Vec<u8>,
    pub json_projection: String,
    pub closure_digest: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct RetirementCommit {
    pub run_id: [u8; 32],
    pub key: CohortKey,
    pub policy_id: String,
    pub policy_digest: [u8; 32],
    pub seed: [u8; 32],
    pub rng_id: String,
    pub proof_input_set_digest: [u8; 32],
    pub expected_input_set_digest: [u8; 32],
    pub population_digest: [u8; 32],
    pub plan_digest: [u8; 32],
    pub projected_state_digest: [u8; 32],
    pub projected_post_state: RetirementPostStateV1,
    pub decisions: Vec<PersistedDecision>,
    pub semantic_atoms: Vec<AtomRecord>,
    pub artifact_pins: Vec<ArtifactPin>,
    pub receipt: PersistedReceipt,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ApplyOutcome {
    Applied,
    AlreadyApplied,
}

/// Explicit transaction boundaries exposed only to the lab fault-injection
/// methods.
///
/// The identifiers and their order are part of crash-campaign evidence. The
/// production methods always use a no-op observer; no environment variable,
/// process-global switch, clock, or random source can activate a fault point.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryFaultPointV1 {
    DraftAppendAfterSequenceClaim,
    DraftAppendAfterEventInsert,
    DraftSealAfterAtomInsert,
    DraftSealAfterStatusUpdate,
    RetirementAfterRunCreation,
    RetirementAfterSemanticInsertion,
    RetirementAfterDecisionPersistence,
    RetirementAfterStateMutation,
    RetirementAfterForgetting,
    RetirementAfterArtifactPins,
    RetirementAfterReceiptInsertion,
    RetirementAfterRunCompletion,
    RetirementBeforeCommit,
}

impl RepositoryFaultPointV1 {
    pub const ORDERED: [Self; 13] = [
        Self::DraftAppendAfterSequenceClaim,
        Self::DraftAppendAfterEventInsert,
        Self::DraftSealAfterAtomInsert,
        Self::DraftSealAfterStatusUpdate,
        Self::RetirementAfterRunCreation,
        Self::RetirementAfterSemanticInsertion,
        Self::RetirementAfterDecisionPersistence,
        Self::RetirementAfterStateMutation,
        Self::RetirementAfterForgetting,
        Self::RetirementAfterArtifactPins,
        Self::RetirementAfterReceiptInsertion,
        Self::RetirementAfterRunCompletion,
        Self::RetirementBeforeCommit,
    ];

    #[must_use]
    pub const fn identifier(self) -> &'static str {
        match self {
            Self::DraftAppendAfterSequenceClaim => "draft_append_after_sequence_claim",
            Self::DraftAppendAfterEventInsert => "draft_append_after_event_insert",
            Self::DraftSealAfterAtomInsert => "draft_seal_after_atom_insert",
            Self::DraftSealAfterStatusUpdate => "draft_seal_after_status_update",
            Self::RetirementAfterRunCreation => "retirement_after_run_creation",
            Self::RetirementAfterSemanticInsertion => "retirement_after_semantic_insertion",
            Self::RetirementAfterDecisionPersistence => "retirement_after_decision_persistence",
            Self::RetirementAfterStateMutation => "retirement_after_state_mutation",
            Self::RetirementAfterForgetting => "retirement_after_forgetting",
            Self::RetirementAfterArtifactPins => "retirement_after_artifact_pins",
            Self::RetirementAfterReceiptInsertion => "retirement_after_receipt_insertion",
            Self::RetirementAfterRunCompletion => "retirement_after_run_completion",
            Self::RetirementBeforeCommit => "retirement_before_commit",
        }
    }
}

type TransactionObserver<'a> = &'a mut dyn FnMut(RepositoryFaultPointV1) -> Result<()>;

#[allow(
    clippy::missing_const_for_fn,
    clippy::unnecessary_wraps,
    reason = "the no-op observer must implement the same fallible callback signature as lab observers"
)]
fn ignore_transaction_boundary(_point: RepositoryFaultPointV1) -> Result<()> {
    Ok(())
}

#[allow(
    clippy::missing_const_for_fn,
    clippy::unnecessary_wraps,
    reason = "the no-op observer must implement the same fallible callback signature as the lab GC observer"
)]
fn ignore_artifact_gc_boundary() -> Result<()> {
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IntegrityReport {
    pub schema_version: u32,
    pub sqlite_integrity: String,
    pub foreign_key_violations: Vec<String>,
    pub verified_artifacts: u64,
}

pub struct SqliteRepository {
    connection: Connection,
    artifacts: ArtifactStore,
}

impl SqliteRepository {
    pub fn open(config: &StoreConfig) -> Result<Self> {
        if let Some(parent) = config.database_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        let mut connection = Connection::open_with_flags(
            &config.database_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(config.busy_timeout)?;
        configure_connection(&connection)?;
        migrate(&mut connection)?;
        configure_connection(&connection)?;
        let artifacts = ArtifactStore::open(&config.artifact_root)?;
        Ok(Self {
            connection,
            artifacts,
        })
    }

    #[must_use]
    pub const fn artifact_store(&self) -> &ArtifactStore {
        &self.artifacts
    }

    /// Digest the exact repository implementation compiled into a synthetic
    /// crash campaign.
    #[cfg(feature = "lab-fault-injection")]
    #[must_use]
    pub fn fault_injection_source_digest() -> Digest32 {
        Digest32::hash_prefixed(
            b"naome-memory:sqlite:fault-surface-source:v1\0",
            include_bytes!("repository.rs"),
        )
    }

    /// Digest every logical `SQLite` row and content-addressed artifact.
    ///
    /// This read-only lab surface makes pre/post crash comparison independent
    /// of WAL page layout and host paths. Tables, rows, and relative CAS paths
    /// are explicitly sorted.
    #[cfg(feature = "lab-fault-injection")]
    pub fn fault_injection_state_digest(&self) -> Result<Digest32> {
        let database = logical_database_digest(&self.connection)?;
        let artifacts = logical_artifact_tree_digest(self.artifacts.root())?;
        let mut hasher = domain_hasher(b"naome-memory:sqlite:repository-state:v1\0");
        hasher.update(database.0);
        hasher.update(artifacts.0);
        Ok(Digest32(hasher.finalize().into()))
    }

    pub fn install_policy(&mut self, policy: &PolicyV1, installed_at_us: u64) -> Result<()> {
        policy.validate_poc_v1()?;
        let policy_id = &policy.policy_id;
        let policy_digest = policy.digest()?.0;
        let canonical_body = policy.canonical_bytes()?;
        let json_projection = serde_json::to_string(policy)?;
        let installed_at = sqlite_integer(installed_at_us, "installed_at_us")?;
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO policies(policy_id, policy_digest, canonical_body, json_projection, installed_at_us)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(policy_id) DO NOTHING",
            params![
                policy_id,
                &policy_digest[..],
                &canonical_body,
                &json_projection,
                installed_at
            ],
        )?;
        let persisted = transaction.query_row(
            "SELECT policy_digest, canonical_body, json_projection FROM policies WHERE policy_id = ?1",
            [policy_id],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?;
        if persisted.0 != policy_digest
            || persisted.1 != canonical_body
            || persisted.2 != json_projection
        {
            return Err(StoreError::ImmutableConflict(format!("policy {policy_id}")));
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn create_draft(
        &mut self,
        draft_id: &str,
        memory_space_id: &str,
        repository_id: Option<&str>,
        task_id: Option<&str>,
        agent_id: Option<&str>,
        session_id: &str,
        created_at_us: u64,
    ) -> Result<()> {
        let created_at = sqlite_integer(created_at_us, "created_at_us")?;
        let inserted = self.connection.execute(
            "INSERT INTO episode_drafts(
                draft_id, memory_space_id, repository_id, task_id, agent_id, session_id,
                created_at_us, next_sequence, status
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, 'open')
             ON CONFLICT(draft_id) DO NOTHING",
            params![
                draft_id,
                memory_space_id,
                repository_id,
                task_id,
                agent_id,
                session_id,
                created_at
            ],
        )?;
        if inserted == 0 {
            return Err(StoreError::DraftConflict(format!(
                "draft {draft_id} already exists"
            )));
        }
        Ok(())
    }

    pub fn append_event(&mut self, draft_id: &str, event: &DraftEvent) -> Result<()> {
        self.append_event_observed(draft_id, event, &mut ignore_transaction_boundary)
    }

    /// Append through the production transaction while synchronously
    /// observing explicit fault boundaries.
    ///
    /// This API is compiled only for the synthetic lab/crash profile. The
    /// observer must itself terminate or block the process to emulate a hard
    /// crash; returning continues the real transaction unchanged.
    #[cfg(feature = "lab-fault-injection")]
    pub fn append_event_with_fault_injection(
        &mut self,
        draft_id: &str,
        event: &DraftEvent,
        observer: TransactionObserver<'_>,
    ) -> Result<()> {
        self.append_event_observed(draft_id, event, observer)
    }

    fn append_event_observed(
        &mut self,
        draft_id: &str,
        event: &DraftEvent,
        observer: TransactionObserver<'_>,
    ) -> Result<()> {
        let sequence = sqlite_integer(event.sequence, "sequence")?;
        let event_at = sqlite_integer(event.event_at_us, "event_at_us")?;
        let computed = sha256(&event.canonical_payload);
        if computed != event.payload_digest {
            return Err(StoreError::Integrity(
                "episode event payload digest mismatch".into(),
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE episode_drafts
             SET next_sequence = next_sequence + 1
             WHERE draft_id = ?1 AND status = 'open' AND next_sequence = ?2",
            params![draft_id, sequence],
        )?;
        if changed != 1 {
            return Err(StoreError::DraftConflict(format!(
                "draft {draft_id} is not open at sequence {}",
                event.sequence
            )));
        }
        observer(RepositoryFaultPointV1::DraftAppendAfterSequenceClaim)?;
        transaction.execute(
            "INSERT INTO episode_events(
                draft_id, sequence, event_at_us, event_kind, canonical_payload, payload_digest
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                draft_id,
                sequence,
                event_at,
                event.event_kind,
                event.canonical_payload,
                &event.payload_digest[..]
            ],
        )?;
        observer(RepositoryFaultPointV1::DraftAppendAfterEventInsert)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn load_events(&self, draft_id: &str) -> Result<Vec<DraftEvent>> {
        let mut statement = self.connection.prepare(
            "SELECT sequence, event_at_us, event_kind, canonical_payload, payload_digest
             FROM episode_events WHERE draft_id = ?1 ORDER BY sequence ASC",
        )?;
        let rows = statement.query_map([draft_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
                row.get::<_, Vec<u8>>(4)?,
            ))
        })?;
        let mut events = Vec::new();
        for row in rows {
            let (sequence, event_at, event_kind, canonical_payload, payload_digest) = row?;
            events.push(DraftEvent {
                sequence: unsigned_integer(sequence, "sequence")?,
                event_at_us: unsigned_integer(event_at, "event_at_us")?,
                event_kind,
                canonical_payload,
                payload_digest: digest_array(&payload_digest, "payload_digest")?,
            });
        }
        Ok(events)
    }

    #[cfg(test)]
    fn put_atom(&mut self, atom: &AtomRecord) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        put_atom_transaction(&transaction, atom)?;
        transaction.commit()?;
        Ok(())
    }

    #[cfg(test)]
    fn put_atoms_batch(&mut self, atoms: &[AtomRecord]) -> Result<AtomBatchInsertOutcome> {
        if atoms.len() > MAX_ATOM_INSERT_BATCH {
            return Err(StoreError::InvalidInput(format!(
                "atom batch contains {}; maximum is {MAX_ATOM_INSERT_BATCH}",
                atoms.len()
            )));
        }
        if atoms.is_empty() {
            return Ok(AtomBatchInsertOutcome {
                requested: 0,
                inserted: 0,
                already_present: 0,
            });
        }
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut inserted = 0_usize;
        for atom in atoms {
            if put_atom_transaction(&transaction, atom)? {
                inserted = inserted.saturating_add(1);
            }
        }
        transaction.commit()?;
        Ok(AtomBatchInsertOutcome {
            requested: atoms.len(),
            inserted,
            already_present: atoms.len().saturating_sub(inserted),
        })
    }

    /// Inserts a bounded batch through typed core authority. This fast path is
    /// intentionally limited to atoms without artifact or relation links;
    /// linked atoms must use [`SqliteRepository::put_core_atom`] so link and CAS
    /// closure are validated in the same operation.
    pub fn put_core_atoms_batch(
        &mut self,
        atoms: &[(MemoryAtomBodyV1, AtomStateV1)],
    ) -> Result<AtomBatchInsertOutcome> {
        if atoms.len() > MAX_ATOM_INSERT_BATCH {
            return Err(StoreError::InvalidInput(format!(
                "atom batch contains {}; maximum is {MAX_ATOM_INSERT_BATCH}",
                atoms.len()
            )));
        }
        if atoms
            .iter()
            .any(|(body, _)| !body.artifacts.is_empty() || !body.relations.is_empty())
        {
            return Err(StoreError::InvalidInput(
                "typed batch insertion accepts only unlinked atoms".to_owned(),
            ));
        }
        if atoms.is_empty() {
            return Ok(AtomBatchInsertOutcome {
                requested: 0,
                inserted: 0,
                already_present: 0,
            });
        }
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut inserted = 0_usize;
        for (body, state) in atoms {
            validate_initial_atom_state(body, state)?;
            let atom = AtomRecord::from_core(body, state)?;
            if put_atom_transaction(&transaction, &atom)? {
                inserted = inserted.saturating_add(1);
            }
        }
        transaction.commit()?;
        Ok(AtomBatchInsertOutcome {
            requested: atoms.len(),
            inserted,
            already_present: atoms.len().saturating_sub(inserted),
        })
    }

    pub fn put_core_atom(&mut self, body: &MemoryAtomBodyV1, state: &AtomStateV1) -> Result<()> {
        validate_initial_atom_state(body, state)?;
        let atom = AtomRecord::from_core(body, state)?;
        let (artifact_links, relation_links) = self.prepare_core_links(body)?;
        validate_declared_core_link_closure(&atom, &artifact_links, &relation_links)?;
        self.validate_atom_links(&atom.memory_space_id, &artifact_links, &relation_links)?;
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        put_atom_transaction(&transaction, &atom)?;
        insert_atom_links_transaction(
            &transaction,
            atom.atom_id,
            &artifact_links,
            &relation_links,
        )?;
        transaction.commit()?;
        Ok(())
    }

    #[cfg(test)]
    fn seal_draft(&mut self, draft_id: &str, atom: &AtomRecord) -> Result<()> {
        self.seal_draft_with_links(draft_id, atom, &[], &[])
    }

    /// Seal through the production transaction while observing explicit lab
    /// fault boundaries.
    #[cfg(feature = "lab-fault-injection")]
    pub fn seal_core_draft_with_fault_injection(
        &mut self,
        draft_id: &str,
        body: &MemoryAtomBodyV1,
        state: &AtomStateV1,
        observer: &mut dyn FnMut(RepositoryFaultPointV1) -> Result<()>,
    ) -> Result<()> {
        validate_initial_atom_state(body, state)?;
        let atom = AtomRecord::from_core(body, state)?;
        let (artifact_links, relation_links) = self.prepare_core_links(body)?;
        self.seal_draft_with_links_observed(
            draft_id,
            &atom,
            &artifact_links,
            &relation_links,
            observer,
        )
    }

    pub(crate) fn seal_draft_with_links(
        &mut self,
        draft_id: &str,
        atom: &AtomRecord,
        artifact_links: &[AtomArtifactLink],
        relation_links: &[AtomRelationLink],
    ) -> Result<()> {
        self.seal_draft_with_links_observed(
            draft_id,
            atom,
            artifact_links,
            relation_links,
            &mut ignore_transaction_boundary,
        )
    }

    fn seal_draft_with_links_observed(
        &mut self,
        draft_id: &str,
        atom: &AtomRecord,
        artifact_links: &[AtomArtifactLink],
        relation_links: &[AtomRelationLink],
        observer: TransactionObserver<'_>,
    ) -> Result<()> {
        validate_declared_core_link_closure(atom, artifact_links, relation_links)?;
        self.validate_atom_links(&atom.memory_space_id, artifact_links, relation_links)?;
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let draft_scope = transaction
            .query_row(
                "SELECT memory_space_id, repository_id, task_id, agent_id, session_id, status
                 FROM episode_drafts WHERE draft_id = ?1",
                [draft_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| StoreError::DraftConflict(format!("unknown draft {draft_id}")))?;
        if draft_scope.5 != "open"
            || draft_scope.0 != atom.memory_space_id
            || draft_scope.1 != atom.repository_id
            || draft_scope.2 != atom.task_id
            || draft_scope.3 != atom.agent_id
            || Some(draft_scope.4) != atom.session_id
        {
            return Err(StoreError::DraftConflict(format!(
                "draft {draft_id} status or scope does not match sealed atom"
            )));
        }
        put_atom_transaction(&transaction, atom)?;
        insert_atom_links_transaction(&transaction, atom.atom_id, artifact_links, relation_links)?;
        observer(RepositoryFaultPointV1::DraftSealAfterAtomInsert)?;
        let changed = transaction.execute(
            "UPDATE episode_drafts SET status = 'sealed', sealed_atom_id = ?2
             WHERE draft_id = ?1 AND status = 'open'",
            params![draft_id, &atom.atom_id[..]],
        )?;
        if changed != 1 {
            return Err(StoreError::DraftConflict(format!(
                "draft {draft_id} was concurrently sealed"
            )));
        }
        observer(RepositoryFaultPointV1::DraftSealAfterStatusUpdate)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn seal_core_draft(
        &mut self,
        draft_id: &str,
        body: &MemoryAtomBodyV1,
        state: &AtomStateV1,
    ) -> Result<()> {
        validate_initial_atom_state(body, state)?;
        let atom = AtomRecord::from_core(body, state)?;
        let (artifact_links, relation_links) = self.prepare_core_links(body)?;
        self.seal_draft_with_links(draft_id, &atom, &artifact_links, &relation_links)
    }

    /// Atomically seals every target-sized atom in one deterministic episode
    /// chain and points the draft at the newest member. Relation targets that
    /// are part of the same chain are validated before the transaction and
    /// become resolvable after all immutable headers have been inserted.
    pub fn seal_core_draft_chain(
        &mut self,
        draft_id: &str,
        atoms: &[(MemoryAtomBodyV1, AtomStateV1)],
    ) -> Result<()> {
        validate_core_episode_chain(atoms)?;
        let mut batch_memory_spaces = BTreeMap::<[u8; 32], String>::new();
        let mut prepared = Vec::with_capacity(atoms.len());
        for (body, state) in atoms {
            let atom = AtomRecord::from_core(body, state)?;
            if batch_memory_spaces
                .insert(atom.atom_id, atom.memory_space_id.clone())
                .is_some()
            {
                return Err(StoreError::InvalidInput(
                    "episode chain contains a duplicate atom ID".to_owned(),
                ));
            }
            let (artifact_links, relation_links) = self.prepare_core_links(body)?;
            validate_declared_core_link_closure(&atom, &artifact_links, &relation_links)?;
            prepared.push((atom, artifact_links, relation_links));
        }
        for (atom, artifact_links, relation_links) in &prepared {
            self.validate_atom_links_with_batch(
                &atom.memory_space_id,
                artifact_links,
                relation_links,
                &batch_memory_spaces,
            )?;
        }

        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let draft_scope = transaction
            .query_row(
                "SELECT memory_space_id, repository_id, task_id, agent_id, session_id, status
                 FROM episode_drafts WHERE draft_id = ?1",
                [draft_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| StoreError::DraftConflict(format!("unknown draft {draft_id}")))?;
        for (atom, _, _) in &prepared {
            if draft_scope.5 != "open"
                || draft_scope.0 != atom.memory_space_id
                || draft_scope.1 != atom.repository_id
                || draft_scope.2 != atom.task_id
                || draft_scope.3 != atom.agent_id
                || Some(draft_scope.4.as_str()) != atom.session_id.as_deref()
            {
                return Err(StoreError::DraftConflict(format!(
                    "draft {draft_id} status or scope does not match every episode-chain atom"
                )));
            }
        }
        for (atom, _, _) in &prepared {
            put_atom_transaction(&transaction, atom)?;
        }
        for (atom, artifact_links, relation_links) in &prepared {
            insert_atom_links_transaction(
                &transaction,
                atom.atom_id,
                artifact_links,
                relation_links,
            )?;
        }
        let newest = prepared.last().ok_or_else(|| {
            StoreError::InvalidInput("episode chain must not be empty".to_owned())
        })?;
        let changed = transaction.execute(
            "UPDATE episode_drafts SET status = 'sealed', sealed_atom_id = ?2
             WHERE draft_id = ?1 AND status = 'open'",
            params![draft_id, &newest.0.atom_id[..]],
        )?;
        if changed != 1 {
            return Err(StoreError::DraftConflict(format!(
                "draft {draft_id} was concurrently sealed"
            )));
        }
        transaction.commit()?;
        Ok(())
    }

    fn prepare_core_links(
        &mut self,
        body: &MemoryAtomBodyV1,
    ) -> Result<(Vec<AtomArtifactLink>, Vec<AtomRelationLink>)> {
        let mut artifact_links = Vec::with_capacity(body.artifacts.len());
        for artifact in &body.artifacts {
            let digest = ArtifactDigest(artifact.digest.0);
            if let Some(inline) = artifact.inline_payload.as_deref() {
                if inline.len() > 4 * 1024 {
                    return Err(StoreError::InvalidInput(
                        "inline artifact exceeds 4 KiB".into(),
                    ));
                }
                let record = self.artifacts.ingest_bytes(inline)?;
                if record.digest != digest || record.byte_len != artifact.size_bytes {
                    return Err(StoreError::Integrity(
                        "inline artifact digest or declared size mismatch".into(),
                    ));
                }
                self.register_artifact(&record, body.interval.recorded_at_us)?;
            } else {
                let record = load_artifact_record(&self.connection, digest)?.ok_or_else(|| {
                    StoreError::ArtifactMissing {
                        path: self.artifacts.root().join(format!("digest:{digest}")),
                    }
                })?;
                if record.byte_len != artifact.size_bytes {
                    return Err(StoreError::Integrity(
                        "artifact declared size does not match CAS metadata".into(),
                    ));
                }
                self.artifacts.verify(&record)?;
            }
            artifact_links.push(AtomArtifactLink {
                artifact_digest: digest,
                role: artifact.media_type.clone(),
                retention_allowed: artifact.retention_allowed,
            });
        }
        let relation_links = body
            .relations
            .iter()
            .map(|relation| {
                Ok(AtomRelationLink {
                    target_atom_id: relation.target.digest().0,
                    relation_kind: relation_kind_text(relation.kind),
                    canonical_relation: relation.canonical_bytes()?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok((artifact_links, relation_links))
    }

    fn validate_atom_links(
        &self,
        source_memory_space_id: &str,
        artifact_links: &[AtomArtifactLink],
        relation_links: &[AtomRelationLink],
    ) -> Result<()> {
        self.validate_atom_links_with_batch(
            source_memory_space_id,
            artifact_links,
            relation_links,
            &BTreeMap::new(),
        )
    }

    fn validate_atom_links_with_batch(
        &self,
        source_memory_space_id: &str,
        artifact_links: &[AtomArtifactLink],
        relation_links: &[AtomRelationLink],
        batch_memory_spaces: &BTreeMap<[u8; 32], String>,
    ) -> Result<()> {
        let mut artifacts_seen = BTreeSet::new();
        for link in artifact_links {
            if link.role.is_empty()
                || !artifacts_seen.insert((link.artifact_digest, link.role.as_str()))
            {
                return Err(StoreError::InvalidInput(
                    "artifact links require non-empty, unique digest/role pairs".into(),
                ));
            }
            let record =
                load_artifact_record(&self.connection, link.artifact_digest)?.ok_or_else(|| {
                    StoreError::ArtifactMissing {
                        path: self
                            .artifacts
                            .root()
                            .join(format!("digest:{}", link.artifact_digest)),
                    }
                })?;
            let status: String = self.connection.query_row(
                "SELECT integrity_status FROM artifacts WHERE artifact_digest = ?1",
                [&link.artifact_digest.0[..]],
                |row| row.get(0),
            )?;
            if status != "verified" {
                return Err(StoreError::Integrity(format!(
                    "artifact {} is not verified",
                    link.artifact_digest
                )));
            }
            self.artifacts.verify(&record)?;
        }
        let mut relations_seen = BTreeSet::new();
        for link in relation_links {
            if link.relation_kind.is_empty()
                || link.canonical_relation.is_empty()
                || !relations_seen.insert((link.target_atom_id, link.relation_kind.as_str()))
            {
                return Err(StoreError::InvalidInput(
                    "relation links require non-empty, unique target/kind pairs and authority bytes"
                        .into(),
                ));
            }
            let batch_target_memory_space = batch_memory_spaces.get(&link.target_atom_id);
            let persisted_target_memory_space;
            let target_memory_space_id = if let Some(memory_space_id) = batch_target_memory_space {
                memory_space_id.as_str()
            } else {
                persisted_target_memory_space = load_atom_memory_space(
                    &self.connection,
                    link.target_atom_id,
                    "relation target",
                )?;
                persisted_target_memory_space.as_str()
            };
            if target_memory_space_id != source_memory_space_id {
                return Err(StoreError::InvalidInput(format!(
                    "relation target {} belongs to a different memory space",
                    hex::encode(link.target_atom_id)
                )));
            }
        }
        Ok(())
    }

    pub fn register_artifact(&mut self, record: &ArtifactRecord, created_at_us: u64) -> Result<()> {
        self.artifacts.verify(record)?;
        let byte_len = sqlite_integer(record.byte_len, "artifact byte_len")?;
        let created_at = sqlite_integer(created_at_us, "created_at_us")?;
        self.connection.execute(
            "INSERT INTO artifacts(
                artifact_digest, byte_len, relative_path, integrity_status, created_at_us
             ) VALUES (?1, ?2, ?3, 'verified', ?4)
             ON CONFLICT(artifact_digest) DO NOTHING",
            params![
                &record.digest.0[..],
                byte_len,
                record.relative_path,
                created_at
            ],
        )?;
        let persisted = load_artifact_record(&self.connection, record.digest)?
            .ok_or_else(|| StoreError::Integrity("artifact insert disappeared".into()))?;
        if persisted != *record {
            return Err(StoreError::ImmutableConflict(format!(
                "artifact {}",
                record.digest
            )));
        }
        Ok(())
    }

    #[cfg(test)]
    fn add_relation(
        &mut self,
        source_atom_id: [u8; 32],
        target_atom_id: [u8; 32],
        relation_kind: &str,
        canonical_relation: &[u8],
    ) -> Result<()> {
        if relation_kind.is_empty() {
            return Err(StoreError::InvalidInput(
                "relation kind must not be empty".into(),
            ));
        }
        let relation_digest = sha256(canonical_relation);
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        validate_relation_memory_space(&transaction, source_atom_id, target_atom_id)?;
        transaction.execute(
            "INSERT INTO atom_relations(
                source_atom_id, target_atom_id, relation_kind, relation_digest
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(source_atom_id, target_atom_id, relation_kind) DO NOTHING",
            params![
                &source_atom_id[..],
                &target_atom_id[..],
                relation_kind,
                &relation_digest[..]
            ],
        )?;
        let persisted: Vec<u8> = transaction.query_row(
            "SELECT relation_digest FROM atom_relations
             WHERE source_atom_id = ?1 AND target_atom_id = ?2 AND relation_kind = ?3",
            params![&source_atom_id[..], &target_atom_id[..], relation_kind],
            |row| row.get(0),
        )?;
        if persisted != relation_digest {
            return Err(StoreError::ImmutableConflict(format!(
                "relation {} -> {} ({relation_kind})",
                hex::encode(source_atom_id),
                hex::encode(target_atom_id)
            )));
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn add_feedback(
        &mut self,
        feedback_id: [u8; 32],
        atom_id: [u8; 32],
        as_of_us: u64,
        positive: bool,
        source: &str,
        evidence_digest: [u8; 32],
    ) -> Result<()> {
        if source.is_empty() {
            return Err(StoreError::InvalidInput(
                "feedback source must not be empty".into(),
            ));
        }
        let outcome = if positive { 1_i64 } else { -1_i64 };
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let inserted = transaction.execute(
            "INSERT INTO feedback(
                feedback_id, atom_id, as_of_us, outcome, source, evidence_digest
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(feedback_id) DO NOTHING",
            params![
                &feedback_id[..],
                &atom_id[..],
                sqlite_integer(as_of_us, "as_of_us")?,
                outcome,
                source,
                &evidence_digest[..]
            ],
        )?;
        if inserted == 0 {
            let persisted = transaction.query_row(
                "SELECT atom_id, as_of_us, outcome, source, evidence_digest
                 FROM feedback WHERE feedback_id = ?1",
                [&feedback_id[..]],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Vec<u8>>(4)?,
                    ))
                },
            )?;
            if persisted.0 != atom_id
                || persisted.1 != sqlite_integer(as_of_us, "as_of_us")?
                || persisted.2 != outcome
                || persisted.3 != source
                || persisted.4 != evidence_digest
            {
                return Err(StoreError::ImmutableConflict(format!(
                    "feedback {}",
                    hex::encode(feedback_id)
                )));
            }
            return Ok(());
        }
        let column = if positive {
            "feedback_positive"
        } else {
            "feedback_negative"
        };
        let changed = transaction.execute(
            &format!("UPDATE atom_state SET {column} = {column} + 1 WHERE atom_id = ?1"),
            [&atom_id[..]],
        )?;
        if changed != 1 {
            return Err(StoreError::Integrity(
                "feedback target atom state is missing".into(),
            ));
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn record_retrievals(&mut self, atom_ids: &[[u8; 32]]) -> Result<()> {
        let unique: BTreeSet<_> = atom_ids.iter().copied().collect();
        if unique.len() != atom_ids.len() {
            return Err(StoreError::InvalidInput(
                "retrieval atom IDs must be unique".into(),
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        for atom_id in atom_ids {
            let changed = transaction.execute(
                "UPDATE atom_state SET retrieval_count = retrieval_count + 1
                 WHERE atom_id = ?1 AND content_status = 'present'",
                [&atom_id[..]],
            )?;
            if changed != 1 {
                return Err(StoreError::Integrity(format!(
                    "retrieval target {} is unavailable",
                    hex::encode(atom_id)
                )));
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn search_candidates(
        &self,
        scope: &SearchScope,
        fts_query: &str,
        limit: u16,
    ) -> Result<Vec<SearchCandidate>> {
        if limit == 0 || limit > 512 {
            return Err(StoreError::InvalidInput(
                "candidate limit must be in 1..=512".into(),
            ));
        }
        if !scope.memory_space_wide && scope.repository_id.is_none() {
            return Err(StoreError::InvalidInput(
                "repository-local search requires repository_id".into(),
            ));
        }
        if fts_query.trim().is_empty() {
            return Err(StoreError::InvalidInput(
                "FTS query must not be empty".into(),
            ));
        }
        let sql = "SELECT h.atom_id, h.atom_kind, b.json_projection,
                          s.feedback_positive, s.feedback_negative, s.retrieval_count
                   FROM atom_search AS f
                   JOIN atom_headers AS h ON lower(hex(h.atom_id)) = f.atom_id
                   JOIN atom_bodies AS b ON b.atom_id = h.atom_id
                   JOIN atom_state AS s ON s.atom_id = h.atom_id
                   WHERE atom_search MATCH ?1
                     AND h.memory_space_id = ?2
                     AND (?4 = 1 OR h.repository_id = ?5)
                     AND s.content_status = 'present'
                   ORDER BY h.atom_id ASC
                   LIMIT ?3";
        let mut statement = self.connection.prepare(sql)?;
        let rows = statement.query_map(
            params![
                fts_query,
                scope.memory_space_id,
                i64::from(limit),
                i64::from(scope.memory_space_wide),
                scope.repository_id
            ],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )?;
        let mut candidates = Vec::new();
        for row in rows {
            let (atom_id, atom_kind, json_projection, positive, negative, retrievals) = row?;
            candidates.push(SearchCandidate {
                atom_id: digest_array(&atom_id, "atom_id")?,
                atom_kind,
                json_projection,
                feedback_positive: unsigned_integer(positive, "feedback_positive")?,
                feedback_negative: unsigned_integer(negative, "feedback_negative")?,
                retrieval_count: unsigned_integer(retrievals, "retrieval_count")?,
            });
        }
        Ok(candidates)
    }

    /// Builds the complete, integrity-checked core candidate envelope for one
    /// retrieval request. Candidate generation is store-owned: callers cannot
    /// inject bodies, mutable state, scope, or lexical scores.
    pub fn retrieval_candidates(
        &self,
        query: &RetrievalQueryV1,
        policy: &PolicyV1,
    ) -> Result<RetrievalCandidatesV1> {
        validate_retrieval_request(query, policy)?;

        let tokens = normalized_query_tokens(&query.terms);
        let mut signals = BTreeMap::<[u8; 32], RetrievalGenerationSignals>::new();
        self.collect_fts_generation_matches(query, &tokens, &mut signals)?;
        self.collect_structured_generation_matches(
            query,
            "topic_keys_json",
            &query.topic_keys,
            |entry| entry.topics = entry.topics.saturating_add(1),
            &mut signals,
        )?;
        self.collect_structured_generation_matches(
            query,
            "entity_ids_json",
            &query.entity_ids,
            |entry| entry.entities = entry.entities.saturating_add(1),
            &mut signals,
        )?;

        let token_count = u64::try_from(tokens.len()).unwrap_or(u64::MAX);
        let topic_count = u64::try_from(query.topic_keys.len()).unwrap_or(u64::MAX);
        let entity_count = u64::try_from(query.entity_ids.len()).unwrap_or(u64::MAX);
        let mut selected = signals
            .into_iter()
            .map(|(atom_id, signals)| {
                let lexical_score = coverage_score(signals.lexical, token_count);
                let topic_score = coverage_score(signals.topics, topic_count);
                let entity_score = coverage_score(signals.entities, entity_count);
                let generation_score = u64::from(
                    policy
                        .retrieval_lexical_weight
                        .multiply(lexical_score)
                        .get(),
                )
                .saturating_add(u64::from(
                    policy.retrieval_semantic_weight.multiply(topic_score).get(),
                ))
                .saturating_add(u64::from(
                    policy.retrieval_entity_weight.multiply(entity_score).get(),
                ));
                (atom_id, lexical_score, generation_score)
            })
            .collect::<Vec<_>>();
        selected.sort_unstable_by(|left, right| {
            right.2.cmp(&left.2).then_with(|| left.0.cmp(&right.0))
        });
        selected.truncate(policy.retrieval_candidate_max.min(512));

        let mut ranked = Vec::with_capacity(selected.len());
        for (atom_id, lexical_score, _) in selected {
            ranked.push(self.load_retrieval_candidate(atom_id, lexical_score)?);
        }
        ranked.sort_unstable_by_key(|candidate| candidate.atom_id);

        let mut episodic_pool = Vec::new();
        if query.spontaneous.is_some() {
            for atom_id in self.episodic_pool_atom_ids(query)? {
                let candidate = self.load_retrieval_candidate(atom_id, Ppm::ZERO)?;
                if candidate.body.retention_permission
                    != RetentionPermissionV1::SemanticAndExactAllowed
                {
                    return Err(StoreError::Integrity(format!(
                        "episodic LTM atom {} lacks exact-retention permission",
                        candidate.atom_id
                    )));
                }
                episodic_pool.push(candidate);
            }
        }

        Ok(RetrievalCandidatesV1 {
            ranked,
            episodic_pool,
        })
    }

    /// Generates candidates, executes the pure ranking contract, then records
    /// successful recalls. Retrieval counts are observational state only and
    /// are never read by the utility score.
    pub fn retrieve(
        &mut self,
        query: &RetrievalQueryV1,
        policy: &PolicyV1,
    ) -> Result<RetrievalResultV1> {
        let candidates = self.retrieval_candidates(query, policy)?;
        let result = rank_retrieval(query, &candidates, policy)?;
        let mut recalled = result
            .hits
            .iter()
            .map(|hit| hit.atom_id.digest().0)
            .collect::<BTreeSet<_>>();
        if let Some(spontaneous) = result.spontaneous.as_ref() {
            recalled.extend(spontaneous.hits.iter().map(|hit| hit.atom_id.digest().0));
        }
        self.record_retrievals(&recalled.into_iter().collect::<Vec<_>>())?;
        Ok(result)
    }

    fn collect_fts_generation_matches(
        &self,
        query: &RetrievalQueryV1,
        tokens: &BTreeSet<String>,
        signals: &mut BTreeMap<[u8; 32], RetrievalGenerationSignals>,
    ) -> Result<()> {
        let sql = "SELECT h.atom_id
                   FROM atom_search AS f
                   JOIN atom_headers AS h ON lower(hex(h.atom_id)) = f.atom_id
                   JOIN atom_state AS s ON s.atom_id = h.atom_id
                   WHERE atom_search MATCH ?1
                     AND h.memory_space_id = ?2
                     AND (?3 = 1 OR h.repository_id = ?4)
                     AND s.content_status = 'present'
                   ORDER BY h.atom_id ASC";
        let mut statement = self.connection.prepare(sql)?;
        for token in tokens {
            let fts_query = format!("\"{}\"", token.replace('"', "\"\""));
            let rows = statement.query_map(
                params![
                    fts_query,
                    query.memory_space_id,
                    i64::from(query.memory_space_wide),
                    query.repository_id
                ],
                |row| row.get::<_, Vec<u8>>(0),
            )?;
            let mut matched_once = BTreeSet::new();
            for row in rows {
                matched_once.insert(digest_array(&row?, "atom_id")?);
            }
            for atom_id in matched_once {
                let entry = signals.entry(atom_id).or_default();
                entry.lexical = entry.lexical.saturating_add(1);
            }
        }
        Ok(())
    }

    fn collect_structured_generation_matches(
        &self,
        query: &RetrievalQueryV1,
        json_column: &'static str,
        keys: &BTreeSet<String>,
        increment: impl Fn(&mut RetrievalGenerationSignals),
        signals: &mut BTreeMap<[u8; 32], RetrievalGenerationSignals>,
    ) -> Result<()> {
        if !matches!(json_column, "topic_keys_json" | "entity_ids_json") {
            return Err(StoreError::InvalidInput(
                "unsupported structured retrieval column".to_owned(),
            ));
        }
        let sql = format!(
            "SELECT h.atom_id
             FROM search_projection AS p
             JOIN atom_headers AS h ON h.atom_id = p.atom_id
             JOIN atom_state AS s ON s.atom_id = h.atom_id
             WHERE h.memory_space_id = ?1
               AND (?2 = 1 OR h.repository_id = ?3)
               AND s.content_status = 'present'
               AND EXISTS (
                   SELECT 1 FROM json_each(p.{json_column}) AS member
                   WHERE member.type = 'text' AND member.value = ?4
               )
             ORDER BY h.atom_id ASC"
        );
        let mut statement = self.connection.prepare(&sql)?;
        for key in keys {
            let rows = statement.query_map(
                params![
                    query.memory_space_id,
                    i64::from(query.memory_space_wide),
                    query.repository_id,
                    key
                ],
                |row| row.get::<_, Vec<u8>>(0),
            )?;
            let mut matched_once = BTreeSet::new();
            for row in rows {
                matched_once.insert(digest_array(&row?, "atom_id")?);
            }
            for atom_id in matched_once {
                increment(signals.entry(atom_id).or_default());
            }
        }
        Ok(())
    }

    fn episodic_pool_atom_ids(&self, query: &RetrievalQueryV1) -> Result<Vec<[u8; 32]>> {
        let mut statement = self.connection.prepare(
            "SELECT h.atom_id
             FROM atom_headers AS h
             JOIN atom_state AS s ON s.atom_id = h.atom_id
             WHERE h.memory_space_id = ?1
               AND (?2 = 1 OR h.repository_id = ?3)
               AND h.atom_kind = 'episode'
               AND s.retention_tier IN ('ltm_episodic', 'ltm_dual')
               AND s.content_status = 'present'
             ORDER BY h.atom_id ASC",
        )?;
        let rows = statement.query_map(
            params![
                query.memory_space_id,
                i64::from(query.memory_space_wide),
                query.repository_id
            ],
            |row| row.get::<_, Vec<u8>>(0),
        )?;
        let mut atom_ids = Vec::new();
        for row in rows {
            atom_ids.push(digest_array(&row?, "atom_id")?);
        }
        Ok(atom_ids)
    }

    fn load_retrieval_candidate(
        &self,
        atom_id: [u8; 32],
        lexical_score: Ppm,
    ) -> Result<RetrievalCandidateV1> {
        let stored = self.connection.query_row(
            "SELECT h.atom_kind, h.memory_space_id, h.repository_id, h.task_id, h.agent_id,
                    h.session_id, h.recorded_at_us, h.body_digest, h.body_bytes,
                    h.retention_permission, h.provenance_digest,
                    b.canonical_body, b.json_projection,
                    s.retention_tier, s.content_status, s.expires_at_us, s.superseded_by,
                    s.feedback_positive, s.feedback_negative, s.retrieval_count,
                    s.retirement_run_id,
                    p.searchable_text, p.topic_keys_json, p.entity_ids_json, p.outcome_class,
                    p.projection_digest
             FROM atom_headers AS h
             JOIN atom_bodies AS b ON b.atom_id = h.atom_id
             JOIN atom_state AS s ON s.atom_id = h.atom_id
             JOIN search_projection AS p ON p.atom_id = h.atom_id
             WHERE h.atom_id = ?1",
            [&atom_id[..]],
            |row| {
                Ok(StoredRetrievalAtom {
                    atom_kind: row.get(0)?,
                    memory_space_id: row.get(1)?,
                    repository_id: row.get(2)?,
                    task_id: row.get(3)?,
                    agent_id: row.get(4)?,
                    session_id: row.get(5)?,
                    recorded_at_us: row.get(6)?,
                    body_digest: row.get(7)?,
                    body_bytes: row.get(8)?,
                    retention_permission: row.get(9)?,
                    provenance_digest: row.get(10)?,
                    canonical_body: row.get(11)?,
                    json_projection: row.get(12)?,
                    retention_tier: row.get(13)?,
                    content_status: row.get(14)?,
                    expires_at_us: row.get(15)?,
                    superseded_by: row.get(16)?,
                    feedback_positive: row.get(17)?,
                    feedback_negative: row.get(18)?,
                    retrieval_count: row.get(19)?,
                    retirement_run_id: row.get(20)?,
                    searchable_text: row.get(21)?,
                    topic_keys_json: row.get(22)?,
                    entity_ids_json: row.get(23)?,
                    outcome_class: row.get(24)?,
                    projection_digest: row.get(25)?,
                })
            },
        )?;
        let body: MemoryAtomBodyV1 = serde_json::from_str(&stored.json_projection)?;
        let state = AtomStateV1 {
            retention_tier: retention_tier_from_store(&stored.retention_tier)?,
            content_status: content_status_from_store(&stored.content_status)?,
            expires_at_us: stored
                .expires_at_us
                .map(|value| unsigned_integer(value, "expires_at_us"))
                .transpose()?,
            superseded_by: stored
                .superseded_by
                .as_deref()
                .map(|value| digest_array(value, "superseded_by"))
                .transpose()?
                .map(|value| AtomId(Digest32(value))),
            feedback_positive: unsigned_integer(stored.feedback_positive, "feedback_positive")?,
            feedback_negative: unsigned_integer(stored.feedback_negative, "feedback_negative")?,
            retrieval_count: unsigned_integer(stored.retrieval_count, "retrieval_count")?,
            retirement_run_id: stored
                .retirement_run_id
                .as_deref()
                .map(|value| digest_array(value, "retirement_run_id"))
                .transpose()?
                .map(Digest32),
        };
        let expected = AtomRecord::from_core(&body, &state)?;
        let stored_body_digest = digest_array(&stored.body_digest, "body_digest")?;
        let stored_provenance_digest =
            digest_array(&stored.provenance_digest, "provenance_digest")?;
        let stored_projection_digest =
            digest_array(&stored.projection_digest, "projection_digest")?;
        let body_bytes = unsigned_integer(stored.body_bytes, "body_bytes")?;
        let expected_body_bytes = u64::try_from(expected.canonical_body.len()).map_err(|_| {
            StoreError::Integrity("retrieval atom body length exceeds u64".to_owned())
        })?;
        let authority_matches = expected.atom_id == atom_id
            && expected.atom_kind == stored.atom_kind
            && expected.memory_space_id == stored.memory_space_id
            && expected.repository_id == stored.repository_id
            && expected.task_id == stored.task_id
            && expected.agent_id == stored.agent_id
            && expected.session_id == stored.session_id
            && expected.recorded_at_us
                == unsigned_integer(stored.recorded_at_us, "recorded_at_us")?
            && expected.body_digest == stored_body_digest
            && expected_body_bytes == body_bytes
            && expected.retention_permission == stored.retention_permission
            && expected.provenance_digest == stored_provenance_digest
            && expected.canonical_body == stored.canonical_body
            && expected.json_projection == stored.json_projection
            && expected.retention_tier == stored.retention_tier
            && expected.content_status == stored.content_status
            && expected.expires_at_us == state.expires_at_us
            && expected.searchable_text == stored.searchable_text
            && expected.topic_keys_json == stored.topic_keys_json
            && expected.entity_ids_json == stored.entity_ids_json
            && expected.outcome_class == stored.outcome_class
            && sha256_projection(&expected) == stored_projection_digest;
        if !authority_matches {
            return Err(StoreError::Integrity(format!(
                "retrieval atom {} failed body, state, header, or search-projection closure",
                hex::encode(atom_id)
            )));
        }
        let fts_rows = self.connection.query_row(
            "SELECT COUNT(*), MIN(searchable_text), MAX(searchable_text)
             FROM atom_search WHERE atom_id = ?1",
            [hex::encode(atom_id)],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )?;
        if fts_rows.0 != 1
            || fts_rows.1.as_deref() != Some(expected.searchable_text.as_str())
            || fts_rows.2.as_deref() != Some(expected.searchable_text.as_str())
        {
            return Err(StoreError::Integrity(format!(
                "retrieval atom {} failed FTS projection closure",
                hex::encode(atom_id)
            )));
        }
        let integrity = self.core_artifact_integrity(
            atom_id,
            &body,
            state.retention_tier == RetentionTierV1::ShortTerm,
        )?;
        if integrity != IntegrityStatusV1::Complete {
            return Err(StoreError::Integrity(format!(
                "retrieval atom {} is not integrity-complete: {integrity:?}",
                hex::encode(atom_id)
            )));
        }
        Ok(RetrievalCandidateV1 {
            atom_id: AtomId(Digest32(atom_id)),
            body,
            state,
            lexical_score,
            integrity,
        })
    }

    pub fn snapshot(&self, key: &CohortKey) -> Result<CohortSnapshot> {
        snapshot_connection(&self.connection, key)
    }

    pub fn snapshot_core(&self, key: &CohortKey) -> Result<RetirementSnapshotV1> {
        let persisted = self.snapshot(key)?;
        let mut atoms = Vec::with_capacity(persisted.atoms.len());
        for header in &persisted.atoms {
            atoms.push(self.load_core_candidate(header.atom_id)?);
        }
        Ok(RetirementSnapshotV1 {
            contract_version: "retirement-snapshot-v1".into(),
            memory_space_id: key.memory_space_id.clone(),
            cohort_day: key.cohort_day,
            as_of_us: key.as_of_us,
            atoms,
        })
    }

    pub fn apply_core_plan(&mut self, plan: &RetirementPlanV1) -> Result<ApplyRetirementOutcomeV1> {
        self.apply_core_plan_observed(plan, &mut ignore_transaction_boundary)
    }

    /// Apply a validated core plan through the production transaction while
    /// observing explicit lab fault boundaries.
    #[cfg(feature = "lab-fault-injection")]
    pub fn apply_core_plan_with_fault_injection(
        &mut self,
        plan: &RetirementPlanV1,
        observer: TransactionObserver<'_>,
    ) -> Result<ApplyRetirementOutcomeV1> {
        self.apply_core_plan_observed(plan, observer)
    }

    fn apply_core_plan_observed(
        &mut self,
        plan: &RetirementPlanV1,
        observer: TransactionObserver<'_>,
    ) -> Result<ApplyRetirementOutcomeV1> {
        let plan_digest = plan.digest()?;
        let prior = self
            .connection
            .query_row(
                "SELECT run_id, projected_state_digest, status
                 FROM retirement_runs WHERE plan_digest = ?1",
                [plan_digest.as_bytes()],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        if let Some((run_id, state_digest, status)) = prior {
            if state_digest == plan.projected_state_digest.0 && status == "applied" {
                let run_id = digest_array(&run_id, "run_id")?;
                verify_persisted_core_receipt(&self.connection, run_id, Some(plan))?;
                return Ok(outcome_from_plan(plan, true));
            }
            return Err(StoreError::RetirementConflict);
        }
        let cohort_already_retired = self
            .connection
            .query_row(
                "SELECT 1 FROM retirement_runs
                 WHERE memory_space_id = ?1 AND cohort_day = ?2",
                params![
                    plan.memory_space_id,
                    sqlite_integer(plan.cohort_day, "cohort_day")?
                ],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some();
        if cohort_already_retired {
            return Err(StoreError::RetirementConflict);
        }
        let key = CohortKey {
            memory_space_id: plan.memory_space_id.clone(),
            cohort_day: plan.cohort_day,
            as_of_us: plan.as_of_us,
        };
        let local_snapshot = self.snapshot(&key)?;
        let core_snapshot = self.snapshot_core(&key)?;
        let observed_input_digest = naome_memory_core::retirement_input_digest(&core_snapshot)?;
        if observed_input_digest != plan.input_set_digest {
            return Err(StoreError::StalePlan {
                expected: plan.input_set_digest.to_hex(),
                actual: observed_input_digest.to_hex(),
            });
        }
        let expected_plan = plan_retirement(&core_snapshot, &PolicyV1::poc_v1(), plan.seed)?;
        if expected_plan != *plan {
            return Err(StoreError::InvalidRetirementPlan(
                "plan does not match deterministic recomputation from the current cohort"
                    .to_owned(),
            ));
        }
        let expected = outcome_from_plan(plan, false);
        let mut receipt_repository = ExpectedOutcomeRepository(expected.clone());
        let receipt = naome_memory_core::apply_retirement(&mut receipt_repository, plan)?;
        let commit = build_core_commit(plan, &receipt, &local_snapshot)?;
        let apply_outcome = self.apply_retirement_observed(&commit, observer)?;
        Ok(ApplyRetirementOutcomeV1 {
            already_applied: apply_outcome == ApplyOutcome::AlreadyApplied,
            ..expected
        })
    }

    fn load_core_candidate(&self, atom_id: [u8; 32]) -> Result<RetirementCandidateV1> {
        let persisted = self.connection.query_row(
            "SELECT b.json_projection, s.retention_tier, s.content_status, s.expires_at_us,
                    s.superseded_by, s.feedback_positive, s.feedback_negative,
                    s.retrieval_count, s.retirement_run_id
             FROM atom_headers AS h
             JOIN atom_bodies AS b ON b.atom_id = h.atom_id
             JOIN atom_state AS s ON s.atom_id = h.atom_id
             WHERE h.atom_id = ?1",
            [&atom_id[..]],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<Vec<u8>>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<Vec<u8>>>(8)?,
                ))
            },
        )?;
        let body: MemoryAtomBodyV1 = serde_json::from_str(&persisted.0)?;
        let computed_id = body.atom_id()?;
        if computed_id.digest().0 != atom_id {
            return Err(StoreError::Integrity(format!(
                "atom {} JSON projection failed authority verification",
                hex::encode(atom_id)
            )));
        }
        let state = AtomStateV1 {
            retention_tier: retention_tier_from_store(&persisted.1)?,
            content_status: content_status_from_store(&persisted.2)?,
            expires_at_us: persisted
                .3
                .map(|value| unsigned_integer(value, "expires_at_us"))
                .transpose()?,
            superseded_by: persisted
                .4
                .as_deref()
                .map(|value| digest_array(value, "superseded_by"))
                .transpose()?
                .map(|value| AtomId(Digest32(value))),
            feedback_positive: unsigned_integer(persisted.5, "feedback_positive")?,
            feedback_negative: unsigned_integer(persisted.6, "feedback_negative")?,
            retrieval_count: unsigned_integer(persisted.7, "retrieval_count")?,
            retirement_run_id: persisted
                .8
                .as_deref()
                .map(|value| digest_array(value, "retirement_run_id"))
                .transpose()?
                .map(Digest32),
        };
        let integrity = self.core_artifact_integrity(atom_id, &body, true)?;
        let exact_package_bytes = body.exact_package_bytes()?;
        Ok(RetirementCandidateV1 {
            atom_id: computed_id,
            body_digest: computed_id.digest(),
            body: Some(body),
            state,
            integrity,
            exact_package_bytes,
        })
    }

    fn core_artifact_integrity(
        &self,
        atom_id: [u8; 32],
        body: &MemoryAtomBodyV1,
        require_all_declared: bool,
    ) -> Result<IntegrityStatusV1> {
        for artifact in &body.artifacts {
            if let Some(inline) = artifact.inline_payload.as_deref() {
                if u64::try_from(inline.len()).ok() != Some(artifact.size_bytes)
                    || sha256(inline) != artifact.digest.0
                {
                    return Ok(IntegrityStatusV1::DigestMismatch);
                }
                continue;
            }
            let digest = ArtifactDigest(artifact.digest.0);
            let linked = self
                .connection
                .query_row(
                    "SELECT 1 FROM artifact_refs WHERE artifact_digest = ?1 AND atom_id = ?2",
                    params![artifact.digest.as_bytes(), &atom_id[..]],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .is_some();
            if !require_all_declared && !artifact.retention_allowed {
                if linked {
                    return Ok(IntegrityStatusV1::DigestMismatch);
                }
                continue;
            }
            let Some(record) = load_artifact_record(&self.connection, digest)? else {
                return Ok(IntegrityStatusV1::MissingArtifact);
            };
            if !linked || record.byte_len != artifact.size_bytes {
                return Ok(IntegrityStatusV1::MissingArtifact);
            }
            match self.artifacts.verify(&record) {
                Ok(()) => {}
                Err(StoreError::ArtifactMissing { .. }) => {
                    return Ok(IntegrityStatusV1::MissingArtifact);
                }
                Err(StoreError::ArtifactDigestMismatch { .. }) => {
                    return Ok(IntegrityStatusV1::DigestMismatch);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(IntegrityStatusV1::Complete)
    }

    #[cfg(test)]
    fn apply_retirement(&mut self, commit: &RetirementCommit) -> Result<ApplyOutcome> {
        self.apply_retirement_observed(commit, &mut ignore_transaction_boundary)
    }

    fn apply_retirement_observed(
        &mut self,
        commit: &RetirementCommit,
        observer: TransactionObserver<'_>,
    ) -> Result<ApplyOutcome> {
        validate_retirement_commit(commit)?;
        verify_persisted_poc_policy(&self.connection)?;
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

        if let Some((plan_digest, receipt_digest, status)) = transaction
            .query_row(
                "SELECT plan_digest, receipt_digest, status FROM retirement_runs WHERE run_id = ?1",
                [&commit.run_id[..]],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, Option<Vec<u8>>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
        {
            if plan_digest == commit.plan_digest
                && receipt_digest.as_deref() == Some(&commit.receipt.receipt_digest[..])
                && status == "applied"
            {
                verify_persisted_core_receipt(&transaction, commit.run_id, None)?;
                return Ok(ApplyOutcome::AlreadyApplied);
            }
            return Err(StoreError::RetirementConflict);
        }

        let existing_cohort_run = transaction
            .query_row(
                "SELECT run_id FROM retirement_runs
                 WHERE memory_space_id = ?1 AND cohort_day = ?2",
                params![
                    commit.key.memory_space_id,
                    sqlite_integer(commit.key.cohort_day, "cohort_day")?
                ],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?;
        if existing_cohort_run.is_some() {
            return Err(StoreError::RetirementConflict);
        }

        let current = snapshot_connection(&transaction, &commit.key)?;
        if current.input_set_digest != commit.expected_input_set_digest {
            return Err(StoreError::StalePlan {
                expected: hex::encode(commit.expected_input_set_digest),
                actual: hex::encode(current.input_set_digest),
            });
        }
        validate_decision_closure(&current, commit)?;
        validate_pins(&transaction, &self.artifacts, &current, commit)?;

        insert_retirement_run(&transaction, commit)?;
        insert_retirement_post_state(&transaction, commit)?;
        observer(RepositoryFaultPointV1::RetirementAfterRunCreation)?;
        for semantic_atom in &commit.semantic_atoms {
            if semantic_atom.atom_kind != "semantic"
                || semantic_atom.memory_space_id != commit.key.memory_space_id
                || semantic_atom.retention_tier != "ltm_semantic"
                || semantic_atom.content_status != "present"
            {
                return Err(StoreError::InvalidRetirementPlan(
                    "semantic output has invalid kind, scope, tier, or state".into(),
                ));
            }
            let semantic_body: MemoryAtomBodyV1 =
                serde_json::from_str(&semantic_atom.json_projection)?;
            if !semantic_body.artifacts.is_empty() {
                return Err(StoreError::InvalidRetirementPlan(
                    "poc-v1 semantic output cannot declare artifacts".into(),
                ));
            }
            let relation_links = semantic_body
                .relations
                .iter()
                .map(|relation| {
                    Ok(AtomRelationLink {
                        target_atom_id: relation.target.digest().0,
                        relation_kind: relation_kind_text(relation.kind),
                        canonical_relation: relation.canonical_bytes()?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            validate_declared_core_link_closure(semantic_atom, &[], &relation_links)?;
            if !put_atom_transaction(&transaction, semantic_atom)? {
                return Err(StoreError::RetirementConflict);
            }
            insert_atom_links_transaction(
                &transaction,
                semantic_atom.atom_id,
                &[],
                &relation_links,
            )?;
        }
        observer(RepositoryFaultPointV1::RetirementAfterSemanticInsertion)?;

        persist_retirement_decisions(&transaction, commit)?;
        observer(RepositoryFaultPointV1::RetirementAfterDecisionPersistence)?;
        apply_decisions_set_based(&transaction, commit, observer)?;

        {
            let mut insert_pin = transaction.prepare(
                "INSERT INTO artifact_pins(artifact_digest, run_id, atom_id, pinned_at_us)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            let mut clear_gc_mark = transaction.prepare(
                "UPDATE artifacts SET gc_marked_at_us = NULL WHERE artifact_digest = ?1",
            )?;
            let pinned_at_us = sqlite_integer(commit.key.as_of_us, "as_of_us")?;
            for pin in &commit.artifact_pins {
                ensure_exact_change_count(
                    "artifact pin insertion",
                    insert_pin.execute(params![
                        &pin.artifact_digest.0[..],
                        &commit.run_id[..],
                        &pin.atom_id[..],
                        pinned_at_us
                    ])?,
                    1,
                )?;
                ensure_exact_change_count(
                    "artifact GC mark clearing",
                    clear_gc_mark.execute([&pin.artifact_digest.0[..]])?,
                    1,
                )?;
            }
        }
        observer(RepositoryFaultPointV1::RetirementAfterArtifactPins)?;

        validate_json(&commit.receipt.json_projection, "receipt")?;
        ensure_exact_change_count(
            "proof receipt insertion",
            transaction.execute(
                "INSERT INTO proof_receipts(
                receipt_digest, run_id, canonical_receipt, json_projection, closure_digest,
                created_at_us
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    &commit.receipt.receipt_digest[..],
                    &commit.run_id[..],
                    commit.receipt.canonical_receipt,
                    commit.receipt.json_projection,
                    &commit.receipt.closure_digest[..],
                    sqlite_integer(commit.key.as_of_us, "as_of_us")?
                ],
            )?,
            1,
        )?;
        observer(RepositoryFaultPointV1::RetirementAfterReceiptInsertion)?;
        ensure_exact_change_count(
            "retirement run completion",
            transaction.execute(
                "UPDATE retirement_runs
             SET status = 'applied', completed_at_us = ?2, receipt_digest = ?3
             WHERE run_id = ?1 AND status = 'applying'",
                params![
                    &commit.run_id[..],
                    sqlite_integer(commit.key.as_of_us, "as_of_us")?,
                    &commit.receipt.receipt_digest[..]
                ],
            )?,
            1,
        )?;
        observer(RepositoryFaultPointV1::RetirementAfterRunCompletion)?;
        verify_persisted_core_receipt(&transaction, commit.run_id, None)?;
        observer(RepositoryFaultPointV1::RetirementBeforeCommit)?;
        transaction.commit()?;
        Ok(ApplyOutcome::Applied)
    }

    pub fn check_integrity(&self, verify_artifacts: bool) -> Result<IntegrityReport> {
        let sqlite_integrity: String =
            self.connection
                .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if sqlite_integrity != "ok" {
            return Err(StoreError::Integrity(format!(
                "PRAGMA integrity_check returned {sqlite_integrity}"
            )));
        }

        let mut foreign_key_statement = self.connection.prepare("PRAGMA foreign_key_check")?;
        let violations = foreign_key_statement.query_map([], |row| {
            Ok(format!(
                "table={} rowid={} parent={} fk={}",
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?
                    .map_or_else(|| "none".into(), |value| value.to_string()),
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?
            ))
        })?;
        let mut foreign_key_violations = Vec::new();
        for violation in violations {
            foreign_key_violations.push(violation?);
        }
        if !foreign_key_violations.is_empty() {
            return Err(StoreError::Integrity(format!(
                "foreign key violations: {}",
                foreign_key_violations.join("; ")
            )));
        }

        verify_persisted_poc_policy(&self.connection)?;

        let verified_artifacts =
            verify_all_atom_closure(&self.connection, &self.artifacts, verify_artifacts)?;

        let applied_run_ids = {
            let mut statement = self.connection.prepare(
                "SELECT run_id FROM retirement_runs WHERE status = 'applied' ORDER BY run_id",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
            let mut run_ids = Vec::new();
            for row in rows {
                run_ids.push(digest_array(&row?, "run_id")?);
            }
            run_ids
        };
        for run_id in applied_run_ids {
            verify_persisted_core_receipt(&self.connection, run_id, None)?;
        }
        Ok(IntegrityReport {
            schema_version: SCHEMA_VERSION,
            sqlite_integrity,
            foreign_key_violations,
            verified_artifacts,
        })
    }

    pub fn garbage_collect(
        &mut self,
        as_of_us: u64,
        grace_period_us: u64,
        dry_run: bool,
    ) -> Result<GcReport> {
        self.garbage_collect_observed(
            as_of_us,
            grace_period_us,
            dry_run,
            &mut ignore_artifact_gc_boundary,
        )
    }

    /// Run artifact GC while observing the boundary after a durable unlink
    /// and before the surrounding `SQLite` transaction commits.
    ///
    /// This method exists only in the synthetic crash profile. Returning from
    /// the observer continues the production GC path unchanged; a hard-crash
    /// harness blocks inside the observer and terminates the child process.
    #[cfg(feature = "lab-fault-injection")]
    pub fn garbage_collect_with_fault_injection(
        &mut self,
        as_of_us: u64,
        grace_period_us: u64,
        dry_run: bool,
        observer: &mut dyn FnMut() -> Result<()>,
    ) -> Result<GcReport> {
        self.garbage_collect_observed(as_of_us, grace_period_us, dry_run, observer)
    }

    fn garbage_collect_observed(
        &mut self,
        as_of_us: u64,
        grace_period_us: u64,
        dry_run: bool,
        observer: &mut dyn FnMut() -> Result<()>,
    ) -> Result<GcReport> {
        if grace_period_us < MICROSECONDS_PER_DAY {
            return Err(StoreError::InvalidInput(format!(
                "artifact GC grace period must be at least {MICROSECONDS_PER_DAY} microseconds"
            )));
        }
        let as_of = sqlite_integer(as_of_us, "as_of_us")?;
        // Finalized CAS objects are independently enumerated so a crash after
        // durable rename but before SQLite registration cannot hide bytes from
        // the two-stage grace policy.
        let discovered = self.artifacts.discover_records()?;
        let entries = if dry_run {
            gc_dry_run_entries(&self.connection, &discovered, as_of_us, grace_period_us)?
        } else {
            let artifacts = &self.artifacts;
            let transaction = self
                .connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let persisted = load_all_artifact_records(&transaction)?;
            validate_discovered_artifact_metadata(&persisted, &discovered)?;

            let mut newly_marked = BTreeSet::new();
            for record in &discovered {
                if persisted.contains_key(&record.digest) {
                    continue;
                }
                ensure_single_gc_change(
                    "orphan artifact recovery mark",
                    transaction.execute(
                        "INSERT INTO artifacts(
                            artifact_digest, byte_len, relative_path, integrity_status,
                            created_at_us, gc_marked_at_us
                         ) VALUES (?1, ?2, ?3, 'verified', ?4, ?4)",
                        params![
                            &record.digest.0[..],
                            sqlite_integer(record.byte_len, "artifact byte_len")?,
                            record.relative_path,
                            as_of
                        ],
                    )?,
                )?;
                newly_marked.insert(record.digest);
            }

            let candidates = load_unreferenced_artifacts(&transaction)?;
            let mut entries = Vec::with_capacity(candidates.len());
            for (digest, (record, marked_at)) in candidates {
                let action = if newly_marked.contains(&digest) {
                    GcAction::Marked
                } else if let Some(marked_at) = marked_at {
                    let marked_at_us = unsigned_integer(marked_at, "gc_marked_at_us")?;
                    if as_of_us.saturating_sub(marked_at_us) < grace_period_us {
                        GcAction::WaitingForGrace
                    } else {
                        let deleted = transaction.execute(
                            "DELETE FROM artifacts
                             WHERE artifact_digest = ?1
                               AND gc_marked_at_us = ?2
                               AND NOT EXISTS (
                                   SELECT 1 FROM artifact_refs AS r
                                   WHERE r.artifact_digest = artifacts.artifact_digest
                               )
                               AND NOT EXISTS (
                                   SELECT 1 FROM artifact_pins AS p
                                   WHERE p.artifact_digest = artifacts.artifact_digest
                               )",
                            params![&record.digest.0[..], marked_at],
                        )?;
                        if deleted == 1 {
                            // The unlink occurs while BEGIN IMMEDIATE prevents
                            // a new reference. If SQLite commit is interrupted,
                            // the retained marked row is safely recovered on
                            // the next sweep, where a missing file is expected.
                            artifacts.remove_verified_or_missing(&record)?;
                            observer()?;
                            GcAction::Deleted
                        } else {
                            GcAction::WaitingForGrace
                        }
                    }
                } else {
                    ensure_single_gc_change(
                        "artifact GC mark",
                        transaction.execute(
                            "UPDATE artifacts SET gc_marked_at_us = ?2
                             WHERE artifact_digest = ?1
                               AND gc_marked_at_us IS NULL
                               AND NOT EXISTS (
                                   SELECT 1 FROM artifact_refs AS r
                                   WHERE r.artifact_digest = artifacts.artifact_digest
                               )
                               AND NOT EXISTS (
                                   SELECT 1 FROM artifact_pins AS p
                                   WHERE p.artifact_digest = artifacts.artifact_digest
                               )",
                            params![&record.digest.0[..], as_of],
                        )?,
                    )?;
                    GcAction::Marked
                };
                entries.push(GcEntry {
                    digest,
                    byte_len: record.byte_len,
                    action,
                });
            }
            transaction.commit()?;
            entries
        };
        Ok(GcReport {
            dry_run,
            as_of_us,
            grace_period_us,
            entries,
        })
    }

    #[cfg(test)]
    const fn connection(&self) -> &Connection {
        &self.connection
    }
}

impl RetirementRepositoryV1 for SqliteRepository {
    fn apply_plan_atomically(
        &mut self,
        plan: &RetirementPlanV1,
    ) -> naome_memory_core::Result<ApplyRetirementOutcomeV1> {
        self.apply_core_plan(plan).map_err(|error| match error {
            StoreError::StalePlan { actual, .. } => actual.parse::<Digest32>().map_or_else(
                |_| {
                    naome_memory_core::MemoryError::RepositoryRejected(
                        "store returned an invalid stale digest".into(),
                    )
                },
                |observed| naome_memory_core::MemoryError::StalePlan {
                    expected: plan.input_set_digest,
                    observed,
                },
            ),
            other => naome_memory_core::MemoryError::RepositoryRejected(other.to_string()),
        })
    }
}

#[derive(Clone)]
struct ExpectedOutcomeRepository(ApplyRetirementOutcomeV1);

impl RetirementRepositoryV1 for ExpectedOutcomeRepository {
    fn apply_plan_atomically(
        &mut self,
        _plan: &RetirementPlanV1,
    ) -> naome_memory_core::Result<ApplyRetirementOutcomeV1> {
        Ok(self.0.clone())
    }
}

fn outcome_from_plan(plan: &RetirementPlanV1, already_applied: bool) -> ApplyRetirementOutcomeV1 {
    let exact_artifact_digests = plan
        .decisions
        .iter()
        .filter(|decision| decision.exact_retained)
        .map(|decision| (decision.atom_id, decision.exact_artifact_digests.clone()))
        .collect();
    ApplyRetirementOutcomeV1 {
        observed_input_digest: plan.input_set_digest,
        state_digest: plan.projected_state_digest,
        applied_decision_count: plan.decisions.len(),
        semantic_atom_ids: plan
            .semantic_atoms
            .iter()
            .map(|semantic| semantic.atom_id)
            .collect(),
        episodic_atom_ids: plan.episodic_winners.clone(),
        exact_artifact_digests,
        already_applied,
    }
}

fn build_core_commit(
    plan: &RetirementPlanV1,
    receipt: &naome_memory_core::ProofReceiptV1,
    snapshot: &CohortSnapshot,
) -> Result<RetirementCommit> {
    let pre_states: BTreeMap<_, _> = snapshot
        .atoms
        .iter()
        .map(|atom| (atom.atom_id, atom.state_digest))
        .collect();
    let mut decisions = Vec::with_capacity(plan.decisions.len());
    let mut artifact_pins = Vec::new();
    for decision in &plan.decisions {
        let canonical_details = decision.canonical_bytes()?;
        let decision_name = if decision.exact_retained && !decision.semantic_atom_ids.is_empty() {
            "semantic_and_episodic"
        } else if decision.exact_retained {
            "episodic_winner"
        } else if decision.forget_episode_body && !decision.semantic_atom_ids.is_empty() {
            "semantic_source"
        } else if decision.forget_episode_body {
            "forgotten"
        } else {
            "unchanged"
        };
        decisions.push(PersistedDecision {
            atom_id: decision.atom_id.digest().0,
            pre_state_digest: *pre_states
                .get(&decision.atom_id.digest().0)
                .ok_or_else(|| {
                    StoreError::InvalidRetirementPlan(
                        "core decision is outside the local cohort snapshot".into(),
                    )
                })?,
            decision: decision_name.into(),
            details_digest: sha256(&canonical_details),
            canonical_details,
        });
        artifact_pins.extend(
            decision
                .exact_artifact_digests
                .iter()
                .map(|digest| ArtifactPin {
                    atom_id: decision.atom_id.digest().0,
                    artifact_digest: ArtifactDigest(digest.0),
                }),
        );
    }
    let semantic_atoms = plan
        .semantic_atoms
        .iter()
        .map(semantic_record)
        .collect::<Result<Vec<_>>>()?;
    let canonical_receipt = receipt.canonical_bytes()?;
    let closure_bytes = receipt.closure.canonical_bytes()?;
    Ok(RetirementCommit {
        run_id: receipt.run_id.0,
        key: snapshot.key.clone(),
        policy_id: plan.policy_id.clone(),
        policy_digest: plan.policy_digest.0,
        seed: *plan.seed.as_bytes(),
        rng_id: plan.rng_identifier.clone(),
        expected_input_set_digest: snapshot.input_set_digest,
        proof_input_set_digest: plan.input_set_digest.0,
        population_digest: plan.episodic_population_digest.0,
        plan_digest: plan.digest()?.0,
        projected_state_digest: plan.projected_state_digest.0,
        projected_post_state: plan.projected_post_state.clone(),
        decisions,
        semantic_atoms,
        artifact_pins,
        receipt: PersistedReceipt {
            receipt_digest: receipt.receipt_id.0,
            canonical_receipt,
            json_projection: serde_json::to_string(receipt)?,
            closure_digest: sha256(&closure_bytes),
        },
    })
}

fn semantic_record(semantic: &PlannedSemanticAtomV1) -> Result<AtomRecord> {
    let state = AtomStateV1 {
        retention_tier: RetentionTierV1::SemanticLongTerm,
        content_status: ContentStatusV1::Present,
        expires_at_us: None,
        superseded_by: None,
        feedback_positive: 0,
        feedback_negative: 0,
        retrieval_count: 0,
        retirement_run_id: None,
    };
    AtomRecord::from_core(&semantic.body, &state)
}

fn retention_tier_from_store(value: &str) -> Result<RetentionTierV1> {
    match value {
        "stm" => Ok(RetentionTierV1::ShortTerm),
        "ltm_semantic" => Ok(RetentionTierV1::SemanticLongTerm),
        "ltm_episodic" => Ok(RetentionTierV1::EpisodicLongTerm),
        "ltm_dual" => Ok(RetentionTierV1::DualLongTerm),
        "forgotten" => Ok(RetentionTierV1::HeaderOnly),
        _ => Err(StoreError::Integrity(format!(
            "unknown persisted retention tier {value}"
        ))),
    }
}

fn retention_permission_from_store(value: &str) -> Result<RetentionPermissionV1> {
    match value {
        "transient_only" => Ok(RetentionPermissionV1::TransientOnly),
        "semantic_allowed" => Ok(RetentionPermissionV1::SemanticAllowed),
        "semantic_and_exact_allowed" => Ok(RetentionPermissionV1::SemanticAndExactAllowed),
        _ => Err(StoreError::Integrity(format!(
            "unknown retention permission {value}"
        ))),
    }
}

fn content_status_from_store(value: &str) -> Result<ContentStatusV1> {
    match value {
        "present" => Ok(ContentStatusV1::Present),
        "forgotten" => Ok(ContentStatusV1::Forgotten),
        "quarantined" => Ok(ContentStatusV1::Corrupt),
        _ => Err(StoreError::Integrity(format!(
            "unknown persisted content status {value}"
        ))),
    }
}

fn relation_kind_text(kind: MemoryRelationKindV1) -> String {
    let value = match kind {
        MemoryRelationKindV1::Continues => "continues",
        MemoryRelationKindV1::Supports => "supports",
        MemoryRelationKindV1::Contradicts => "contradicts",
        MemoryRelationKindV1::Supersedes => "supersedes",
        MemoryRelationKindV1::DerivedFrom => "derived_from",
    };
    value.to_owned()
}

fn validate_initial_atom_state(body: &MemoryAtomBodyV1, state: &AtomStateV1) -> Result<()> {
    if state.superseded_by.is_some()
        || state.feedback_positive != 0
        || state.feedback_negative != 0
        || state.retrieval_count != 0
        || state.retirement_run_id.is_some()
    {
        return Err(StoreError::InvalidInput(
            "initial typed atom insertion cannot import mutable counters, supersession, or a retirement-run binding"
                .to_owned(),
        ));
    }
    if state.content_status != ContentStatusV1::Present {
        return Err(StoreError::InvalidInput(
            "typed atom insertion requires present immutable content".to_owned(),
        ));
    }
    match (&body.payload, state.retention_tier) {
        (MemoryPayloadV1::Episode(_), RetentionTierV1::ShortTerm) => {
            let expected_expiry = body
                .interval
                .recorded_at_us
                .checked_add(PolicyV1::poc_v1().stm_horizon_us)
                .ok_or_else(|| {
                    StoreError::InvalidInput(
                        "episode recorded time plus STM horizon overflows u64".to_owned(),
                    )
                })?;
            if state.expires_at_us != Some(expected_expiry) {
                return Err(StoreError::InvalidInput(
                    "STM episode expiry must equal recorded_at_us plus the policy horizon"
                        .to_owned(),
                ));
            }
        }
        (
            MemoryPayloadV1::Episode(_),
            RetentionTierV1::EpisodicLongTerm | RetentionTierV1::DualLongTerm,
        )
        | (MemoryPayloadV1::Semantic(_), RetentionTierV1::SemanticLongTerm) => {
            if state.expires_at_us.is_some() {
                return Err(StoreError::InvalidInput(
                    "initial long-term atom state must not carry an STM expiry".to_owned(),
                ));
            }
        }
        _ => {
            return Err(StoreError::InvalidInput(
                "atom kind and initial retention tier are incoherent".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_core_episode_chain(atoms: &[(MemoryAtomBodyV1, AtomStateV1)]) -> Result<()> {
    let Some((first_body, first_state)) = atoms.first() else {
        return Err(StoreError::InvalidInput(
            "episode chain must not be empty".to_owned(),
        ));
    };
    let mut previous_id: Option<AtomId> = None;
    let mut previous_sequence_end: Option<u64> = None;
    for (body, state) in atoms {
        validate_initial_atom_state(body, state)?;
        if body.scope != first_body.scope
            || body.interval.recorded_at_us != first_body.interval.recorded_at_us
            || state != first_state
        {
            return Err(StoreError::InvalidInput(
                "episode-chain scope, recorded time, and initial state must be identical"
                    .to_owned(),
            ));
        }
        let MemoryPayloadV1::Episode(payload) = &body.payload else {
            return Err(StoreError::InvalidInput(
                "episode chain contains a semantic atom".to_owned(),
            ));
        };
        let continues_targets = body
            .relations
            .iter()
            .filter_map(|relation| {
                (relation.kind == MemoryRelationKindV1::Continues).then_some(relation.target)
            })
            .collect::<Vec<_>>();
        let expected_continues_targets = payload.continues.into_iter().collect::<Vec<_>>();
        if continues_targets != expected_continues_targets {
            return Err(StoreError::InvalidInput(
                "episode payload and declared continues relation do not match exactly".to_owned(),
            ));
        }
        if let (Some(expected_previous), Some(previous_end)) = (previous_id, previous_sequence_end)
        {
            let expected_start = previous_end.checked_add(1).ok_or_else(|| {
                StoreError::InvalidInput("episode-chain sequence overflows u64".to_owned())
            })?;
            if payload.continues != Some(expected_previous)
                || payload.event_sequence_start != expected_start
            {
                return Err(StoreError::InvalidInput(
                    "episode-chain members are not a contiguous newest-to-previous sequence"
                        .to_owned(),
                ));
            }
        }
        if payload.event_sequence_start > payload.event_sequence_end {
            return Err(StoreError::InvalidInput(
                "episode-chain member has an inverted event sequence".to_owned(),
            ));
        }
        previous_id = Some(body.atom_id()?);
        previous_sequence_end = Some(payload.event_sequence_end);
    }
    Ok(())
}

fn validate_declared_core_link_closure(
    atom: &AtomRecord,
    artifact_links: &[AtomArtifactLink],
    relation_links: &[AtomRelationLink],
) -> Result<()> {
    let Ok(body) = serde_json::from_str::<MemoryAtomBodyV1>(&atom.json_projection) else {
        // The low-level record API also supports deliberately minimal test and
        // migration projections. Core projections are recognized by their
        // complete typed shape and then receive the stronger closure check.
        return Ok(());
    };
    let canonical_body = body.canonical_bytes()?;
    let body_id = body.atom_id()?.digest().0;
    if canonical_body != atom.canonical_body
        || body_id != atom.atom_id
        || body_id != atom.body_digest
    {
        return Err(StoreError::Integrity(
            "typed atom projection does not match its canonical authority body".to_owned(),
        ));
    }

    let mut expected_artifacts = body
        .artifacts
        .iter()
        .map(|artifact| {
            (
                artifact.digest.0,
                artifact.media_type.clone(),
                artifact.retention_allowed,
            )
        })
        .collect::<Vec<_>>();
    let mut observed_artifacts = artifact_links
        .iter()
        .map(|link| {
            (
                link.artifact_digest.0,
                link.role.clone(),
                link.retention_allowed,
            )
        })
        .collect::<Vec<_>>();
    expected_artifacts.sort_unstable();
    observed_artifacts.sort_unstable();
    if expected_artifacts != observed_artifacts {
        return Err(StoreError::InvalidInput(
            "artifact links do not close over the typed atom body".to_owned(),
        ));
    }

    let mut expected_relations = body
        .relations
        .iter()
        .map(|relation| {
            Ok((
                relation.target.digest().0,
                relation_kind_text(relation.kind),
                relation.canonical_bytes()?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut observed_relations = relation_links
        .iter()
        .map(|link| {
            (
                link.target_atom_id,
                link.relation_kind.clone(),
                link.canonical_relation.clone(),
            )
        })
        .collect::<Vec<_>>();
    expected_relations.sort_unstable();
    observed_relations.sort_unstable();
    if expected_relations != observed_relations {
        return Err(StoreError::InvalidInput(
            "relation links do not close over the typed atom body".to_owned(),
        ));
    }
    Ok(())
}

fn configure_connection(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;
         PRAGMA trusted_schema = OFF;
         PRAGMA temp_store = MEMORY;",
    )?;
    let foreign_keys: i64 = connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    if foreign_keys != 1 {
        return Err(StoreError::Integrity(
            "SQLite foreign key enforcement could not be enabled".into(),
        ));
    }
    let journal_mode: String = connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(StoreError::Integrity(format!(
            "SQLite WAL mode could not be enabled: {journal_mode}"
        )));
    }
    Ok(())
}

fn load_atom_memory_space(
    connection: &Connection,
    atom_id: [u8; 32],
    role: &str,
) -> Result<String> {
    connection
        .query_row(
            "SELECT memory_space_id FROM atom_headers WHERE atom_id = ?1",
            [&atom_id[..]],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| {
            StoreError::Integrity(format!(
                "{role} atom {} does not exist",
                hex::encode(atom_id)
            ))
        })
}

fn validate_relation_memory_space(
    connection: &Connection,
    source_atom_id: [u8; 32],
    target_atom_id: [u8; 32],
) -> Result<()> {
    let source_space = load_atom_memory_space(connection, source_atom_id, "relation source")?;
    let target_space = load_atom_memory_space(connection, target_atom_id, "relation target")?;
    if source_space != target_space {
        return Err(StoreError::InvalidInput(format!(
            "relation {} -> {} crosses memory spaces",
            hex::encode(source_atom_id),
            hex::encode(target_atom_id)
        )));
    }
    Ok(())
}

fn verify_all_atom_closure(
    connection: &Connection,
    artifact_store: &ArtifactStore,
    verify_artifacts: bool,
) -> Result<u64> {
    let policy = PolicyV1::poc_v1();
    let policy_digest = policy.digest()?;
    let headers = load_header_authorities(connection)?;
    let artifacts = load_artifact_authorities(connection)?;
    let artifact_refs = load_artifact_links(connection, "artifact_refs")?;
    let artifact_provenance = load_artifact_links(connection, "artifact_provenance_edges")?;
    let provenance = load_provenance_authorities(connection, &headers)?;
    let relations = load_relation_authorities(connection, &headers)?;
    let artifact_pins = load_artifact_pins(connection)?;
    let retirement_runs = load_retirement_run_authorities(connection)?;
    let retirement_decisions = load_retirement_decision_authorities(connection)?;
    let mut fts = load_fts_authorities(connection)?;

    validate_live_artifact_authorities(&artifact_refs, &artifacts)?;
    validate_artifact_pin_authorities(&artifact_pins, &artifacts, &headers, &retirement_runs)?;

    let mut statement = connection.prepare(
        "SELECT h.atom_id, h.atom_kind, h.memory_space_id, h.repository_id, h.task_id,
                h.agent_id, h.session_id, h.recorded_at_us, h.body_digest, h.body_bytes,
                h.retention_permission, h.provenance_digest,
                b.canonical_body, b.json_projection,
                s.retention_tier, s.content_status, s.expires_at_us, s.superseded_by,
                s.feedback_positive, s.feedback_negative, s.retrieval_count,
                s.retirement_run_id,
                COALESCE(f.positive_count, 0), COALESCE(f.negative_count, 0),
                p.atom_id, p.memory_space_id, p.repository_id, p.searchable_text,
                p.topic_keys_json, p.entity_ids_json, p.outcome_class, p.projection_digest
         FROM atom_headers AS h
         LEFT JOIN atom_bodies AS b ON b.atom_id = h.atom_id
         LEFT JOIN atom_state AS s ON s.atom_id = h.atom_id
         LEFT JOIN (
             SELECT atom_id,
                    SUM(CASE WHEN outcome = 1 THEN 1 ELSE 0 END) AS positive_count,
                    SUM(CASE WHEN outcome = -1 THEN 1 ELSE 0 END) AS negative_count
             FROM feedback
             GROUP BY atom_id
         ) AS f ON f.atom_id = h.atom_id
         LEFT JOIN search_projection AS p ON p.atom_id = h.atom_id
         ORDER BY h.atom_id",
    )?;
    let rows = statement.query_map([], stored_integrity_atom_from_row)?;
    let mut scanned = 0_usize;
    for row in rows {
        let stored = row?;
        verify_stored_atom_closure(
            &stored,
            &policy,
            policy_digest,
            &headers,
            &artifacts,
            &artifact_refs,
            &artifact_provenance,
            &provenance,
            &relations,
            &artifact_pins,
            &retirement_runs,
            &retirement_decisions,
            &mut fts,
        )?;
        scanned = scanned.saturating_add(1);
    }
    if scanned != headers.len() {
        return Err(StoreError::Integrity(
            "atom header scan did not close over every header".to_owned(),
        ));
    }
    if provenance.len() != headers.len() {
        return Err(StoreError::Integrity(
            "immutable provenance does not close over every atom header".to_owned(),
        ));
    }
    if let Some(atom_id) = fts.keys().next() {
        return Err(StoreError::Integrity(format!(
            "orphan or unaccounted FTS row remains for atom {}",
            hex::encode(atom_id)
        )));
    }

    let mut verified = 0_u64;
    if verify_artifacts {
        for authority in artifacts.values() {
            if authority.integrity_status == "verified" {
                artifact_store.verify(&authority.record)?;
                verified = verified.saturating_add(1);
            }
        }
    }
    Ok(verified)
}

fn stored_integrity_atom_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<StoredIntegrityAtom> {
    Ok(StoredIntegrityAtom {
        atom_id: row.get(0)?,
        atom_kind: row.get(1)?,
        memory_space_id: row.get(2)?,
        repository_id: row.get(3)?,
        task_id: row.get(4)?,
        agent_id: row.get(5)?,
        session_id: row.get(6)?,
        recorded_at_us: row.get(7)?,
        body_digest: row.get(8)?,
        body_bytes: row.get(9)?,
        retention_permission: row.get(10)?,
        provenance_digest: row.get(11)?,
        canonical_body: row.get(12)?,
        json_projection: row.get(13)?,
        retention_tier: row.get(14)?,
        content_status: row.get(15)?,
        expires_at_us: row.get(16)?,
        superseded_by: row.get(17)?,
        feedback_positive: row.get(18)?,
        feedback_negative: row.get(19)?,
        retrieval_count: row.get(20)?,
        retirement_run_id: row.get(21)?,
        observed_feedback_positive: row.get(22)?,
        observed_feedback_negative: row.get(23)?,
        search_atom_id: row.get(24)?,
        search_memory_space_id: row.get(25)?,
        search_repository_id: row.get(26)?,
        searchable_text: row.get(27)?,
        topic_keys_json: row.get(28)?,
        entity_ids_json: row.get(29)?,
        outcome_class: row.get(30)?,
        projection_digest: row.get(31)?,
    })
}

fn load_header_authorities(connection: &Connection) -> Result<BTreeMap<[u8; 32], HeaderAuthority>> {
    let mut statement = connection.prepare(
        "SELECT atom_id, memory_space_id, atom_kind, body_digest
         FROM atom_headers ORDER BY atom_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Vec<u8>>(3)?,
        ))
    })?;
    let mut headers = BTreeMap::new();
    for row in rows {
        let (atom_id, memory_space_id, atom_kind, body_digest) = row?;
        let atom_id = digest_array(&atom_id, "integrity atom_id")?;
        let body_digest = digest_array(&body_digest, "integrity body_digest")?;
        if atom_id != body_digest
            || memory_space_id.is_empty()
            || !matches!(atom_kind.as_str(), "episode" | "semantic")
        {
            return Err(StoreError::Integrity(format!(
                "atom {} has an invalid immutable header",
                hex::encode(atom_id)
            )));
        }
        if headers
            .insert(
                atom_id,
                HeaderAuthority {
                    memory_space_id,
                    atom_kind,
                    body_digest,
                },
            )
            .is_some()
        {
            return Err(StoreError::Integrity(
                "duplicate atom header authority".to_owned(),
            ));
        }
    }
    Ok(headers)
}

fn load_artifact_authorities(
    connection: &Connection,
) -> Result<BTreeMap<ArtifactDigest, ArtifactAuthority>> {
    let mut statement = connection.prepare(
        "SELECT artifact_digest, byte_len, relative_path, integrity_status
         FROM artifacts ORDER BY artifact_digest",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((artifact_record_from_row(row)?, row.get::<_, String>(3)?))
    })?;
    let mut artifacts = BTreeMap::new();
    for row in rows {
        let (record, integrity_status) = row?;
        let expected_path = artifact_relative_path(record.digest);
        if record.byte_len > crate::MAX_ARTIFACT_BYTES
            || record.relative_path != expected_path
            || !matches!(integrity_status.as_str(), "verified" | "quarantined")
        {
            return Err(StoreError::Integrity(format!(
                "artifact {} has invalid metadata authority",
                record.digest
            )));
        }
        if artifacts
            .insert(
                record.digest,
                ArtifactAuthority {
                    record,
                    integrity_status,
                },
            )
            .is_some()
        {
            return Err(StoreError::Integrity(
                "duplicate artifact authority".to_owned(),
            ));
        }
    }
    Ok(artifacts)
}

fn load_provenance_authorities(
    connection: &Connection,
    headers: &BTreeMap<[u8; 32], HeaderAuthority>,
) -> Result<BTreeMap<[u8; 32], ProvenanceAuthority>> {
    let mut source_statement = connection.prepare(
        "SELECT ordinal, source_event_digest FROM atom_provenance_sources
         WHERE atom_id = ?1 ORDER BY ordinal",
    )?;
    let mut statement = connection.prepare(
        "SELECT atom_id, producer, policy_digest, canonical_provenance,
                json_projection, provenance_digest
         FROM atom_provenance ORDER BY atom_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Vec<u8>>(2)?,
            row.get::<_, Vec<u8>>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Vec<u8>>(5)?,
        ))
    })?;
    let mut authorities = BTreeMap::new();
    for row in rows {
        let (atom_id, producer, policy_digest, canonical, json, provenance_digest) = row?;
        let atom_id = digest_array(&atom_id, "provenance atom_id")?;
        if !headers.contains_key(&atom_id) || producer.is_empty() {
            return Err(StoreError::Integrity(format!(
                "atom provenance {} has no header or producer",
                hex::encode(atom_id)
            )));
        }
        let source_rows = source_statement.query_map([&atom_id[..]], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        let mut sources = Vec::new();
        for (expected_ordinal, source) in source_rows.enumerate() {
            let (ordinal, digest) = source?;
            if ordinal != i64::try_from(expected_ordinal).unwrap_or(i64::MAX) {
                return Err(StoreError::Integrity(format!(
                    "atom provenance {} source ordinals are not contiguous",
                    hex::encode(atom_id)
                )));
            }
            sources.push(digest_array(&digest, "source_event_digest")?);
        }
        let parsed: ProvenanceV1 = serde_json::from_str(&json)?;
        let parsed_sources = parsed
            .source_event_digests
            .iter()
            .map(|digest| digest.0)
            .collect::<Vec<_>>();
        let policy_digest = digest_array(&policy_digest, "provenance policy_digest")?;
        let provenance_digest = digest_array(&provenance_digest, "provenance_digest")?;
        if parsed.producer != producer
            || parsed.policy_digest.0 != policy_digest
            || parsed.canonical_bytes()? != canonical
            || parsed_sources != sources
            || Digest32::hash_prefixed(b"naome-memory:provenance:v1\0", &canonical).0
                != provenance_digest
        {
            return Err(StoreError::Integrity(format!(
                "atom provenance {} failed canonical closure",
                hex::encode(atom_id)
            )));
        }
        if authorities
            .insert(
                atom_id,
                ProvenanceAuthority {
                    producer,
                    policy_digest,
                    canonical_provenance: canonical,
                    json_projection: json,
                    provenance_digest,
                    source_event_digests: sources,
                },
            )
            .is_some()
        {
            return Err(StoreError::Integrity(
                "duplicate atom provenance authority".to_owned(),
            ));
        }
    }
    Ok(authorities)
}

fn artifact_relative_path(digest: ArtifactDigest) -> String {
    let hex = digest.to_hex();
    format!("sha256/{}/{}/{}", &hex[..2], &hex[2..4], hex)
}

fn load_artifact_links(
    connection: &Connection,
    table: &'static str,
) -> Result<BTreeMap<[u8; 32], Vec<ArtifactLinkAuthority>>> {
    let sql = match table {
        "artifact_refs" => {
            "SELECT atom_id, artifact_digest, role, retention_allowed
             FROM artifact_refs ORDER BY atom_id, artifact_digest, role"
        }
        "artifact_provenance_edges" => {
            "SELECT atom_id, artifact_digest, role, retention_allowed
             FROM artifact_provenance_edges ORDER BY atom_id, artifact_digest, role"
        }
        _ => {
            return Err(StoreError::Integrity(
                "unsupported artifact-link authority table".to_owned(),
            ));
        }
    };
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, Vec<u8>>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    let mut links = BTreeMap::<[u8; 32], Vec<ArtifactLinkAuthority>>::new();
    for row in rows {
        let (atom_id, digest, role, retention_allowed) = row?;
        let atom_id = digest_array(&atom_id, "artifact link atom_id")?;
        let digest = ArtifactDigest(digest_array(&digest, "artifact link digest")?);
        if role.is_empty() {
            return Err(StoreError::Integrity(format!(
                "{table} contains an empty artifact role"
            )));
        }
        links
            .entry(atom_id)
            .or_default()
            .push(ArtifactLinkAuthority {
                digest,
                role,
                retention_allowed: sqlite_bool(retention_allowed, "artifact retention_allowed")?,
            });
    }
    Ok(links)
}

fn load_relation_authorities(
    connection: &Connection,
    headers: &BTreeMap<[u8; 32], HeaderAuthority>,
) -> Result<BTreeMap<[u8; 32], Vec<RelationAuthority>>> {
    let mut statement = connection.prepare(
        "SELECT source_atom_id, target_atom_id, relation_kind, relation_digest
         FROM atom_relations ORDER BY source_atom_id, target_atom_id, relation_kind",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, Vec<u8>>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Vec<u8>>(3)?,
        ))
    })?;
    let mut relations = BTreeMap::<[u8; 32], Vec<RelationAuthority>>::new();
    for row in rows {
        let (source, target, relation_kind, relation_digest) = row?;
        let source = digest_array(&source, "relation source_atom_id")?;
        let target = digest_array(&target, "relation target_atom_id")?;
        let relation_digest = digest_array(&relation_digest, "relation_digest")?;
        if source == target || !is_relation_kind(&relation_kind) {
            return Err(StoreError::Integrity(format!(
                "relation {} -> {} has invalid authority",
                hex::encode(source),
                hex::encode(target)
            )));
        }
        let source_header = headers.get(&source).ok_or_else(|| {
            StoreError::Integrity(format!(
                "relation source {} is missing",
                hex::encode(source)
            ))
        })?;
        let target_header = headers.get(&target).ok_or_else(|| {
            StoreError::Integrity(format!(
                "relation target {} is missing",
                hex::encode(target)
            ))
        })?;
        if source_header.memory_space_id != target_header.memory_space_id {
            return Err(StoreError::Integrity(format!(
                "relation {} -> {} crosses memory spaces",
                hex::encode(source),
                hex::encode(target)
            )));
        }
        relations
            .entry(source)
            .or_default()
            .push(RelationAuthority {
                target_atom_id: target,
                relation_kind,
                relation_digest,
            });
    }
    Ok(relations)
}

fn is_relation_kind(value: &str) -> bool {
    matches!(
        value,
        "continues" | "supports" | "contradicts" | "supersedes" | "derived_from"
    )
}

fn load_artifact_pins(connection: &Connection) -> Result<ArtifactPinsByAtom> {
    let mut statement = connection.prepare(
        "SELECT atom_id, artifact_digest, run_id
         FROM artifact_pins ORDER BY atom_id, artifact_digest, run_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, Vec<u8>>(1)?,
            row.get::<_, Vec<u8>>(2)?,
        ))
    })?;
    let mut pins = BTreeMap::<[u8; 32], Vec<(ArtifactDigest, [u8; 32])>>::new();
    for row in rows {
        let (atom_id, digest, run_id) = row?;
        pins.entry(digest_array(&atom_id, "artifact pin atom_id")?)
            .or_default()
            .push((
                ArtifactDigest(digest_array(&digest, "artifact pin digest")?),
                digest_array(&run_id, "artifact pin run_id")?,
            ));
    }
    Ok(pins)
}

fn load_retirement_run_authorities(
    connection: &Connection,
) -> Result<BTreeMap<[u8; 32], RetirementRunAuthority>> {
    let mut statement = connection
        .prepare("SELECT run_id, memory_space_id, status FROM retirement_runs ORDER BY run_id")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut runs = BTreeMap::new();
    for row in rows {
        let (run_id, memory_space_id, status) = row?;
        let run_id = digest_array(&run_id, "retirement run_id")?;
        if memory_space_id.is_empty()
            || !matches!(status.as_str(), "applying" | "applied" | "rejected")
        {
            return Err(StoreError::Integrity(format!(
                "retirement run {} has invalid authority",
                hex::encode(run_id)
            )));
        }
        runs.insert(
            run_id,
            RetirementRunAuthority {
                memory_space_id,
                status,
            },
        );
    }
    Ok(runs)
}

fn load_retirement_decision_authorities(
    connection: &Connection,
) -> Result<RetirementDecisionAuthorities> {
    let mut statement = connection.prepare(
        "SELECT run_id, atom_id, decision
         FROM retirement_decisions ORDER BY run_id, atom_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, Vec<u8>>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut decisions = BTreeMap::new();
    for row in rows {
        let (run_id, atom_id, decision) = row?;
        decisions.insert(
            (
                digest_array(&run_id, "retirement decision run_id")?,
                digest_array(&atom_id, "retirement decision atom_id")?,
            ),
            decision,
        );
    }
    Ok(decisions)
}

fn load_fts_authorities(connection: &Connection) -> Result<BTreeMap<[u8; 32], FtsAuthority>> {
    let mut statement =
        connection.prepare("SELECT atom_id, searchable_text FROM atom_search ORDER BY atom_id")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut authorities = BTreeMap::new();
    for row in rows {
        let (atom_id_text, searchable_text) = row?;
        let atom_id_bytes = hex::decode(&atom_id_text).map_err(|error| {
            StoreError::Integrity(format!("FTS atom ID is not hexadecimal: {error}"))
        })?;
        let atom_id = digest_array(&atom_id_bytes, "FTS atom_id")?;
        if atom_id_text != hex::encode(atom_id) {
            return Err(StoreError::Integrity(format!(
                "FTS atom ID {atom_id_text} is not canonical lowercase hexadecimal"
            )));
        }
        let text_digest = sha256(searchable_text.as_bytes());
        let entry = authorities.entry(atom_id).or_insert(FtsAuthority {
            row_count: 0,
            searchable_text_digest: text_digest,
            has_conflicting_text: false,
        });
        entry.row_count = entry.row_count.saturating_add(1);
        entry.has_conflicting_text |= entry.searchable_text_digest != text_digest;
    }
    Ok(authorities)
}

fn validate_live_artifact_authorities(
    artifact_refs: &BTreeMap<[u8; 32], Vec<ArtifactLinkAuthority>>,
    artifacts: &BTreeMap<ArtifactDigest, ArtifactAuthority>,
) -> Result<()> {
    for links in artifact_refs.values() {
        for link in links {
            let artifact = artifacts.get(&link.digest).ok_or_else(|| {
                StoreError::Integrity(format!(
                    "live artifact reference {} has no metadata authority",
                    link.digest
                ))
            })?;
            if artifact.integrity_status != "verified" {
                return Err(StoreError::Integrity(format!(
                    "live artifact reference {} is not verified",
                    link.digest
                )));
            }
        }
    }
    Ok(())
}

fn validate_artifact_pin_authorities(
    pins: &BTreeMap<[u8; 32], Vec<(ArtifactDigest, [u8; 32])>>,
    artifacts: &BTreeMap<ArtifactDigest, ArtifactAuthority>,
    headers: &BTreeMap<[u8; 32], HeaderAuthority>,
    runs: &BTreeMap<[u8; 32], RetirementRunAuthority>,
) -> Result<()> {
    for (atom_id, atom_pins) in pins {
        let header = headers.get(atom_id).ok_or_else(|| {
            StoreError::Integrity(format!(
                "artifact pin references missing atom {}",
                hex::encode(atom_id)
            ))
        })?;
        for (digest, run_id) in atom_pins {
            let artifact = artifacts.get(digest).ok_or_else(|| {
                StoreError::Integrity(format!("artifact pin {digest} has no metadata authority"))
            })?;
            let run = runs.get(run_id).ok_or_else(|| {
                StoreError::Integrity(format!(
                    "artifact pin references missing run {}",
                    hex::encode(run_id)
                ))
            })?;
            if artifact.integrity_status != "verified"
                || run.status != "applied"
                || run.memory_space_id != header.memory_space_id
            {
                return Err(StoreError::Integrity(format!(
                    "artifact pin {digest} failed run, scope, or integrity closure"
                )));
            }
        }
    }
    Ok(())
}

fn sqlite_bool(value: i64, field: &'static str) -> Result<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(StoreError::Integrity(format!(
            "invalid SQLite boolean for {field}: {value}"
        ))),
    }
}

#[allow(clippy::too_many_arguments)]
fn verify_stored_atom_closure(
    stored: &StoredIntegrityAtom,
    policy: &PolicyV1,
    policy_digest: Digest32,
    headers: &BTreeMap<[u8; 32], HeaderAuthority>,
    artifacts: &BTreeMap<ArtifactDigest, ArtifactAuthority>,
    artifact_refs: &BTreeMap<[u8; 32], Vec<ArtifactLinkAuthority>>,
    artifact_provenance: &BTreeMap<[u8; 32], Vec<ArtifactLinkAuthority>>,
    provenance: &BTreeMap<[u8; 32], ProvenanceAuthority>,
    relations: &BTreeMap<[u8; 32], Vec<RelationAuthority>>,
    artifact_pins: &ArtifactPinsByAtom,
    retirement_runs: &BTreeMap<[u8; 32], RetirementRunAuthority>,
    retirement_decisions: &RetirementDecisionAuthorities,
    fts: &mut BTreeMap<[u8; 32], FtsAuthority>,
) -> Result<()> {
    let atom_id = digest_array(&stored.atom_id, "integrity atom_id")?;
    let header = headers.get(&atom_id).ok_or_else(|| {
        StoreError::Integrity(format!(
            "atom {} is absent from the header authority index",
            hex::encode(atom_id)
        ))
    })?;
    let body_digest = digest_array(&stored.body_digest, "integrity body_digest")?;
    let provenance_digest = digest_array(&stored.provenance_digest, "provenance_digest")?;
    let provenance = provenance.get(&atom_id).ok_or_else(|| {
        StoreError::Integrity(format!(
            "atom {} is missing immutable provenance authority",
            hex::encode(atom_id)
        ))
    })?;
    let body_bytes = unsigned_integer(stored.body_bytes, "body_bytes")?;
    let recorded_at_us = unsigned_integer(stored.recorded_at_us, "recorded_at_us")?;
    if body_digest != atom_id
        || header.body_digest != atom_id
        || stored.atom_kind != header.atom_kind
        || stored.memory_space_id != header.memory_space_id
        || body_bytes == 0
        || body_bytes > policy.atom_hard_max_bytes
        || stored.memory_space_id.is_empty()
        || provenance.provenance_digest != provenance_digest
        || provenance.policy_digest != policy_digest.0
        || provenance.producer.is_empty()
        || stored.session_id.as_deref().is_none_or(str::is_empty)
        || [
            stored.repository_id.as_deref(),
            stored.task_id.as_deref(),
            stored.agent_id.as_deref(),
        ]
        .into_iter()
        .any(|value| value == Some(""))
    {
        return Err(StoreError::Integrity(format!(
            "atom {} failed immutable header or scope authority",
            hex::encode(atom_id)
        )));
    }

    let retention_tier_text = stored.retention_tier.as_deref().ok_or_else(|| {
        StoreError::Integrity(format!(
            "atom {} is missing mutable state",
            hex::encode(atom_id)
        ))
    })?;
    let content_status_text = stored.content_status.as_deref().ok_or_else(|| {
        StoreError::Integrity(format!(
            "atom {} is missing mutable state",
            hex::encode(atom_id)
        ))
    })?;
    let retention_tier = retention_tier_from_store(retention_tier_text)?;
    let content_status = content_status_from_store(content_status_text)?;
    let feedback_positive = unsigned_integer(
        stored.feedback_positive.ok_or_else(|| {
            StoreError::Integrity(format!(
                "atom {} is missing mutable state",
                hex::encode(atom_id)
            ))
        })?,
        "feedback_positive",
    )?;
    let feedback_negative = unsigned_integer(
        stored.feedback_negative.ok_or_else(|| {
            StoreError::Integrity(format!(
                "atom {} is missing mutable state",
                hex::encode(atom_id)
            ))
        })?,
        "feedback_negative",
    )?;
    let retrieval_count = unsigned_integer(
        stored.retrieval_count.ok_or_else(|| {
            StoreError::Integrity(format!(
                "atom {} is missing mutable state",
                hex::encode(atom_id)
            ))
        })?,
        "retrieval_count",
    )?;
    let expires_at_us = stored
        .expires_at_us
        .map(|value| unsigned_integer(value, "expires_at_us"))
        .transpose()?;
    let superseded_by = stored
        .superseded_by
        .as_deref()
        .map(|value| digest_array(value, "superseded_by"))
        .transpose()?;
    let retirement_run_id = stored
        .retirement_run_id
        .as_deref()
        .map(|value| digest_array(value, "retirement_run_id"))
        .transpose()?;
    let state = AtomStateV1 {
        retention_tier,
        content_status,
        expires_at_us,
        superseded_by: superseded_by.map(|value| AtomId(Digest32(value))),
        feedback_positive,
        feedback_negative,
        retrieval_count,
        retirement_run_id: retirement_run_id.map(Digest32),
    };
    validate_complete_mutable_state(
        atom_id,
        stored,
        recorded_at_us,
        &state,
        headers,
        retirement_runs,
        retirement_decisions,
        policy,
    )?;

    let expected_positive = unsigned_integer(
        stored.observed_feedback_positive,
        "observed feedback_positive",
    )?;
    let expected_negative = unsigned_integer(
        stored.observed_feedback_negative,
        "observed feedback_negative",
    )?;
    if feedback_positive != expected_positive || feedback_negative != expected_negative {
        return Err(StoreError::Integrity(format!(
            "atom {} feedback counters do not close over feedback authority rows",
            hex::encode(atom_id)
        )));
    }

    let live_artifacts = artifact_refs.get(&atom_id).map_or(&[][..], Vec::as_slice);
    let provenance_artifacts = artifact_provenance
        .get(&atom_id)
        .map_or(&[][..], Vec::as_slice);
    let declared_relations = relations.get(&atom_id).map_or(&[][..], Vec::as_slice);
    let pins = artifact_pins.get(&atom_id).map_or(&[][..], Vec::as_slice);
    if content_status == ContentStatusV1::Forgotten {
        if retention_tier != RetentionTierV1::HeaderOnly
            || stored.canonical_body.is_some()
            || stored.json_projection.is_some()
            || stored.search_atom_id.is_some()
            || stored.search_memory_space_id.is_some()
            || stored.searchable_text.is_some()
            || stored.topic_keys_json.is_some()
            || stored.entity_ids_json.is_some()
            || stored.projection_digest.is_some()
            || !live_artifacts.is_empty()
            || !pins.is_empty()
            || fts.remove(&atom_id).is_some()
        {
            return Err(StoreError::Integrity(format!(
                "forgotten atom {} retains body, search, FTS, live artifact, or pin state",
                hex::encode(atom_id)
            )));
        }
        return Ok(());
    }

    let canonical_body = stored.canonical_body.as_deref().ok_or_else(|| {
        StoreError::Integrity(format!(
            "present atom {} is missing its body",
            hex::encode(atom_id)
        ))
    })?;
    let json_projection = stored.json_projection.as_deref().ok_or_else(|| {
        StoreError::Integrity(format!(
            "present atom {} is missing its JSON projection",
            hex::encode(atom_id)
        ))
    })?;
    let body: MemoryAtomBodyV1 = serde_json::from_str(json_projection).map_err(|error| {
        StoreError::Integrity(format!(
            "atom {} has an invalid typed JSON projection: {error}",
            hex::encode(atom_id)
        ))
    })?;
    validate_typed_body_structure(&body, atom_id, headers, artifacts, policy, policy_digest)?;
    let expected = AtomRecord::from_core(&body, &state)?;
    let canonical_len = u64::try_from(canonical_body.len())
        .map_err(|_| StoreError::Integrity("atom body length exceeds u64".to_owned()))?;
    let search_atom_id = stored
        .search_atom_id
        .as_deref()
        .map(|value| digest_array(value, "search projection atom_id"))
        .transpose()?;
    let projection_digest = stored
        .projection_digest
        .as_deref()
        .map(|value| digest_array(value, "projection_digest"))
        .transpose()?;
    let authority_matches = expected.atom_id == atom_id
        && expected.atom_kind == stored.atom_kind
        && expected.memory_space_id == stored.memory_space_id
        && expected.repository_id == stored.repository_id
        && expected.task_id == stored.task_id
        && expected.agent_id == stored.agent_id
        && expected.session_id == stored.session_id
        && expected.recorded_at_us == recorded_at_us
        && expected.body_digest == body_digest
        && canonical_len == body_bytes
        && expected.retention_permission == stored.retention_permission
        && expected.provenance_digest == provenance_digest
        && expected.provenance_producer == provenance.producer
        && expected.provenance_policy_digest == provenance.policy_digest
        && expected.canonical_provenance == provenance.canonical_provenance
        && expected.provenance_json_projection == provenance.json_projection
        && expected.provenance_source_digests == provenance.source_event_digests
        && expected.canonical_body == canonical_body
        && expected.json_projection == json_projection
        && expected.retention_tier == retention_tier_text
        && expected.content_status == content_status_text
        && expected.expires_at_us == expires_at_us
        && search_atom_id == Some(atom_id)
        && stored.search_memory_space_id.as_deref() == Some(expected.memory_space_id.as_str())
        && stored.search_repository_id == expected.repository_id
        && stored.searchable_text.as_deref() == Some(expected.searchable_text.as_str())
        && stored.topic_keys_json.as_deref() == Some(expected.topic_keys_json.as_str())
        && stored.entity_ids_json.as_deref() == Some(expected.entity_ids_json.as_str())
        && stored.outcome_class == expected.outcome_class
        && projection_digest == Some(sha256_projection(&expected));
    if !authority_matches {
        return Err(StoreError::Integrity(format!(
            "atom {} failed body, JSON, header, state, or search-projection closure",
            hex::encode(atom_id)
        )));
    }

    let fts_authority = fts.remove(&atom_id).ok_or_else(|| {
        StoreError::Integrity(format!(
            "atom {} is missing its FTS row",
            hex::encode(atom_id)
        ))
    })?;
    if fts_authority.row_count != 1
        || fts_authority.has_conflicting_text
        || fts_authority.searchable_text_digest != sha256(expected.searchable_text.as_bytes())
    {
        return Err(StoreError::Integrity(format!(
            "atom {} failed exact FTS closure",
            hex::encode(atom_id)
        )));
    }

    let mut expected_provenance_artifacts = body
        .artifacts
        .iter()
        .map(|artifact| ArtifactLinkAuthority {
            digest: ArtifactDigest(artifact.digest.0),
            role: artifact.media_type.clone(),
            retention_allowed: artifact.retention_allowed,
        })
        .collect::<Vec<_>>();
    expected_provenance_artifacts.sort_unstable();
    let mut expected_live_artifacts = expected_provenance_artifacts
        .iter()
        .filter(|artifact| {
            !matches!(
                state.retention_tier,
                RetentionTierV1::EpisodicLongTerm | RetentionTierV1::DualLongTerm
            ) || artifact.retention_allowed
        })
        .cloned()
        .collect::<Vec<_>>();
    expected_live_artifacts.sort_unstable();
    let mut observed_live_artifacts = live_artifacts.to_vec();
    observed_live_artifacts.sort_unstable();
    let mut observed_provenance_artifacts = provenance_artifacts.to_vec();
    observed_provenance_artifacts.sort_unstable();
    if expected_live_artifacts != observed_live_artifacts
        || expected_provenance_artifacts != observed_provenance_artifacts
    {
        return Err(StoreError::Integrity(format!(
            "atom {} failed declared artifact-link closure",
            hex::encode(atom_id)
        )));
    }

    let mut expected_relations = body
        .relations
        .iter()
        .map(|relation| {
            Ok(RelationAuthority {
                target_atom_id: relation.target.digest().0,
                relation_kind: relation_kind_text(relation.kind),
                relation_digest: sha256(&relation.canonical_bytes()?),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    expected_relations.sort_unstable();
    let mut observed_relations = declared_relations.to_vec();
    observed_relations.sort_unstable();
    if expected_relations != observed_relations {
        return Err(StoreError::Integrity(format!(
            "atom {} failed declared relation closure",
            hex::encode(atom_id)
        )));
    }

    let expected_pins = expected_artifact_pins(&body, &state)?;
    let mut observed_pins = pins.to_vec();
    observed_pins.sort_unstable();
    if expected_pins != observed_pins {
        return Err(StoreError::Integrity(format!(
            "atom {} failed exact artifact-pin closure",
            hex::encode(atom_id)
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_complete_mutable_state(
    atom_id: [u8; 32],
    stored: &StoredIntegrityAtom,
    recorded_at_us: u64,
    state: &AtomStateV1,
    headers: &BTreeMap<[u8; 32], HeaderAuthority>,
    runs: &BTreeMap<[u8; 32], RetirementRunAuthority>,
    decisions: &RetirementDecisionAuthorities,
    policy: &PolicyV1,
) -> Result<()> {
    let status_is_body_bearing = matches!(
        state.content_status,
        ContentStatusV1::Present | ContentStatusV1::Corrupt
    );
    let structural_state_valid = match state.retention_tier {
        RetentionTierV1::ShortTerm => {
            stored.atom_kind == "episode"
                && status_is_body_bearing
                && state.retirement_run_id.is_none()
                && state.superseded_by.is_none()
                && state.expires_at_us == recorded_at_us.checked_add(policy.stm_horizon_us)
        }
        RetentionTierV1::SemanticLongTerm => {
            stored.atom_kind == "semantic"
                && status_is_body_bearing
                && state.expires_at_us.is_none()
                && state.retirement_run_id.is_none()
        }
        RetentionTierV1::EpisodicLongTerm | RetentionTierV1::DualLongTerm => {
            stored.atom_kind == "episode"
                && status_is_body_bearing
                && state.expires_at_us.is_none()
                && state.superseded_by.is_none()
                && state.retirement_run_id.is_some()
        }
        RetentionTierV1::HeaderOnly => {
            state.content_status == ContentStatusV1::Forgotten
                && state.expires_at_us.is_none()
                && state.superseded_by.is_none()
                && state.retirement_run_id.is_some()
        }
    };
    if !structural_state_valid {
        return Err(StoreError::Integrity(format!(
            "atom {} has an invalid complete mutable state",
            hex::encode(atom_id)
        )));
    }

    if let Some(target) = state.superseded_by {
        let target_id = target.digest().0;
        let target_header = headers.get(&target_id).ok_or_else(|| {
            StoreError::Integrity(format!(
                "atom {} supersession target is missing",
                hex::encode(atom_id)
            ))
        })?;
        if target_id == atom_id
            || stored.atom_kind != "semantic"
            || target_header.atom_kind != "semantic"
            || target_header.memory_space_id != stored.memory_space_id
        {
            return Err(StoreError::Integrity(format!(
                "atom {} has an invalid supersession boundary",
                hex::encode(atom_id)
            )));
        }
    }

    if let Some(run_id) = state.retirement_run_id {
        let run_id = run_id.0;
        let run = runs.get(&run_id).ok_or_else(|| {
            StoreError::Integrity(format!(
                "atom {} references missing retirement run {}",
                hex::encode(atom_id),
                hex::encode(run_id)
            ))
        })?;
        let decision = decisions.get(&(run_id, atom_id)).ok_or_else(|| {
            StoreError::Integrity(format!(
                "atom {} has no decision in its retirement run",
                hex::encode(atom_id)
            ))
        })?;
        let decision_matches = match state.retention_tier {
            RetentionTierV1::EpisodicLongTerm => {
                matches!(
                    decision.as_str(),
                    "episodic_winner" | "semantic_and_episodic"
                )
            }
            RetentionTierV1::DualLongTerm => decision == "semantic_and_episodic",
            RetentionTierV1::HeaderOnly => {
                matches!(decision.as_str(), "semantic_source" | "forgotten")
            }
            RetentionTierV1::ShortTerm | RetentionTierV1::SemanticLongTerm => false,
        };
        if run.status != "applied"
            || run.memory_space_id != stored.memory_space_id
            || !decision_matches
        {
            return Err(StoreError::Integrity(format!(
                "atom {} failed retirement-run state closure",
                hex::encode(atom_id)
            )));
        }
    }
    Ok(())
}

fn validate_typed_body_structure(
    body: &MemoryAtomBodyV1,
    atom_id: [u8; 32],
    headers: &BTreeMap<[u8; 32], HeaderAuthority>,
    artifacts: &BTreeMap<ArtifactDigest, ArtifactAuthority>,
    policy: &PolicyV1,
    policy_digest: Digest32,
) -> Result<()> {
    naome_memory_core::validate_memory_atom_body(body, policy).map_err(|error| {
        StoreError::Integrity(format!(
            "atom {} failed canonical core body validation: {error}",
            hex::encode(atom_id)
        ))
    })?;
    if body.contract_version != "atom-v1"
        || body.scope.memory_space_id.is_empty()
        || body.scope.session_id.is_empty()
        || body.trigger.is_empty()
        || body.provenance.producer.is_empty()
        || body.provenance.policy_digest != policy_digest
        || body.interval.started_at_us > body.interval.ended_at_us
        || body.interval.ended_at_us > body.interval.recorded_at_us
        || [
            body.scope.repository_id.as_deref(),
            body.scope.task_id.as_deref(),
            body.scope.agent_id.as_deref(),
        ]
        .into_iter()
        .any(|value| value == Some(""))
        || body.observations.iter().any(|observation| {
            observation.at_us < body.interval.started_at_us
                || observation.at_us > body.interval.ended_at_us
        })
    {
        return Err(StoreError::Integrity(format!(
            "atom {} has invalid typed body structure, scope, time, or provenance",
            hex::encode(atom_id)
        )));
    }

    let mut artifact_keys = BTreeSet::new();
    let declared_artifact_digests = body
        .artifacts
        .iter()
        .map(|artifact| artifact.digest)
        .collect::<BTreeSet<_>>();
    for artifact in &body.artifacts {
        let digest = ArtifactDigest(artifact.digest.0);
        let authority = artifacts.get(&digest).ok_or_else(|| {
            StoreError::Integrity(format!(
                "atom {} declares missing artifact {digest}",
                hex::encode(atom_id)
            ))
        })?;
        if artifact.media_type.is_empty()
            || artifact.size_bytes > policy.artifact_max_bytes
            || authority.record.byte_len != artifact.size_bytes
            || authority.integrity_status != "verified"
            || !artifact_keys.insert((artifact.digest, artifact.media_type.as_str()))
        {
            return Err(StoreError::Integrity(format!(
                "atom {} has invalid artifact declaration {digest}",
                hex::encode(atom_id)
            )));
        }
        if let Some(inline) = artifact.inline_payload.as_deref() {
            let inline_len = u64::try_from(inline.len()).map_err(|_| {
                StoreError::Integrity("inline artifact length exceeds u64".to_owned())
            })?;
            if inline_len > policy.inline_payload_max_bytes
                || inline_len != artifact.size_bytes
                || Digest32::hash_prefixed(&[], inline) != artifact.digest
            {
                return Err(StoreError::Integrity(format!(
                    "atom {} has invalid inline artifact authority",
                    hex::encode(atom_id)
                )));
            }
        }
    }
    if body.observations.iter().any(|observation| {
        observation
            .artifact_digest
            .is_some_and(|digest| !declared_artifact_digests.contains(&digest))
    }) {
        return Err(StoreError::Integrity(format!(
            "atom {} observation artifact provenance is not declared",
            hex::encode(atom_id)
        )));
    }

    let mut relation_keys = BTreeSet::new();
    for relation in &body.relations {
        let target_id = relation.target.digest().0;
        let target = headers.get(&target_id).ok_or_else(|| {
            StoreError::Integrity(format!(
                "atom {} declares missing relation target {}",
                hex::encode(atom_id),
                hex::encode(target_id)
            ))
        })?;
        let relation_kind = relation_kind_text(relation.kind);
        if target_id == atom_id
            || target.memory_space_id != body.scope.memory_space_id
            || !relation_keys.insert((target_id, relation_kind))
        {
            return Err(StoreError::Integrity(format!(
                "atom {} has an invalid or duplicate relation declaration",
                hex::encode(atom_id)
            )));
        }
    }

    match &body.payload {
        MemoryPayloadV1::Episode(payload) => {
            let continues = body
                .relations
                .iter()
                .filter_map(|relation| {
                    (relation.kind == MemoryRelationKindV1::Continues).then_some(relation.target)
                })
                .collect::<Vec<_>>();
            if payload.event_sequence_start > payload.event_sequence_end
                || continues != payload.continues.into_iter().collect::<Vec<_>>()
            {
                return Err(StoreError::Integrity(format!(
                    "episode atom {} has invalid sequence or continues closure",
                    hex::encode(atom_id)
                )));
            }
        }
        MemoryPayloadV1::Semantic(payload) => {
            let source_ids = payload
                .source_ids
                .iter()
                .map(|value| value.digest().0)
                .collect::<Vec<_>>();
            let strictly_sorted = source_ids.windows(2).all(|pair| pair[0] < pair[1]);
            let source_digest_keys = payload
                .source_body_digests
                .keys()
                .map(|value| value.digest().0)
                .collect::<Vec<_>>();
            let derived_from = body
                .relations
                .iter()
                .filter_map(|relation| {
                    (relation.kind == MemoryRelationKindV1::DerivedFrom)
                        .then_some(relation.target.digest().0)
                })
                .collect::<Vec<_>>();
            let provenance_digests = source_ids.iter().copied().map(Digest32).collect::<Vec<_>>();
            if source_ids.len() < policy.min_semantic_sources
                || !strictly_sorted
                || source_digest_keys != source_ids
                || derived_from != source_ids
                || body.provenance.source_event_digests != provenance_digests
                || payload.consensus_numerator != policy.consensus_numerator
                || payload.consensus_denominator != policy.consensus_denominator
                || body.retention_permission != RetentionPermissionV1::SemanticAllowed
            {
                return Err(StoreError::Integrity(format!(
                    "semantic atom {} has incomplete source provenance",
                    hex::encode(atom_id)
                )));
            }
            for source_id in &payload.source_ids {
                let source = headers.get(&source_id.digest().0).ok_or_else(|| {
                    StoreError::Integrity(format!(
                        "semantic atom {} has a missing source",
                        hex::encode(atom_id)
                    ))
                })?;
                if source.atom_kind != "episode"
                    || source.memory_space_id != body.scope.memory_space_id
                    || payload.source_body_digests.get(source_id).copied()
                        != Some(Digest32(source.body_digest))
                {
                    return Err(StoreError::Integrity(format!(
                        "semantic atom {} has invalid source digest or scope provenance",
                        hex::encode(atom_id)
                    )));
                }
            }
        }
    }
    Ok(())
}

fn expected_artifact_pins(
    body: &MemoryAtomBodyV1,
    state: &AtomStateV1,
) -> Result<Vec<(ArtifactDigest, [u8; 32])>> {
    if !matches!(
        state.retention_tier,
        RetentionTierV1::EpisodicLongTerm | RetentionTierV1::DualLongTerm
    ) {
        return Ok(Vec::new());
    }
    let run_id = state.retirement_run_id.ok_or_else(|| {
        StoreError::Integrity("episodic LTM atom is missing retirement run authority".to_owned())
    })?;
    let mut pins = body
        .artifacts
        .iter()
        .filter(|artifact| artifact.retention_allowed)
        .map(|artifact| (ArtifactDigest(artifact.digest.0), run_id.0))
        .collect::<Vec<_>>();
    pins.sort_unstable();
    pins.dedup();
    Ok(pins)
}

fn verify_persisted_poc_policy(connection: &Connection) -> Result<()> {
    let persisted = connection
        .query_row(
            "SELECT policy_digest, canonical_body, json_projection, installed_at_us
             FROM policies WHERE policy_id = 'poc-v1'",
            [],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| StoreError::Integrity("poc-v1 policy authority is missing".to_owned()))?;
    let policy_count: i64 =
        connection.query_row("SELECT COUNT(*) FROM policies", [], |row| row.get(0))?;
    let expected = PolicyV1::poc_v1();
    let expected_digest = expected.digest()?;
    let expected_canonical = expected.canonical_bytes()?;
    let expected_json = serde_json::to_string(&expected)?;
    if policy_count != 1
        || persisted.0 != expected_digest.0
        || persisted.1 != expected_canonical
        || persisted.2 != expected_json
        || persisted.3 < 0
    {
        return Err(StoreError::Integrity(
            "persisted poc-v1 policy does not match committed authority".to_owned(),
        ));
    }
    Ok(())
}

fn verify_persisted_core_receipt(
    connection: &Connection,
    run_id: [u8; 32],
    expected_plan: Option<&RetirementPlanV1>,
) -> Result<ProofReceiptV1> {
    let run = connection
        .query_row(
            "SELECT memory_space_id, cohort_day, as_of_us, policy_id, policy_digest, seed,
                    rng_id, input_set_digest, population_digest, plan_digest,
                    projected_state_digest, status, receipt_digest
             FROM retirement_runs WHERE run_id = ?1",
            [&run_id[..]],
            |row| {
                Ok(StoredAppliedRun {
                    memory_space_id: row.get(0)?,
                    cohort_day: row.get(1)?,
                    as_of_us: row.get(2)?,
                    policy_id: row.get(3)?,
                    policy_digest: row.get(4)?,
                    seed: row.get(5)?,
                    rng_identifier: row.get(6)?,
                    input_set_digest: row.get(7)?,
                    population_digest: row.get(8)?,
                    plan_digest: row.get(9)?,
                    projected_state_digest: row.get(10)?,
                    status: row.get(11)?,
                    receipt_digest: row.get(12)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| {
            StoreError::Integrity(format!(
                "applied retirement run {} is missing",
                hex::encode(run_id)
            ))
        })?;
    if run.status != "applied" {
        return Err(StoreError::Integrity(format!(
            "retirement run {} is not applied",
            hex::encode(run_id)
        )));
    }
    let run_receipt_digest = run
        .receipt_digest
        .as_deref()
        .ok_or_else(|| StoreError::Integrity("applied run is missing receipt_digest".to_owned()))?;
    let persisted_receipt = connection
        .query_row(
            "SELECT receipt_digest, canonical_receipt, json_projection, closure_digest,
                    created_at_us
             FROM proof_receipts WHERE run_id = ?1",
            [&run_id[..]],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            StoreError::Integrity(format!(
                "applied retirement run {} has no proof receipt",
                hex::encode(run_id)
            ))
        })?;
    let receipt_digest = digest_array(&persisted_receipt.0, "receipt_digest")?;
    let closure_digest = digest_array(&persisted_receipt.3, "closure_digest")?;
    if run_receipt_digest != receipt_digest
        || unsigned_integer(persisted_receipt.4, "receipt created_at_us")?
            != unsigned_integer(run.as_of_us, "run as_of_us")?
    {
        return Err(StoreError::Integrity(
            "retirement run and proof receipt binding mismatch".to_owned(),
        ));
    }
    let receipt: ProofReceiptV1 = serde_json::from_str(&persisted_receipt.2)?;
    let canonical_receipt = receipt.canonical_bytes()?;
    let canonical_closure = receipt.closure.canonical_bytes()?;
    let committed_policy = PolicyV1::poc_v1();
    let expected_run_id = Digest32::hash_prefixed(
        b"naome-memory:retirement-run:v1\0",
        receipt.plan_digest.as_bytes(),
    );
    let expected_invariants = [
        "budgets_structurally_bounded",
        "plan_structure_verified",
        "repository_outcome_closure_verified",
    ]
    .into_iter()
    .map(|name| {
        (
            name.to_owned(),
            naome_memory_core::InvariantStatusV1::Verified,
        )
    })
    .collect::<BTreeMap<_, _>>();
    if receipt.contract_version != "proof-receipt-v1"
        || receipt.receipt_id.0 != receipt_digest
        || receipt.recompute_receipt_id()? != receipt.receipt_id
        || canonical_receipt != persisted_receipt.1
        || serde_json::to_string(&receipt)? != persisted_receipt.2
        || sha256(&canonical_closure) != closure_digest
        || receipt.run_id.0 != run_id
        || receipt.run_id != expected_run_id
        || receipt.memory_space_id != run.memory_space_id
        || receipt.cohort_day != unsigned_integer(run.cohort_day, "cohort_day")?
        || receipt.as_of_us != unsigned_integer(run.as_of_us, "as_of_us")?
        || receipt.policy_id != run.policy_id
        || receipt.policy_digest.0 != digest_array(&run.policy_digest, "policy_digest")?
        || receipt.policy_id != committed_policy.policy_id
        || receipt.policy_digest != committed_policy.digest()?
        || receipt.rng_identifier != "chacha20-rand_chacha-0.3/partial-fisher-yates-v1"
        || receipt.seed.as_bytes() != &digest_array(&run.seed, "seed")?
        || receipt.rng_identifier != run.rng_identifier
        || receipt.input_set_digest.0 != digest_array(&run.input_set_digest, "input_set_digest")?
        || receipt.population_digest.0 != digest_array(&run.population_digest, "population_digest")?
        || receipt.plan_digest.0 != digest_array(&run.plan_digest, "plan_digest")?
        || receipt.state_digest.0
            != digest_array(&run.projected_state_digest, "projected_state_digest")?
        || receipt.overall_status != naome_memory_core::OverallProofStatusV1::Passed
        || receipt.invariants != expected_invariants
    {
        return Err(StoreError::Integrity(format!(
            "proof receipt {} failed authority or run-field verification",
            hex::encode(receipt_digest)
        )));
    }

    let mut statement = connection.prepare(
        "SELECT d.atom_id, d.decision, d.canonical_details, d.details_digest
         FROM retirement_decisions AS d
         WHERE d.run_id = ?1 ORDER BY d.atom_id",
    )?;
    let rows = statement.query_map([&run_id[..]], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Vec<u8>>(2)?,
            row.get::<_, Vec<u8>>(3)?,
        ))
    })?;
    let mut decisions = Vec::new();
    for row in rows {
        let row = row?;
        let atom_id = digest_array(&row.0, "decision atom_id")?;
        let details_digest = digest_array(&row.3, "decision details_digest")?;
        if sha256(&row.2) != details_digest {
            return Err(StoreError::Integrity(format!(
                "retirement decision {} details digest mismatch",
                hex::encode(atom_id)
            )));
        }
        decisions.push((atom_id, row.1, row.2, details_digest));
    }
    let member_ids = {
        let mut statement = connection
            .prepare("SELECT atom_id FROM retirement_members WHERE run_id = ?1 ORDER BY atom_id")?;
        let rows = statement.query_map([&run_id[..]], |row| row.get::<_, Vec<u8>>(0))?;
        let mut values = Vec::new();
        for row in rows {
            values.push(digest_array(&row?, "retirement member atom_id")?);
        }
        values
    };
    let decision_ids = decisions
        .iter()
        .map(|decision| decision.0)
        .collect::<Vec<_>>();
    let decision_authority = decisions
        .iter()
        .map(|decision| decision.2.as_slice())
        .collect::<Vec<_>>();
    if persisted_decision_digest(&decision_authority)? != receipt.decision_digest {
        return Err(StoreError::Integrity(
            "receipt decision digest does not match persisted decision details".to_owned(),
        ));
    }
    let closure_source_ids = receipt
        .closure
        .source_atom_ids
        .iter()
        .map(|atom_id| atom_id.digest().0)
        .collect::<Vec<_>>();
    if decision_ids != member_ids || decision_ids != closure_source_ids {
        return Err(StoreError::Integrity(
            "receipt source closure does not match persisted retirement decisions".to_owned(),
        ));
    }
    let exact_episode_ids = decisions
        .iter()
        .filter(|decision| {
            matches!(
                decision.1.as_str(),
                "episodic_winner" | "semantic_and_episodic"
            )
        })
        .map(|decision| AtomId(Digest32(decision.0)))
        .collect::<Vec<_>>();
    if exact_episode_ids != receipt.closure.exact_episode_ids {
        return Err(StoreError::Integrity(
            "receipt episodic closure does not match persisted decisions".to_owned(),
        ));
    }
    let mut exact_artifacts = receipt
        .closure
        .exact_episode_ids
        .iter()
        .copied()
        .map(|atom_id| (atom_id, Vec::new()))
        .collect::<BTreeMap<_, _>>();
    let mut statement = connection.prepare(
        "SELECT atom_id, artifact_digest FROM artifact_pins
         WHERE run_id = ?1 ORDER BY atom_id, artifact_digest",
    )?;
    let rows = statement.query_map([&run_id[..]], |row| {
        Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
    })?;
    for row in rows {
        let (atom_id, artifact_digest) = row?;
        exact_artifacts
            .entry(AtomId(Digest32(digest_array(&atom_id, "pin atom_id")?)))
            .or_default()
            .push(Digest32(digest_array(
                &artifact_digest,
                "pin artifact_digest",
            )?));
    }
    if exact_artifacts != receipt.closure.exact_artifact_digests {
        return Err(StoreError::Integrity(
            "receipt artifact closure does not match persisted pins".to_owned(),
        ));
    }

    let episodic_population_count: i64 = connection.query_row(
        "SELECT COUNT(*)
         FROM retirement_members AS member
         JOIN atom_headers AS header ON header.atom_id = member.atom_id
         WHERE member.run_id = ?1
           AND header.retention_permission = 'semantic_and_exact_allowed'",
        [&run_id[..]],
        |row| row.get(0),
    )?;
    let episodic_population_count = usize::try_from(episodic_population_count).map_err(|_| {
        StoreError::Integrity("negative or oversized episodic population count".to_owned())
    })?;
    let expected_winner_budget = committed_policy
        .episodic_rate
        .ceil_count(episodic_population_count)
        .min(committed_policy.episodic_count_max);
    let mut episodic_winner_bytes = 0_u64;
    for atom_id in &receipt.closure.exact_episode_ids {
        let (header_body_bytes, canonical_body, artifact_bytes): (i64, Vec<u8>, i64) = connection
            .query_row(
            "SELECT h.body_bytes, b.canonical_body,
                    COALESCE((
                        SELECT SUM(a.byte_len)
                        FROM artifact_refs AS reference
                        JOIN artifacts AS a
                          ON a.artifact_digest = reference.artifact_digest
                        WHERE reference.atom_id = h.atom_id
                          AND reference.retention_allowed = 1
                    ), 0)
             FROM atom_headers AS h
             JOIN atom_bodies AS b ON b.atom_id = h.atom_id
             WHERE h.atom_id = ?1",
            [atom_id.digest().as_bytes()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let header_body_bytes = unsigned_integer(header_body_bytes, "episodic body_bytes")?;
        let body_bytes = u64::try_from(canonical_body.len())
            .map_err(|_| StoreError::Integrity("episodic body length exceeds u64".to_owned()))?;
        if header_body_bytes != body_bytes
            || Digest32::hash_prefixed(ATOM_HASH_DOMAIN, &canonical_body) != atom_id.digest()
        {
            return Err(StoreError::Integrity(format!(
                "episodic winner {atom_id} failed exact body authority verification"
            )));
        }
        let artifact_bytes = unsigned_integer(artifact_bytes, "episodic artifact_bytes")?;
        episodic_winner_bytes = episodic_winner_bytes
            .checked_add(body_bytes)
            .and_then(|total| total.checked_add(artifact_bytes))
            .ok_or_else(|| {
                StoreError::Integrity("episodic winner byte count overflow".to_owned())
            })?;
    }
    if receipt.episodic_population_count != episodic_population_count
        || receipt.episodic_winner_budget != expected_winner_budget
        || receipt.closure.exact_episode_ids.len() != expected_winner_budget
        || receipt.episodic_winner_bytes != episodic_winner_bytes
    {
        return Err(StoreError::Integrity(
            "receipt episodic population, budget, or byte closure mismatch".to_owned(),
        ));
    }

    let source_set = receipt
        .closure
        .source_atom_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let semantic_set = receipt
        .closure
        .semantic_atom_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if source_set.len() != receipt.closure.source_atom_ids.len()
        || semantic_set.len() != receipt.closure.semantic_atom_ids.len()
    {
        return Err(StoreError::Integrity(
            "receipt closure contains duplicate atom IDs".to_owned(),
        ));
    }
    let mut semantic_body_bytes = 0_u64;
    let mut semantic_source_ids_from_outputs = BTreeSet::new();
    for semantic_id in &receipt.closure.semantic_atom_ids {
        let (atom_kind, memory_space_id, body_digest, canonical_body, json_projection) = connection
            .query_row(
                "SELECT h.atom_kind, h.memory_space_id, h.body_digest,
                            b.canonical_body, b.json_projection
                     FROM atom_headers AS h
                     JOIN atom_bodies AS b ON b.atom_id = h.atom_id
                     WHERE h.atom_id = ?1",
                [semantic_id.digest().as_bytes()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::Integrity(format!("receipt semantic atom {semantic_id} is missing"))
            })?;
        let body: MemoryAtomBodyV1 = serde_json::from_str(&json_projection)?;
        let body_authority = body.canonical_bytes()?;
        semantic_body_bytes = semantic_body_bytes
            .checked_add(u64::try_from(canonical_body.len()).map_err(|_| {
                StoreError::Integrity("semantic body byte count exceeds u64".to_owned())
            })?)
            .ok_or_else(|| StoreError::Integrity("semantic body byte count overflow".to_owned()))?;
        if atom_kind != "semantic"
            || memory_space_id != receipt.memory_space_id
            || body.atom_id()? != *semantic_id
            || body_digest != semantic_id.digest().0
            || canonical_body != body_authority
        {
            return Err(StoreError::Integrity(format!(
                "receipt semantic atom {semantic_id} failed authority or scope verification"
            )));
        }
        let MemoryPayloadV1::Semantic(payload) = &body.payload else {
            return Err(StoreError::Integrity(format!(
                "receipt semantic atom {semantic_id} has episode payload"
            )));
        };
        let payload_source_set = payload.source_ids.iter().copied().collect::<BTreeSet<_>>();
        let digest_source_set = payload
            .source_body_digests
            .keys()
            .copied()
            .collect::<BTreeSet<_>>();
        if payload_source_set != digest_source_set
            || !payload_source_set.is_subset(&source_set)
            || payload_source_set.len() != payload.source_ids.len()
        {
            return Err(StoreError::Integrity(format!(
                "receipt semantic atom {semantic_id} has incomplete source provenance"
            )));
        }
        semantic_source_ids_from_outputs.extend(payload_source_set.iter().copied());
        for (source_id, expected_body_digest) in &payload.source_body_digests {
            let persisted_source_digest: Vec<u8> = connection.query_row(
                "SELECT body_digest FROM atom_headers WHERE atom_id = ?1",
                [source_id.digest().as_bytes()],
                |row| row.get(0),
            )?;
            if persisted_source_digest != expected_body_digest.0 {
                return Err(StoreError::Integrity(format!(
                    "semantic atom {semantic_id} source digest mismatch for {source_id}"
                )));
            }
        }
    }
    let exact_set = receipt
        .closure
        .exact_episode_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    for decision in &decisions {
        let atom_id = AtomId(Digest32(decision.0));
        let semantic_source = semantic_source_ids_from_outputs.contains(&atom_id);
        let exact = exact_set.contains(&atom_id);
        let expected_name = match (semantic_source, exact) {
            (true, true) => "semantic_and_episodic",
            (true, false) => "semantic_source",
            (false, true) => "episodic_winner",
            (false, false) => "forgotten",
        };
        if decision.1 != expected_name {
            return Err(StoreError::Integrity(format!(
                "persisted decision classification for {atom_id} is not closed by outputs"
            )));
        }
    }
    let expected_semantic_budget = committed_policy
        .semantic_rate
        .ceil_count(decisions.len())
        .min(committed_policy.semantic_count_max);
    if receipt.semantic_count_budget != expected_semantic_budget
        || receipt.closure.semantic_atom_ids.len() > expected_semantic_budget
        || receipt.semantic_body_bytes != semantic_body_bytes
    {
        return Err(StoreError::Integrity(
            "receipt semantic count or byte closure mismatch".to_owned(),
        ));
    }

    let historical_post_state = load_retirement_post_state(connection, receipt.run_id)?;
    if historical_post_state.digest()? != receipt.state_digest {
        return Err(StoreError::Integrity(
            "receipt state digest does not match immutable retirement post-state evidence"
                .to_owned(),
        ));
    }
    let current_post_state =
        recompute_persisted_post_state(connection, &receipt, &decisions, &exact_artifacts)?;
    validate_live_post_state_transition(&historical_post_state, &current_post_state)?;

    if let Some(plan) = expected_plan {
        let population_matches = receipt.population_digest == plan.episodic_population_digest;
        if receipt.memory_space_id != plan.memory_space_id
            || receipt.cohort_day != plan.cohort_day
            || receipt.as_of_us != plan.as_of_us
            || receipt.policy_id != plan.policy_id
            || receipt.policy_digest != plan.policy_digest
            || receipt.seed != plan.seed
            || receipt.input_set_digest != plan.input_set_digest
            || receipt.rng_identifier != plan.rng_identifier
            || !population_matches
            || receipt.episodic_population_count != plan.episodic_population_count
            || receipt.episodic_winner_budget != plan.episodic_winner_budget
            || receipt.episodic_winner_bytes != plan.episodic_winner_bytes
            || receipt.semantic_count_budget != plan.semantic_count_budget
            || receipt.semantic_body_bytes != plan.semantic_body_bytes
            || receipt.plan_digest != plan.digest()?
            || receipt.decision_digest != plan.decision_digest()?
            || receipt.state_digest != plan.projected_state_digest
            || historical_post_state != plan.projected_post_state
            || receipt.closure != ReceiptClosureV1::from_plan(plan)
            || decisions.len() != plan.decisions.len()
        {
            return Err(StoreError::Integrity(
                "persisted receipt does not close over the replayed core plan".to_owned(),
            ));
        }
        for (persisted, expected) in decisions.iter().zip(&plan.decisions) {
            let expected_name = if expected.exact_retained && !expected.semantic_atom_ids.is_empty()
            {
                "semantic_and_episodic"
            } else if expected.exact_retained {
                "episodic_winner"
            } else if expected.forget_episode_body && !expected.semantic_atom_ids.is_empty() {
                "semantic_source"
            } else if expected.forget_episode_body {
                "forgotten"
            } else {
                "unchanged"
            };
            let expected_details = expected.canonical_bytes()?;
            if persisted.0 != expected.atom_id.digest().0
                || persisted.1 != expected_name
                || persisted.2 != expected_details
                || persisted.3 != sha256(&expected_details)
            {
                return Err(StoreError::Integrity(
                    "persisted decision details do not match the replayed core plan".to_owned(),
                ));
            }
        }
        for expected in &plan.semantic_atoms {
            let persisted: (Vec<u8>, String) = connection.query_row(
                "SELECT canonical_body, json_projection FROM atom_bodies WHERE atom_id = ?1",
                [expected.atom_id.digest().as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if persisted.0 != expected.body.canonical_bytes()?
                || persisted.1 != serde_json::to_string(&expected.body)?
            {
                return Err(StoreError::Integrity(
                    "persisted semantic body does not match the replayed core plan".to_owned(),
                ));
            }
        }
    }
    Ok(receipt)
}

fn load_retirement_post_state(
    connection: &Connection,
    run_id: Digest32,
) -> Result<RetirementPostStateV1> {
    let stored = connection
        .query_row(
            "SELECT state_digest, canonical_state, json_projection
             FROM retirement_post_states WHERE run_id = ?1",
            [run_id.as_bytes()],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            StoreError::Integrity(format!(
                "retirement run {run_id} is missing immutable post-state evidence"
            ))
        })?;
    let state: RetirementPostStateV1 = serde_json::from_str(&stored.2)?;
    if state.canonical_bytes()? != stored.1
        || state.digest()?.0 != digest_array(&stored.0, "retirement state_digest")?
        || serde_json::to_string(&state)? != stored.2
    {
        return Err(StoreError::Integrity(format!(
            "retirement run {run_id} has invalid immutable post-state evidence"
        )));
    }
    Ok(state)
}

fn validate_live_post_state_transition(
    historical: &RetirementPostStateV1,
    current: &RetirementPostStateV1,
) -> Result<()> {
    if historical.memory_space_id != current.memory_space_id
        || historical.source_atoms.len() != current.source_atoms.len()
        || historical.semantic_atoms.len() != current.semantic_atoms.len()
    {
        return Err(StoreError::Integrity(
            "live retirement state no longer closes over the historical atom set".to_owned(),
        ));
    }
    for (historical_atom, current_atom) in historical
        .source_atoms
        .iter()
        .chain(&historical.semantic_atoms)
        .zip(current.source_atoms.iter().chain(&current.semantic_atoms))
    {
        if current_atom.feedback_positive < historical_atom.feedback_positive
            || current_atom.feedback_negative < historical_atom.feedback_negative
            || current_atom.retrieval_count < historical_atom.retrieval_count
        {
            return Err(StoreError::Integrity(format!(
                "live mutable counters for {} moved behind retirement history",
                current_atom.atom_id
            )));
        }
        let mut normalized = current_atom.clone();
        normalized.feedback_positive = historical_atom.feedback_positive;
        normalized.feedback_negative = historical_atom.feedback_negative;
        normalized.retrieval_count = historical_atom.retrieval_count;
        if &normalized != historical_atom {
            return Err(StoreError::Integrity(format!(
                "live structural state for {} diverges from retirement history",
                current_atom.atom_id
            )));
        }
    }
    Ok(())
}

fn recompute_persisted_post_state(
    connection: &Connection,
    receipt: &ProofReceiptV1,
    decisions: &[PersistedDecisionAuthority],
    exact_artifacts: &BTreeMap<AtomId, Vec<Digest32>>,
) -> Result<RetirementPostStateV1> {
    let mut statement = connection.prepare(
        "SELECT h.atom_id, h.atom_kind, h.memory_space_id, h.repository_id, h.task_id,
                h.agent_id, h.session_id, h.recorded_at_us, h.body_digest, h.body_bytes,
                h.retention_permission, h.provenance_digest,
                b.canonical_body, b.json_projection,
                s.retention_tier, s.content_status, s.expires_at_us, s.superseded_by,
                s.feedback_positive, s.feedback_negative, s.retrieval_count,
                s.retirement_run_id,
                p.atom_id, p.searchable_text, p.topic_keys_json, p.entity_ids_json,
                p.outcome_class, p.projection_digest
         FROM retirement_decisions AS d
         JOIN atom_headers AS h ON h.atom_id = d.atom_id
         JOIN atom_state AS s ON s.atom_id = d.atom_id
         LEFT JOIN atom_bodies AS b ON b.atom_id = d.atom_id
         LEFT JOIN search_projection AS p ON p.atom_id = d.atom_id
         WHERE d.run_id = ?1
         ORDER BY d.atom_id",
    )?;
    let rows = statement.query_map([receipt.run_id.as_bytes()], stored_post_state_atom_from_row)?;
    let mut source_rows = Vec::new();
    for row in rows {
        source_rows.push(row?);
    }
    if source_rows.len() != decisions.len() {
        return Err(StoreError::Integrity(
            "persisted post-state does not close over every decision".to_owned(),
        ));
    }
    let mut source_atoms = Vec::with_capacity(source_rows.len());
    for (stored, decision) in source_rows.iter().zip(decisions) {
        let atom_id = AtomId(Digest32(digest_array(
            &stored.atom_id,
            "post-state atom_id",
        )?));
        if atom_id.digest().0 != decision.0 {
            return Err(StoreError::Integrity(
                "persisted post-state order does not match decisions".to_owned(),
            ));
        }
        let pins = exact_artifacts.get(&atom_id).cloned().unwrap_or_default();
        source_atoms.push(post_state_atom_from_stored(
            connection,
            stored,
            &receipt.memory_space_id,
            Some((&decision.1, receipt.run_id)),
            pins,
        )?);
    }

    let forgotten_fts_rows: i64 = connection.query_row(
        "SELECT COUNT(*) FROM atom_search
         WHERE atom_id IN (
             SELECT lower(hex(atom_id)) FROM retirement_decisions
             WHERE run_id = ?1 AND decision IN ('semantic_source', 'forgotten')
         )",
        [receipt.run_id.as_bytes()],
        |row| row.get(0),
    )?;
    let forgotten_artifact_refs: i64 = connection.query_row(
        "SELECT COUNT(*) FROM artifact_refs
         WHERE atom_id IN (
             SELECT atom_id FROM retirement_decisions
             WHERE run_id = ?1 AND decision IN ('semantic_source', 'forgotten')
         )",
        [receipt.run_id.as_bytes()],
        |row| row.get(0),
    )?;
    if forgotten_fts_rows != 0 || forgotten_artifact_refs != 0 {
        return Err(StoreError::Integrity(
            "forgotten source retains FTS or live artifact-reference state".to_owned(),
        ));
    }

    let mut semantic_atoms = Vec::with_capacity(receipt.closure.semantic_atom_ids.len());
    for atom_id in &receipt.closure.semantic_atom_ids {
        let stored = connection
            .query_row(
                "SELECT h.atom_id, h.atom_kind, h.memory_space_id, h.repository_id, h.task_id,
                        h.agent_id, h.session_id, h.recorded_at_us, h.body_digest, h.body_bytes,
                        h.retention_permission, h.provenance_digest,
                        b.canonical_body, b.json_projection,
                        s.retention_tier, s.content_status, s.expires_at_us, s.superseded_by,
                        s.feedback_positive, s.feedback_negative, s.retrieval_count,
                        s.retirement_run_id,
                        p.atom_id, p.searchable_text, p.topic_keys_json, p.entity_ids_json,
                        p.outcome_class, p.projection_digest
                 FROM atom_headers AS h
                 JOIN atom_state AS s ON s.atom_id = h.atom_id
                 LEFT JOIN atom_bodies AS b ON b.atom_id = h.atom_id
                 LEFT JOIN search_projection AS p ON p.atom_id = h.atom_id
                 WHERE h.atom_id = ?1",
                [atom_id.digest().as_bytes()],
                stored_post_state_atom_from_row,
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::Integrity(format!("semantic post-state atom {atom_id} is missing"))
            })?;
        semantic_atoms.push(post_state_atom_from_stored(
            connection,
            &stored,
            &receipt.memory_space_id,
            None,
            Vec::new(),
        )?);
    }
    Ok(RetirementPostStateV1 {
        memory_space_id: receipt.memory_space_id.clone(),
        source_atoms,
        semantic_atoms,
    })
}

fn stored_post_state_atom_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<StoredPostStateAtom> {
    Ok(StoredPostStateAtom {
        atom_id: row.get(0)?,
        atom_kind: row.get(1)?,
        memory_space_id: row.get(2)?,
        repository_id: row.get(3)?,
        task_id: row.get(4)?,
        agent_id: row.get(5)?,
        session_id: row.get(6)?,
        recorded_at_us: row.get(7)?,
        body_digest: row.get(8)?,
        body_bytes: row.get(9)?,
        retention_permission: row.get(10)?,
        provenance_digest: row.get(11)?,
        canonical_body: row.get(12)?,
        json_projection: row.get(13)?,
        retention_tier: row.get(14)?,
        content_status: row.get(15)?,
        expires_at_us: row.get(16)?,
        superseded_by: row.get(17)?,
        feedback_positive: row.get(18)?,
        feedback_negative: row.get(19)?,
        retrieval_count: row.get(20)?,
        retirement_run_id: row.get(21)?,
        search_atom_id: row.get(22)?,
        searchable_text: row.get(23)?,
        topic_keys_json: row.get(24)?,
        entity_ids_json: row.get(25)?,
        outcome_class: row.get(26)?,
        projection_digest: row.get(27)?,
    })
}

fn post_state_atom_from_stored(
    connection: &Connection,
    stored: &StoredPostStateAtom,
    expected_memory_space_id: &str,
    source_decision: Option<(&str, Digest32)>,
    pinned_artifact_digests: Vec<Digest32>,
) -> Result<RetirementPostStateAtomV1> {
    let atom_id = AtomId(Digest32(digest_array(
        &stored.atom_id,
        "post-state atom_id",
    )?));
    let header_body_digest = Digest32(digest_array(&stored.body_digest, "post-state body_digest")?);
    let header_body_bytes = unsigned_integer(stored.body_bytes, "post-state body_bytes")?;
    let atom_kind = match stored.atom_kind.as_str() {
        "episode" => MemoryKindV1::Episode,
        "semantic" => MemoryKindV1::Semantic,
        _ => {
            return Err(StoreError::Integrity(format!(
                "post-state atom {atom_id} has invalid kind"
            )));
        }
    };
    if stored.memory_space_id != expected_memory_space_id
        || header_body_digest != atom_id.digest()
        || header_body_bytes == 0
    {
        return Err(StoreError::Integrity(format!(
            "post-state atom {atom_id} failed header or scope authority"
        )));
    }
    let state = AtomStateV1 {
        retention_tier: retention_tier_from_store(&stored.retention_tier)?,
        content_status: content_status_from_store(&stored.content_status)?,
        expires_at_us: stored
            .expires_at_us
            .map(|value| unsigned_integer(value, "post-state expires_at_us"))
            .transpose()?,
        superseded_by: stored
            .superseded_by
            .as_deref()
            .map(|value| digest_array(value, "post-state superseded_by"))
            .transpose()?
            .map(|value| AtomId(Digest32(value))),
        feedback_positive: unsigned_integer(
            stored.feedback_positive,
            "post-state feedback_positive",
        )?,
        feedback_negative: unsigned_integer(
            stored.feedback_negative,
            "post-state feedback_negative",
        )?,
        retrieval_count: unsigned_integer(stored.retrieval_count, "post-state retrieval_count")?,
        retirement_run_id: stored
            .retirement_run_id
            .as_deref()
            .map(|value| digest_array(value, "post-state retirement_run_id"))
            .transpose()?
            .map(Digest32),
    };
    let (expected_tier, expected_status, expected_run_id, body_required) = match source_decision {
        Some(("episodic_winner" | "semantic_and_episodic", run_id)) => (
            RetentionTierV1::EpisodicLongTerm,
            ContentStatusV1::Present,
            Some(run_id),
            true,
        ),
        Some(("semantic_source" | "forgotten", run_id)) => (
            RetentionTierV1::HeaderOnly,
            ContentStatusV1::Forgotten,
            Some(run_id),
            false,
        ),
        Some((other, _)) => {
            return Err(StoreError::Integrity(format!(
                "core receipt contains unsupported persisted decision {other}"
            )));
        }
        None => (
            RetentionTierV1::SemanticLongTerm,
            ContentStatusV1::Present,
            None,
            true,
        ),
    };
    if (source_decision.is_some() && atom_kind != MemoryKindV1::Episode)
        || (source_decision.is_none() && atom_kind != MemoryKindV1::Semantic)
    {
        return Err(StoreError::Integrity(format!(
            "post-state atom {atom_id} has the wrong source/output kind"
        )));
    }
    if state.retention_tier != expected_tier
        || state.content_status != expected_status
        || state.expires_at_us.is_some()
        || state.superseded_by.is_some()
        || state.retirement_run_id != expected_run_id
    {
        return Err(StoreError::Integrity(format!(
            "post-state atom {atom_id} failed state authority"
        )));
    }

    let stored_body_digest = if body_required {
        let canonical_body = stored.canonical_body.as_deref().ok_or_else(|| {
            StoreError::Integrity(format!("post-state atom {atom_id} is missing its body"))
        })?;
        let json_projection = stored.json_projection.as_deref().ok_or_else(|| {
            StoreError::Integrity(format!(
                "post-state atom {atom_id} is missing its JSON projection"
            ))
        })?;
        let body: MemoryAtomBodyV1 = serde_json::from_str(json_projection)?;
        let expected = AtomRecord::from_core(&body, &state)?;
        let projection_digest = stored.projection_digest.as_deref().ok_or_else(|| {
            StoreError::Integrity(format!(
                "post-state atom {atom_id} is missing its projection digest"
            ))
        })?;
        let projection_digest = digest_array(projection_digest, "post-state projection_digest")?;
        let body_len = u64::try_from(canonical_body.len())
            .map_err(|_| StoreError::Integrity("post-state body length exceeds u64".to_owned()))?;
        let authority_matches = stored.search_atom_id.as_deref() == Some(&atom_id.digest().0[..])
            && stored.searchable_text.as_deref() == Some(expected.searchable_text.as_str())
            && stored.topic_keys_json.as_deref() == Some(expected.topic_keys_json.as_str())
            && stored.entity_ids_json.as_deref() == Some(expected.entity_ids_json.as_str())
            && stored.outcome_class == expected.outcome_class
            && expected.atom_id == atom_id.digest().0
            && expected.atom_kind == stored.atom_kind
            && expected.memory_space_id == stored.memory_space_id
            && expected.repository_id == stored.repository_id
            && expected.task_id == stored.task_id
            && expected.agent_id == stored.agent_id
            && expected.session_id == stored.session_id
            && expected.recorded_at_us
                == unsigned_integer(stored.recorded_at_us, "post-state recorded_at_us")?
            && expected.body_digest == header_body_digest.0
            && body_len == header_body_bytes
            && expected.retention_permission == stored.retention_permission
            && expected.provenance_digest
                == digest_array(&stored.provenance_digest, "post-state provenance_digest")?
            && expected.canonical_body == canonical_body
            && expected.json_projection == json_projection
            && expected.retention_tier == stored.retention_tier
            && expected.content_status == stored.content_status
            && expected.expires_at_us == state.expires_at_us
            && sha256_projection(&expected) == projection_digest;
        if !authority_matches {
            return Err(StoreError::Integrity(format!(
                "post-state atom {atom_id} failed body, header, or search authority"
            )));
        }
        let fts_rows: i64 = connection.query_row(
            "SELECT COUNT(*) FROM atom_search
             WHERE atom_id = ?1 AND searchable_text = ?2",
            params![atom_id.to_hex(), expected.searchable_text],
            |row| row.get(0),
        )?;
        if fts_rows != 1 {
            return Err(StoreError::Integrity(format!(
                "post-state atom {atom_id} failed FTS closure"
            )));
        }
        if source_decision.is_some() && body.retained_artifact_digests() != pinned_artifact_digests
        {
            return Err(StoreError::Integrity(format!(
                "episodic winner {atom_id} body does not match pinned artifact closure"
            )));
        }
        Some(Digest32::hash_prefixed(ATOM_HASH_DOMAIN, canonical_body))
    } else {
        if stored.canonical_body.is_some()
            || stored.json_projection.is_some()
            || stored.search_atom_id.is_some()
            || stored.searchable_text.is_some()
            || stored.topic_keys_json.is_some()
            || stored.entity_ids_json.is_some()
            || stored.projection_digest.is_some()
            || !pinned_artifact_digests.is_empty()
        {
            return Err(StoreError::Integrity(format!(
                "forgotten source {atom_id} retains body, search, or pin state"
            )));
        }
        None
    };
    Ok(RetirementPostStateAtomV1 {
        atom_id,
        atom_kind,
        repository_id: stored.repository_id.clone(),
        task_id: stored.task_id.clone(),
        agent_id: stored.agent_id.clone(),
        session_id: stored.session_id.clone(),
        recorded_at_us: unsigned_integer(stored.recorded_at_us, "post-state recorded_at_us")?,
        header_body_digest,
        header_body_bytes,
        retention_permission: retention_permission_from_store(&stored.retention_permission)?,
        provenance_digest: Digest32(digest_array(
            &stored.provenance_digest,
            "post-state provenance_digest",
        )?),
        stored_body_digest,
        retention_tier: state.retention_tier,
        content_status: state.content_status,
        expires_at_us: state.expires_at_us,
        superseded_by: state.superseded_by,
        feedback_positive: state.feedback_positive,
        feedback_negative: state.feedback_negative,
        retrieval_count: state.retrieval_count,
        retirement_run_bound: state.retirement_run_id.is_some(),
        search_projection_present: stored.search_atom_id.is_some(),
        pinned_artifact_digests,
    })
}

fn persisted_decision_digest(decisions: &[&[u8]]) -> Result<Digest32> {
    let count = u64::try_from(decisions.len())
        .map_err(|_| StoreError::Integrity("persisted decision count exceeds u64".to_owned()))?;
    let mut canonical_array = Vec::new();
    canonical_array.push(6);
    canonical_array.extend_from_slice(&count.to_be_bytes());
    for decision in decisions {
        let length = u64::try_from(decision.len()).map_err(|_| {
            StoreError::Integrity("persisted decision authority bytes exceed u64".to_owned())
        })?;
        canonical_array.extend_from_slice(&length.to_be_bytes());
        canonical_array.extend_from_slice(decision);
    }
    Ok(Digest32::hash_prefixed(
        b"naome-memory:retirement-decisions:v1\0",
        &canonical_array,
    ))
}

fn insert_atom_links_transaction(
    transaction: &Transaction<'_>,
    atom_id: [u8; 32],
    artifact_links: &[AtomArtifactLink],
    relation_links: &[AtomRelationLink],
) -> Result<()> {
    for link in artifact_links {
        transaction.execute(
            "INSERT INTO artifact_refs(artifact_digest, atom_id, role, retention_allowed)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(artifact_digest, atom_id, role) DO NOTHING",
            params![
                &link.artifact_digest.0[..],
                &atom_id[..],
                link.role,
                i64::from(link.retention_allowed)
            ],
        )?;
        let persisted: i64 = transaction.query_row(
            "SELECT retention_allowed FROM artifact_refs
             WHERE artifact_digest = ?1 AND atom_id = ?2 AND role = ?3",
            params![&link.artifact_digest.0[..], &atom_id[..], link.role],
            |row| row.get(0),
        )?;
        if persisted != i64::from(link.retention_allowed) {
            return Err(StoreError::ImmutableConflict(format!(
                "artifact link {} / {} / {}",
                link.artifact_digest,
                hex::encode(atom_id),
                link.role
            )));
        }
        transaction.execute(
            "INSERT INTO artifact_provenance_edges(
                atom_id, artifact_digest, role, retention_allowed
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(atom_id, artifact_digest, role) DO NOTHING",
            params![
                &atom_id[..],
                &link.artifact_digest.0[..],
                link.role,
                i64::from(link.retention_allowed)
            ],
        )?;
        let persisted_provenance: i64 = transaction.query_row(
            "SELECT retention_allowed FROM artifact_provenance_edges
             WHERE atom_id = ?1 AND artifact_digest = ?2 AND role = ?3",
            params![&atom_id[..], &link.artifact_digest.0[..], link.role],
            |row| row.get(0),
        )?;
        if persisted_provenance != i64::from(link.retention_allowed) {
            return Err(StoreError::ImmutableConflict(format!(
                "artifact provenance {} / {} / {}",
                link.artifact_digest,
                hex::encode(atom_id),
                link.role
            )));
        }
        transaction.execute(
            "UPDATE artifacts SET gc_marked_at_us = NULL WHERE artifact_digest = ?1",
            [&link.artifact_digest.0[..]],
        )?;
    }
    for link in relation_links {
        validate_relation_memory_space(transaction, atom_id, link.target_atom_id)?;
        let relation_digest = sha256(&link.canonical_relation);
        transaction.execute(
            "INSERT INTO atom_relations(
                source_atom_id, target_atom_id, relation_kind, relation_digest
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(source_atom_id, target_atom_id, relation_kind) DO NOTHING",
            params![
                &atom_id[..],
                &link.target_atom_id[..],
                link.relation_kind,
                &relation_digest[..]
            ],
        )?;
        let persisted: Vec<u8> = transaction.query_row(
            "SELECT relation_digest FROM atom_relations
             WHERE source_atom_id = ?1 AND target_atom_id = ?2 AND relation_kind = ?3",
            params![&atom_id[..], &link.target_atom_id[..], link.relation_kind],
            |row| row.get(0),
        )?;
        if persisted != relation_digest {
            return Err(StoreError::ImmutableConflict(format!(
                "relation {} -> {} ({})",
                hex::encode(atom_id),
                hex::encode(link.target_atom_id),
                link.relation_kind
            )));
        }
    }
    Ok(())
}

fn put_atom_transaction(transaction: &Transaction<'_>, atom: &AtomRecord) -> Result<bool> {
    validate_atom(atom)?;
    if let Some((body_digest, canonical_body)) = transaction
        .query_row(
            "SELECT h.body_digest, b.canonical_body
             FROM atom_headers AS h
             LEFT JOIN atom_bodies AS b ON b.atom_id = h.atom_id
             WHERE h.atom_id = ?1",
            [&atom.atom_id[..]],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Option<Vec<u8>>>(1)?)),
        )
        .optional()?
    {
        if body_digest != atom.body_digest
            || canonical_body.as_deref() != Some(&atom.canonical_body)
        {
            return Err(StoreError::ImmutableConflict(format!(
                "atom {}",
                hex::encode(atom.atom_id)
            )));
        }
        verify_atom_provenance_transaction(transaction, atom)?;
        return Ok(false);
    }

    if atom.retention_tier == "stm" {
        let expires_at_us = atom.expires_at_us.ok_or_else(|| {
            StoreError::InvalidInput("STM atom insertion requires an expiry".to_owned())
        })?;
        let cohort_day = expires_at_us / MICROSECONDS_PER_DAY;
        let retired = transaction
            .query_row(
                "SELECT 1 FROM retirement_runs
                 WHERE memory_space_id = ?1 AND cohort_day = ?2",
                params![
                    atom.memory_space_id,
                    sqlite_integer(cohort_day, "cohort_day")?
                ],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some();
        if retired {
            return Err(StoreError::RetirementConflict);
        }
    }

    transaction.execute(
        "INSERT INTO atom_headers(
            atom_id, atom_kind, memory_space_id, repository_id, task_id, agent_id, session_id,
            recorded_at_us, body_digest, body_bytes, retention_permission, provenance_digest
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            &atom.atom_id[..],
            atom.atom_kind,
            atom.memory_space_id,
            atom.repository_id,
            atom.task_id,
            atom.agent_id,
            atom.session_id,
            sqlite_integer(atom.recorded_at_us, "recorded_at_us")?,
            &atom.body_digest[..],
            sqlite_integer(
                u64::try_from(atom.canonical_body.len()).map_err(|_| {
                    StoreError::InvalidRetirementPlan("atom body length exceeds u64".into())
                })?,
                "body_bytes"
            )?,
            atom.retention_permission,
            &atom.provenance_digest[..]
        ],
    )?;
    transaction.execute(
        "INSERT INTO atom_bodies(atom_id, canonical_body, json_projection)
         VALUES (?1, ?2, ?3)",
        params![&atom.atom_id[..], atom.canonical_body, atom.json_projection],
    )?;
    insert_atom_provenance_transaction(transaction, atom)?;
    transaction.execute(
        "INSERT INTO atom_state(
            atom_id, retention_tier, content_status, expires_at_us,
            feedback_positive, feedback_negative, retrieval_count
         ) VALUES (?1, ?2, ?3, ?4, 0, 0, 0)",
        params![
            &atom.atom_id[..],
            atom.retention_tier,
            atom.content_status,
            atom.expires_at_us
                .map(|value| sqlite_integer(value, "expires_at_us"))
                .transpose()?
        ],
    )?;
    let projection_digest = sha256_projection(atom);
    transaction.execute(
        "INSERT INTO search_projection(
            atom_id, memory_space_id, repository_id, searchable_text, topic_keys_json,
            entity_ids_json, outcome_class, projection_digest
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            &atom.atom_id[..],
            atom.memory_space_id,
            atom.repository_id,
            atom.searchable_text,
            atom.topic_keys_json,
            atom.entity_ids_json,
            atom.outcome_class,
            &projection_digest[..]
        ],
    )?;
    transaction.execute(
        "INSERT INTO atom_search(atom_id, searchable_text) VALUES (?1, ?2)",
        params![hex::encode(atom.atom_id), atom.searchable_text],
    )?;
    Ok(true)
}

fn validate_atom(atom: &AtomRecord) -> Result<()> {
    if atom.canonical_body.len() > MAX_ATOM_BYTES {
        return Err(StoreError::Integrity(format!(
            "atom body exceeds {MAX_ATOM_BYTES} bytes"
        )));
    }
    let authority_digest = Digest32::hash_prefixed(ATOM_HASH_DOMAIN, &atom.canonical_body).0;
    if authority_digest != atom.body_digest || authority_digest != atom.atom_id {
        return Err(StoreError::Integrity(
            "atom ID or body digest does not match canonical authority bytes".into(),
        ));
    }
    validate_json(&atom.json_projection, "atom")?;
    validate_json(&atom.provenance_json_projection, "atom provenance")?;
    validate_json(&atom.topic_keys_json, "topic keys")?;
    validate_json(&atom.entity_ids_json, "entity ids")?;
    if !matches!(atom.atom_kind.as_str(), "episode" | "semantic") {
        return Err(StoreError::Integrity("invalid atom kind".into()));
    }
    if !matches!(
        atom.retention_permission.as_str(),
        "transient_only" | "semantic_allowed" | "semantic_and_exact_allowed"
    ) {
        return Err(StoreError::Integrity("invalid retention permission".into()));
    }
    if !matches!(
        atom.retention_tier.as_str(),
        "stm" | "ltm_semantic" | "ltm_episodic" | "ltm_dual" | "forgotten"
    ) || !matches!(
        atom.content_status.as_str(),
        "present" | "forgotten" | "quarantined"
    ) {
        return Err(StoreError::Integrity("invalid atom state".into()));
    }
    let provenance: ProvenanceV1 = serde_json::from_str(&atom.provenance_json_projection)?;
    let expected_policy_digest = PolicyV1::poc_v1().digest()?.0;
    let expected_provenance_digest =
        Digest32::hash_prefixed(b"naome-memory:provenance:v1\0", &atom.canonical_provenance).0;
    let projected_sources = provenance
        .source_event_digests
        .iter()
        .map(|digest| digest.0)
        .collect::<Vec<_>>();
    if provenance.canonical_bytes()? != atom.canonical_provenance
        || provenance.producer != atom.provenance_producer
        || provenance.policy_digest.0 != atom.provenance_policy_digest
        || atom.provenance_policy_digest != expected_policy_digest
        || expected_provenance_digest != atom.provenance_digest
        || projected_sources != atom.provenance_source_digests
    {
        return Err(StoreError::Integrity(
            "atom provenance authority is inconsistent".to_owned(),
        ));
    }
    Ok(())
}

fn insert_atom_provenance_transaction(
    transaction: &Transaction<'_>,
    atom: &AtomRecord,
) -> Result<()> {
    ensure_exact_change_count(
        "atom provenance insertion",
        transaction.execute(
            "INSERT INTO atom_provenance(
                atom_id, producer, policy_digest, canonical_provenance,
                json_projection, provenance_digest
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                &atom.atom_id[..],
                atom.provenance_producer,
                &atom.provenance_policy_digest[..],
                atom.canonical_provenance,
                atom.provenance_json_projection,
                &atom.provenance_digest[..]
            ],
        )?,
        1,
    )?;
    let mut insert_source = transaction.prepare(
        "INSERT INTO atom_provenance_sources(atom_id, ordinal, source_event_digest)
         VALUES (?1, ?2, ?3)",
    )?;
    for (ordinal, digest) in atom.provenance_source_digests.iter().enumerate() {
        ensure_exact_change_count(
            "atom provenance source insertion",
            insert_source.execute(params![
                &atom.atom_id[..],
                i64::try_from(ordinal).map_err(|_| {
                    StoreError::InvalidInput(
                        "atom provenance source ordinal exceeds i64".to_owned(),
                    )
                })?,
                &digest[..]
            ])?,
            1,
        )?;
    }
    Ok(())
}

fn verify_atom_provenance_transaction(
    transaction: &Transaction<'_>,
    atom: &AtomRecord,
) -> Result<()> {
    let stored = transaction
        .query_row(
            "SELECT producer, policy_digest, canonical_provenance, json_projection,
                    provenance_digest
             FROM atom_provenance WHERE atom_id = ?1",
            [&atom.atom_id[..]],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            StoreError::Integrity(format!(
                "atom {} is missing immutable provenance",
                hex::encode(atom.atom_id)
            ))
        })?;
    let mut statement = transaction.prepare(
        "SELECT source_event_digest FROM atom_provenance_sources
         WHERE atom_id = ?1 ORDER BY ordinal",
    )?;
    let rows = statement.query_map([&atom.atom_id[..]], |row| row.get::<_, Vec<u8>>(0))?;
    let mut sources = Vec::new();
    for row in rows {
        sources.push(digest_array(&row?, "provenance source_event_digest")?);
    }
    if stored.0 != atom.provenance_producer
        || stored.1 != atom.provenance_policy_digest
        || stored.2 != atom.canonical_provenance
        || stored.3 != atom.provenance_json_projection
        || stored.4 != atom.provenance_digest
        || sources != atom.provenance_source_digests
    {
        return Err(StoreError::ImmutableConflict(format!(
            "atom provenance {}",
            hex::encode(atom.atom_id)
        )));
    }
    Ok(())
}

fn snapshot_connection(connection: &Connection, key: &CohortKey) -> Result<CohortSnapshot> {
    let lower =
        key.cohort_day
            .checked_mul(MICROSECONDS_PER_DAY)
            .ok_or(StoreError::IntegerOutOfRange {
                field: "cohort lower bound",
                value: key.cohort_day,
            })?;
    let upper = lower
        .checked_add(MICROSECONDS_PER_DAY)
        .ok_or(StoreError::IntegerOutOfRange {
            field: "cohort upper bound",
            value: lower,
        })?;
    let mut statement = connection.prepare(
        "SELECT h.atom_id, h.body_digest, h.body_bytes, h.retention_permission, h.session_id,
                s.retention_tier, s.content_status, s.expires_at_us, s.superseded_by,
                s.feedback_positive, s.feedback_negative, s.retrieval_count,
                s.retirement_run_id,
                COALESCE((
                    SELECT SUM(a.byte_len)
                    FROM artifact_refs AS ar
                    JOIN artifacts AS a ON a.artifact_digest = ar.artifact_digest
                    WHERE ar.atom_id = h.atom_id AND ar.retention_allowed = 1
                ), 0), b.canonical_body
         FROM atom_headers AS h
         JOIN atom_state AS s ON s.atom_id = h.atom_id
         LEFT JOIN atom_bodies AS b ON b.atom_id = h.atom_id
         WHERE h.memory_space_id = ?1
           AND h.atom_kind = 'episode'
           AND s.retention_tier = 'stm'
           AND s.content_status = 'present'
           AND s.expires_at_us >= ?2 AND s.expires_at_us < ?3
         ORDER BY h.atom_id ASC
         LIMIT 100001",
    )?;
    let rows = statement.query_map(
        params![
            key.memory_space_id,
            sqlite_integer(lower, "cohort lower bound")?,
            sqlite_integer(upper, "cohort upper bound")?
        ],
        |row| {
            Ok(SnapshotRow {
                atom_id: row.get(0)?,
                body_digest: row.get(1)?,
                body_bytes: row.get(2)?,
                retention_permission: row.get(3)?,
                session_id: row.get(4)?,
                retention_tier: row.get(5)?,
                content_status: row.get(6)?,
                expires_at_us: row.get(7)?,
                superseded_by: row.get(8)?,
                feedback_positive: row.get(9)?,
                feedback_negative: row.get(10)?,
                retrieval_count: row.get(11)?,
                retirement_run_id: row.get(12)?,
                artifact_bytes: row.get(13)?,
                canonical_body: row.get(14)?,
            })
        },
    )?;
    let mut atoms = Vec::new();
    for row in rows {
        let row = row?;
        let atom_id = digest_array(&row.atom_id, "atom_id")?;
        let body_digest = digest_array(&row.body_digest, "body_digest")?;
        let state_digest = row.state_digest();
        let canonical_body = row.canonical_body.ok_or_else(|| {
            StoreError::Integrity(format!(
                "STM atom {} is missing its body",
                hex::encode(atom_id)
            ))
        })?;
        let authority_digest = Digest32::hash_prefixed(ATOM_HASH_DOMAIN, &canonical_body).0;
        if authority_digest != atom_id || authority_digest != body_digest {
            return Err(StoreError::Integrity(format!(
                "STM atom {} failed body authority verification",
                hex::encode(atom_id)
            )));
        }
        atoms.push(SnapshotAtom {
            atom_id,
            body_digest,
            state_digest,
            body_bytes: unsigned_integer(row.body_bytes, "body_bytes")?,
            artifact_bytes: unsigned_integer(row.artifact_bytes, "artifact_bytes")?,
            retention_permission: row.retention_permission,
            session_id: row.session_id,
        });
    }
    if atoms.len() > MAX_COHORT_ATOMS {
        return Err(StoreError::Integrity(format!(
            "cohort exceeds the fail-closed limit of {MAX_COHORT_ATOMS} atoms"
        )));
    }
    let input_set_digest = digest_snapshot(key, &atoms);
    Ok(CohortSnapshot {
        key: key.clone(),
        atoms,
        input_set_digest,
    })
}

struct SnapshotRow {
    atom_id: Vec<u8>,
    body_digest: Vec<u8>,
    body_bytes: i64,
    retention_permission: String,
    session_id: Option<String>,
    retention_tier: String,
    content_status: String,
    expires_at_us: Option<i64>,
    superseded_by: Option<Vec<u8>>,
    feedback_positive: i64,
    feedback_negative: i64,
    retrieval_count: i64,
    retirement_run_id: Option<Vec<u8>>,
    artifact_bytes: i64,
    canonical_body: Option<Vec<u8>>,
}

impl SnapshotRow {
    fn state_digest(&self) -> [u8; 32] {
        let mut hasher = domain_hasher(b"naome-memory:sqlite:atom-state:v1\0");
        hash_frame(&mut hasher, self.retention_tier.as_bytes());
        hash_frame(&mut hasher, self.content_status.as_bytes());
        hash_optional_i64(&mut hasher, self.expires_at_us);
        hash_optional(&mut hasher, self.superseded_by.as_deref());
        hash_i64(&mut hasher, self.feedback_positive);
        hash_i64(&mut hasher, self.feedback_negative);
        hash_i64(&mut hasher, self.retrieval_count);
        hash_optional(&mut hasher, self.retirement_run_id.as_deref());
        hasher.finalize().into()
    }
}

fn digest_snapshot(key: &CohortKey, atoms: &[SnapshotAtom]) -> [u8; 32] {
    let mut hasher = domain_hasher(b"naome-memory:sqlite:cohort-snapshot:v1\0");
    hash_frame(&mut hasher, key.memory_space_id.as_bytes());
    hasher.update(key.cohort_day.to_be_bytes());
    hasher.update(key.as_of_us.to_be_bytes());
    hasher.update(u64::try_from(atoms.len()).unwrap_or(u64::MAX).to_be_bytes());
    for atom in atoms {
        hasher.update(atom.atom_id);
        hasher.update(atom.body_digest);
        hasher.update(atom.state_digest);
        hasher.update(atom.body_bytes.to_be_bytes());
        hasher.update(atom.artifact_bytes.to_be_bytes());
        hash_frame(&mut hasher, atom.retention_permission.as_bytes());
        hash_optional(&mut hasher, atom.session_id.as_deref().map(str::as_bytes));
    }
    hasher.finalize().into()
}

fn validate_retirement_commit(commit: &RetirementCommit) -> Result<()> {
    validate_json(&commit.receipt.json_projection, "receipt")?;
    if commit.projected_post_state.memory_space_id != commit.key.memory_space_id
        || commit.projected_post_state.digest()?.0 != commit.projected_state_digest
        || commit.projected_post_state.source_atoms.len() != commit.decisions.len()
        || commit.projected_post_state.semantic_atoms.len() != commit.semantic_atoms.len()
    {
        return Err(StoreError::InvalidRetirementPlan(
            "projected post-state does not close over the retirement commit".to_owned(),
        ));
    }
    if commit.decisions.len() > MAX_COHORT_ATOMS {
        return Err(StoreError::InvalidRetirementPlan(
            "decision count exceeds cohort limit".into(),
        ));
    }
    let mut previous = None;
    for decision in &commit.decisions {
        if previous.is_some_and(|value| value >= decision.atom_id) {
            return Err(StoreError::InvalidRetirementPlan(
                "decisions must be unique and sorted by atom_id".into(),
            ));
        }
        previous = Some(decision.atom_id);
        if !matches!(
            decision.decision.as_str(),
            "semantic_source"
                | "episodic_winner"
                | "semantic_and_episodic"
                | "forgotten"
                | "unchanged"
        ) {
            return Err(StoreError::InvalidRetirementPlan(format!(
                "unknown decision {}",
                decision.decision
            )));
        }
        if sha256(&decision.canonical_details) != decision.details_digest {
            return Err(StoreError::InvalidRetirementPlan(
                "decision details digest mismatch".into(),
            ));
        }
    }
    Ok(())
}

fn validate_decision_closure(snapshot: &CohortSnapshot, commit: &RetirementCommit) -> Result<()> {
    if snapshot.atoms.len() != commit.decisions.len() {
        return Err(StoreError::InvalidRetirementPlan(format!(
            "decision closure has {} decisions for {} input atoms",
            commit.decisions.len(),
            snapshot.atoms.len()
        )));
    }
    for (atom, decision) in snapshot.atoms.iter().zip(&commit.decisions) {
        let atom_id_mismatch = atom.atom_id != decision.atom_id;
        let state_mismatch = atom.state_digest != decision.pre_state_digest;
        if atom_id_mismatch || state_mismatch {
            return Err(StoreError::InvalidRetirementPlan(
                "decision closure does not match cohort membership and state".into(),
            ));
        }
    }
    let semantic_count_budget = snapshot.atoms.len().saturating_mul(2).saturating_add(99) / 100;
    let semantic_count_budget = semantic_count_budget.min(100);
    if commit.semantic_atoms.len() > semantic_count_budget {
        return Err(StoreError::InvalidRetirementPlan(format!(
            "{} semantic atoms exceed count budget {semantic_count_budget}",
            commit.semantic_atoms.len()
        )));
    }
    let semantic_bytes = commit.semantic_atoms.iter().try_fold(0_u64, |sum, atom| {
        let length = u64::try_from(atom.canonical_body.len()).map_err(|_| {
            StoreError::InvalidRetirementPlan("semantic byte count exceeds u64".into())
        })?;
        sum.checked_add(length).ok_or_else(|| {
            StoreError::InvalidRetirementPlan("semantic byte budget overflow".into())
        })
    })?;
    if semantic_bytes > 8 * 1024 * 1024 {
        return Err(StoreError::InvalidRetirementPlan(format!(
            "semantic body bytes {semantic_bytes} exceed 8 MiB budget"
        )));
    }
    Ok(())
}

fn validate_pins(
    transaction: &Transaction<'_>,
    artifacts: &ArtifactStore,
    snapshot: &CohortSnapshot,
    commit: &RetirementCommit,
) -> Result<()> {
    let winners: BTreeSet<_> = commit
        .decisions
        .iter()
        .filter(|decision| {
            matches!(
                decision.decision.as_str(),
                "episodic_winner" | "semantic_and_episodic"
            )
        })
        .map(|decision| decision.atom_id)
        .collect();
    let population = snapshot
        .atoms
        .iter()
        .filter(|atom| atom.retention_permission == "semantic_and_exact_allowed")
        .count();
    let expected_winners = population.saturating_mul(5).saturating_add(999) / 1_000;
    let expected_winners = expected_winners.min(25);
    if winners.len() != expected_winners {
        return Err(StoreError::InvalidRetirementPlan(format!(
            "episodic winner count {} does not match required lottery count {expected_winners}",
            winners.len()
        )));
    }
    let snapshot_by_id: BTreeMap<_, _> = snapshot
        .atoms
        .iter()
        .map(|atom| (atom.atom_id, atom))
        .collect();
    let mut seen_pins = BTreeSet::new();
    let mut pinned_digests = BTreeSet::new();
    for pin in &commit.artifact_pins {
        if !winners.contains(&pin.atom_id) || !seen_pins.insert((pin.atom_id, pin.artifact_digest))
        {
            return Err(StoreError::InvalidRetirementPlan(
                "artifact pin is duplicated or does not belong to an episodic winner".into(),
            ));
        }
        let allowed = transaction
            .query_row(
                "SELECT 1 FROM artifact_refs
                 WHERE atom_id = ?1 AND artifact_digest = ?2 AND retention_allowed = 1",
                params![&pin.atom_id[..], &pin.artifact_digest.0[..]],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some();
        if !allowed {
            return Err(StoreError::InvalidRetirementPlan(
                "artifact pin lacks a retention-allowed source reference".into(),
            ));
        }
        let record = load_artifact_record(transaction, pin.artifact_digest)?.ok_or_else(|| {
            StoreError::InvalidRetirementPlan("pinned artifact record is missing".into())
        })?;
        artifacts.verify(&record)?;
        pinned_digests.insert(pin.artifact_digest);
    }

    for winner in winners {
        let atom = snapshot_by_id.get(&winner).ok_or_else(|| {
            StoreError::InvalidRetirementPlan("episodic winner is outside cohort".into())
        })?;
        if atom.retention_permission != "semantic_and_exact_allowed" {
            return Err(StoreError::InvalidRetirementPlan(
                "episodic winner is not exact-retention eligible".into(),
            ));
        }
        if atom.exact_package_bytes() > MAX_EXACT_PACKAGE_BYTES {
            return Err(StoreError::InvalidRetirementPlan(
                "episodic winner exceeds exact package size limit".into(),
            ));
        }
        let expected: BTreeSet<ArtifactDigest> = {
            let mut statement = transaction.prepare(
                "SELECT artifact_digest FROM artifact_refs
                 WHERE atom_id = ?1 AND retention_allowed = 1
                 ORDER BY artifact_digest",
            )?;
            let rows = statement.query_map([&winner[..]], |row| row.get::<_, Vec<u8>>(0))?;
            let mut values = BTreeSet::new();
            for row in rows {
                values.insert(ArtifactDigest::from_slice(&row?, "artifact_digest")?);
            }
            values
        };
        let actual: BTreeSet<_> = commit
            .artifact_pins
            .iter()
            .filter(|pin| pin.atom_id == winner)
            .map(|pin| pin.artifact_digest)
            .collect();
        if actual != expected {
            return Err(StoreError::InvalidRetirementPlan(
                "episodic winner does not pin complete retention-allowed artifact closure".into(),
            ));
        }
    }

    let mut pinned_bytes = 0_u64;
    for digest in pinned_digests {
        let byte_len: i64 = transaction.query_row(
            "SELECT byte_len FROM artifacts WHERE artifact_digest = ?1",
            [&digest.0[..]],
            |row| row.get(0),
        )?;
        pinned_bytes = pinned_bytes
            .checked_add(unsigned_integer(byte_len, "artifact byte_len")?)
            .ok_or_else(|| {
                StoreError::InvalidRetirementPlan("pinned byte budget overflow".into())
            })?;
    }
    if pinned_bytes > MAX_PINNED_BYTES_PER_COHORT {
        return Err(StoreError::InvalidRetirementPlan(format!(
            "pinned artifact bytes {pinned_bytes} exceed cohort budget {MAX_PINNED_BYTES_PER_COHORT}"
        )));
    }
    Ok(())
}

fn insert_retirement_run(transaction: &Transaction<'_>, commit: &RetirementCommit) -> Result<()> {
    let inserted = transaction.execute(
        "INSERT INTO retirement_runs(
            run_id, memory_space_id, cohort_day, as_of_us, policy_id, policy_digest, seed,
            rng_id, input_set_digest, store_snapshot_digest, population_digest, plan_digest,
            projected_state_digest,
            status, started_at_us
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 'applying', ?4)",
        params![
            &commit.run_id[..],
            commit.key.memory_space_id,
            sqlite_integer(commit.key.cohort_day, "cohort_day")?,
            sqlite_integer(commit.key.as_of_us, "as_of_us")?,
            commit.policy_id,
            &commit.policy_digest[..],
            &commit.seed[..],
            commit.rng_id,
            &commit.proof_input_set_digest[..],
            &commit.expected_input_set_digest[..],
            &commit.population_digest[..],
            &commit.plan_digest[..],
            &commit.projected_state_digest[..]
        ],
    )?;
    ensure_exact_change_count("retirement run insertion", inserted, 1)?;
    Ok(())
}

fn insert_retirement_post_state(
    transaction: &Transaction<'_>,
    commit: &RetirementCommit,
) -> Result<()> {
    let canonical_state = commit.projected_post_state.canonical_bytes()?;
    let state_digest = commit.projected_post_state.digest()?.0;
    if state_digest != commit.projected_state_digest {
        return Err(StoreError::InvalidRetirementPlan(
            "projected post-state does not match its committed digest".to_owned(),
        ));
    }
    ensure_exact_change_count(
        "retirement post-state insertion",
        transaction.execute(
            "INSERT INTO retirement_post_states(
                run_id, state_digest, canonical_state, json_projection
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                &commit.run_id[..],
                &state_digest[..],
                canonical_state,
                serde_json::to_string(&commit.projected_post_state)?
            ],
        )?,
        1,
    )?;
    Ok(())
}

fn persist_retirement_decisions(
    transaction: &Transaction<'_>,
    commit: &RetirementCommit,
) -> Result<()> {
    let mut insert_member = transaction.prepare(
        "INSERT INTO retirement_members(run_id, atom_id, pre_state_digest)
         VALUES (?1, ?2, ?3)",
    )?;
    let mut insert_decision = transaction.prepare(
        "INSERT INTO retirement_decisions(
            run_id, atom_id, decision, canonical_details, details_digest
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for decision in &commit.decisions {
        ensure_exact_change_count(
            "retirement member insertion",
            insert_member.execute(params![
                &commit.run_id[..],
                &decision.atom_id[..],
                &decision.pre_state_digest[..]
            ])?,
            1,
        )?;
        ensure_exact_change_count(
            "retirement decision insertion",
            insert_decision.execute(params![
                &commit.run_id[..],
                &decision.atom_id[..],
                decision.decision,
                decision.canonical_details,
                &decision.details_digest[..]
            ])?,
            1,
        )?;
    }
    Ok(())
}

fn apply_decisions_set_based(
    transaction: &Transaction<'_>,
    commit: &RetirementCommit,
    observer: TransactionObserver<'_>,
) -> Result<()> {
    let mut episodic_count = 0_usize;
    let mut forgotten_count = 0_usize;
    let mut unchanged_count = 0_usize;
    for decision in &commit.decisions {
        match decision.decision.as_str() {
            "episodic_winner" | "semantic_and_episodic" => {
                episodic_count = episodic_count.saturating_add(1);
            }
            "semantic_source" | "forgotten" => {
                forgotten_count = forgotten_count.saturating_add(1);
            }
            "unchanged" => {
                unchanged_count = unchanged_count.saturating_add(1);
            }
            _ => {
                return Err(StoreError::InvalidRetirementPlan(
                    "unknown retirement decision".into(),
                ));
            }
        }
    }

    let episodic_changed = transaction.execute(
        "UPDATE atom_state
         SET retention_tier = 'ltm_episodic', content_status = 'present',
             expires_at_us = NULL, retirement_run_id = ?1
         WHERE atom_id IN (
             SELECT atom_id FROM retirement_decisions
             WHERE run_id = ?1
               AND decision IN ('episodic_winner', 'semantic_and_episodic')
         )
           AND retention_tier = 'stm' AND content_status = 'present'",
        [&commit.run_id[..]],
    )?;
    ensure_exact_change_count(
        "episodic state transition",
        episodic_changed,
        episodic_count,
    )?;

    let forgotten_changed = transaction.execute(
        "UPDATE atom_state
         SET retention_tier = 'forgotten', content_status = 'forgotten',
             expires_at_us = NULL, retirement_run_id = ?1
         WHERE atom_id IN (
             SELECT atom_id FROM retirement_decisions
             WHERE run_id = ?1 AND decision IN ('semantic_source', 'forgotten')
         )
           AND retention_tier = 'stm' AND content_status = 'present'",
        [&commit.run_id[..]],
    )?;
    ensure_exact_change_count(
        "forgotten state transition",
        forgotten_changed,
        forgotten_count,
    )?;

    let unchanged = transaction.execute(
        "UPDATE atom_state
         SET retirement_run_id = ?1
         WHERE atom_id IN (
             SELECT atom_id FROM retirement_decisions
             WHERE run_id = ?1 AND decision = 'unchanged'
         )
           AND retention_tier = 'stm' AND content_status = 'present'",
        [&commit.run_id[..]],
    )?;
    ensure_exact_change_count("unchanged state binding", unchanged, unchanged_count)?;
    observer(RepositoryFaultPointV1::RetirementAfterStateMutation)?;

    let deleted_bodies = transaction.execute(
        "DELETE FROM atom_bodies
         WHERE atom_id IN (
             SELECT atom_id FROM retirement_decisions
             WHERE run_id = ?1 AND decision IN ('semantic_source', 'forgotten')
         )",
        [&commit.run_id[..]],
    )?;
    ensure_exact_change_count("forgotten body deletion", deleted_bodies, forgotten_count)?;

    let deleted_projections = transaction.execute(
        "DELETE FROM search_projection
         WHERE atom_id IN (
             SELECT atom_id FROM retirement_decisions
             WHERE run_id = ?1 AND decision IN ('semantic_source', 'forgotten')
         )",
        [&commit.run_id[..]],
    )?;
    ensure_exact_change_count(
        "forgotten search projection deletion",
        deleted_projections,
        forgotten_count,
    )?;

    // `atom_search.atom_id` is intentionally UNINDEXED inside FTS5. Deleting it once
    // for the complete decision set avoids a full virtual-table scan per forgotten atom.
    let deleted_fts_rows = transaction.execute(
        "DELETE FROM atom_search
         WHERE atom_id IN (
             SELECT lower(hex(atom_id)) FROM retirement_decisions
             WHERE run_id = ?1 AND decision IN ('semantic_source', 'forgotten')
         )",
        [&commit.run_id[..]],
    )?;
    ensure_exact_change_count("forgotten FTS deletion", deleted_fts_rows, forgotten_count)?;

    transaction.execute(
        "DELETE FROM artifact_refs
         WHERE atom_id IN (
             SELECT atom_id FROM retirement_decisions
             WHERE run_id = ?1 AND decision IN ('semantic_source', 'forgotten')
         ) OR (
             retention_allowed = 0 AND atom_id IN (
                 SELECT atom_id FROM retirement_decisions
                 WHERE run_id = ?1
                   AND decision IN ('episodic_winner', 'semantic_and_episodic')
             )
         )",
        [&commit.run_id[..]],
    )?;
    observer(RepositoryFaultPointV1::RetirementAfterForgetting)?;
    Ok(())
}

const fn ensure_exact_change_count(_operation: &str, actual: usize, expected: usize) -> Result<()> {
    if actual != expected {
        return Err(StoreError::RetirementConflict);
    }
    Ok(())
}

fn ensure_single_gc_change(operation: &str, actual: usize) -> Result<()> {
    if actual != 1 {
        return Err(StoreError::Integrity(format!(
            "{operation} changed {actual} rows; expected exactly one"
        )));
    }
    Ok(())
}

fn load_artifact_record(
    connection: &Connection,
    digest: ArtifactDigest,
) -> Result<Option<ArtifactRecord>> {
    connection
        .query_row(
            "SELECT artifact_digest, byte_len, relative_path
             FROM artifacts WHERE artifact_digest = ?1",
            [&digest.0[..]],
            artifact_record_from_row,
        )
        .optional()
        .map_err(StoreError::from)
}

fn load_all_artifact_records(
    connection: &Connection,
) -> Result<BTreeMap<ArtifactDigest, ArtifactRecord>> {
    let mut statement = connection.prepare(
        "SELECT artifact_digest, byte_len, relative_path
         FROM artifacts ORDER BY artifact_digest",
    )?;
    let rows = statement.query_map([], artifact_record_from_row)?;
    let mut records = BTreeMap::new();
    for row in rows {
        let record = row?;
        if records.insert(record.digest, record).is_some() {
            return Err(StoreError::Integrity(
                "duplicate artifact digest escaped the primary key".into(),
            ));
        }
    }
    Ok(records)
}

fn load_unreferenced_artifacts(
    connection: &Connection,
) -> Result<BTreeMap<ArtifactDigest, (ArtifactRecord, Option<i64>)>> {
    let mut statement = connection.prepare(
        "SELECT a.artifact_digest, a.byte_len, a.relative_path, a.gc_marked_at_us
         FROM artifacts AS a
         WHERE NOT EXISTS (
             SELECT 1 FROM artifact_refs AS r WHERE r.artifact_digest = a.artifact_digest
         ) AND NOT EXISTS (
             SELECT 1 FROM artifact_pins AS p WHERE p.artifact_digest = a.artifact_digest
         )
         ORDER BY a.artifact_digest",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            artifact_record_from_row(row)?,
            row.get::<_, Option<i64>>(3)?,
        ))
    })?;
    let mut candidates = BTreeMap::new();
    for row in rows {
        let (record, marked_at) = row?;
        if candidates
            .insert(record.digest, (record, marked_at))
            .is_some()
        {
            return Err(StoreError::Integrity(
                "duplicate GC candidate escaped the primary key".into(),
            ));
        }
    }
    Ok(candidates)
}

fn validate_discovered_artifact_metadata(
    persisted: &BTreeMap<ArtifactDigest, ArtifactRecord>,
    discovered: &[ArtifactRecord],
) -> Result<()> {
    for record in persisted.values() {
        ArtifactStore::validate_record_metadata(record)?;
    }
    for record in discovered {
        if let Some(expected) = persisted.get(&record.digest)
            && expected != record
        {
            return Err(StoreError::Integrity(format!(
                "artifact {} CAS metadata disagrees with SQLite",
                record.digest
            )));
        }
    }
    Ok(())
}

fn gc_dry_run_entries(
    connection: &Connection,
    discovered: &[ArtifactRecord],
    as_of_us: u64,
    grace_period_us: u64,
) -> Result<Vec<GcEntry>> {
    let persisted = load_all_artifact_records(connection)?;
    validate_discovered_artifact_metadata(&persisted, discovered)?;
    let mut entries = BTreeMap::new();
    for (digest, (record, marked_at)) in load_unreferenced_artifacts(connection)? {
        let action = if let Some(marked_at) = marked_at {
            let marked_at_us = unsigned_integer(marked_at, "gc_marked_at_us")?;
            if as_of_us.saturating_sub(marked_at_us) < grace_period_us {
                GcAction::WaitingForGrace
            } else {
                GcAction::WouldDelete
            }
        } else {
            GcAction::WouldMark
        };
        entries.insert(
            digest,
            GcEntry {
                digest,
                byte_len: record.byte_len,
                action,
            },
        );
    }
    for record in discovered {
        if !persisted.contains_key(&record.digest) {
            entries.insert(
                record.digest,
                GcEntry {
                    digest: record.digest,
                    byte_len: record.byte_len,
                    action: GcAction::WouldMark,
                },
            );
        }
    }
    Ok(entries.into_values().collect())
}

fn artifact_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArtifactRecord> {
    let digest: Vec<u8> = row.get(0)?;
    let digest = <[u8; 32]>::try_from(digest.as_slice()).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Blob,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "artifact digest is not 32 bytes",
            )),
        )
    })?;
    let byte_len: i64 = row.get(1)?;
    let byte_len = u64::try_from(byte_len).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })?;
    Ok(ArtifactRecord {
        digest: ArtifactDigest(digest),
        byte_len,
        relative_path: row.get(2)?,
    })
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn sha256_projection(atom: &AtomRecord) -> [u8; 32] {
    let mut hasher = domain_hasher(b"naome-memory:sqlite:search-projection:v1\0");
    hash_frame(&mut hasher, atom.searchable_text.as_bytes());
    hash_frame(&mut hasher, atom.topic_keys_json.as_bytes());
    hash_frame(&mut hasher, atom.entity_ids_json.as_bytes());
    hash_optional(
        &mut hasher,
        atom.outcome_class.as_deref().map(str::as_bytes),
    );
    hasher.finalize().into()
}

fn searchable_text(body: &MemoryAtomBodyV1) -> String {
    let mut fields = Vec::new();
    fields.push(body.trigger.as_str());
    if let Some(goal) = body.goal.as_deref() {
        fields.push(goal);
    }
    fields.extend(body.plan.iter().map(String::as_str));
    fields.extend(body.observations.iter().map(|value| value.content.as_str()));
    fields.extend(
        body.interpretations
            .iter()
            .map(|value| value.content.as_str()),
    );
    fields.extend(body.beliefs.iter().map(|value| value.proposition.as_str()));
    fields.extend(body.decisions.iter().map(|value| value.decision.as_str()));
    fields.extend(body.actions.iter().map(|value| value.action.as_str()));
    if let Some(outcome) = body.outcome.as_ref() {
        fields.push(outcome.summary.as_str());
    }
    fields.extend(body.topic_keys.iter().map(String::as_str));
    fields.extend(body.entity_ids.iter().map(String::as_str));
    fields.join("\n")
}

fn validate_retrieval_request(query: &RetrievalQueryV1, policy: &PolicyV1) -> Result<()> {
    // The pure ranker is the public query-contract authority. Running it with
    // an empty store envelope validates version, scope, limits, policy, and
    // flashback budget without introducing a second drifting validator.
    rank_retrieval(
        query,
        &RetrievalCandidatesV1 {
            ranked: Vec::new(),
            episodic_pool: Vec::new(),
        },
        policy,
    )?;
    Ok(())
}

fn normalized_query_tokens(terms: &[String]) -> BTreeSet<String> {
    terms
        .iter()
        .flat_map(|term| term.split(|character: char| !character.is_alphanumeric()))
        .filter(|token| !token.is_empty())
        .map(|token| token.chars().flat_map(char::to_lowercase).collect())
        .collect()
}

fn coverage_score(hits: u64, population: u64) -> Ppm {
    if population == 0 {
        Ppm::ZERO
    } else {
        Ppm::ratio(hits.min(population), population)
    }
}

#[cfg(feature = "lab-fault-injection")]
fn logical_database_digest(connection: &Connection) -> Result<Digest32> {
    let tables = {
        let mut statement = connection.prepare(
            "SELECT name, COALESCE(sql, '')
             FROM sqlite_schema
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut tables = Vec::new();
        for row in rows {
            tables.push(row?);
        }
        tables
    };

    let mut hasher = domain_hasher(b"naome-memory:sqlite:logical-state:v1\0");
    hasher.update(
        u64::try_from(tables.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for (table_name, schema_sql) in tables {
        hash_frame(&mut hasher, table_name.as_bytes());
        hash_frame(&mut hasher, schema_sql.as_bytes());
        let quoted_name = format!("\"{}\"", table_name.replace('"', "\"\""));
        let mut statement = connection.prepare(&format!("SELECT * FROM {quoted_name}"))?;
        let column_count = statement.column_count();
        let mut rows = statement.query([])?;
        let mut encoded_rows = Vec::new();
        while let Some(row) = rows.next()? {
            let mut encoded = Vec::new();
            encoded.extend_from_slice(
                &u64::try_from(column_count)
                    .unwrap_or(u64::MAX)
                    .to_be_bytes(),
            );
            for column in 0..column_count {
                encode_sql_value(&mut encoded, row.get_ref(column)?);
            }
            encoded_rows.push(encoded);
        }
        encoded_rows.sort_unstable();
        hasher.update(
            u64::try_from(encoded_rows.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        for row in encoded_rows {
            hash_frame(&mut hasher, &row);
        }
    }
    Ok(Digest32(hasher.finalize().into()))
}

#[cfg(feature = "lab-fault-injection")]
fn logical_artifact_tree_digest(root: &std::path::Path) -> Result<Digest32> {
    let mut entries = Vec::new();
    collect_artifact_tree_entries(root, root, &mut entries)?;
    entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = domain_hasher(b"naome-memory:sqlite:artifact-tree:v1\0");
    hasher.update(
        u64::try_from(entries.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for (relative_path, byte_len, digest) in entries {
        hash_frame(&mut hasher, relative_path.as_bytes());
        hasher.update(byte_len.to_be_bytes());
        hasher.update(digest);
    }
    Ok(Digest32(hasher.finalize().into()))
}

#[cfg(feature = "lab-fault-injection")]
fn collect_artifact_tree_entries(
    root: &std::path::Path,
    directory: &std::path::Path,
    entries: &mut Vec<(String, u64, [u8; 32])>,
) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            collect_artifact_tree_entries(root, &path, entries)?;
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|error| StoreError::Integrity(error.to_string()))?
                .to_str()
                .ok_or_else(|| StoreError::Integrity("artifact CAS path is not UTF-8".to_owned()))?
                .replace(std::path::MAIN_SEPARATOR, "/");
            let bytes = fs::read(&path)?;
            entries.push((
                relative,
                u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                sha256(&bytes),
            ));
        } else {
            return Err(StoreError::Integrity(
                "artifact CAS contains a non-file, non-directory entry".to_owned(),
            ));
        }
    }
    Ok(())
}

#[cfg(feature = "lab-fault-injection")]
fn encode_sql_value(output: &mut Vec<u8>, value: rusqlite::types::ValueRef<'_>) {
    match value {
        rusqlite::types::ValueRef::Null => output.push(0),
        rusqlite::types::ValueRef::Integer(value) => {
            output.push(1);
            output.extend_from_slice(&value.to_be_bytes());
        }
        rusqlite::types::ValueRef::Real(value) => {
            output.push(2);
            output.extend_from_slice(&value.to_bits().to_be_bytes());
        }
        rusqlite::types::ValueRef::Text(value) => {
            output.push(3);
            output.extend_from_slice(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
            output.extend_from_slice(value);
        }
        rusqlite::types::ValueRef::Blob(value) => {
            output.push(4);
            output.extend_from_slice(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
            output.extend_from_slice(value);
        }
    }
}

fn domain_hasher(domain: &[u8]) -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher
}

fn hash_frame(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(bytes);
}

fn hash_optional(hasher: &mut Sha256, bytes: Option<&[u8]>) {
    if let Some(bytes) = bytes {
        hasher.update([1]);
        hash_frame(hasher, bytes);
    } else {
        hasher.update([0]);
    }
}

fn hash_optional_i64(hasher: &mut Sha256, value: Option<i64>) {
    if let Some(value) = value {
        hasher.update([1]);
        hash_i64(hasher, value);
    } else {
        hasher.update([0]);
    }
}

fn hash_i64(hasher: &mut Sha256, value: i64) {
    hasher.update(value.to_be_bytes());
}

fn digest_array(bytes: &[u8], field: &'static str) -> Result<[u8; 32]> {
    <[u8; 32]>::try_from(bytes).map_err(|_| StoreError::InvalidDigestLength {
        field,
        actual: bytes.len(),
    })
}

fn sqlite_integer(value: u64, field: &'static str) -> Result<i64> {
    i64::try_from(value).map_err(|_| StoreError::IntegerOutOfRange { field, value })
}

fn unsigned_integer(value: i64, field: &'static str) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| StoreError::Integrity(format!("negative SQLite integer for {field}: {value}")))
}

fn validate_json(value: &str, label: &str) -> Result<()> {
    serde_json::from_str::<serde_json::Value>(value)
        .map(|_| ())
        .map_err(|error| StoreError::Integrity(format!("invalid {label} JSON: {error}")))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;

    use super::*;

    use naome_memory_core::{
        ArtifactRefV1, EpisodePayloadV1, FormationSignalsV1, MemoryPayloadV1, MemoryRelationKindV1,
        MemoryRelationV1, MemoryScopeV1, ObservationV1, PolicyV1, ProvenanceV1, SealReasonV1,
        Seed32, TimeIntervalV1,
    };

    fn repository() -> (tempfile::TempDir, SqliteRepository) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = StoreConfig::new(
            directory.path().join("memory.db"),
            directory.path().join("artifacts"),
        );
        let repository = SqliteRepository::open(&config).expect("open repository");
        (directory, repository)
    }

    fn atom(id: u8, expires_at_us: u64) -> AtomRecord {
        let policy_digest = PolicyV1::poc_v1().digest().expect("policy digest");
        let mut body = core_body(policy_digest);
        body.scope.session_id = format!("session-{id}");
        body.trigger = format!("memory {id}");
        body.interval.recorded_at_us = 10;
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
        AtomRecord::from_core(&body, &state).expect("typed atom record")
    }

    fn commit_for(snapshot: &CohortSnapshot, exact: bool) -> RetirementCommit {
        let policy_digest = PolicyV1::poc_v1().digest().expect("policy digest");
        let details = b"decision".to_vec();
        let decisions = snapshot
            .atoms
            .iter()
            .map(|candidate| PersistedDecision {
                atom_id: candidate.atom_id,
                pre_state_digest: candidate.state_digest,
                decision: if exact {
                    "episodic_winner".into()
                } else {
                    "forgotten".into()
                },
                canonical_details: details.clone(),
                details_digest: sha256(&details),
            })
            .collect::<Vec<_>>();
        let source_atom_ids = decisions
            .iter()
            .map(|decision| AtomId(Digest32(decision.atom_id)))
            .collect::<Vec<_>>();
        let exact_episode_ids = if exact {
            source_atom_ids.clone()
        } else {
            Vec::new()
        };
        let exact_artifact_digests = exact_episode_ids
            .iter()
            .copied()
            .map(|atom_id| (atom_id, Vec::new()))
            .collect();
        let decision_authority = decisions
            .iter()
            .map(|decision| decision.canonical_details.as_slice())
            .collect::<Vec<_>>();
        let decision_digest =
            persisted_decision_digest(&decision_authority).expect("decision digest");
        let episodic_population_count = snapshot
            .atoms
            .iter()
            .filter(|atom| atom.retention_permission == "semantic_and_exact_allowed")
            .count();
        let episodic_winner_budget = if exact { exact_episode_ids.len() } else { 0 };
        let episodic_winner_bytes = if exact {
            snapshot
                .atoms
                .iter()
                .map(SnapshotAtom::exact_package_bytes)
                .sum()
        } else {
            0
        };
        let semantic_count_budget = PolicyV1::poc_v1()
            .semantic_rate
            .ceil_count(snapshot.atoms.len())
            .min(PolicyV1::poc_v1().semantic_count_max);
        let provenance_digest = Digest32::hash_prefixed(
            b"naome-memory:provenance:v1\0",
            &core_body(policy_digest)
                .provenance
                .canonical_bytes()
                .expect("provenance authority"),
        );
        let projected_post_state = RetirementPostStateV1 {
            memory_space_id: snapshot.key.memory_space_id.clone(),
            source_atoms: snapshot
                .atoms
                .iter()
                .map(|candidate| RetirementPostStateAtomV1 {
                    atom_id: AtomId(Digest32(candidate.atom_id)),
                    atom_kind: MemoryKindV1::Episode,
                    repository_id: Some("repo-a".to_owned()),
                    task_id: Some("task-a".to_owned()),
                    agent_id: Some("agent-a".to_owned()),
                    session_id: candidate.session_id.clone(),
                    recorded_at_us: 10,
                    header_body_digest: Digest32(candidate.body_digest),
                    header_body_bytes: candidate.body_bytes,
                    retention_permission: retention_permission_from_store(
                        &candidate.retention_permission,
                    )
                    .expect("retention permission"),
                    provenance_digest,
                    stored_body_digest: exact.then_some(Digest32(candidate.body_digest)),
                    retention_tier: if exact {
                        RetentionTierV1::EpisodicLongTerm
                    } else {
                        RetentionTierV1::HeaderOnly
                    },
                    content_status: if exact {
                        ContentStatusV1::Present
                    } else {
                        ContentStatusV1::Forgotten
                    },
                    expires_at_us: None,
                    superseded_by: None,
                    feedback_positive: 0,
                    feedback_negative: 0,
                    retrieval_count: 0,
                    retirement_run_bound: true,
                    search_projection_present: exact,
                    pinned_artifact_digests: Vec::new(),
                })
                .collect(),
            semantic_atoms: Vec::new(),
        };
        let projected_state_digest = projected_post_state
            .digest()
            .expect("projected post-state digest");
        let invariants = [
            "budgets_structurally_bounded",
            "plan_structure_verified",
            "repository_outcome_closure_verified",
        ]
        .into_iter()
        .map(|name| {
            (
                name.to_owned(),
                naome_memory_core::InvariantStatusV1::Verified,
            )
        })
        .collect();
        let plan_digest = Digest32([35; 32]);
        let run_id =
            Digest32::hash_prefixed(b"naome-memory:retirement-run:v1\0", plan_digest.as_bytes());
        let mut receipt = ProofReceiptV1 {
            contract_version: "proof-receipt-v1".to_owned(),
            receipt_id: Digest32::ZERO,
            run_id,
            memory_space_id: snapshot.key.memory_space_id.clone(),
            cohort_day: snapshot.key.cohort_day,
            as_of_us: snapshot.key.as_of_us,
            policy_id: "poc-v1".to_owned(),
            policy_digest,
            seed: Seed32(Digest32([33; 32])),
            input_set_digest: Digest32(snapshot.input_set_digest),
            rng_identifier: "chacha20-rand_chacha-0.3/partial-fisher-yates-v1".to_owned(),
            population_digest: Digest32([34; 32]),
            episodic_population_count,
            episodic_winner_budget,
            episodic_winner_bytes,
            semantic_count_budget,
            semantic_body_bytes: 0,
            plan_digest,
            decision_digest,
            state_digest: projected_state_digest,
            closure: ReceiptClosureV1 {
                source_atom_ids,
                semantic_atom_ids: Vec::new(),
                exact_episode_ids,
                exact_artifact_digests,
            },
            invariants,
            overall_status: naome_memory_core::OverallProofStatusV1::Passed,
        };
        receipt.receipt_id = receipt.recompute_receipt_id().expect("receipt ID");
        let canonical_receipt = receipt.canonical_bytes().expect("canonical receipt");
        let closure_digest = sha256(
            &receipt
                .closure
                .canonical_bytes()
                .expect("canonical closure"),
        );
        RetirementCommit {
            run_id: run_id.0,
            key: snapshot.key.clone(),
            policy_id: "poc-v1".into(),
            policy_digest: policy_digest.0,
            seed: [33; 32],
            rng_id: "chacha20-rand_chacha-0.3/partial-fisher-yates-v1".into(),
            proof_input_set_digest: snapshot.input_set_digest,
            expected_input_set_digest: snapshot.input_set_digest,
            population_digest: [34; 32],
            plan_digest: [35; 32],
            projected_state_digest: projected_state_digest.0,
            projected_post_state,
            decisions,
            semantic_atoms: Vec::new(),
            artifact_pins: Vec::new(),
            receipt: PersistedReceipt {
                receipt_digest: receipt.receipt_id.0,
                canonical_receipt,
                json_projection: serde_json::to_string(&receipt).expect("receipt JSON"),
                closure_digest,
            },
        }
    }

    fn core_body(policy_digest: Digest32) -> MemoryAtomBodyV1 {
        MemoryAtomBodyV1 {
            contract_version: "atom-v1".into(),
            scope: MemoryScopeV1 {
                memory_space_id: "space-a".into(),
                repository_id: Some("repo-a".into()),
                task_id: Some("task-a".into()),
                agent_id: Some("agent-a".into()),
                session_id: "session-a".into(),
            },
            interval: TimeIntervalV1 {
                started_at_us: 1,
                ended_at_us: 2,
                recorded_at_us: 3,
            },
            trigger: "test episode".into(),
            seal_reason: SealReasonV1::Checkpoint,
            goal: Some("verify store integration".into()),
            plan: Vec::new(),
            internal_state_before: BTreeMap::new(),
            internal_state_after: BTreeMap::new(),
            observations: vec![ObservationV1 {
                at_us: 1,
                source: "synthetic-test".to_owned(),
                content: "typed storage fixture".to_owned(),
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
            topic_keys: BTreeSet::from(["store".into()]),
            entity_ids: BTreeSet::new(),
            outcome_class: None,
            goal_key: Some("store-integration".into()),
            provenance: ProvenanceV1 {
                producer: "sqlite-test".into(),
                source_event_digests: vec![Digest32::hash_prefixed(
                    b"naome-memory:sqlite-test-event:v1\0",
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

    fn exact_stm_state(body: &MemoryAtomBodyV1) -> AtomStateV1 {
        AtomStateV1 {
            retention_tier: RetentionTierV1::ShortTerm,
            content_status: ContentStatusV1::Present,
            expires_at_us: Some(
                body.interval
                    .recorded_at_us
                    .checked_add(PolicyV1::poc_v1().stm_horizon_us)
                    .expect("bounded STM expiry"),
            ),
            superseded_by: None,
            feedback_positive: 0,
            feedback_negative: 0,
            retrieval_count: 0,
            retirement_run_id: None,
        }
    }

    #[test]
    fn database_uses_strict_schema_wal_and_foreign_keys() {
        let (_directory, repository) = repository();
        let strict: i64 = repository
            .connection()
            .query_row(
                "SELECT strict FROM pragma_table_list WHERE name = 'atom_headers'",
                [],
                |row| row.get(0),
            )
            .expect("strict table query");
        let journal: String = repository
            .connection()
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("journal mode");
        let foreign_keys: i64 = repository
            .connection()
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("foreign keys");
        assert_eq!(strict, 1);
        assert_eq!(journal.to_ascii_lowercase(), "wal");
        assert_eq!(foreign_keys, 1);
    }

    #[test]
    fn policy_installation_is_exact_and_integrity_checked() {
        let (_directory, mut repository) = repository();
        let mut override_policy = PolicyV1::poc_v1();
        override_policy.retrieval_default_k = 11;
        assert!(matches!(
            repository.install_policy(&override_policy, 1),
            Err(StoreError::Core(
                naome_memory_core::MemoryError::PolicyMismatch
            ))
        ));
        repository
            .install_policy(&PolicyV1::poc_v1(), 1)
            .expect("install exact policy");
        repository
            .check_integrity(false)
            .expect("verified exact policy authority");
        repository
            .connection()
            .execute(
                "UPDATE policies SET canonical_body = x'00' WHERE policy_id = 'poc-v1'",
                [],
            )
            .expect("tamper policy authority");
        assert!(matches!(
            repository.check_integrity(false),
            Err(StoreError::Integrity(_))
        ));
    }

    #[test]
    fn full_integrity_scan_rejects_unretired_body_projection_search_and_state_tampering() {
        let (_directory, mut repository) = repository();
        let policy = PolicyV1::poc_v1();
        repository
            .install_policy(&policy, 1)
            .expect("install policy");
        let body = core_body(policy.digest().expect("policy digest"));
        let state = exact_stm_state(&body);
        let authority = AtomRecord::from_core(&body, &state).expect("atom authority");
        repository
            .put_core_atom(&body, &state)
            .expect("persist core atom");
        repository
            .check_integrity(false)
            .expect("baseline full integrity");

        repository
            .connection()
            .execute(
                "UPDATE atom_bodies SET canonical_body = x'00' WHERE atom_id = ?1",
                [&authority.atom_id[..]],
            )
            .expect("tamper unretired body");
        assert!(repository.check_integrity(false).is_err());
        repository
            .connection()
            .execute(
                "UPDATE atom_bodies SET canonical_body = ?2 WHERE atom_id = ?1",
                params![&authority.atom_id[..], &authority.canonical_body],
            )
            .expect("restore unretired body");

        repository
            .connection()
            .execute(
                "UPDATE atom_bodies
                 SET json_projection = json_set(json_projection, '$.trigger', 'tampered')
                 WHERE atom_id = ?1",
                [&authority.atom_id[..]],
            )
            .expect("tamper typed JSON projection");
        assert!(repository.check_integrity(false).is_err());
        repository
            .connection()
            .execute(
                "UPDATE atom_bodies SET json_projection = ?2 WHERE atom_id = ?1",
                params![&authority.atom_id[..], &authority.json_projection],
            )
            .expect("restore typed JSON projection");

        repository
            .connection()
            .execute(
                "UPDATE search_projection SET searchable_text = 'tampered search'
                 WHERE atom_id = ?1",
                [&authority.atom_id[..]],
            )
            .expect("tamper search projection");
        assert!(repository.check_integrity(false).is_err());
        repository
            .connection()
            .execute(
                "UPDATE search_projection SET searchable_text = ?2 WHERE atom_id = ?1",
                params![&authority.atom_id[..], &authority.searchable_text],
            )
            .expect("restore search projection");

        repository
            .connection()
            .execute(
                "DELETE FROM atom_search WHERE atom_id = ?1",
                [hex::encode(authority.atom_id)],
            )
            .expect("delete FTS authority row");
        assert!(repository.check_integrity(false).is_err());
        repository
            .connection()
            .execute(
                "INSERT INTO atom_search(atom_id, searchable_text) VALUES (?1, ?2)",
                params![hex::encode(authority.atom_id), &authority.searchable_text],
            )
            .expect("restore FTS authority row");

        repository
            .connection()
            .execute(
                "UPDATE atom_state SET feedback_positive = 1 WHERE atom_id = ?1",
                [&authority.atom_id[..]],
            )
            .expect("tamper mutable state");
        assert!(repository.check_integrity(false).is_err());
        repository
            .connection()
            .execute(
                "UPDATE atom_state SET feedback_positive = 0 WHERE atom_id = ?1",
                [&authority.atom_id[..]],
            )
            .expect("restore mutable state");
        repository
            .check_integrity(false)
            .expect("restored full integrity closure");
    }

    #[test]
    fn full_integrity_scan_rejects_relation_and_artifact_closure_tampering() {
        let (_directory, mut repository) = repository();
        let policy = PolicyV1::poc_v1();
        let policy_digest = policy.digest().expect("policy digest");
        repository
            .install_policy(&policy, 1)
            .expect("install policy");

        let mut target = core_body(policy_digest);
        target.scope.session_id = "integrity-target".to_owned();
        target.trigger = "integrity relation target".to_owned();
        let target_state = exact_stm_state(&target);
        repository
            .put_core_atom(&target, &target_state)
            .expect("persist relation target");

        let inline = b"integrity artifact bytes".to_vec();
        let artifact_digest = Digest32::hash_prefixed(&[], &inline);
        let mut source = core_body(policy_digest);
        source.scope.session_id = "integrity-source".to_owned();
        source.trigger = "integrity relation source".to_owned();
        source.relations.push(MemoryRelationV1 {
            kind: MemoryRelationKindV1::Supports,
            target: target.atom_id().expect("target ID"),
        });
        source.artifacts.push(ArtifactRefV1 {
            digest: artifact_digest,
            size_bytes: u64::try_from(inline.len()).expect("inline length"),
            media_type: "application/octet-stream".to_owned(),
            retention_allowed: true,
            inline_payload: Some(inline),
        });
        let source_state = exact_stm_state(&source);
        repository
            .put_core_atom(&source, &source_state)
            .expect("persist linked atom");
        repository
            .check_integrity(true)
            .expect("baseline link and CAS integrity");

        let source_id = source.atom_id().expect("source ID").digest().0;
        let target_id = target.atom_id().expect("target ID").digest().0;
        let relation = source.relations[0]
            .canonical_bytes()
            .expect("relation authority");
        repository
            .connection()
            .execute(
                "DELETE FROM atom_relations
                 WHERE source_atom_id = ?1 AND target_atom_id = ?2 AND relation_kind = 'supports'",
                params![&source_id[..], &target_id[..]],
            )
            .expect("delete declared relation");
        assert!(repository.check_integrity(false).is_err());
        repository
            .connection()
            .execute(
                "INSERT INTO atom_relations(
                    source_atom_id, target_atom_id, relation_kind, relation_digest
                 ) VALUES (?1, ?2, 'supports', ?3)",
                params![&source_id[..], &target_id[..], &sha256(&relation)[..]],
            )
            .expect("restore declared relation");

        repository
            .connection()
            .execute(
                "DELETE FROM artifact_refs WHERE atom_id = ?1",
                [&source_id[..]],
            )
            .expect("delete live artifact closure");
        assert!(repository.check_integrity(false).is_err());
        repository
            .connection()
            .execute(
                "INSERT INTO artifact_refs(artifact_digest, atom_id, role, retention_allowed)
                 VALUES (?1, ?2, 'application/octet-stream', 1)",
                params![artifact_digest.as_bytes(), &source_id[..]],
            )
            .expect("restore live artifact closure");

        repository
            .connection()
            .execute(
                "DELETE FROM artifact_provenance_edges WHERE atom_id = ?1",
                [&source_id[..]],
            )
            .expect("delete immutable artifact provenance");
        assert!(repository.check_integrity(false).is_err());
        repository
            .connection()
            .execute(
                "INSERT INTO artifact_provenance_edges(
                    atom_id, artifact_digest, role, retention_allowed
                 ) VALUES (?1, ?2, 'application/octet-stream', 1)",
                params![&source_id[..], artifact_digest.as_bytes()],
            )
            .expect("restore immutable artifact provenance");
        repository
            .check_integrity(true)
            .expect("restored relation, artifact, and CAS closure");
    }

    #[test]
    fn draft_append_is_sequence_guarded_and_seals_atomically() {
        let (_directory, mut repository) = repository();
        repository
            .create_draft(
                "draft-1",
                "space-a",
                Some("repo-a"),
                Some("task-a"),
                Some("agent-a"),
                "session-1",
                1,
            )
            .expect("create draft");
        let payload = b"event".to_vec();
        repository
            .append_event(
                "draft-1",
                &DraftEvent {
                    sequence: 0,
                    event_at_us: 2,
                    event_kind: "observation".into(),
                    payload_digest: sha256(&payload),
                    canonical_payload: payload,
                },
            )
            .expect("append event");
        assert!(
            repository
                .append_event(
                    "draft-1",
                    &DraftEvent {
                        sequence: 0,
                        event_at_us: 3,
                        event_kind: "duplicate".into(),
                        canonical_payload: Vec::new(),
                        payload_digest: sha256(&[]),
                    }
                )
                .is_err()
        );
        repository
            .seal_draft("draft-1", &atom(1, MICROSECONDS_PER_DAY + 1))
            .expect("seal draft");
        assert_eq!(repository.load_events("draft-1").expect("events").len(), 1);
    }

    #[test]
    fn typed_episode_chain_is_one_atomic_seal_and_points_to_newest_atom() {
        let (_directory, mut repository) = repository();
        repository
            .create_draft(
                "chain-draft",
                "space-a",
                Some("repo-a"),
                Some("task-a"),
                Some("agent-a"),
                "session-a",
                1,
            )
            .expect("create chain draft");
        let policy_digest = PolicyV1::poc_v1().digest().expect("policy digest");
        let first = core_body(policy_digest);
        let first_id = first.atom_id().expect("first atom ID");
        let mut second = core_body(policy_digest);
        second.interval.started_at_us = 2;
        second.observations[0].at_us = 2;
        second.trigger = "test episode continuation".to_owned();
        second.payload = MemoryPayloadV1::Episode(EpisodePayloadV1 {
            event_sequence_start: 1,
            event_sequence_end: 1,
            continues: Some(first_id),
        });
        second.relations = vec![MemoryRelationV1 {
            kind: MemoryRelationKindV1::Continues,
            target: first_id,
        }];
        let second_id = second.atom_id().expect("second atom ID");
        let state = exact_stm_state(&first);
        repository
            .seal_core_draft_chain("chain-draft", &[(first, state.clone()), (second, state)])
            .expect("seal typed chain");

        let (status, sealed_atom_id): (String, Vec<u8>) = repository
            .connection()
            .query_row(
                "SELECT status, sealed_atom_id FROM episode_drafts WHERE draft_id = 'chain-draft'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("sealed draft state");
        let atom_count: i64 = repository
            .connection()
            .query_row("SELECT COUNT(*) FROM atom_headers", [], |row| row.get(0))
            .expect("atom count");
        let relation_count: i64 = repository
            .connection()
            .query_row("SELECT COUNT(*) FROM atom_relations", [], |row| row.get(0))
            .expect("relation count");
        assert_eq!(status, "sealed");
        assert_eq!(sealed_atom_id, second_id.digest().0);
        assert_eq!(atom_count, 2);
        assert_eq!(relation_count, 1);
    }

    #[test]
    fn typed_episode_chain_rolls_back_when_a_later_atom_is_invalid() {
        let (_directory, mut repository) = repository();
        repository
            .create_draft(
                "chain-draft",
                "space-a",
                Some("repo-a"),
                Some("task-a"),
                Some("agent-a"),
                "session-a",
                1,
            )
            .expect("create chain draft");
        let policy_digest = PolicyV1::poc_v1().digest().expect("policy digest");
        let first = core_body(policy_digest);
        let first_id = first.atom_id().expect("first atom ID");
        let mut oversized = core_body(policy_digest);
        oversized.trigger = "oversized continuation".to_owned();
        oversized.goal = Some("x".repeat(MAX_ATOM_BYTES + 1));
        oversized.payload = MemoryPayloadV1::Episode(EpisodePayloadV1 {
            event_sequence_start: 1,
            event_sequence_end: 1,
            continues: Some(first_id),
        });
        oversized.relations = vec![MemoryRelationV1 {
            kind: MemoryRelationKindV1::Continues,
            target: first_id,
        }];
        let state = AtomStateV1 {
            retention_tier: RetentionTierV1::ShortTerm,
            content_status: ContentStatusV1::Present,
            expires_at_us: Some(MICROSECONDS_PER_DAY + 1),
            superseded_by: None,
            feedback_positive: 0,
            feedback_negative: 0,
            retrieval_count: 0,
            retirement_run_id: None,
        };
        assert!(
            repository
                .seal_core_draft_chain(
                    "chain-draft",
                    &[(first, state.clone()), (oversized, state)],
                )
                .is_err()
        );
        let draft_status: String = repository
            .connection()
            .query_row(
                "SELECT status FROM episode_drafts WHERE draft_id = 'chain-draft'",
                [],
                |row| row.get(0),
            )
            .expect("draft state after rollback");
        let atom_count: i64 = repository
            .connection()
            .query_row("SELECT COUNT(*) FROM atom_headers", [], |row| row.get(0))
            .expect("atom count after rollback");
        assert_eq!(draft_status, "open");
        assert_eq!(atom_count, 0);
    }

    #[test]
    fn seal_with_bad_relation_rolls_back_atom_links_and_draft_status() {
        let (_directory, mut repository) = repository();
        repository
            .create_draft(
                "draft-1",
                "space-a",
                Some("repo-a"),
                Some("task-a"),
                Some("agent-a"),
                "session-1",
                1,
            )
            .expect("create draft");
        let sealed = atom(1, MICROSECONDS_PER_DAY + 1);
        let bad_relation = AtomRelationLink {
            target_atom_id: [99; 32],
            relation_kind: "continues".into(),
            canonical_relation: b"missing-target-relation".to_vec(),
        };

        assert!(
            repository
                .seal_draft_with_links("draft-1", &sealed, &[], &[bad_relation])
                .is_err()
        );
        let draft_status: String = repository
            .connection()
            .query_row(
                "SELECT status FROM episode_drafts WHERE draft_id = 'draft-1'",
                [],
                |row| row.get(0),
            )
            .expect("draft status");
        let atom_count: i64 = repository
            .connection()
            .query_row("SELECT COUNT(*) FROM atom_headers", [], |row| row.get(0))
            .expect("atom count");
        let relation_count: i64 = repository
            .connection()
            .query_row("SELECT COUNT(*) FROM atom_relations", [], |row| row.get(0))
            .expect("relation count");
        assert_eq!(draft_status, "open");
        assert_eq!(atom_count, 0);
        assert_eq!(relation_count, 0);
    }

    #[test]
    fn relations_cannot_cross_memory_spaces_on_any_write_path() {
        let (_directory, mut repository) = repository();
        let source = atom(1, MICROSECONDS_PER_DAY + 1);
        let mut target = atom(2, MICROSECONDS_PER_DAY + 1);
        target.memory_space_id = "space-b".to_owned();
        repository.put_atom(&source).expect("source atom");
        repository.put_atom(&target).expect("target atom");
        assert!(matches!(
            repository.add_relation(
                source.atom_id,
                target.atom_id,
                "supports",
                b"cross-space relation"
            ),
            Err(StoreError::InvalidInput(_))
        ));

        let transaction = repository
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .expect("transaction");
        let result = insert_atom_links_transaction(
            &transaction,
            source.atom_id,
            &[],
            &[AtomRelationLink {
                target_atom_id: target.atom_id,
                relation_kind: "supports".to_owned(),
                canonical_relation: b"transaction cross-space relation".to_vec(),
            }],
        );
        assert!(matches!(result, Err(StoreError::InvalidInput(_))));
        drop(transaction);
        let relation_count: i64 = repository
            .connection()
            .query_row("SELECT COUNT(*) FROM atom_relations", [], |row| row.get(0))
            .expect("relation count");
        assert_eq!(relation_count, 0);
    }

    #[test]
    fn bounded_batch_insert_is_atomic_and_idempotent() {
        let (_directory, mut repository) = repository();
        let first = atom(1, MICROSECONDS_PER_DAY + 1);
        let second = atom(2, MICROSECONDS_PER_DAY + 2);
        let mut invalid = second.clone();
        invalid.body_digest = [0; 32];

        assert!(
            repository
                .put_atoms_batch(&[first.clone(), invalid])
                .is_err()
        );
        let after_rollback: i64 = repository
            .connection()
            .query_row("SELECT COUNT(*) FROM atom_headers", [], |row| row.get(0))
            .expect("atom count after rollback");
        assert_eq!(after_rollback, 0);

        let inserted = repository
            .put_atoms_batch(&[first.clone(), second.clone()])
            .expect("batch insert");
        assert_eq!(
            inserted,
            AtomBatchInsertOutcome {
                requested: 2,
                inserted: 2,
                already_present: 0,
            }
        );
        let replay = repository
            .put_atoms_batch(&[first, second])
            .expect("batch replay");
        assert_eq!(
            replay,
            AtomBatchInsertOutcome {
                requested: 2,
                inserted: 0,
                already_present: 2,
            }
        );
    }

    #[test]
    fn typed_insertion_rejects_invalid_body_and_initial_lifecycle_state() {
        let (_directory, mut repository) = repository();
        let policy_digest = PolicyV1::poc_v1().digest().expect("policy digest");
        let body = core_body(policy_digest);

        let mut wrong_expiry = exact_stm_state(&body);
        wrong_expiry.expires_at_us = wrong_expiry.expires_at_us.map(|value| value + 1);
        assert!(matches!(
            repository.put_core_atom(&body, &wrong_expiry),
            Err(StoreError::InvalidInput(_))
        ));

        let mut forgotten = exact_stm_state(&body);
        forgotten.content_status = ContentStatusV1::Forgotten;
        assert!(matches!(
            repository.put_core_atom(&body, &forgotten),
            Err(StoreError::InvalidInput(_))
        ));

        let mut invalid_body = body.clone();
        invalid_body.observations[0].at_us = invalid_body.interval.ended_at_us + 1;
        assert!(matches!(
            repository.put_core_atom(&invalid_body, &exact_stm_state(&invalid_body)),
            Err(StoreError::Core(
                naome_memory_core::MemoryError::InvalidAtomBody(_)
            ))
        ));
        let atom_count: i64 = repository
            .connection()
            .query_row("SELECT COUNT(*) FROM atom_headers", [], |row| row.get(0))
            .expect("atom count");
        assert_eq!(atom_count, 0);
    }

    #[test]
    fn snapshot_is_stable_sorted_and_space_isolated() {
        let (_directory, mut repository) = repository();
        repository
            .put_atom(&atom(2, MICROSECONDS_PER_DAY + 1))
            .expect("atom two");
        repository
            .put_atom(&atom(1, MICROSECONDS_PER_DAY + 2))
            .expect("atom one");
        let mut other = atom(3, MICROSECONDS_PER_DAY + 3);
        other.memory_space_id = "space-b".into();
        repository.put_atom(&other).expect("other space");

        let key = CohortKey {
            memory_space_id: "space-a".into(),
            cohort_day: 1,
            as_of_us: MICROSECONDS_PER_DAY * 2,
        };
        let first = repository.snapshot(&key).expect("snapshot");
        let second = repository.snapshot(&key).expect("snapshot replay");
        assert_eq!(first, second);
        assert_eq!(first.atoms.len(), 2);
        assert!(first.atoms[0].atom_id < first.atoms[1].atom_id);
    }

    #[test]
    fn fts_candidates_are_scope_filtered_and_feedback_is_idempotent() {
        let (_directory, mut repository) = repository();
        let first = atom(1, MICROSECONDS_PER_DAY + 1);
        let mut second = atom(2, MICROSECONDS_PER_DAY + 2);
        second.repository_id = Some("repo-b".into());
        repository.put_atom(&first).expect("first atom");
        repository.put_atom(&second).expect("second atom");

        let local = repository
            .search_candidates(
                &SearchScope {
                    memory_space_id: "space-a".into(),
                    repository_id: Some("repo-a".into()),
                    memory_space_wide: false,
                },
                "memory",
                10,
            )
            .expect("repository-local search");
        assert_eq!(local.len(), 1);
        repository
            .add_feedback([41; 32], first.atom_id, 100, true, "test", [42; 32])
            .expect("feedback");
        repository
            .add_feedback([41; 32], first.atom_id, 100, true, "test", [42; 32])
            .expect("idempotent feedback");
        repository
            .record_retrievals(&[first.atom_id])
            .expect("retrieval count");
        let wide = repository
            .search_candidates(
                &SearchScope {
                    memory_space_id: "space-a".into(),
                    repository_id: None,
                    memory_space_wide: true,
                },
                "memory",
                10,
            )
            .expect("memory-space-wide search");
        assert_eq!(wide.len(), 2);
        let updated = wide
            .iter()
            .find(|candidate| candidate.atom_id == first.atom_id)
            .expect("updated candidate");
        assert_eq!(updated.feedback_positive, 1);
        assert_eq!(updated.retrieval_count, 1);
    }

    #[test]
    fn artifact_gc_requires_grace_and_dry_run_does_not_mutate() {
        let (directory, mut repository) = repository();
        let record = repository
            .artifact_store()
            .ingest_bytes(b"garbage")
            .expect("ingest");
        repository
            .register_artifact(&record, 100)
            .expect("register");

        assert!(matches!(
            repository.garbage_collect(200, MICROSECONDS_PER_DAY - 1, true),
            Err(StoreError::InvalidInput(_))
        ));
        let dry = repository
            .garbage_collect(200, MICROSECONDS_PER_DAY, true)
            .expect("dry run");
        assert_eq!(dry.entries[0].action, GcAction::WouldMark);
        let mark = repository
            .garbage_collect(200, MICROSECONDS_PER_DAY, false)
            .expect("mark");
        assert_eq!(mark.entries[0].action, GcAction::Marked);
        let wait = repository
            .garbage_collect(200 + MICROSECONDS_PER_DAY - 1, MICROSECONDS_PER_DAY, false)
            .expect("wait");
        assert_eq!(wait.entries[0].action, GcAction::WaitingForGrace);
        let delete = repository
            .garbage_collect(200 + MICROSECONDS_PER_DAY, MICROSECONDS_PER_DAY, false)
            .expect("delete");
        assert_eq!(delete.entries[0].action, GcAction::Deleted);
        assert!(
            !directory
                .path()
                .join("artifacts")
                .join(record.relative_path)
                .exists()
        );
    }

    #[test]
    fn artifact_gc_adopts_and_collects_a_finalized_unregistered_orphan() {
        let (directory, mut repository) = repository();
        let record = repository
            .artifact_store()
            .ingest_bytes(b"renamed before registration")
            .expect("ingest without SQLite registration");

        let dry = repository
            .garbage_collect(200, MICROSECONDS_PER_DAY, true)
            .expect("discover orphan without mutation");
        assert_eq!(dry.entries.len(), 1);
        assert_eq!(dry.entries[0].digest, record.digest);
        assert_eq!(dry.entries[0].action, GcAction::WouldMark);
        let rows_after_dry_run: i64 = repository
            .connection()
            .query_row("SELECT COUNT(*) FROM artifacts", [], |row| row.get(0))
            .expect("artifact count after dry run");
        assert_eq!(rows_after_dry_run, 0);

        let marked = repository
            .garbage_collect(200, MICROSECONDS_PER_DAY, false)
            .expect("adopt orphan with a recovery mark");
        assert_eq!(marked.entries[0].action, GcAction::Marked);
        let persisted_mark: i64 = repository
            .connection()
            .query_row(
                "SELECT gc_marked_at_us FROM artifacts WHERE artifact_digest = ?1",
                [&record.digest.0[..]],
                |row| row.get(0),
            )
            .expect("persisted orphan mark");
        assert_eq!(persisted_mark, 200);

        let deleted = repository
            .garbage_collect(200 + MICROSECONDS_PER_DAY, MICROSECONDS_PER_DAY, false)
            .expect("delete orphan after grace");
        assert_eq!(deleted.entries[0].action, GcAction::Deleted);
        assert!(
            !directory
                .path()
                .join("artifacts")
                .join(&record.relative_path)
                .exists()
        );
        let rows_after_delete: i64 = repository
            .connection()
            .query_row("SELECT COUNT(*) FROM artifacts", [], |row| row.get(0))
            .expect("artifact count after orphan deletion");
        assert_eq!(rows_after_delete, 0);
    }

    #[test]
    fn artifact_gc_recovers_when_unlink_preceded_sqlite_commit() {
        let (directory, mut repository) = repository();
        let record = repository
            .artifact_store()
            .ingest_bytes(b"interrupted sweep")
            .expect("ingest");
        repository
            .register_artifact(&record, 100)
            .expect("register");
        repository
            .garbage_collect(200, MICROSECONDS_PER_DAY, false)
            .expect("mark");

        // This is the durable intermediate state after a process unlinked the
        // file inside BEGIN IMMEDIATE but stopped before SQLite committed.
        fs::remove_file(
            directory
                .path()
                .join("artifacts")
                .join(&record.relative_path),
        )
        .expect("simulate committed unlink");

        let recovered = repository
            .garbage_collect(200 + MICROSECONDS_PER_DAY, MICROSECONDS_PER_DAY, false)
            .expect("recover missing GC-eligible file");
        assert_eq!(recovered.entries.len(), 1);
        assert_eq!(recovered.entries[0].digest, record.digest);
        assert_eq!(recovered.entries[0].action, GcAction::Deleted);
        let rows_after_recovery: i64 = repository
            .connection()
            .query_row("SELECT COUNT(*) FROM artifacts", [], |row| row.get(0))
            .expect("artifact count after recovery");
        assert_eq!(rows_after_recovery, 0);
    }

    #[test]
    fn integrity_check_detects_artifact_tampering() {
        let (directory, mut repository) = repository();
        repository
            .install_policy(&PolicyV1::poc_v1(), 1)
            .expect("install policy");
        let record = repository
            .artifact_store()
            .ingest_bytes(b"original")
            .expect("ingest");
        repository
            .register_artifact(&record, 100)
            .expect("register");
        fs::write(
            directory
                .path()
                .join("artifacts")
                .join(&record.relative_path),
            b"changed!",
        )
        .expect("tamper");
        assert!(repository.check_integrity(true).is_err());
    }

    #[test]
    fn retirement_apply_is_atomic_and_idempotent() {
        let (_directory, mut repository) = repository();
        repository
            .install_policy(&PolicyV1::poc_v1(), 1)
            .expect("install policy");
        repository
            .put_atom(&atom(1, MICROSECONDS_PER_DAY + 1))
            .expect("put atom");
        let key = CohortKey {
            memory_space_id: "space-a".into(),
            cohort_day: 1,
            as_of_us: MICROSECONDS_PER_DAY * 2,
        };
        let snapshot = repository.snapshot(&key).expect("snapshot");
        let commit = commit_for(&snapshot, true);

        assert_eq!(
            repository.apply_retirement(&commit).expect("first apply"),
            ApplyOutcome::Applied
        );
        assert_eq!(
            repository.apply_retirement(&commit).expect("replay"),
            ApplyOutcome::AlreadyApplied
        );
        let mut conflicting = commit.clone();
        conflicting.run_id = [91; 32];
        conflicting.plan_digest = [92; 32];
        assert!(matches!(
            repository.apply_retirement(&conflicting),
            Err(StoreError::RetirementConflict)
        ));
        let state: (String, String) = repository
            .connection()
            .query_row(
                "SELECT retention_tier, content_status FROM atom_state WHERE atom_id = ?1",
                [&snapshot.atoms[0].atom_id[..]],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("state");
        assert_eq!(state, ("ltm_episodic".into(), "present".into()));
    }

    #[test]
    fn retirement_forgets_a_batch_with_complete_persisted_closure() {
        let (_directory, mut repository) = repository();
        repository
            .install_policy(&PolicyV1::poc_v1(), 1)
            .expect("install policy");
        let atoms = (0_u8..128)
            .map(|id| {
                let mut atom = atom(id, MICROSECONDS_PER_DAY + u64::from(id) + 1);
                atom.retention_permission = "transient_only".into();
                atom
            })
            .collect::<Vec<_>>();
        repository
            .put_atoms_batch(&atoms)
            .expect("persist atom batch");
        let key = CohortKey {
            memory_space_id: "space-a".into(),
            cohort_day: 1,
            as_of_us: MICROSECONDS_PER_DAY * 2,
        };
        let snapshot = repository.snapshot(&key).expect("snapshot");
        let commit = commit_for(&snapshot, false);

        assert_eq!(
            repository.apply_retirement(&commit).expect("batch apply"),
            ApplyOutcome::Applied
        );
        assert_eq!(
            repository.apply_retirement(&commit).expect("batch replay"),
            ApplyOutcome::AlreadyApplied
        );
        for table in ["retirement_members", "retirement_decisions"] {
            let count: i64 = repository
                .connection()
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("closure row count");
            assert_eq!(count, 128);
        }
        for table in ["atom_bodies", "search_projection", "atom_search"] {
            let count: i64 = repository
                .connection()
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("forgotten projection count");
            assert_eq!(count, 0);
        }
        let forgotten_states: i64 = repository
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM atom_state
                 WHERE retention_tier = 'forgotten' AND content_status = 'forgotten'
                   AND retirement_run_id = ?1",
                [&commit.run_id[..]],
                |row| row.get(0),
            )
            .expect("forgotten state count");
        assert_eq!(forgotten_states, 128);
    }

    #[test]
    fn retirement_projection_count_mismatch_rolls_back_set_based_apply() {
        let (_directory, mut repository) = repository();
        repository
            .install_policy(&PolicyV1::poc_v1(), 1)
            .expect("install policy");
        let mut first = atom(1, MICROSECONDS_PER_DAY + 1);
        first.retention_permission = "transient_only".into();
        let missing_fts_id = hex::encode(first.atom_id);
        repository.put_atom(&first).expect("put atom");
        repository
            .connection()
            .execute(
                "DELETE FROM atom_search WHERE atom_id = ?1",
                [missing_fts_id],
            )
            .expect("simulate incomplete FTS closure");
        let key = CohortKey {
            memory_space_id: "space-a".into(),
            cohort_day: 1,
            as_of_us: MICROSECONDS_PER_DAY * 2,
        };
        let snapshot = repository.snapshot(&key).expect("snapshot");
        let commit = commit_for(&snapshot, false);

        assert!(matches!(
            repository.apply_retirement(&commit),
            Err(StoreError::RetirementConflict)
        ));
        let state: (String, String, Option<Vec<u8>>) = repository
            .connection()
            .query_row(
                "SELECT retention_tier, content_status, retirement_run_id
                 FROM atom_state WHERE atom_id = ?1",
                [&first.atom_id[..]],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("rolled back state");
        let run_count: i64 = repository
            .connection()
            .query_row("SELECT COUNT(*) FROM retirement_runs", [], |row| row.get(0))
            .expect("rolled back run count");
        assert_eq!(state, ("stm".into(), "present".into(), None));
        assert_eq!(run_count, 0);
    }

    #[test]
    fn retirement_rejects_stale_snapshot_without_writes() {
        let (_directory, mut repository) = repository();
        repository
            .install_policy(&PolicyV1::poc_v1(), 1)
            .expect("install policy");
        repository
            .put_atom(&atom(1, MICROSECONDS_PER_DAY + 1))
            .expect("put initial atom");
        let key = CohortKey {
            memory_space_id: "space-a".into(),
            cohort_day: 1,
            as_of_us: MICROSECONDS_PER_DAY * 2,
        };
        let snapshot = repository.snapshot(&key).expect("snapshot");
        let commit = commit_for(&snapshot, true);
        repository
            .put_atom(&atom(2, MICROSECONDS_PER_DAY + 2))
            .expect("concurrent atom");

        assert!(matches!(
            repository.apply_retirement(&commit),
            Err(StoreError::StalePlan { .. })
        ));
        let runs: i64 = repository
            .connection()
            .query_row("SELECT COUNT(*) FROM retirement_runs", [], |row| row.get(0))
            .expect("run count");
        assert_eq!(runs, 0);
    }

    #[test]
    fn core_apply_contract_persists_and_replays_same_receipt() {
        let (_directory, mut repository) = repository();
        let policy = PolicyV1::poc_v1();
        let policy_digest = policy.digest().expect("policy digest");
        repository
            .install_policy(&policy, 1)
            .expect("install policy");
        let body = core_body(policy_digest);
        let mut forgotten_body = core_body(policy_digest);
        forgotten_body.scope.session_id = "session-forgotten".into();
        forgotten_body.trigger = "forgotten test episode".into();
        forgotten_body.retention_permission = RetentionPermissionV1::SemanticAllowed;
        let expires_at_us = body
            .interval
            .recorded_at_us
            .checked_add(policy.stm_horizon_us)
            .expect("bounded expiry");
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
        repository
            .put_core_atom(&body, &state)
            .expect("put core atom");
        repository
            .put_core_atom(&forgotten_body, &state)
            .expect("put core atom that will be forgotten");
        let cohort_day = expires_at_us / MICROSECONDS_PER_DAY;
        let cohort_end_us = cohort_day
            .checked_add(1)
            .and_then(|day| day.checked_mul(MICROSECONDS_PER_DAY))
            .expect("bounded cohort end");
        let snapshot = repository
            .snapshot_core(&CohortKey {
                memory_space_id: "space-a".into(),
                cohort_day,
                as_of_us: cohort_end_us,
            })
            .expect("core snapshot");
        let plan = naome_memory_core::plan_retirement(&snapshot, &policy, Seed32::new([44; 32]))
            .expect("retirement plan");
        let conflicting_seed_plan =
            naome_memory_core::plan_retirement(&snapshot, &policy, Seed32::new([45; 32]))
                .expect("conflicting seed plan");

        let first =
            naome_memory_core::apply_retirement(&mut repository, &plan).expect("first core apply");
        let replay = naome_memory_core::apply_retirement(&mut repository, &plan)
            .expect("idempotent core replay");
        assert_eq!(first, replay);
        assert!(matches!(
            repository.apply_core_plan(&conflicting_seed_plan),
            Err(StoreError::RetirementConflict)
        ));
        let receipts: i64 = repository
            .connection()
            .query_row("SELECT COUNT(*) FROM proof_receipts", [], |row| row.get(0))
            .expect("receipt count");
        assert_eq!(receipts, 1);

        let exact_id = body.atom_id().expect("exact atom id");
        let forgotten_id = forgotten_body.atom_id().expect("forgotten atom id");
        assert!(plan.episodic_winners.contains(&exact_id));
        assert!(!plan.episodic_winners.contains(&forgotten_id));
        repository
            .add_feedback(
                [91; 32],
                exact_id.digest().0,
                cohort_end_us,
                true,
                "test",
                [92; 32],
            )
            .expect("post-retirement feedback");
        repository
            .record_retrievals(&[exact_id.digest().0])
            .expect("post-retirement retrieval");
        repository
            .check_integrity(false)
            .expect("legitimate mutable counters preserve historical receipt validity");
        repository
            .apply_core_plan(&plan)
            .expect("receipt replay remains valid after mutable counter advances");

        let forgotten_provenance_sources: i64 = repository
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM atom_provenance_sources WHERE atom_id = ?1",
                [forgotten_id.digest().as_bytes()],
                |row| row.get(0),
            )
            .expect("forgotten provenance source count");
        assert_eq!(forgotten_provenance_sources, 1);
        let forgotten_source_digest = forgotten_body.provenance.source_event_digests[0];
        repository
            .connection()
            .execute(
                "DELETE FROM atom_provenance_sources WHERE atom_id = ?1",
                [forgotten_id.digest().as_bytes()],
            )
            .expect("tamper forgotten event provenance");
        assert!(repository.check_integrity(false).is_err());
        repository
            .connection()
            .execute(
                "INSERT INTO atom_provenance_sources(atom_id, ordinal, source_event_digest)
                 VALUES (?1, 0, ?2)",
                params![
                    forgotten_id.digest().as_bytes(),
                    forgotten_source_digest.as_bytes()
                ],
            )
            .expect("restore forgotten event provenance");
        repository
            .check_integrity(false)
            .expect("restored event provenance verifies");

        let mut late_body = core_body(policy_digest);
        late_body.scope.session_id = "session-too-late".to_owned();
        late_body.trigger = "late cohort insertion".to_owned();
        assert!(matches!(
            repository.put_core_atom(&late_body, &exact_stm_state(&late_body)),
            Err(StoreError::RetirementConflict)
        ));
        let forgotten_record = AtomRecord::from_core(&forgotten_body, &state)
            .expect("forgotten atom record authority");

        repository
            .connection()
            .execute(
                "UPDATE atom_state SET retention_tier = 'ltm_dual' WHERE atom_id = ?1",
                [exact_id.digest().as_bytes()],
            )
            .expect("tamper exact state");
        assert!(repository.check_integrity(false).is_err());
        repository
            .connection()
            .execute(
                "UPDATE atom_state SET retention_tier = 'ltm_episodic' WHERE atom_id = ?1",
                [exact_id.digest().as_bytes()],
            )
            .expect("restore exact state");
        repository
            .check_integrity(false)
            .expect("restored exact state verifies");

        let exact_canonical: Vec<u8> = repository
            .connection()
            .query_row(
                "SELECT canonical_body FROM atom_bodies WHERE atom_id = ?1",
                [exact_id.digest().as_bytes()],
                |row| row.get(0),
            )
            .expect("exact canonical body");
        repository
            .connection()
            .execute(
                "UPDATE atom_bodies SET canonical_body = x'00' WHERE atom_id = ?1",
                [exact_id.digest().as_bytes()],
            )
            .expect("tamper exact body");
        assert!(repository.check_integrity(false).is_err());
        repository
            .connection()
            .execute(
                "UPDATE atom_bodies SET canonical_body = ?2 WHERE atom_id = ?1",
                params![exact_id.digest().as_bytes(), exact_canonical],
            )
            .expect("restore exact body");
        repository
            .check_integrity(false)
            .expect("restored exact body verifies");

        repository
            .connection()
            .execute(
                "INSERT INTO atom_bodies(atom_id, canonical_body, json_projection)
                 VALUES (?1, ?2, ?3)",
                params![
                    forgotten_id.digest().as_bytes(),
                    forgotten_record.canonical_body,
                    forgotten_record.json_projection
                ],
            )
            .expect("restore forgotten body without authority");
        assert!(repository.check_integrity(false).is_err());
        repository
            .connection()
            .execute(
                "DELETE FROM atom_bodies WHERE atom_id = ?1",
                [forgotten_id.digest().as_bytes()],
            )
            .expect("remove restored forgotten body");

        repository
            .connection()
            .execute(
                "INSERT INTO search_projection(
                    atom_id, memory_space_id, repository_id, searchable_text, topic_keys_json,
                    entity_ids_json, outcome_class, projection_digest
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    forgotten_id.digest().as_bytes(),
                    forgotten_record.memory_space_id,
                    forgotten_record.repository_id,
                    forgotten_record.searchable_text,
                    forgotten_record.topic_keys_json,
                    forgotten_record.entity_ids_json,
                    forgotten_record.outcome_class,
                    &sha256_projection(&forgotten_record)[..]
                ],
            )
            .expect("restore forgotten search projection without authority");
        assert!(repository.check_integrity(false).is_err());
        repository
            .connection()
            .execute(
                "DELETE FROM search_projection WHERE atom_id = ?1",
                [forgotten_id.digest().as_bytes()],
            )
            .expect("remove restored forgotten projection");

        repository
            .connection()
            .execute(
                "UPDATE atom_state
                 SET retention_tier = 'stm', content_status = 'present'
                 WHERE atom_id = ?1",
                [forgotten_id.digest().as_bytes()],
            )
            .expect("tamper forgotten state");
        assert!(repository.check_integrity(false).is_err());
        repository
            .connection()
            .execute(
                "UPDATE atom_state
                 SET retention_tier = 'forgotten', content_status = 'forgotten'
                 WHERE atom_id = ?1",
                [forgotten_id.digest().as_bytes()],
            )
            .expect("restore forgotten state");
        repository
            .check_integrity(false)
            .expect("restored mixed post-state verifies");

        repository
            .connection()
            .execute(
                "UPDATE retirement_decisions SET decision = 'semantic_source'
                 WHERE atom_id = ?1",
                [forgotten_id.digest().as_bytes()],
            )
            .expect("tamper decision classification");
        assert!(repository.check_integrity(false).is_err());
        repository
            .connection()
            .execute(
                "UPDATE retirement_decisions SET decision = 'forgotten'
                 WHERE atom_id = ?1",
                [forgotten_id.digest().as_bytes()],
            )
            .expect("restore decision classification");
        repository
            .check_integrity(false)
            .expect("restored decision closure verifies");

        repository
            .connection()
            .execute(
                "UPDATE atom_headers SET repository_id = 'tampered-repo'
                 WHERE atom_id = ?1",
                [forgotten_id.digest().as_bytes()],
            )
            .expect("tamper forgotten header");
        assert!(repository.check_integrity(false).is_err());
        repository
            .connection()
            .execute(
                "UPDATE atom_headers SET repository_id = 'repo-a' WHERE atom_id = ?1",
                [forgotten_id.digest().as_bytes()],
            )
            .expect("restore forgotten header");
        repository
            .check_integrity(false)
            .expect("restored header closure verifies");

        let original_json: String = repository
            .connection()
            .query_row("SELECT json_projection FROM proof_receipts", [], |row| {
                row.get(0)
            })
            .expect("receipt JSON");
        repository
            .connection()
            .execute(
                "UPDATE proof_receipts
                 SET json_projection = json_set(json_projection, '$.memory_space_id', 'tampered')",
                [],
            )
            .expect("tamper receipt JSON");
        assert!(matches!(
            repository.apply_core_plan(&plan),
            Err(StoreError::Integrity(_))
        ));
        assert!(repository.check_integrity(false).is_err());
        repository
            .connection()
            .execute(
                "UPDATE proof_receipts SET json_projection = ?1",
                [original_json],
            )
            .expect("restore receipt JSON");
        repository
            .apply_core_plan(&plan)
            .expect("verified receipt replay after restore");

        let historical_state: Vec<u8> = repository
            .connection()
            .query_row(
                "SELECT canonical_state FROM retirement_post_states",
                [],
                |row| row.get(0),
            )
            .expect("historical post-state authority");
        repository
            .connection()
            .execute(
                "UPDATE retirement_post_states SET canonical_state = x'00'",
                [],
            )
            .expect("tamper historical post-state");
        assert!(repository.check_integrity(false).is_err());
        repository
            .connection()
            .execute(
                "UPDATE retirement_post_states SET canonical_state = ?1",
                [historical_state],
            )
            .expect("restore historical post-state");
        repository
            .check_integrity(false)
            .expect("restored historical post-state verifies");

        repository
            .connection()
            .execute_batch("PRAGMA foreign_keys = OFF;")
            .expect("disable FK for corruption simulation");
        repository
            .connection()
            .execute("DELETE FROM proof_receipts", [])
            .expect("remove receipt");
        repository
            .connection()
            .execute_batch("PRAGMA foreign_keys = ON;")
            .expect("restore FK enforcement");
        assert!(repository.apply_core_plan(&plan).is_err());
        assert!(repository.check_integrity(false).is_err());
    }

    #[test]
    fn episodic_retirement_drops_retention_disallowed_external_bytes_but_keeps_provenance() {
        let (_directory, mut repository) = repository();
        let policy = PolicyV1::poc_v1();
        repository
            .install_policy(&policy, 1)
            .expect("install policy");
        let record = repository
            .artifact_store()
            .ingest_bytes(b"transient external detail")
            .expect("ingest transient artifact");
        repository
            .register_artifact(&record, 1)
            .expect("register transient artifact");
        let mut body = core_body(policy.digest().expect("policy digest"));
        body.observations[0].artifact_digest = Some(Digest32(record.digest.0));
        body.artifacts = vec![ArtifactRefV1 {
            digest: Digest32(record.digest.0),
            size_bytes: record.byte_len,
            media_type: "application/x-naome-transient".to_owned(),
            retention_allowed: false,
            inline_payload: None,
        }];
        let atom_id = body.atom_id().expect("atom id");
        repository
            .put_core_atom(&body, &exact_stm_state(&body))
            .expect("insert exact-eligible atom");
        let expiry = body.interval.recorded_at_us + policy.stm_horizon_us;
        let cohort_day = expiry / MICROSECONDS_PER_DAY;
        let as_of_us = (cohort_day + 1) * MICROSECONDS_PER_DAY;
        let snapshot = repository
            .snapshot_core(&CohortKey {
                memory_space_id: body.scope.memory_space_id.clone(),
                cohort_day,
                as_of_us,
            })
            .expect("closed cohort snapshot");
        let plan =
            plan_retirement(&snapshot, &policy, Seed32::new([71; 32])).expect("retirement plan");
        assert_eq!(plan.episodic_winners, vec![atom_id]);
        repository.apply_core_plan(&plan).expect("apply retirement");

        let live_refs: i64 = repository
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM artifact_refs WHERE atom_id = ?1",
                [atom_id.digest().as_bytes()],
                |row| row.get(0),
            )
            .expect("live artifact refs");
        let provenance_edges: i64 = repository
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM artifact_provenance_edges WHERE atom_id = ?1",
                [atom_id.digest().as_bytes()],
                |row| row.get(0),
            )
            .expect("artifact provenance edges");
        let pins: i64 = repository
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM artifact_pins WHERE atom_id = ?1",
                [atom_id.digest().as_bytes()],
                |row| row.get(0),
            )
            .expect("artifact pins");
        assert_eq!((live_refs, provenance_edges, pins), (0, 1, 0));
        repository
            .check_integrity(true)
            .expect("metadata-only transient artifact closure");
        let gc = repository
            .garbage_collect(as_of_us, MICROSECONDS_PER_DAY, false)
            .expect("mark unreferenced transient artifact");
        assert_eq!(gc.entries.len(), 1);
        assert_eq!(gc.entries[0].action, GcAction::Marked);
    }
}
