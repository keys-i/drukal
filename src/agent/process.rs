//! Run child processes with cancellation, timeouts and bounded output
//!
//! Child processes receive a filtered environment so unrelated credentials stay private

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

pub const MAX_OUTPUT: usize = 1_000_000;
const PROCESS_POLL: Duration = Duration::from_millis(50);
const SENSITIVE_ENVIRONMENT: &[&str] = &[
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
    "ANTHROPIC_API_KEY",
    "KOELU_GEMINI_API_KEY",
    "KOELU_CEREBRAS_API_KEY",
    "KOELU_XAI_API_KEY",
    "KOELU_GROQ_API_KEY",
    "KOELU_CLOUDFLARE_API_TOKEN",
    "KOELU_CLOUDFLARE_ACCOUNT_ID",
    "KOELU_OPENROUTER_API_KEY",
    "KOELU_LAYA_API_KEY",
    "KOELU_APP_PRIVATE_KEY",
    "KOELU_APP_PRIVATE_KEY_FILE",
    "KOELU_APP_CLIENT_ID",
    "KOELU_APP_ID",
    "KOELU_APP_TOKEN_COMMAND",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_HOST",
    "GH_REPO",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "KOELU_PUSH_TOKEN",
];

#[derive(Debug)]
/// Captured output and exit status from one child process
pub struct ProcessOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug)]
enum StreamMessage {
    Data(Stream, Vec<u8>),
    Done,
    Failed(String),
}

#[derive(Clone, Copy, Debug)]
enum Stream {
    Stdout,
    Stderr,
}

#[allow(clippy::too_many_arguments)]
/// Run an executable directly and stop it on timeout, cancellation or excess output
///
/// The output limit applies to stdout and stderr together
/// A nonzero exit is retained in the result so callers can explain expected failures
pub fn execute<I, S>(
    program: &OsStr,
    arguments: I,
    directory: &Path,
    prompt: &[u8],
    timeout: Duration,
    environment: &BTreeMap<String, String>,
    combine_output: bool,
    cancel_file: Option<&Path>,
) -> Result<ProcessOutput>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    if timeout.is_zero() || timeout > Duration::from_secs(86_400) {
        bail!("timeout must be between one second and 24 hours");
    }
    if cancellation_requested(cancel_file)? {
        bail!("run cancelled");
    }
    let mut command = Command::new(program);
    command
        .args(arguments)
        .current_dir(directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    scrub_environment(&mut command);
    command.envs(environment);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("could not start {}", program.to_string_lossy()))?;
    let pid = child.id();
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("child input was unavailable"))?;
    let input_data = prompt.to_vec();
    let writer = thread::spawn(move || -> std::io::Result<()> { input.write_all(&input_data) });

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("child output was unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("child error output was unavailable"))?;
    let (sender, receiver) = mpsc::sync_channel(16);
    let readers = [
        read_stream(stdout, Stream::Stdout, sender.clone()),
        read_stream(stderr, Stream::Stderr, sender),
    ];
    let deadline = Instant::now() + timeout;
    let mut out = Vec::with_capacity(16_384);
    let mut err = Vec::with_capacity(4_096);
    let mut done = 0;
    let mut status: Option<ExitStatus> = None;

    let result = loop {
        match cancellation_requested(cancel_file) {
            Ok(true) => {
                terminate_group(pid, &mut child);
                break Err(anyhow!("run cancelled"));
            }
            Ok(false) => {}
            Err(error) => {
                terminate_group(pid, &mut child);
                break Err(error);
            }
        }
        if Instant::now() >= deadline {
            terminate_group(pid, &mut child);
            break Err(anyhow!("command exceeded its timeout"));
        }
        match receiver.recv_timeout(PROCESS_POLL) {
            Ok(StreamMessage::Data(stream, bytes)) => {
                if out.len() + err.len() + bytes.len() > MAX_OUTPUT {
                    terminate_group(pid, &mut child);
                    break Err(anyhow!("command produced too much output (limit 1 MB)"));
                }
                if combine_output || matches!(stream, Stream::Stdout) {
                    out.extend_from_slice(&bytes);
                } else {
                    err.extend_from_slice(&bytes);
                }
            }
            Ok(StreamMessage::Done) => done += 1,
            Ok(StreamMessage::Failed(message)) => {
                terminate_group(pid, &mut child);
                break Err(anyhow!(message));
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) if done < 2 => {
                terminate_group(pid, &mut child);
                break Err(anyhow!("command output readers stopped unexpectedly"));
            }
            Err(RecvTimeoutError::Disconnected) => {}
        }
        if status.is_none() {
            status = child.try_wait().context("could not query command status")?;
        }
        if status.is_some() && done == 2 {
            break Ok(ProcessOutput {
                code: status.and_then(|value| value.code()).unwrap_or(-1),
                stdout: decode_output(out),
                stderr: decode_output(err),
            });
        }
    };

    drop(receiver);
    if status.is_none() {
        let _ = child.wait();
    }
    let _ = writer.join();
    for reader in readers {
        let _ = reader.join();
    }
    result
}

