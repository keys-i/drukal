//! Connect agent commands to review, repair and local execution

use std::env;
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use serde_json::json;

use crate::Result;
use crate::agent::{self, Harness};
use crate::delivery::quality::{AcceptanceCheck, Plan, Task};
use crate::delivery::{self, Config, DeliveryAuth};
use crate::github::GitHub;
use crate::reviews;
use crate::reviews::repair;
use crate::ui::{OutputMode, Theme, Ui};

use super::{
    AgentArgs, AutoRepairArgs, DoctorArgs, PrepareRepairArgs, ResolveArgs, RespondArgs, ReviewArgs,
};

pub(super) fn doctor(arguments: DoctorArgs, theme: Theme, output: OutputMode) -> Result<()> {
    let mut ui = Ui::new(theme, output, 3);
    ui.title("Koelu doctor", "Check what's ready on this machine");
    let mut rows = Vec::new();
    let mut ready = true;
    for name in ["git", "gh"] {
        let installed = agent::which(name).is_some();
        ready &= installed;
        ui.stage(&format!(
            "{name}: {}",
            if installed { "installed" } else { "missing" }
        ));
        rows.push(json!({"requirement": name, "ready": installed}));
    }
    let harness = match agent::executable(arguments.harness) {
        Ok(_) => true,
        Err(error) => {
            ui.warning(&error.to_string());
            false
        }
    };
    ready &= harness;
    ui.stage(&format!(
        "{}: {}",
        arguments.harness.as_str(),
        if harness { "ready" } else { "needs attention" }
    ));
    rows.push(json!({"requirement": arguments.harness.as_str(), "ready": harness}));
    if output == OutputMode::Json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"ready": ready, "requirements": rows}))?
        );
    } else if ready {
        ui.success("This machine is ready for local coding");
        ui.note("Opening a pull request also needs push access and fixed acceptance checks");
    }
    if ready {
        Ok(())
    } else {
        bail!("one or more readiness checks failed")
    }
}

pub(super) fn native_agent(arguments: AgentArgs) -> Result<()> {
    let (program, prefix) = if arguments.harness == Harness::Command {
        let configured =
            agent::split_command(&env::var("KOELU_AGENT_COMMAND").unwrap_or_default())?;
        let (program, prefix) = configured
            .split_first()
            .ok_or_else(|| anyhow!("set KOELU_AGENT_COMMAND"))?;
        (
            program.clone(),
            prefix.iter().map(OsString::from).collect::<Vec<_>>(),
        )
    } else {
        (arguments.harness.as_str().to_owned(), Vec::new())
    };
    let binary = agent::which(&program).ok_or_else(|| anyhow!("install the selected harness"))?;
    let mut command = Command::new(binary);
    command
        .env_clear()
        .envs(agent::safe_environment())
        .args(prefix)
        .args(arguments.arguments);
    let status = command
        .status()
        .context("could not run native harness command")?;
    if status.success() {
        Ok(())
    } else {
        bail!("native harness exited with {}", status.code().unwrap_or(1))
    }
}

pub(super) fn resolve(arguments: ResolveArgs) -> Result<()> {
    if arguments.pr == 0 {
        bail!("PR number must be positive");
    }
    let github = GitHub::new(&arguments.repo, &env::var("GH_TOKEN").unwrap_or_default())?;
    let (pull, dependency, metadata) = reviews::resolve(&github, arguments.pr)?;
    action_output(&[
        ("dependency", dependency.to_string()),
        (
            "head",
            pull["head"]["sha"].as_str().unwrap_or_default().to_owned(),
        ),
        (
            "update_type",
            metadata
                .as_ref()
                .map(|value| value.update_type.clone())
                .unwrap_or_default(),
        ),
        (
            "maintainer_changes",
            metadata.map_or_else(|| "unknown".to_owned(), |value| value.maintainer_changes),
        ),
    ])
}

pub(super) fn review(arguments: ReviewArgs) -> Result<()> {
    if arguments.pr == 0 {
        bail!("PR number must be positive");
    }
    let github = GitHub::new(&arguments.repo, &env::var("GH_TOKEN").unwrap_or_default())?;
    let required: Vec<String> = serde_json::from_str(&env::var("REQUIRED_CHECKS")?)?;
    let outcome = reviews::review_pr(
        &github,
        arguments.pr,
        &required,
        env::var("KOELU_MODEL")
            .ok()
            .filter(|value| !value.is_empty())
            .as_deref(),
        &env::var("APP_SLUG").unwrap_or_default(),
        arguments.harness,
        match env::var("KOELU_REPOSITORY_PRIVATE").ok().as_deref() {
            Some("true") => Some(true),
            Some("false") => Some(false),
            _ => None,
        },
        &env::var("UPDATE_TYPE").unwrap_or_default(),
        &env::var("MAINTAINER_CHANGES").unwrap_or_default(),
        &env::var("EXPECTED_HEAD").unwrap_or_default(),
        Duration::from_secs(180),
    )?;
    action_output(&[("approved", outcome.approved.to_string())])
}

