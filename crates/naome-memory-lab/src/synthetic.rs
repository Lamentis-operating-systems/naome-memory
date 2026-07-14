use std::collections::{BTreeMap, BTreeSet};

use naome_memory_core::{
    ArtifactRefV1, Digest32, EpisodeBufferV1, EpisodeEventV1, FormationSignalsV1, MemoryAtomBodyV1,
    MemoryScopeV1, ObservationV1, OutcomeV1, PolicyV1, Ppm, RetentionPermissionV1,
    SealEpisodeRequestV1, SealReasonV1, Seed32, seal_episode,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::Result;

const GOLDEN_SEED_DOMAIN: &[u8] = b"naome-memory:dataset:golden-v1\0";
const CALIBRATION_SEED_DOMAIN: &[u8] = b"naome-memory:dataset:calibration-v1\0";
const HOLDOUT_SEED_DOMAIN: &[u8] = b"naome-memory:dataset:holdout-v1\0";
const ADVERSARIAL_SEED_DOMAIN: &[u8] = b"naome-memory:dataset:adversarial-v1\0";
const SCALE_SEED_DOMAIN: &[u8] = b"naome-memory:dataset:scale-v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DatasetIdV1 {
    #[serde(rename = "golden-v1")]
    Golden,
    #[serde(rename = "calibration-v1")]
    Calibration,
    #[serde(rename = "holdout-v1")]
    Holdout,
    #[serde(rename = "adversarial-v1")]
    Adversarial,
    #[serde(rename = "scale-v1")]
    Scale,
}

