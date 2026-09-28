//! Exercise workflow script selection using local fixtures

use std::fs;
use std::process::Command;

const SOLVE: &str = include_str!("../.github/workflows/solve.yml");

fn run_block(step: &str) -> String {
    let section = SOLVE
        .split_once(&format!("      - name: {step}\n"))
        .unwrap()
        .1;
    let run = section.split_once("        run: |\n").unwrap().1;
    run.lines()
        .take_while(|line| line.starts_with("          "))
        .map(|line| line.strip_prefix("          ").unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn pinned_solver_scripts_support_new_and_legacy_names() -> koelu::Result<()> {
    for (step, directory, new, legacy) in [
        (
            "Validate pull request trust boundary",
            "koelu-workflow-scripts/.github/workflows/scripts",
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
        let source = run_block(step);
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
