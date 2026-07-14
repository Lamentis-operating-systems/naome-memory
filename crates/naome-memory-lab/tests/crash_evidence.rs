#![forbid(unsafe_code)]

use std::error::Error;
use std::process::Command;

use naome_memory_core::Digest32;
use naome_memory_lab::{
    ClaimStatusV1, CrashCampaignEvidenceV1, ProofProfileV1, run_proof_with_crash_evidence,
    validate_crash_campaign_evidence,
};
use naome_memory_sqlite::RepositoryFaultPointV1;
use serde_json::Value;

#[path = "support/schema.rs"]
mod schema_support;

#[test]
fn generated_child_process_campaign_closes_atomicity_evidence() -> Result<(), Box<dyn Error>> {
    let executable = env!("CARGO_BIN_EXE_naome-memory-crash-harness");
    let output = Command::new(executable).output()?;
    assert!(
        output.status.success(),
        "crash harness failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let evidence: CrashCampaignEvidenceV1 = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        validate_crash_campaign_evidence(&evidence)?,
        evidence.campaign_digest
    );
    assert_eq!(
        evidence
            .scenarios
            .iter()
            .map(|scenario| scenario.fault_point.as_str())
            .collect::<Vec<_>>(),
        RepositoryFaultPointV1::ORDERED
            .iter()
            .map(|point| point.identifier())
            .collect::<Vec<_>>()
    );

    let replay_output = Command::new(executable).output()?;
    assert!(
        replay_output.status.success(),
        "fresh crash campaign failed: {}",
        String::from_utf8_lossy(&replay_output.stderr)
    );
    let replayed_evidence: CrashCampaignEvidenceV1 = serde_json::from_slice(&replay_output.stdout)?;
    assert_eq!(
        replayed_evidence, evidence,
        "a fresh child-process campaign must reproduce the complete typed evidence"
    );

    let mut stale_repository = evidence.clone();
    stale_repository.repository_source_digest = Digest32::ZERO;
    stale_repository.campaign_digest = stale_repository.recompute_campaign_digest()?;
    assert!(validate_crash_campaign_evidence(&stale_repository).is_err());

    let mut reordered = evidence.clone();
    reordered.scenarios.swap(0, 1);
    reordered.campaign_digest = reordered.recompute_campaign_digest()?;
    assert!(validate_crash_campaign_evidence(&reordered).is_err());

    let mut negative = evidence.clone();
    negative.scenarios[0].complete_pre_state_observed = false;
    negative.scenarios[0].result_digest = negative.scenarios[0].recompute_result_digest()?;
    negative.campaign_digest = negative.recompute_campaign_digest()?;
    assert!(validate_crash_campaign_evidence(&negative).is_err());

    let bundle = run_proof_with_crash_evidence(ProofProfileV1::Pr, Some(evidence))?;
    assert_eq!(
        bundle
            .receipt
            .invariant_statuses
            .get("apply_atomic_and_idempotent"),
        Some(&ClaimStatusV1::Verified)
    );
    assert_eq!(
        bundle.receipt.crash_campaign_digest,
        bundle
            .decision_ledger
            .crash_campaign_evidence
            .as_ref()
            .map(|campaign| campaign.campaign_digest)
    );
    validate_projection(
        &serde_json::to_value(&bundle.receipt)?,
        "lab-proof-receipt-v1.schema.json",
    )?;
    validate_projection(
        &serde_json::to_value(&bundle.decision_ledger)?,
        "decision-ledger-v1.schema.json",
    )?;
    Ok(())
}

fn validate_projection(instance: &Value, schema_name: &str) -> Result<(), Box<dyn Error>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../schemas")
        .join(schema_name);
    let schema: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    schema_support::validate(instance, &schema).map_err(std::io::Error::other)?;
    Ok(())
}
