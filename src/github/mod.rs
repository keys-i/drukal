//! Read repository state and send requests with the caller’s authorization

use std::collections::BTreeMap;
use std::env;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use regex::Regex;
use serde_json::Value;

use crate::Result;
use crate::agent;

pub mod apps;
mod auth;
pub mod mentions;
pub mod reviews;
pub(crate) mod service;
pub mod setup;

pub(crate) use auth::{
    InstallationTokenScope, authenticated_app, mint_installation_tokens,
    mint_repository_installation_token, public_app,
};

#[derive(Clone, Debug)]
pub struct GitHub {
    repo: String,
    token: String,
}

impl GitHub {
    pub fn new(repo: &str, token: &str) -> Result<Self> {
        validate_repository(repo)?;
        if token.is_empty() {
            bail!("a GitHub App installation token is required");
        }
        Ok(Self {
            repo: repo.to_owned(),
            token: token.to_owned(),
        })
    }

    #[must_use]
    pub fn repo(&self) -> &str {
        &self.repo
    }

    pub fn api(&self, path: &str, payload: Option<&Value>, method: &str) -> Result<Value> {
        let endpoint = format!("repos/{}/{}", self.repo, path);
        api_with_token(&endpoint, payload, method, false, Some(&self.token), None)?
            .ok_or_else(|| anyhow!("GitHub returned no response"))
    }

    pub fn api_optional(
        &self,
        path: &str,
        payload: Option<&Value>,
        method: &str,
    ) -> Result<Option<Value>> {
        let endpoint = format!("repos/{}/{}", self.repo, path);
        api_with_token(&endpoint, payload, method, true, Some(&self.token), None)
    }

    pub fn raw_optional(&self, path: &str) -> Result<Option<String>> {
        let endpoint = format!("repos/{}/{}", self.repo, path);
        let arguments = vec![
            "api".to_owned(),
            "--header".to_owned(),
            "Accept: application/vnd.github.raw+json".to_owned(),
            endpoint.clone(),
        ];
        gh_with_token(
            &arguments,
            None,
            true,
            Some(&self.token),
            None,
            Path::new("."),
        )
        .with_context(|| format!("GitHub API GET {}", safe_endpoint_label(&endpoint)))
    }

    pub fn pages(&self, path: &str, key: Option<&str>) -> Result<Vec<Value>> {
        let mut rows = Vec::new();
        for page in 1..32 {
            let separator = if path.contains('?') { '&' } else { '?' };
            let value = self.api(
                &format!("{path}{separator}per_page=100&page={page}"),
                None,
                "GET",
            )?;
            let batch = page_rows(value, key)?;
            let last_page = batch.len() < 100;
            rows.extend(batch);
            if last_page {
                return Ok(rows);
            }
        }
        bail!("GitHub results exceeded the review limit; review manually")
    }

    /// Read every unseen comment from a newest first endpoint and return oldest first
    ///
    /// A fetch error or a backlog over 31 pages returns an error rather than partial results
    pub fn pages_after_id(&self, path: &str, after: Option<u64>) -> Result<Vec<Value>> {
        read_comment_pages(after, 31, true, |page| self.page(path, page))
    }

    /// Return a bounded newest-first window after a cursor, reordered oldest first
    pub fn recent_pages_after_id(
        &self,
        path: &str,
        after: Option<u64>,
        pages: usize,
    ) -> Result<Vec<Value>> {
        read_comment_pages(after, pages, false, |page| self.page(path, page))
    }

    pub fn page(&self, path: &str, page: u64) -> Result<Vec<Value>> {
        if page == 0 {
            bail!("GitHub page numbers start at one");
        }
        let separator = if path.contains('?') { '&' } else { '?' };
        let value = self.api(
            &format!("{path}{separator}per_page=100&page={page}"),
            None,
            "GET",
        )?;
        page_rows(value, None)
    }
}

/// Keep the fetched rows owned so nested response data is never duplicated
fn page_rows(mut value: Value, key: Option<&str>) -> Result<Vec<Value>> {
    let rows = match key {
        Some(key) => value
            .get_mut(key)
            .and_then(Value::as_array_mut)
            .ok_or_else(|| anyhow!("GitHub response omitted {key}"))?,
        None => value
            .as_array_mut()
            .ok_or_else(|| anyhow!("GitHub response was not a list"))?,
    };
    Ok(std::mem::take(rows))
}