fn decode_output(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}

fn cancellation_requested(path: Option<&Path>) -> Result<bool> {
    path.map(Path::try_exists)
        .transpose()
        .context("could not read cancellation state")
        .map(Option::unwrap_or_default)
}

fn read_stream(
    mut stream: impl Read + Send + 'static,
    kind: Stream,
    sender: mpsc::SyncSender<StreamMessage>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0_u8; 32_768];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => {
                    let _ = sender.send(StreamMessage::Done);
                    return;
                }
                Ok(length) => {
                    if sender
                        .send(StreamMessage::Data(kind, buffer[..length].to_vec()))
                        .is_err()
                    {
                        return;
                    }
                }
                Err(error) => {
                    let _ = sender.send(StreamMessage::Failed(error.to_string()));
                    return;
                }
            }
        }
    })
}

fn terminate_group(pid: u32, child: &mut std::process::Child) {
    #[cfg(unix)]
    if let Ok(pid) = i32::try_from(pid) {
        let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn scrub_environment(command: &mut Command) {
    for (name, _) in env::vars_os() {
        if sensitive_environment_name(&name.to_string_lossy()) {
            command.env_remove(name);
        }
    }
}

#[must_use]
pub fn safe_environment() -> BTreeMap<String, String> {
    env::vars()
        .filter(|(name, _)| !sensitive_environment_name(name))
        .collect()
}

fn sensitive_environment_name(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    SENSITIVE_ENVIRONMENT.contains(&name.as_str())
        || name.ends_with("_API_KEY")
        || name.ends_with("_TOKEN")
        || name.ends_with("_SECRET")
        || name.ends_with("_PASSWORD")
        || name.ends_with("_PRIVATE_KEY")
        || name.ends_with("_CREDENTIAL")
        || name.ends_with("_CREDENTIALS")
        || name.ends_with("_COOKIE")
        || name.starts_with("AWS_")
        || name.starts_with("AZURE_")
        || name.starts_with("GCP_")
        || name.starts_with("GOOGLE_")
        || matches!(
            name.as_str(),
            "API_KEY"
                | "TOKEN"
                | "SECRET"
                | "PASSWORD"
                | "PRIVATE_KEY"
                | "SSH_AUTH_SOCK"
                | "GIT_ASKPASS"
                | "GIT_SSH_COMMAND"
                | "DOCKER_CONFIG"
                | "NETRC"
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn output_decode_preserves_valid_and_replaces_malformed_utf8() {
        for (bytes, expected) in [
            (b"ready\n".as_slice(), "ready\n"),
            (&[b'r', 0x80, b'y'], "r\u{fffd}y"),
        ] {
            assert_eq!(decode_output(bytes.to_vec()), expected);
        }
    }

    #[test]
    fn process_preserves_input_streams_and_exit_status() -> Result<()> {
        let output = execute(
            OsStr::new("sh"),
            ["-c", "cat; printf err >&2; exit 7"],
            Path::new("."),
            b"input",
            Duration::from_secs(2),
            &BTreeMap::new(),
            false,
            None,
        )?;
        assert_eq!(output.code, 7);
        assert_eq!(output.stdout, "input");
        assert_eq!(output.stderr, "err");
        Ok(())
    }

    #[test]
    fn cancelled_process_never_starts() -> Result<()> {
        let directory = tempdir()?;
        let cancelled = directory.path().join("cancelled");
        let sentinel = directory.path().join("started");
        std::fs::write(&cancelled, b"cancelled")?;
        let error = execute(
            OsStr::new("sh"),
            [
                OsStr::new("-c"),
                OsStr::new("touch \"$1\""),
                OsStr::new("test"),
                sentinel.as_os_str(),
            ],
            directory.path(),
            b"",
            Duration::from_secs(2),
            &BTreeMap::new(),
            false,
            Some(&cancelled),
        )
        .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        assert!(!sentinel.exists());
        Ok(())
    }

    #[test]
    fn active_process_stops_when_cancellation_appears() -> Result<()> {
        let directory = tempdir()?;
        let cancelled = directory.path().join("cancelled");
        let error = execute(
            OsStr::new("sh"),
            [
                OsStr::new("-c"),
                OsStr::new("printf cancelled > \"$1\"; while :; do :; done"),
                OsStr::new("test"),
                cancelled.as_os_str(),
            ],
            directory.path(),
            b"",
            Duration::from_secs(2),
            &BTreeMap::new(),
            false,
            Some(&cancelled),
        )
        .unwrap_err();
        assert!(cancelled.exists());
        assert!(error.to_string().contains("cancelled"));
        Ok(())
    }

    #[test]
    fn stalled_process_exceeds_its_timeout() {
        let error = execute(
            OsStr::new("sh"),
            ["-c", "while :; do :; done"],
            Path::new("."),
            b"",
            Duration::from_millis(100),
            &BTreeMap::new(),
            false,
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("timeout"));
    }

    #[test]
    fn combined_output_accepts_the_limit_and_rejects_overflow() -> Result<()> {
        let output = execute(
            OsStr::new("sh"),
            ["-c", &format!("head -c {MAX_OUTPUT} /dev/zero")],
            Path::new("."),
            b"",
            Duration::from_secs(2),
            &BTreeMap::new(),
            false,
            None,
        )?;
        assert_eq!(output.code, 0);
        assert_eq!(output.stdout.len(), MAX_OUTPUT);
        assert!(output.stderr.is_empty());
        for command in [
            format!("head -c {} /dev/zero", MAX_OUTPUT + 1),
            format!("head -c {} /dev/zero >&2", MAX_OUTPUT + 1),
            format!(
                "head -c {} /dev/zero; head -c {} /dev/zero >&2",
                MAX_OUTPUT / 2,
                MAX_OUTPUT / 2 + 1
            ),
        ] {
            let error = execute(
                OsStr::new("sh"),
                ["-c", &command],
                Path::new("."),
                b"",
                Duration::from_secs(2),
                &BTreeMap::new(),
                false,
                None,
            )
            .unwrap_err();
            assert!(error.to_string().contains("too much output"), "{command}");
        }
        Ok(())
    }

    #[test]
    fn unsafe_environment_values_are_removed() {
        let values = safe_environment();
        for name in SENSITIVE_ENVIRONMENT {
            assert!(!values.contains_key(*name));
        }
        for name in [
            "ACME_API_KEY",
            "KOELU_LAYA_API_KEY",
            "KOELU_APP_ID",
            "KOELU_APP_TOKEN_COMMAND",
            "CLOUD_TOKEN",
            "AWS_PROFILE",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "SSH_AUTH_SOCK",
            "DOCKER_CONFIG",
        ] {
            assert!(sensitive_environment_name(name), "{name}");
        }
        for name in ["HOME", "PATH", "CARGO_HOME", "TOKENIZERS_PARALLELISM"] {
            assert!(!sensitive_environment_name(name), "{name}");
        }
    }
}
