use super::setup::{accept_terms, setup_consent_preview};
use super::*;
use crate::setup;
use anyhow::anyhow;
use serde_json::Value;

#[test]
fn cli_matrix_covers_human_commands_and_help_without_help_subcommands() {
    for arguments in [
        vec!["drukal", "--help"],
        vec!["drukal", "setup", "--help"],
        vec!["drukal", "code", "--help"],
        vec!["drukal", "dependasolve", "--help"],
        vec!["drukal", "agent", "--help"],
        vec!["drukal", "agent", "run", "--help"],
        vec!["drukal", "agent", "doctor", "--help"],
        vec!["drukal", "runs", "--help"],
        vec!["drukal", "inspect", "--help"],
        vec!["drukal", "cancel", "--help"],
        vec!["drukal", "resume", "--help"],
        vec!["drukal", "apply", "--help"],
        vec![
            "drukal", "--theme", "tide", "--output", "json", "agent", "doctor", "--help",
        ],
    ] {
        let result = Cli::try_parse_from(arguments);
        assert!(result.is_err_and(|error| error.exit_code() == 0));
    }

    for arguments in [vec!["drukal", "help"], vec!["drukal", "agent", "help"]] {
        let result = Cli::try_parse_from(arguments);
        assert!(result.is_err_and(|error| error.exit_code() != 0));
    }

    for arguments in [vec!["drukal", "--help"], vec!["drukal", "agent", "--help"]] {
        let help = Cli::try_parse_from(arguments)
            .expect_err("help exits after rendering")
            .to_string();
        assert!(
            !help
                .lines()
                .any(|line| line.trim_start().starts_with("help")),
            "help must not be a generated subcommand: {help}"
        );
    }

    let help = Cli::try_parse_from(["drukal", "setup", "--help"])
        .expect_err("help exits after rendering")
        .to_string();
    assert!(
        !help.contains("app-client-id"),
        "setup keeps App keys central"
    );
    assert!(
        !help.contains("private-key") && !help.contains("gemini") && !help.contains("cerebras"),
        "setup must not ask target repositories for provider credentials"
    );
}

#[test]
fn parser_accepts_agent_hierarchy_and_dependasolve_check_aliases() {
    for arguments in [
        vec![
            "drukal",
            "dependasolve",
            "--repo",
            "owner/repo",
            "--check",
            "test",
        ],
        vec!["drukal", "setup", "--repo", "owner/repo", "--check", "test"],
        vec![
            "drukal",
            "dependasolve",
            "--repo",
            "owner/repo",
            "--checks",
            "test",
            "lint",
        ],
        vec!["drukal", "agent", "run", "--", "exec", "--help"],
        vec!["drukal", "agent", "doctor"],
        vec!["drukal", "agent", "ask", "What changed?"],
        vec![
            "drukal",
            "agent",
            "follow-up",
            "run_0123456789abcdef0123456789abcdef",
            "Why?",
        ],
        vec![
            "drukal",
            "agent",
            "serve",
            "--owner",
            "keys-i",
            "--app-client-id",
            "Iv1.abc",
            "--app-private-key-file",
            "/secure/drukal.pem",
            "--once",
        ],
        vec![
            "drukal",
            "agent",
            "resolve",
            "--repo",
            "owner/repo",
            "--pr",
            "1",
        ],
        vec![
            "drukal",
            "agent",
            "review",
            "--repo",
            "owner/repo",
            "--pr",
            "1",
        ],
        vec![
            "drukal",
            "agent",
            "respond",
            "--repo",
            "owner/repo",
            "--issue",
            "1",
            "--comment",
            "2",
        ],
        vec![
            "drukal",
            "code",
            "fix it",
            "--check",
            "test",
            "--pr",
            "--repo",
            "owner/repo",
            "--mcp",
            "docs",
            "--ghost",
        ],
    ] {
        Cli::try_parse_from(arguments).expect("command must parse");
    }

    let cli = Cli::try_parse_from([
        "drukal",
        "dependasolve",
        "--repo",
        "owner/repo",
        "--check",
        "test",
        "--no-overwrite",
    ])
    .expect("solver-ref must be optional");
    let Commands::Dependasolve(arguments) = cli.command else {
        panic!("dependasolve command expected");
    };
    assert!(arguments.solver_ref.is_none());
    assert!(arguments.no_overwrite);

    for command in ["doctor", "resolve", "review"] {
        let result = Cli::try_parse_from(["drukal", command, "--repo", "owner/repo", "--pr", "1"]);
        assert!(result.is_err(), "{command} must live under drukal agent");
    }
    let result = Cli::try_parse_from(["drukal", "agent", "sweep", "--owner", "keys-i"]);
    assert!(result.is_err(), "sweep is not a public agent command");
}

#[test]
fn setup_requires_explicit_consent_for_json_output() {
    let error = accept_terms(
        "owner/repo",
        &["test".to_owned()],
        Theme::Plain,
        OutputMode::Json,
        false,
        false,
    )
    .expect_err("JSON setup cannot prompt");
    let message = error.to_string();
    assert!(message.contains(setup::TERMS_URL));
    assert!(message.contains(setup::PRIVACY_URL));
    assert!(message.contains("--accept-terms"));
    assert!(
        accept_terms(
            "owner/repo",
            &["test".to_owned()],
            Theme::Plain,
            OutputMode::Json,
            true,
            false,
        )
        .unwrap()
    );

    let preview = setup_consent_preview("owner/repo", &["test".to_owned()], false);
    for expected in [
        "owner/repo",
        "`test` as CI evidence",
        ".github/drukal.toml",
        "closed issue",
        "keys-i/drukal",
        setup::TERMS_URL,
        setup::PRIVACY_URL,
    ] {
        assert!(preview.contains(expected), "{expected}: {preview}");
    }
}

#[test]
fn json_failures_share_one_bounded_contract() {
    for (arguments, kind, expected) in [
        (
            vec![
                "drukal",
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
            "runtime",
            "--solver-ref requires",
        ),
        (
            vec!["drukal", "dependasolve", "--output=json"],
            "usage",
            "required arguments",
        ),
    ] {
        let error = run_from(arguments).expect_err("invalid input must fail");
        let failure = error
            .downcast_ref::<CliFailure>()
            .expect("JSON failure must retain its output mode");
        assert_eq!(failure.output, OutputMode::Json);
        let document: Value =
            serde_json::from_str(&json_error_document(failure.kind, &failure.message))
                .expect("failure must be valid JSON");
        assert_eq!(document["schema"], 1);
        assert_eq!(document["status"], "error");
        assert_eq!(document["error"]["kind"], kind);
        assert!(
            document["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(expected))
        );
    }
    let long = anyhow!("{}", "x".repeat(MAX_ERROR_CHARACTERS + 1));
    let failure = CliFailure::new(OutputMode::Json, &long);
    assert!(failure.message.ends_with("[Error truncated]"));
    assert!(
        failure.message.chars().count()
            <= MAX_ERROR_CHARACTERS + "\n[Error truncated]".chars().count()
    );
}