fn read_comment_pages(
    after: Option<u64>,
    pages: usize,
    require_complete: bool,
    mut fetch: impl FnMut(u64) -> Result<Vec<Value>>,
) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    for page in 1..=pages {
        let batch = fetch(page as u64)?;
        let last_page = batch.len() < 100;
        for row in batch {
            let id = row["id"]
                .as_u64()
                .ok_or_else(|| anyhow!("GitHub response omitted an item ID"))?;
            if after.is_some_and(|after| id <= after) {
                rows.reverse();
                return Ok(rows);
            }
            rows.push(row);
        }
        if last_page {
            rows.reverse();
            return Ok(rows);
        }
    }
    if require_complete {
        bail!("GitHub comment backlog exceeded the sweep limit; no cursor was advanced");
    }
    rows.reverse();
    Ok(rows)
}

pub fn validate_repository(value: &str) -> Result<()> {
    let expression = Regex::new(r"^[A-Za-z0-9][A-Za-z0-9-]{0,38}/[A-Za-z0-9_.-]{1,100}$")?;
    if !expression.is_match(value) || matches!(value.split('/').nth(1), Some("." | "..")) {
        bail!("use an explicit OWNER/REPO");
    }
    Ok(())
}

pub fn gh(arguments: &[String], data: Option<&str>, missing: bool) -> Result<Option<String>> {
    gh_in_directory(arguments, data, missing, Path::new("."))
}

/// Run an ambient-authenticated GitHub CLI request from a specific checkout
pub fn gh_in_directory(
    arguments: &[String],
    data: Option<&str>,
    missing: bool,
    directory: &Path,
) -> Result<Option<String>> {
    gh_with_token(arguments, data, missing, None, None, directory)
}

fn gh_with_token(
    arguments: &[String],
    data: Option<&str>,
    missing: bool,
    token: Option<&str>,
    cancel_file: Option<&Path>,
    directory: &Path,
) -> Result<Option<String>> {
    let binary = agent::which("gh").ok_or_else(|| anyhow!("install GitHub CLI"))?;
    let mut ambient_auth = BTreeMap::new();
    for name in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(value) = env::var(name) {
            ambient_auth.insert(name.to_owned(), value);
        }
    }
    let environment = github_environment(token, &ambient_auth);
    let output = agent::execute(
        binary.as_os_str(),
        arguments,
        directory,
        data.unwrap_or_default().as_bytes(),
        Duration::from_secs(180),
        &environment,
        false,
        cancel_file,
    )?;
    if output.code != 0 {
        if is_missing_response(&output.stderr, missing) {
            return Ok(None);
        }
        bail!("{}", github_failure(&output.stderr, output.code));
    }
    Ok(Some(output.stdout))
}

fn is_missing_response(stderr: &str, missing: bool) -> bool {
    missing && github_status(stderr) == Some(404)
}

fn github_status(stderr: &str) -> Option<u16> {
    Regex::new(r"(?i)(?:http(?:/[0-9.]+)?|status(?: code)?)\D{0,12}([1-5][0-9]{2})")
        .ok()?
        .captures(stderr)?
        .get(1)?
        .as_str()
        .parse()
        .ok()
}

fn github_failure(stderr: &str, code: i32) -> String {
    match github_status(stderr) {
        Some(401) => "GitHub authentication failed (401); sign in with gh auth login or refresh the App token".to_owned(),
        Some(403) => {
            "GitHub access denied (403); confirm the App installation or CLI account can access the repository".to_owned()
        }
        Some(404) => {
            "GitHub repository or API resource was not found (404); confirm the repository name and App installation".to_owned()
        }
        Some(422) => {
            "GitHub rejected the request (422); check the repository configuration and requested change".to_owned()
        }
        Some(429) => "GitHub rate limit reached (429); wait before retrying".to_owned(),
        _ if stderr.to_ascii_lowercase().contains("gh auth login")
            || stderr
                .to_ascii_lowercase()
                .contains("not logged into any github hosts") =>
        {
            "GitHub CLI is not authenticated; run gh auth login".to_owned()
        }
        _ => format!("GitHub request failed (exit {code}); check gh auth status and repository access"),
    }
}

