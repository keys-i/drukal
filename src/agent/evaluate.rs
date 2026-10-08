//! Collect structured agent replies without accepting partial or oversized results

use std::borrow::Cow;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use tempfile::tempdir;

use super::harness::{command, command_usage, run_cancellable};
use super::{Harness, ProcessOutput, Usage};

const MAX_STRUCTURED_RESPONSE: usize = 64_000;

#[allow(clippy::too_many_arguments)]
/// Ask for one JSON object and reject missing, partial or oversized replies
pub fn evaluate(
    prompt: &str,
    schema: &Value,
    directory: &Path,
    instructions: &str,
    model: Option<&str>,
    evidence_only: bool,
    harness: Harness,
    timeout: Duration,
    usage: Option<&mut Usage>,
) -> Result<Value> {
    evaluate_cancellable(
        prompt,
        schema,
        directory,
        instructions,
        model,
        evidence_only,
        harness,
        timeout,
        usage,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
/// Collect a structured reply while honoring the caller’s cancellation file
///
/// Native review tools get read permissions and cannot load additional MCP servers
pub fn evaluate_cancellable(
    prompt: &str,
    schema: &Value,
    directory: &Path,
    instructions: &str,
    model: Option<&str>,
    evidence_only: bool,
    harness: Harness,
    timeout: Duration,
    usage: Option<&mut Usage>,
    cancel_file: Option<&Path>,
) -> Result<Value> {
    let mut command = command(
        directory,
        instructions,
        model,
        1,
        true,
        harness,
        evidence_only,
        None,
    )?;
    let scratch = tempdir().context("could not create response workspace")?;
    let schema_path = scratch.path().join("schema.json");
    let output_path = scratch.path().join("result.json");
    fs::write(&schema_path, serde_json::to_vec(schema)?)?;
    let mut adjusted_prompt = Cow::Borrowed(prompt);
    if harness == Harness::Codex {
        command.arguments.extend(
            [
                "--ignore-user-config",
                "-c",
                "web_search=\"disabled\"",
                "-c",
                "mcp_servers={}",
            ]
            .into_iter()
            .map(std::ffi::OsString::from),
        );
        if evidence_only {
            command.arguments.extend(
                ["--skip-git-repo-check", "-c", "features.shell_tool=false"]
                    .into_iter()
                    .map(std::ffi::OsString::from),
            );
        }
        command.arguments.extend([
            "--output-schema".into(),
            schema_path.into_os_string(),
            "--output-last-message".into(),
            output_path.clone().into_os_string(),
            "-".into(),
        ]);
    } else if harness == Harness::Claude {
        command.arguments.extend(
            [
                "--output-format",
                "json",
                "--json-schema",
                &serde_json::to_string(schema)?,
            ]
            .into_iter()
            .map(std::ffi::OsString::from),
        );
    } else {
        adjusted_prompt = Cow::Owned(format!(
            "{instructions}\nReturn only JSON matching this schema:\n{}\n{prompt}",
            serde_json::to_string(schema)?
        ));
    }
    let mut environment = super::harness::model_environment(model, harness)?;
    if harness == Harness::Command {
        environment.insert(
            "DRUKAL_MODEL".to_owned(),
            model.unwrap_or_default().to_owned(),
        );
        environment.insert("DRUKAL_READ_ONLY".to_owned(), "1".to_owned());
    }
    let result = run_cancellable(
        command,
        &adjusted_prompt,
        directory,
        harness,
        timeout,
        &environment,
        usage,
        cancel_file,
    )?;
    if result.code != 0 {
        bail!(
            "{} stopped before completing its review; nothing was published",
            harness.as_str()
        );
    }
    let text = if harness == Harness::Codex {
        read_structured_response(&output_path)?
    } else {
        result.stdout
    };
    if text.len() > MAX_STRUCTURED_RESPONSE {
        bail!("agent response is too large; nothing was published");
    }
    let mut response: Value = serde_json::from_str(&text)
        .context("agent returned an invalid or missing review; nothing was published")?;
    if harness == Harness::Claude {
        if response.get("is_error").and_then(Value::as_bool) == Some(true)
            || response.get("subtype").and_then(Value::as_str) != Some("success")
        {
            bail!("Claude returned an incomplete result; nothing was published");
        }
        response = response
            .get("structured_output")
            .cloned()
            .ok_or_else(|| anyhow!("Claude returned no structured result"))?;
    }
    if !response.is_object() {
        bail!("agent response must be a JSON object");
    }
    Ok(response)
}

/// Extract the last human answer from the harness’s output format
pub fn worker_message(output: &ProcessOutput, harness: Harness) -> Result<String> {
    if harness == Harness::Claude {
        let value: Value = serde_json::from_str(&output.stdout)?;
        return Ok(value
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned());
    }
    if harness == Harness::Command {
        let (result, _) = command_usage(&output.stdout)?;
        return Ok(match result {
            Value::String(value) => value,
            _ => output.stdout.clone(),
        });
    }
    for line in output.stdout.lines().rev() {
        let event: Value = serde_json::from_str(line).context("Codex emitted invalid JSONL")?;
        if let Some(text) = event
            .get("item")
            .and_then(Value::as_object)
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("agent_message"))
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
        {
            return Ok(text.to_owned());
        }
    }
    Ok(String::new())
}

fn read_structured_response(path: &Path) -> Result<String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .context("agent returned an invalid or missing review; nothing was published")?
        .take((MAX_STRUCTURED_RESPONSE + 1) as u64)
        .read_to_end(&mut bytes)
        .context("agent returned an invalid or missing review; nothing was published")?;
    if bytes.len() > MAX_STRUCTURED_RESPONSE {
        bail!("agent response is too large; nothing was published");
    }
    String::from_utf8(bytes)
        .context("agent returned an invalid or missing review; nothing was published")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_response_file_has_a_fixed_limit() -> Result<()> {
        let temporary = tempdir()?;
        for (name, length, accepted) in [
            ("at-limit", MAX_STRUCTURED_RESPONSE, true),
            ("over-limit", MAX_STRUCTURED_RESPONSE + 1, false),
        ] {
            let path = temporary.path().join(name);
            fs::write(&path, vec![b'x'; length])?;
            assert_eq!(read_structured_response(&path).is_ok(), accepted, "{name}");
        }
        Ok(())
    }

    #[test]
    fn missing_or_non_utf8_responses_are_rejected() -> Result<()> {
        let directory = tempdir()?;
        let path = directory.path().join("reply");
        assert!(read_structured_response(&path).is_err());
        fs::write(&path, [0xff])?;
        assert!(read_structured_response(&path).is_err());
        Ok(())
    }
}
