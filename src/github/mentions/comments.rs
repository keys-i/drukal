//! Reply in the review thread where a trusted PR mention was posted

use anyhow::bail;
use serde_json::{Value, json};

use crate::Result;
use crate::agent::Harness;
use crate::github::GitHub;

use super::{
    Invocation, TrustedPrompt, USAGE, answer, invocation, issue_comment_count, issue_is_open,
    neutralize, prior_reply_exists, recent_comments, reply_marker, trusted_prompt,
};

pub(crate) fn respond(
    github: &GitHub,
    number: u64,
    comment: u64,
    model: Option<&str>,
    harness: Harness,
    private: Option<bool>,
) -> Result<()> {
    if number == 0 || comment == 0 {
        bail!("pull request and comment numbers must be positive");
    }
    let source = github.api(&format!("pulls/comments/{comment}"), None, "GET")?;
    let Some(prompt) = trusted_review_prompt(&source, github.repo(), number, comment)? else {
        return Ok(());
    };
    let pull = github.api(&format!("issues/{number}"), None, "GET")?;
    if !issue_is_open(&pull, number)? {
        return Ok(());
    }
    if !pull["pull_request"].is_object() {
        bail!("review comment does not belong to a pull request");
    }
    let replies = github.recent_pages_after_id(
        &format!("pulls/{number}/comments?sort=created&direction=desc"),
        None,
        4,
    )?;
    if prior_reply_exists(&replies, comment) {
        return Ok(());
    }
    let body = match invocation(&prompt.prompt) {
        Invocation::Ask(request) if request.is_empty() => USAGE.to_owned(),
        Invocation::Ask(request) => {
            let conversation = recent_comments(github, number, issue_comment_count(&pull)?)?;
            neutralize(&answer(
                github,
                number,
                &pull,
                &conversation,
                comment,
                &request,
                model,
                harness,
                private,
                Some(&source),
            )?)
        }
        Invocation::WriteRequest(_) | Invocation::Approve(_) => {
            "Post the change request or approval in this PR’s **Conversation** tab with `@koelu`. I’ll prepare the change through the existing approval flow.".to_owned()
        }
    };
    let thread = source["in_reply_to_id"].as_u64().unwrap_or(comment);
    if thread == 0 {
        bail!("GitHub returned an invalid review thread");
    }
    github.api(
        &format!("pulls/{number}/comments/{thread}/replies"),
        Some(&json!({"body": format!("{}\n\n{body}", reply_marker(comment))})),
        "POST",
    )?;
    Ok(())
}

fn trusted_review_prompt(
    comment: &Value,
    repo: &str,
    number: u64,
    id: u64,
) -> Result<Option<TrustedPrompt>> {
    let expected = format!("https://api.github.com/repos/{repo}/pulls/{number}");
    if comment["pull_request_url"].as_str() != Some(&expected) {
        bail!("review comment does not belong to the requested pull request");
    }
    let mut comment = comment.clone();
    comment["issue_url"] = json!(format!(
        "https://api.github.com/repos/{repo}/issues/{number}"
    ));
    trusted_prompt(&comment, number, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_mentions_are_bound_to_the_repository_pull_and_author() -> Result<()> {
        let mut comment = json!({
            "id": 7,
            "pull_request_url": "https://api.github.com/repos/owner/repo/pulls/2",
            "author_association": "COLLABORATOR",
            "user": {"login": "maintainer"},
            "body": "@koelu[bot]\nreview this line",
        });
        assert_eq!(
            trusted_review_prompt(&comment, "owner/repo", 2, 7)?
                .unwrap()
                .prompt,
            "review this line"
        );
        assert!(trusted_review_prompt(&comment, "other/repo", 2, 7).is_err());
        assert!(trusted_review_prompt(&comment, "owner/repo", 3, 7).is_err());
        assert!(trusted_review_prompt(&comment, "owner/repo", 2, 8).is_err());
        comment["author_association"] = json!("CONTRIBUTOR");
        assert!(trusted_review_prompt(&comment, "owner/repo", 2, 7).is_err());
        Ok(())
    }
}