fn safe_endpoint_label(endpoint: &str) -> String {
    const LIMIT: usize = 160;
    let mut redact_next = false;
    let mut label = String::new();
    for (index, segment) in endpoint.split('/').enumerate() {
        if index != 0 {
            label.push('/');
        }
        if redact_next {
            label.push_str("<redacted>");
            redact_next = false;
            continue;
        }
        let segment: String = segment
            .chars()
            .filter(|character| !character.is_control())
            .collect();
        redact_next = segment == "app-manifests";
        label.push_str(&segment);
    }
    if label.chars().count() <= LIMIT {
        return label;
    }
    let shortened: String = label.chars().take(LIMIT - 1).collect();
    format!("{shortened}…")
}

fn github_environment(
    token: Option<&str>,
    ambient_auth: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    match token {
        Some(token) => BTreeMap::from([("GH_TOKEN".to_owned(), token.to_owned())]),
        None => ambient_auth
            .iter()
            .filter(|(name, _)| matches!(name.as_str(), "GH_TOKEN" | "GITHUB_TOKEN"))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect(),
    }
}

pub fn api(
    endpoint: &str,
    payload: Option<&Value>,
    method: &str,
    missing: bool,
) -> Result<Option<Value>> {
    api_with_token(endpoint, payload, method, missing, None, None)
}

pub fn api_authenticated(
    endpoint: &str,
    payload: Option<&Value>,
    method: &str,
    missing: bool,
    token: &str,
) -> Result<Option<Value>> {
    if token.is_empty() {
        bail!("a GitHub App installation token is required");
    }
    api_with_token(endpoint, payload, method, missing, Some(token), None)
}

/// Run an installation-token request that stops when the caller cancels it
pub fn api_authenticated_cancellable(
    endpoint: &str,
    payload: Option<&Value>,
    method: &str,
    missing: bool,
    token: &str,
    cancel_file: &Path,
) -> Result<Option<Value>> {
    if token.is_empty() {
        bail!("a GitHub App installation token is required");
    }
    api_with_token(
        endpoint,
        payload,
        method,
        missing,
        Some(token),
        Some(cancel_file),
    )
}

pub fn authenticated_pages(endpoint: &str, key: &str, token: &str) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    for page in 1..=32 {
        let separator = if endpoint.contains('?') { '&' } else { '?' };
        let value = api_authenticated(
            &format!("{endpoint}{separator}per_page=100&page={page}"),
            None,
            "GET",
            false,
            token,
        )?
        .ok_or_else(|| anyhow!("GitHub returned no installed repositories"))?;
        let batch = page_rows(value, Some(key))?;
        let last_page = batch.len() < 100;
        rows.extend(batch);
        if last_page {
            return Ok(rows);
        }
    }
    bail!("GitHub results exceeded the service limit; narrow the App installation")
}

pub fn api_cancellable(
    endpoint: &str,
    payload: Option<&Value>,
    method: &str,
    missing: bool,
    cancel_file: &Path,
) -> Result<Option<Value>> {
    api_with_token(endpoint, payload, method, missing, None, Some(cancel_file))
}

