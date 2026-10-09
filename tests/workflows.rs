//! Exercise workflow script selection using local fixtures

use std::fs;
use std::process::Command;

const SOLVE: &str = include_str!("../.github/workflows/solve.yml");
const ORCHESTRATE: &str = include_str!("../.github/workflows/orchestrate.yml");
const CREDENTIALS: &str = include_str!("../.github/workflows/credentials.yml");

fn run_block(workflow: &str, step: &str) -> String {
    let section = workflow
        .split_once(&format!("      - name: {step}\n"))
        .unwrap()
        .1;
    let run = section.split_once("        run: |\n").unwrap().1;
    run.lines()
        .take_while(|line| line.is_empty() || line.starts_with("          "))
        .map(|line| line.strip_prefix("          ").unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn credential_checks_reject_failed_auth_without_disclosing_responses() -> drukal::Result<()> {
    let source = run_block(CREDENTIALS, "Check provider credentials");
    let python = source
        .strip_prefix("python3 - <<'PY'\n")
        .unwrap()
        .strip_suffix("\nPY")
        .unwrap();
    let script = format!(
        r#"import json
from unittest.mock import patch
namespace = {{"__name__": "credential_test"}}
exec({}, namespace)
check = namespace["check"]
with patch("http.client.HTTPSConnection") as connection:
    response = connection.return_value.getresponse.return_value
    for host, status, body, expected in [
        ("api.groq.com", 200, {{"data": []}}, "valid"),
        ("api.groq.com", 401, {{"error": "private response"}}, "HTTP 401"),
        ("api.groq.com", 200, {{"error": "private response"}}, "invalid response"),
        ("api.cloudflare.com", 200, {{"success": True, "result": {{"status": "active"}}}}, "valid"),
        ("api.cloudflare.com", 200, {{"success": True, "result": {{"status": "expired"}}}}, "inactive token"),
        ("api.cloudflare.com", 200, {{"success": False, "result": None}}, "invalid response"),
    ]:
        response.status = status
        response.read.return_value = json.dumps(body).encode()
        assert check(host, "/models", "private-key") == expected
        assert connection.return_value.close.called
    connection.return_value.request.side_effect = OSError("private response")
    assert check("api.groq.com", "/models", "private-key") == "network error"
assert check("api.groq.com", "/models", "") == "missing"
assert check("api.groq.com", "/models", "private-key\r\ninjected") == "invalid key format"
"#,
        serde_json::to_string(python)?
    );
    let output = Command::new("python3").args(["-c", &script]).output()?;
    assert!(output.status.success(), "credential self-check failed");
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    Ok(())
}

#[test]
fn pinned_solver_scripts_support_new_and_legacy_names() -> drukal::Result<()> {
    for (step, directory, new, legacy) in [
        (
            "Validate pull request trust boundary",
            "drukal-workflow-scripts/.github/workflows/scripts",
            "trust.sh",
            "pr-trust.sh",
        ),
        (
            "Fetch current pull request state",
            ".github/workflows/scripts",
            "state.sh",
            "pr-state.sh",
        ),
    ] {
        let source = run_block(SOLVE, step);
        for (modern, old, expected) in [
            (true, false, Some("new")),
            (false, true, Some("legacy")),
            (true, true, Some("new")),
            (false, false, None),
        ] {
            let root = tempfile::tempdir()?;
            let scripts = root.path().join(directory);
            fs::create_dir_all(&scripts)?;
            for (exists, name, message) in [(modern, new, "new"), (old, legacy, "legacy")] {
                if exists {
                    fs::write(scripts.join(name), format!("printf '%s' '{message}'\n"))?;
                }
            }
            let output = Command::new("bash")
                .args(["-c", &source])
                .current_dir(root.path())
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .output()?;
            assert_eq!(
                output.status.success(),
                expected.is_some(),
                "{step} modern={modern} legacy={old}"
            );
            if let Some(expected) = expected {
                assert_eq!(String::from_utf8(output.stdout)?, expected);
                assert!(output.stderr.is_empty());
            }
        }
    }
    Ok(())
}

#[test]
fn pr_mentions_select_their_own_review_and_repair_target() -> drukal::Result<()> {
    let root = tempfile::tempdir()?;
    let binary = root.path().join("target/release/drukal");
    fs::create_dir_all(binary.parent().unwrap())?;
    fs::write(
        &binary,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > args\nprintf '%s\\n' '{\"include\":[{\"number\":5}]}'\n",
    )?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))?;
    let source = run_block(ORCHESTRATE, "Find consented pull requests");
    for event in [
        "issue_comment",
        "pull_request_review_comment",
        "pull_request_target",
        "workflow_run",
    ] {
        for number in ["5", "0", "bad"] {
            let output = Command::new("bash")
                .args(["-c", &source])
                .current_dir(root.path())
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .env("GITHUB_REPOSITORY", "owner/repo")
                .env("GITHUB_RUN_NUMBER", "12")
                .env("GITHUB_EVENT_NAME", event)
                .env("DRUKAL_EVENT_PR", number)
                .env("GITHUB_OUTPUT", root.path().join("outputs"))
                .output()?;
            let valid = number == "5";
            // A workflow completion without a PR deliberately produces an empty matrix
            assert_eq!(
                output.status.success(),
                valid || event == "workflow_run",
                "{event} {number}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            if valid {
                assert_eq!(
                    fs::read_to_string(root.path().join("args"))?,
                    "agent\ntargets\n--max-reviews\n1\n--repo\nowner/repo\n--pr\n5\n"
                );
            }
        }
    }
    Ok(())
}
