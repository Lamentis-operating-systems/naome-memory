use crate::{CanonicalBytes as _, Digest32, Ppm, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

pub const ATOM_HASH_DOMAIN: &[u8] = b"naome-memory:atom:v1\0";

/// Content address of an immutable atom body.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct AtomId(pub Digest32);

impl AtomId {
    pub fn from_body(body: &MemoryAtomBodyV1) -> Result<Self> {
        Ok(Self(Digest32::hash_prefixed(
            ATOM_HASH_DOMAIN,
            &body.canonical_bytes()?,
        )))
    }

    pub const fn digest(self) -> Digest32 {
        self.0
    }

    pub fn to_hex(self) -> String {
        self.0.to_hex()
    }
}

impl fmt::Display for AtomId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for AtomId {
    type Err = crate::MemoryError;

    fn from_str(value: &str) -> Result<Self> {
        Ok(Self(Digest32::from_str(value)?))
    }
}

/// Explicit random input. It is never synthesized inside the core crate.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Seed32(pub Digest32);

impl Seed32 {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(Digest32(bytes))
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryScopeV1 {
    pub memory_space_id: String,
    pub repository_id: Option<String>,
    pub task_id: Option<String>,
    pub agent_id: Option<String>,
    pub session_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeIntervalV1 {
    pub started_at_us: u64,
    pub ended_at_us: u64,
    pub recorded_at_us: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SealReasonV1 {
    GoalCompleted,
    TerminalFailure,
    GoalChanged,
    ContextChanged,
    Checkpoint,
    Handoff,
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionPermissionV1 {
    TransientOnly,
    SemanticAllowed,
    SemanticAndExactAllowed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationV1 {
    pub at_us: u64,
    pub source: String,
    pub content: String,
    pub artifact_digest: Option<Digest32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterpretationV1 {
    pub content: String,
    pub confidence: Ppm,
    pub evidence: Vec<Digest32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeliefV1 {
    pub proposition: String,
    pub confidence: Ppm,
    pub evidence: Vec<Digest32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionRecordV1 {
    pub decision: String,
    pub rationale: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedAlternativeV1 {
    pub alternative: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionRecordV1 {
    pub action: String,
    pub result: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeV1 {
    pub class: String,
    pub summary: String,
    pub succeeded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackSignalV1 {
    pub source: String,
    pub positive: bool,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormationSignalsV1 {
    pub utility: Ppm,
    pub salience: Ppm,
    pub novelty: Ppm,
    pub uncertainty: Ppm,
}

impl Default for FormationSignalsV1 {
    fn default() -> Self {
        Self {
            utility: Ppm::ZERO,
            salience: Ppm::ZERO,
            novelty: Ppm::ZERO,
            uncertainty: Ppm::ZERO,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceV1 {
    pub producer: String,
    pub source_event_digests: Vec<Digest32>,
    pub policy_digest: Digest32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRelationKindV1 {
    Continues,
    Supports,
    Contradicts,
    Supersedes,
    DerivedFrom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRelationV1 {
    pub kind: MemoryRelationKindV1,
    pub target: AtomId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRefV1 {
    pub digest: Digest32,
    pub size_bytes: u64,
    pub media_type: String,
    pub retention_allowed: bool,
    pub inline_payload: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodePayloadV1 {
    pub event_sequence_start: u64,
    pub event_sequence_end: u64,
    pub continues: Option<AtomId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsolidationScoreV1 {
    pub support: Ppm,
    pub diversity: Ppm,
    pub utility: Ppm,
    pub salience: Ppm,
    pub cohesion: Ppm,
    pub novelty: Ppm,
    pub uncertainty: Ppm,
    pub total: Ppm,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticPayloadV1 {
    pub source_ids: Vec<AtomId>,
    pub source_body_digests: BTreeMap<AtomId, Digest32>,
    pub score: ConsolidationScoreV1,
    pub consensus_numerator: u32,
    pub consensus_denominator: u32,
    pub supersedes: Option<AtomId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum MemoryPayloadV1 {
    Episode(EpisodePayloadV1),
    Semantic(SemanticPayloadV1),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKindV1 {
    Episode,
    Semantic,
}

/// Immutable authority body. Mutable counters and retention state live in
/// [`AtomStateV1`] and therefore never change the content address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryAtomBodyV1 {
    pub contract_version: String,
    pub scope: MemoryScopeV1,
    pub interval: TimeIntervalV1,
    pub trigger: String,
    pub seal_reason: SealReasonV1,
    pub goal: Option<String>,
    pub plan: Vec<String>,
    pub internal_state_before: BTreeMap<String, String>,
    pub internal_state_after: BTreeMap<String, String>,
    pub observations: Vec<ObservationV1>,
    pub interpretations: Vec<InterpretationV1>,
    pub beliefs: Vec<BeliefV1>,
    pub decisions: Vec<DecisionRecordV1>,
    pub rejected_alternatives: Vec<RejectedAlternativeV1>,
    pub actions: Vec<ActionRecordV1>,
    pub outcome: Option<OutcomeV1>,
    pub feedback: Vec<FeedbackSignalV1>,
    pub formation_signals: FormationSignalsV1,
    pub topic_keys: BTreeSet<String>,
    pub entity_ids: BTreeSet<String>,
    pub outcome_class: Option<String>,
    pub goal_key: Option<String>,
    pub provenance: ProvenanceV1,
    pub relations: Vec<MemoryRelationV1>,
    pub artifacts: Vec<ArtifactRefV1>,
    pub retention_permission: RetentionPermissionV1,
    pub payload: MemoryPayloadV1,
}

impl MemoryAtomBodyV1 {
    pub fn atom_id(&self) -> Result<AtomId> {
        AtomId::from_body(self)
    }

    pub fn kind(&self) -> MemoryKindV1 {
        match self.payload {
            MemoryPayloadV1::Episode(_) => MemoryKindV1::Episode,
            MemoryPayloadV1::Semantic(_) => MemoryKindV1::Semantic,
        }
    }

    pub fn encoded_size_bytes(&self) -> Result<u64> {
        u64::try_from(self.canonical_bytes()?.len()).map_err(|_| {
            crate::MemoryError::CanonicalEncoding("atom body exceeds u64 length".to_owned())
        })
    }

    pub fn retained_artifact_digests(&self) -> Vec<Digest32> {
        let mut digests: Vec<_> = self
            .artifacts
            .iter()
            .filter(|artifact| artifact.retention_allowed)
            .map(|artifact| artifact.digest)
            .collect();
        digests.sort_unstable();
        digests.dedup();
        digests
    }

    pub fn exact_package_bytes(&self) -> Result<u64> {
        let artifact_bytes = self
            .artifacts
            .iter()
            .filter(|artifact| artifact.retention_allowed)
            .try_fold(0_u64, |sum, artifact| {
                sum.checked_add(artifact.size_bytes).ok_or_else(|| {
                    crate::MemoryError::CanonicalEncoding(
                        "artifact byte total overflowed u64".to_owned(),
                    )
                })
            })?;
        self.encoded_size_bytes()?
            .checked_add(artifact_bytes)
            .ok_or_else(|| {
                crate::MemoryError::CanonicalEncoding(
                    "exact package byte total overflowed u64".to_owned(),
                )
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionTierV1 {
    ShortTerm,
    SemanticLongTerm,
    EpisodicLongTerm,
    DualLongTerm,
    HeaderOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentStatusV1 {
    Present,
    Forgotten,
    Corrupt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtomStateV1 {
    pub retention_tier: RetentionTierV1,
    pub content_status: ContentStatusV1,
    pub expires_at_us: Option<u64>,
    pub superseded_by: Option<AtomId>,
    pub feedback_positive: u64,
    pub feedback_negative: u64,
    pub retrieval_count: u64,
    pub retirement_run_id: Option<Digest32>,
}

impl AtomStateV1 {
    /// Beta(1,1) posterior mean. Retrieval count deliberately has no role.
    pub fn validated_utility(&self) -> Ppm {
        Ppm::ratio(
            self.feedback_positive.saturating_add(1),
            self.feedback_positive
                .saturating_add(self.feedback_negative)
                .saturating_add(2),
        )
    }

    pub const fn is_episodic_ltm(&self) -> bool {
        matches!(
            self.retention_tier,
            RetentionTierV1::EpisodicLongTerm | RetentionTierV1::DualLongTerm
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrityStatusV1 {
    Complete,
    MissingBody,
    MissingArtifact,
    DigestMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetirementCandidateV1 {
    pub atom_id: AtomId,
    pub body_digest: Digest32,
    pub body: Option<MemoryAtomBodyV1>,
    pub state: AtomStateV1,
    pub integrity: IntegrityStatusV1,
    pub exact_package_bytes: u64,
}

impl RetirementCandidateV1 {
    pub fn from_present_body(body: MemoryAtomBodyV1, state: AtomStateV1) -> Result<Self> {
        let atom_id = body.atom_id()?;
        let exact_package_bytes = body.exact_package_bytes()?;
        Ok(Self {
            atom_id,
            body_digest: atom_id.digest(),
            body: Some(body),
            state,
            integrity: IntegrityStatusV1::Complete,
            exact_package_bytes,
        })
    }
}