fn api_with_token(
    endpoint: &str,
    payload: Option<&Value>,
    method: &str,
    missing: bool,
    token: Option<&str>,
    cancel_file: Option<&Path>,
) -> Result<Option<Value>> {
    let mut arguments = vec![
        "api".to_owned(),
        "--method".to_owned(),
        method.to_owned(),
        endpoint.to_owned(),
    ];
    if payload.is_some() {
        arguments.extend(["--input".to_owned(), "-".to_owned()]);
    }
    let output = gh_with_token(
        &arguments,
        payload.map(serde_json::to_string).transpose()?.as_deref(),
        missing,
        token,
        cancel_file,
        Path::new("."),
    )
    .with_context(|| format!("GitHub API {method} {}", safe_endpoint_label(endpoint)))?;
    let Some(output) = output.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    serde_json::from_str(&output)
        .map(Some)
        .context("GitHub returned invalid JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_pages_preserve_nested_rows_in_response_order() -> Result<()> {
        for expected in [
            serde_json::json!([]),
            serde_json::json!([
                {"id": 2, "labels": [{"name": "dependency"}], "body": "first"},
                {"id": 1, "labels": [], "body": "second"}
            ]),
        ] {
            for (response, key) in [
                (expected.clone(), None),
                (
                    serde_json::json!({"check_runs": expected.clone(), "total_count": 2}),
                    Some("check_runs"),
                ),
            ] {
                assert_eq!(Value::Array(page_rows(response, key)?), expected);
            }
        }
        Ok(())
    }

    #[test]
    fn api_pages_reject_missing_or_malformed_arrays() {
        for (response, key, message) in [
            (Value::Null, None, "GitHub response was not a list"),
            (
                serde_json::json!({"secret": "private response"}),
                None,
                "GitHub response was not a list",
            ),
            (
                serde_json::json!([]),
                Some("check_runs"),
                "GitHub response omitted check_runs",
            ),
            (
                serde_json::json!({}),
                Some("check_runs"),
                "GitHub response omitted check_runs",
            ),
            (
                serde_json::json!({"check_runs": null}),
                Some("check_runs"),
                "GitHub response omitted check_runs",
            ),
            (
                serde_json::json!({"check_runs": {}}),
                Some("check_runs"),
                "GitHub response omitted check_runs",
            ),
        ] {
            assert_eq!(page_rows(response, key).unwrap_err().to_string(), message);
        }
    }

    #[test]
    fn comment_backlogs_cross_pages_without_skipping_unseen_mentions() -> Result<()> {
        for (after, newest) in [(None, 150), (Some(50), 200)] {
            let source: Vec<_> = (1..=newest)
                .rev()
                .map(|id| serde_json::json!({"id": id}))
                .collect();
            let fetch = |page: u64| {
                Ok(source
                    .iter()
                    .skip((page as usize - 1) * 100)
                    .take(100)
                    .cloned()
                    .collect())
            };
            let comments = read_comment_pages(after, 31, true, fetch)?;
            let ids: Vec<_> = comments
                .iter()
                .map(|row| row["id"].as_u64().unwrap())
                .collect();
            assert_eq!(ids, ((after.unwrap_or(0) + 1)..=newest).collect::<Vec<_>>());
            assert_eq!(ids.len(), 150);
            assert_eq!(ids.iter().max(), Some(&newest));
        }
        Ok(())
    }

    #[test]
    fn complete_sweeps_refuse_a_partial_backlog() {
        let error = read_comment_pages(None, 1, true, |_| {
            Ok((101..=200)
                .rev()
                .map(|id| serde_json::json!({"id": id}))
                .collect())
        })
        .unwrap_err();
        assert!(error.to_string().contains("no cursor was advanced"));
    }

    #[test]
    fn reply_windows_keep_the_newest_page_in_conversation_order() -> Result<()> {
        let comments = read_comment_pages(None, 1, false, |_| {
            Ok((101..=200)
                .rev()
                .map(|id| serde_json::json!({"id": id}))
                .collect())
        })?;
        let ids = comments
            .iter()
            .map(|row| row["id"].as_u64().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(ids, (101..=200).collect::<Vec<_>>());
        Ok(())
    }

    #[test]
    fn cursor_stops_fetching_after_the_first_seen_comment() -> Result<()> {
        let mut fetched = Vec::new();
        let comments = read_comment_pages(Some(150), 31, true, |page| {
            fetched.push(page);
            Ok((101..=200)
                .rev()
                .map(|id| serde_json::json!({"id": id}))
                .collect())
        })?;
        assert_eq!(fetched, [1]);
        assert_eq!(comments.len(), 50);
        assert_eq!(comments.first().unwrap()["id"], 151);
        assert_eq!(comments.last().unwrap()["id"], 200);
        Ok(())
    }

    #[test]
    fn malformed_comments_and_fetch_failures_do_not_return_partial_results() {
        let error = read_comment_pages(None, 31, true, |_| {
            Ok(vec![
                serde_json::json!({"id": 2}),
                serde_json::json!({"id": "invalid"}),
            ])
        })
        .unwrap_err();
        assert!(error.to_string().contains("item ID"));
        let error = read_comment_pages(None, 31, true, |page| {
            if page == 1 {
                Ok((101..=200)
                    .rev()
                    .map(|id| serde_json::json!({"id": id}))
                    .collect())
            } else {
                Err(anyhow!("page fetch failed"))
            }
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "page fetch failed");
    }

    #[test]
    fn repository_validation_uses_a_compact_case_table() {
        for (value, valid) in [
            ("owner/repo", true),
            ("owner/repo.name", true),
            ("owner", false),
            ("/repo", false),
            ("owner/..", false),
            ("owner/repo/extra", false),
        ] {
            assert_eq!(validate_repository(value).is_ok(), valid, "{value}");
        }
    }

    #[test]
    fn github_environment_keeps_only_authorisation() {
        let ambient = BTreeMap::from([
            ("GH_TOKEN".to_owned(), "ambient-gh".to_owned()),
            ("GITHUB_TOKEN".to_owned(), "ambient-github".to_owned()),
            ("GH_HOST".to_owned(), "attacker.example".to_owned()),
            ("GH_REPO".to_owned(), "attacker/repo".to_owned()),
        ]);
        for (token, expected) in [
            (
                None,
                BTreeMap::from([
                    ("GH_TOKEN".to_owned(), "ambient-gh".to_owned()),
                    ("GITHUB_TOKEN".to_owned(), "ambient-github".to_owned()),
                ]),
            ),
            (
                Some("app-token"),
                BTreeMap::from([("GH_TOKEN".to_owned(), "app-token".to_owned())]),
            ),
        ] {
            assert_eq!(github_environment(token, &ambient), expected);
        }
    }

    #[test]
    fn cancellable_authenticated_requests_require_an_installation_token() {
        assert!(
            api_authenticated_cancellable(
                "repos/owner/repo",
                None,
                "GET",
                false,
                "",
                Path::new("cancel"),
            )
            .is_err()
        );
    }

    #[test]
    fn github_failures_are_classified_without_echoing_stderr() {
        let token = "arbitrary-token-that-must-not-escape";
        for (stderr, status, missing, message, endpoint, hidden) in [
            (
                "request failed (HTTP 404)",
                Some(404),
                true,
                "not found (404)",
                "repos/owner/repo",
                None,
            ),
            (
                "HTTP/2 401 unauthorized",
                Some(401),
                false,
                "authentication failed (401)",
                "repos/owner/repo",
                None,
            ),
            (
                "status code: 403",
                Some(403),
                false,
                "access denied (403)",
                "repos/owner/repo",
                None,
            ),
            (
                "HTTP 422",
                Some(422),
                false,
                "rejected the request (422)",
                "repos/owner/repo",
                None,
            ),
            (
                "HTTP 429",
                Some(429),
                false,
                "rate limit reached (429)",
                "repos/owner/repo",
                None,
            ),
            (
                "To get started with GitHub CLI, please run: gh auth login",
                None,
                false,
                "CLI is not authenticated",
                "repos/owner/repo",
                None,
            ),
            (
                "connection closed with secret arbitrary-token-that-must-not-escape",
                None,
                false,
                "request failed (exit 7)",
                "app-manifests/one-time-code\nwith-control/conversions/abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz",
                Some("one-time-code"),
            ),
        ] {
            assert_eq!(github_status(stderr), status, "{stderr}");
            assert_eq!(is_missing_response(stderr, true), missing, "{stderr}");
            let failure = github_failure(stderr, 7);
            assert!(failure.contains(message), "{stderr}: {failure}");
            assert!(!failure.contains(token), "{stderr}: {failure}");
            let label = safe_endpoint_label(endpoint);
            assert!(label.chars().count() <= 160, "{label}");
            assert!(!label.chars().any(char::is_control), "{label}");
            if let Some(hidden) = hidden {
                assert!(!label.contains(hidden), "{label}");
            }
        }
    }
}