impl DatasetIdV1 {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Golden => "golden-v1",
            Self::Calibration => "calibration-v1",
            Self::Holdout => "holdout-v1",
            Self::Adversarial => "adversarial-v1",
            Self::Scale => "scale-v1",
        }
    }

    pub(crate) const fn seed_domain(self) -> &'static [u8] {
        match self {
            Self::Golden => GOLDEN_SEED_DOMAIN,
            Self::Calibration => CALIBRATION_SEED_DOMAIN,
            Self::Holdout => HOLDOUT_SEED_DOMAIN,
            Self::Adversarial => ADVERSARIAL_SEED_DOMAIN,
            Self::Scale => SCALE_SEED_DOMAIN,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyntheticWorldV1 {
    pub contract_version: String,
    pub dataset_id: DatasetIdV1,
    pub world_id: String,
    pub world_index: u64,
    pub seed: Seed32,
    pub atoms: Vec<MemoryAtomBodyV1>,
    pub relevant_topic: String,
    pub ordinary_query_id: String,
    pub rare_query_ids: Vec<String>,
}

#[must_use]
pub fn derive_dataset_world_seed(dataset_id: DatasetIdV1, world_index: u64) -> Seed32 {
    Seed32(Digest32::hash_prefixed(
        dataset_id.seed_domain(),
        &world_index.to_be_bytes(),
    ))
}

/// Derive a calibration seed. New callers should pass a dataset explicitly
/// through [`derive_dataset_world_seed`].
#[must_use]
pub fn derive_world_seed(world_index: u64) -> Seed32 {
    derive_dataset_world_seed(DatasetIdV1::Calibration, world_index)
}

#[must_use]
pub fn world_id(dataset_id: DatasetIdV1, world_index: u64) -> String {
    format!("{}/world-{world_index:04}", dataset_id.as_str())
}

#[must_use]
pub fn rare_event_key(dataset_id: DatasetIdV1, world_index: u64, atom_index: usize) -> String {
    format!(
        "rare:{}:{world_index:04}:{atom_index:02}",
        dataset_id.as_str()
    )
}

#[must_use]
pub fn rare_query_id(dataset_id: DatasetIdV1, world_index: u64, atom_index: usize) -> String {
    format!(
        "rare-query:{}:{world_index:04}:{atom_index:02}",
        dataset_id.as_str()
    )
}

/// Generate one deterministic world from an explicitly selected dataset.
///
/// The first three, older episodes form the only consolidation-eligible
/// cluster. Every episode also carries one unique, retention-allowed inline
/// artifact and observation token. Semantic consolidation cannot copy those
/// one-off values, so the rare-event judgments test actual exact retention.
///
/// # Errors
///
/// Returns an error if the generated episode violates the core sealing
/// contract or cannot be canonically encoded.
pub fn generate_world_for_dataset(
    dataset_id: DatasetIdV1,
    world_index: u64,
    atom_count: usize,
) -> Result<SyntheticWorldV1> {
    let policy = PolicyV1::poc_v1();
    let seed = derive_dataset_world_seed(dataset_id, world_index);
    let mut atoms = Vec::with_capacity(atom_count);
    let mut rare_query_ids = Vec::with_capacity(atom_count);
    for atom_index in 0..atom_count {
        let atom_index_u64 = u64::try_from(atom_index).unwrap_or(u64::MAX);
        let session_index = atom_index % 3;
        let relevant = atom_index < 3;
        let discriminating_topic = if relevant {
            "build-proof".to_owned()
        } else {
            format!("unrelated-{atom_index:02}")
        };
        let rare_key = rare_event_key(dataset_id, world_index, atom_index);
        let artifact_payload = rare_key.as_bytes().to_vec();
        let artifact_digest = Digest32(Sha256::digest(&artifact_payload).into());
        let scope = MemoryScopeV1 {
            memory_space_id: "synthetic-space-v1".to_owned(),
            repository_id: Some("synthetic-repository".to_owned()),
            task_id: Some(format!("{}:{world_index}", dataset_id.as_str())),
            agent_id: Some(format!("agent-{}", atom_index % 2)),
            session_id: format!(
                "{}:{world_index}:session-{session_index}",
                dataset_id.as_str()
            ),
        };
        let at_us = 10_000_000_u64
            .saturating_add(world_index.saturating_mul(1_000_000))
            .saturating_add(atom_index_u64.saturating_mul(1_000));
        let event = EpisodeEventV1 {
            sequence: 0,
            at_us,
            scope: scope.clone(),
            observation: Some(ObservationV1 {
                at_us,
                source: "synthetic-world-v1".to_owned(),
                content: format!("synthetic observation {world_index}/{atom_index} {rare_key}"),
                artifact_digest: Some(artifact_digest),
            }),
            interpretation: None,
            belief: None,
            decision: None,
            action: None,
        };
        let buffer = EpisodeBufferV1 {
            scope,
            started_at_us: at_us,
            events: vec![event],
            continues: None,
        };
        let request = SealEpisodeRequestV1 {
            as_of_us: at_us.saturating_add(10),
            ended_at_us: at_us.saturating_add(5),
            trigger: "synthetic-test".to_owned(),
            seal_reason: SealReasonV1::Checkpoint,
            goal: Some("validate deterministic memory".to_owned()),
            plan: vec!["observe".to_owned(), "verify".to_owned()],
            internal_state_before: BTreeMap::new(),
            internal_state_after: BTreeMap::new(),
            rejected_alternatives: Vec::new(),
            outcome: Some(OutcomeV1 {
                class: if relevant { "success" } else { "neutral" }.to_owned(),
                summary: "synthetic outcome".to_owned(),
                succeeded: relevant,
            }),
            feedback: Vec::new(),
            formation_signals: FormationSignalsV1 {
                utility: Ppm::from_raw_unchecked(if relevant { 900_000 } else { 100_000 }),
                salience: Ppm::from_raw_unchecked(700_000),
                novelty: Ppm::from_raw_unchecked(700_000),
                uncertainty: Ppm::from_raw_unchecked(50_000),
            },
            topic_keys: BTreeSet::from([discriminating_topic, "memory".to_owned()]),
            entity_ids: BTreeSet::from(["entity:naome-memory".to_owned()]),
            outcome_class: Some(if relevant { "success" } else { "neutral" }.to_owned()),
            goal_key: Some("goal:proof".to_owned()),
            retention_permission: RetentionPermissionV1::SemanticAndExactAllowed,
            artifacts: vec![ArtifactRefV1 {
                digest: artifact_digest,
                size_bytes: u64::try_from(artifact_payload.len()).unwrap_or(u64::MAX),
                media_type: "application/x-naome-rare-event".to_owned(),
                retention_allowed: true,
                inline_payload: Some(artifact_payload),
            }],
            relations: Vec::new(),
        };
        atoms.push(seal_episode(&buffer, &request, &policy)?.body);
        rare_query_ids.push(rare_query_id(dataset_id, world_index, atom_index));
    }
    Ok(SyntheticWorldV1 {
        contract_version: "synthetic-world-v1".to_owned(),
        dataset_id,
        world_id: world_id(dataset_id, world_index),
        world_index,
        seed,
        atoms,
        relevant_topic: "build-proof".to_owned(),
        ordinary_query_id: "ordinary-build-proof-v1".to_owned(),
        rare_query_ids,
    })
}

/// Generate a calibration world for compatibility with early `PoC` callers.
/// Holdout data is never selected implicitly.
///
/// # Errors
///
/// Returns an error if the deterministic world cannot be sealed.
pub fn generate_world(world_index: u64, atom_count: usize) -> Result<SyntheticWorldV1> {
    generate_world_for_dataset(DatasetIdV1::Calibration, world_index, atom_count)
}

#[cfg(test)]
mod tests {
    use super::{DatasetIdV1, generate_world_for_dataset};

    #[test]
    fn world_generation_is_reproducible_and_dataset_separated() -> crate::Result<()> {
        assert_eq!(
            generate_world_for_dataset(DatasetIdV1::Calibration, 7, 20)?,
            generate_world_for_dataset(DatasetIdV1::Calibration, 7, 20)?
        );
        assert_ne!(
            generate_world_for_dataset(DatasetIdV1::Calibration, 7, 20)?,
            generate_world_for_dataset(DatasetIdV1::Holdout, 7, 20)?
        );
        Ok(())
    }
}
