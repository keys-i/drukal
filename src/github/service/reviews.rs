use anyhow::anyhow;
use serde::Serialize;
use serde_json::Value;

use crate::Result;
use crate::github::{self, GitHub};
use crate::setup;

use super::{ServeArgs, owner_matches, record_sweep_failure};

#[derive(Clone, Debug, Serialize)]
pub(super) struct CentralTarget {
    pub(super) repo: String,
    pub(super) owner: String,
    pub(super) name: String,
    pub(super) private: bool,
    pub(super) number: u64,
    pub(super) checks: Vec<String>,
    pub(super) solver_ref: String,
    pub(super) autofix: bool,
}

pub(super) fn central_targets<F>(
    arguments: &ServeArgs,
    token: &str,
    target: Option<(&str, Option<u64>)>,
    select: &mut F,
) -> Result<()>
where
    F: FnMut(CentralTarget),
{
    let repositories = if let Some((repo, _)) = target {
        github::api_authenticated(&format!("repos/{repo}"), None, "GET", false, token)?
            .into_iter()
            .collect()
    } else {
        github::authenticated_pages("installation/repositories", "repositories", token)?
    };
    let mut failures = Vec::new();
    let mut repositories = repositories.iter().collect::<Vec<_>>();
    repositories.sort_by_key(|repository| repository["full_name"].as_str().unwrap_or_default());
    for repository in repositories {
        if repository["archived"].as_bool() == Some(true)
            || repository["disabled"].as_bool() == Some(true)
        {
            continue;
        }
        let Some(name) = repository["full_name"].as_str().filter(|name| {
            owner_matches(repository, arguments.owner.as_deref())
                && github::validate_repository(name).is_ok()
                && target.is_none_or(|(repo, _)| *name == repo)
        }) else {
            continue;
        };
        let private = repository["private"].as_bool();
        let github = match GitHub::new(name, token) {
            Ok(github) => github,
            Err(error) => {
                record_sweep_failure(&mut failures, name, &error);
                continue;
            }
        };
        let configuration = match service_configuration(&github) {
            Ok(Some(configuration)) => configuration,
            Ok(None) => continue,
            Err(error) => {
                record_sweep_failure(&mut failures, name, &error);
                continue;
            }
        };
        let checks = match setup::configuration_checks(&configuration) {
            Ok(checks) => checks,
            Err(error) => {
                record_sweep_failure(&mut failures, name, &error);
                continue;
            }
        };
        let solver_ref = match solver_ref(&configuration) {
            Some(Ok(source)) => source.joined(),
            Some(Err(error)) => {
                record_sweep_failure(
                    &mut failures,
                    name,
                    &anyhow!("Drukal configuration has an invalid trusted solver source: {error}"),
                );
                continue;
            }
            None => {
                record_sweep_failure(
                    &mut failures,
                    name,
                    &anyhow!("Drukal configuration has no trusted solver source"),
                );
                continue;
            }
        };
        let pulls = match target.and_then(|(_, number)| number) {
            Some(number) => github
                .api_optional(&format!("pulls/{number}"), None, "GET")
                .map(|pull| pull.into_iter().collect()),
            None => github.pages("pulls?state=open&sort=created&direction=asc", None),
        };
        let pulls = match pulls {
            Ok(pulls) => pulls,
            Err(error) => {
                record_sweep_failure(&mut failures, name, &error);
                continue;
            }
        };
        for pull in pulls {
            if !reviewable_pull(name, &pull) {
                continue;
            }
            let (Some(number), Some(owner), Some(repository_name)) = (
                pull["number"].as_u64(),
                repository["owner"]["login"].as_str(),
                repository["name"].as_str(),
            ) else {
                record_sweep_failure(
                    &mut failures,
                    name,
                    &anyhow!("GitHub returned an invalid pull request"),
                );
                continue;
            };
            let Some(private) = private else {
                record_sweep_failure(
                    &mut failures,
                    name,
                    &anyhow!("GitHub returned an invalid repository visibility"),
                );
                continue;
            };
            select(CentralTarget {
                repo: name.to_owned(),
                owner: owner.to_owned(),
                name: repository_name.to_owned(),
                private,
                number,
                checks: checks.clone(),
                solver_ref: solver_ref.clone(),
                autofix: setup::autofix_enabled(&configuration),
            });
        }
    }
    for failure in failures {
        eprintln!("Target skipped: {failure}");
    }
    Ok(())
}

fn reviewable_pull(repo: &str, pull: &Value) -> bool {
    pull["state"] == "open"
        && pull["draft"].as_bool() == Some(false)
        && pull["base"]["repo"]["full_name"] == repo
        && (pull["user"]["login"] != "dependabot[bot]" || pull["head"]["repo"]["full_name"] == repo)
}

fn service_configuration(github: &GitHub) -> Result<Option<Value>> {
    let Some(configuration) = setup::repository_configuration(github)? else {
        return Ok(None);
    };
    if setup::verified_configuration(github, &configuration)? {
        Ok(Some(configuration))
    } else {
        Ok(None)
    }
}

fn solver_ref(configuration: &Value) -> Option<Result<setup::SourceRef>> {
    configuration["source"]
        .as_str()
        .map(setup::SourceRef::parse)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{reviewable_pull, solver_ref};

    #[test]
    fn reviews_include_external_contributors_and_keep_repairs_in_the_base_repo() {
        let mut pull = json!({
            "state": "open", "draft": false,
            "base": {"repo": {"full_name": "owner/repo"}},
            "head": {"repo": {"full_name": "contributor/fork"}},
            "user": {"login": "contributor"}, "author_association": "NONE",
        });
        assert!(reviewable_pull("owner/repo", &pull));
        assert!(!reviewable_pull("other/repo", &pull));
        pull["draft"] = json!(true);
        assert!(!reviewable_pull("owner/repo", &pull));
        pull["draft"] = json!(false);
        pull["user"]["login"] = json!("dependabot[bot]");
        assert!(!reviewable_pull("owner/repo", &pull));
        pull["head"]["repo"]["full_name"] = json!("owner/repo");
        assert!(reviewable_pull("owner/repo", &pull));
        pull["state"] = json!("closed");
        assert!(!reviewable_pull("owner/repo", &pull));
    }

    #[test]
    fn solver_source_is_an_explicit_trusted_commit() {
        for (source, valid) in [
            (
                "keys-i/drukal@0123456789abcdef0123456789abcdef01234567",
                true,
            ),
            ("keys-i/drukal@main", false),
            (
                "other/drukal@0123456789abcdef0123456789abcdef01234567",
                false,
            ),
        ] {
            let configuration = json!({"source": source});
            assert_eq!(
                solver_ref(&configuration).is_some_and(|value| value.is_ok()),
                valid
            );
        }
        assert!(solver_ref(&json!({})).is_none());
    }
}
