//! Check CLI output at the process boundary without credentials or network calls

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};

fn invoke(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_drukal"))
        .args(arguments)
        .current_dir(directory)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .output()
        .expect("Drukal must start")
}

#[test]
fn usage_errors_are_one_json_document_on_stderr() -> drukal::Result<()> {
    let directory = tempfile::tempdir()?;
    let output = invoke(directory.path(), &["dependasolve", "--output=json"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let document: Value = serde_json::from_slice(&output.stderr)?;
    assert_eq!(document["schema"], 1);
    assert_eq!(document["status"], "error");
    assert_eq!(document["error"]["kind"], "usage");
    assert!(
        document["error"]["message"]
            .as_str()
            .unwrap()
            .contains("required arguments")
    );
    Ok(())
}

#[test]
fn runtime_errors_are_one_json_document_on_stderr() -> drukal::Result<()> {
    let directory = tempfile::tempdir()?;
    let output = invoke(
        directory.path(),
        &[
            "--output",
            "json",
            "dependasolve",
            "--repo",
            "owner/repo",
            "--solver-ref",
            "invalid",
            "--checks",
            "test",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let document: Value = serde_json::from_slice(&output.stderr)?;
    assert_eq!(document["schema"], 1);
    assert_eq!(document["status"], "error");
    assert_eq!(document["error"]["kind"], "runtime");
    assert!(
        document["error"]["message"]
            .as_str()
            .unwrap()
            .contains("--solver-ref requires")
    );
    assert_eq!(fs::read_dir(directory.path())?.count(), 0);
    Ok(())
}

#[test]
fn setup_preview_lists_toml_and_yaml_without_writing_them() -> drukal::Result<()> {
    let directory = tempfile::tempdir()?;
    let pin = format!("keys-i/drukal@{}", "a".repeat(40));
    let output = invoke(
        directory.path(),
        &[
            "--output",
            "json",
            "dependasolve",
            "--repo",
            "owner/repo",
            "--solver-ref",
            &pin,
            "--checks",
            "test",
            "--checks",
            "test",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let document: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(document["schema"], 1);
    assert_eq!(document["status"], "ok");
    assert_eq!(document["kind"], "dependasolve");
    let result = &document["result"];
    assert_eq!(result["repository"], "owner/repo");
    assert_eq!(result["source"], pin);
    assert_eq!(result["required_checks"], json!(["test"]));
    assert_eq!(result["apply"], false);
    assert_eq!(result["autofix"], false);
    assert_eq!(result["agreement_required"], true);
    let root = directory.path().canonicalize()?;
    assert_eq!(
        result["files"],
        json!([
            root.join(".github/dependabot.yml"),
            root.join(".github/drukal.toml"),
        ])
    );
    assert_eq!(fs::read_dir(directory.path())?.count(), 0);
    Ok(())
}

#[test]
fn setup_preview_preserves_existing_dependabot_config() -> drukal::Result<()> {
    let directory = tempfile::tempdir()?;
    fs::create_dir(directory.path().join(".github"))?;
    let existing = directory.path().join(".github/dependabot.yaml");
    let contents = "version: 2\nupdates: []\n";
    fs::write(&existing, contents)?;
    let pin = format!("keys-i/drukal@{}", "a".repeat(40));
    let output = invoke(
        directory.path(),
        &[
            "dependasolve",
            "--output=json",
            "--repo",
            "owner/repo",
            "--solver-ref",
            &pin,
            "--checks",
            "test",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let document: Value = serde_json::from_slice(&output.stdout)?;
    let expected = directory.path().canonicalize()?.join(".github/drukal.toml");
    assert_eq!(document["result"]["files"], json!([expected]));
    assert_eq!(fs::read_to_string(existing)?, contents);
    assert_eq!(fs::read_dir(directory.path().join(".github"))?.count(), 1);
    Ok(())
}

#[test]
fn setup_preview_refuses_paths_outside_the_selected_directory() -> drukal::Result<()> {
    let directory = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    std::os::unix::fs::symlink(outside.path(), directory.path().join(".github"))?;
    let pin = format!("keys-i/drukal@{}", "a".repeat(40));
    let output = invoke(
        directory.path(),
        &[
            "dependasolve",
            "--output=json",
            "--repo",
            "owner/repo",
            "--solver-ref",
            &pin,
            "--checks",
            "test",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let document: Value = serde_json::from_slice(&output.stderr)?;
    assert_eq!(document["error"]["kind"], "runtime");
    assert!(
        document["error"]["message"]
            .as_str()
            .unwrap()
            .contains("outside --directory")
    );
    assert_eq!(fs::read_dir(outside.path())?.count(), 0);
    Ok(())
}