pub(super) fn respond(arguments: RespondArgs) -> Result<()> {
    if arguments.issue == 0 || arguments.comment == 0 {
        bail!("issue and comment numbers must be positive");
    }
    let github = GitHub::new(&arguments.repo, &env::var("GH_TOKEN").unwrap_or_default())?;
    crate::mentions::respond(
        &github,
        arguments.issue,
        arguments.comment,
        env::var("KOELU_MODEL")
            .ok()
            .filter(|value| !value.is_empty())
            .as_deref(),
        arguments.harness,
    )
}

pub(super) fn prepare_repair(arguments: PrepareRepairArgs) -> Result<()> {
    if arguments.pr == 0 {
        bail!("PR number must be positive");
    }
    let github = GitHub::new(&arguments.repo, &env::var("GH_TOKEN").unwrap_or_default())?;
    repair::prepare(
        &github,
        arguments.pr,
        &arguments.expected_head,
        &arguments.expected_base,
        &arguments.output,
    )
}

pub(super) fn auto_repair(arguments: AutoRepairArgs) -> Result<()> {
    if arguments.pr == 0 {
        bail!("PR number must be positive");
    }
    let token = env::var("GH_TOKEN").unwrap_or_default();
    let github = GitHub::new(&arguments.repo, &token)?;
    let candidate = reviews::autofix::candidate(
        &github,
        arguments.pr,
        &arguments.expected_head,
        &arguments.expected_base,
    )?;
    if arguments.probe {
        return action_output(&[("needed", candidate.is_some().to_string())]);
    }
    let Some(candidate) = candidate else {
        println!("No eligible Dependabot repair is needed");
        return Ok(());
    };
    let directory = arguments
        .directory
        .ok_or_else(|| anyhow!("--directory is required for automatic repair"))?;
    let acceptance = "The Dependabot update is preserved and the Rust checks pass".to_owned();
    let check = "cargo test --all-targets --no-fail-fast --locked".to_owned();
    let model_choices =
        agent::routing::parse_model_choices(&env::var("KOELU_MODEL_CHOICES").unwrap_or_default());
    let initial_model = (!model_choices.is_empty()).then_some(0);
    let plan = Plan {
        acceptance: vec![acceptance],
        scope: candidate.scope.clone(),
        limitations: vec![
            format!(
                "Source: {} PR #{} at {}",
                arguments.repo, arguments.pr, arguments.expected_head
            ),
            "A maintainer reviews and merges the replacement PR".to_owned(),
        ],
        performance_required: false,
        model_index: initial_model,
        tasks: vec![Task {
            description:
                "Repair the verified Dependabot update and its failing test or merge conflict"
                    .to_owned(),
            scope: candidate.scope,
            acceptance: vec![0],
            depends_on: Vec::new(),
            model_index: initial_model,
        }],
    };
    let config = Config {
        task: candidate.task,
        directory,
        repo: Some(arguments.repo.clone()),
        checks: vec![
            "cargo fmt --check".to_owned(),
            "cargo clippy --all-targets --all-features --locked -- -D warnings".to_owned(),
            check.clone(),
            "cargo build --release --locked".to_owned(),
        ],
        harness: Harness::Command,
        agents: 1,
        model: env::var("KOELU_MODEL")
            .ok()
            .filter(|model| !model.is_empty()),
        model_choices,
        review_model: None,
        plan: Some(plan),
        acceptance_checks: vec![AcceptanceCheck {
            criterion: 0,
            command: check,
            expected_exit: 0,
            expected_output: Some(String::new()),
            files: Vec::new(),
        }],
        max_tokens: None,
        orchestrator_model: None,
        orchestrator_harness: None,
        base: Some(candidate.base_ref),
        attempts: 2,
        timeout: Duration::from_secs(30 * 60),
        benchmarks: Vec::new(),
        benchmark_runs: 1,
        benchmark_warmups: 0,
        benchmark_metric: None,
        max_benchmark_noise: 10.0,
        max_regression: 5.0,
        max_files: 20,
        max_lines: 1_000,
        theme: Theme::Plain,
        output: OutputMode::Json,
        seed_patch: None,
        resumed_from: None,
        expected_start: Some(candidate.base_sha),
        mcp_servers: Vec::new(),
        ghost: false,
    };
    let auth = DeliveryAuth::repair_installation(
        &arguments.repo,
        token,
        arguments.pr,
        &arguments.expected_head,
        &arguments.expected_base,
    )?;
    let state = delivery::deliver_hosted(config, auth)?;
    println!("{}", serde_json::to_string(&state)?);
    Ok(())
}

fn action_output(values: &[(&str, String)]) -> Result<()> {
    if let Some(path) = env::var_os("GITHUB_OUTPUT") {
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        for (key, value) in values {
            writeln!(file, "{key}={value}")?;
        }
    } else {
        let value = values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), json!(value)))
            .collect::<serde_json::Map<_, _>>();
        println!("{}", serde_json::to_string(&value)?);
    }
    Ok(())
}
