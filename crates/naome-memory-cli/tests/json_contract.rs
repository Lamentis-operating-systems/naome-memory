#![forbid(unsafe_code)]

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn decode_single_object(output: &[u8]) -> Result<Value, Box<dyn std::error::Error>> {
    let value: Value = serde_json::from_slice(output)?;
    if !value.is_object() {
        return Err("CLI stdout was not one JSON object".into());
    }
    Ok(value)
}

#[test]
fn json_help_is_exactly_one_schema_discriminated_object() -> Result<(), Box<dyn std::error::Error>>
{
    let output = Command::new(env!("CARGO_BIN_EXE_naome-memory"))
        .args(["--json", "--help"])
        .output()?;
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let value = decode_single_object(&output.stdout)?;
    let object = value
        .as_object()
        .ok_or("CLI stdout was not one JSON object")?;
    assert_eq!(object.len(), 4);
    assert_eq!(value["contract_version"], "naome-memory.cli-response.v1");
    assert_eq!(value["status"], "help");
    assert_eq!(value["exit_code"], 0);
    assert!(
        value["output"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
    );
    Ok(())
}

#[test]
fn json_parse_error_is_exactly_one_object_and_stderr_stays_empty()
-> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_naome-memory"))
        .args(["--json", "not-a-command"])
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    let value = decode_single_object(&output.stdout)?;
    assert_eq!(value["contract_version"], "naome-memory.cli-response.v1");
    assert_eq!(value["status"], "error");
    assert_eq!(value["exit_code"], 2);
    assert!(value["error"].as_str().is_some_and(|text| !text.is_empty()));
    Ok(())
}

#[test]
fn json_success_is_exactly_one_object_and_stderr_stays_empty()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("memory.db");
    let artifacts = directory.path().join("artifacts");
    let policy = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../policies/poc-v1.toml");
    let output = Command::new(env!("CARGO_BIN_EXE_naome-memory"))
        .args(["--json", "--policy"])
        .arg(policy)
        .args(["db", "init", "--database"])
        .arg(database)
        .arg("--artifacts")
        .arg(artifacts)
        .args(["--as-of-us", "1"])
        .output()?;
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let value = decode_single_object(&output.stdout)?;
    assert_eq!(value["contract_version"], "naome-memory.cli-response.v1");
    assert_eq!(value["command"], "db.init");
    assert_eq!(value["status"], "succeeded");
    assert_eq!(value["result"]["policy_id"], "poc-v1");
    Ok(())
}
