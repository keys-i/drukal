//! Keep the newest reply visible and fold consecutive older replies

use std::collections::HashSet;

use anyhow::{anyhow, bail};
use serde_json::{Value, json};

use super::GitHub;
use crate::Result;

const QUERY: &str = r#"
query($owner: String!, $name: String!, $number: Int!, $before: String) {
  repository(owner: $owner, name: $name) {
    issueOrPullRequest(number: $number) {
      ... on Issue {
        replies: timelineItems(last: 100, before: $before, itemTypes: [ISSUE_COMMENT]) {
          ...Replies
        }
      }
      ... on PullRequest {
        replies: timelineItems(last: 100, before: $before, itemTypes: [ISSUE_COMMENT, PULL_REQUEST_REVIEW]) {
          nodes {
            ...Reply
            ... on PullRequestReview {
              __typename
              fullDatabaseId id body state commit { oid }
              author { login __typename }
              isMinimized viewerCanMinimize
            }
          }
          pageInfo { hasPreviousPage startCursor }
        }
      }
    }
  }
}
fragment Replies on IssueTimelineItemsConnection {
  nodes { ...Reply }
  pageInfo { hasPreviousPage startCursor }
}
fragment Reply on IssueComment {
  __typename
  fullDatabaseId id body author { login __typename }
  isMinimized viewerCanMinimize
}
"#;

pub(crate) enum Response<'a> {
    Comment,
    Review { head: &'a str, event: &'a str },
    Thread { thread: u64 },
}

impl Response<'_> {
    /// Editing a review must preserve its commit and approval state
    pub(crate) fn can_edit(&self, previous: &Value) -> bool {
        match self {
            Self::Comment => previous["__typename"] == "IssueComment",
            Self::Review {
                event: "COMMENT", ..
            } if previous["__typename"] == "IssueComment" => true,
            Self::Review { head, event } => {
                previous["__typename"] == "PullRequestReview"
                    && previous["commit"]["oid"] == *head
                    && previous["state"]
                        == if *event == "APPROVE" {
                            "APPROVED"
                        } else {
                            "COMMENTED"
                        }
            }
            Self::Thread { .. } => previous["__typename"] == "PullRequestReviewComment",
        }
    }
}

impl GitHub {
    fn graphql(&self, payload: &Value) -> Result<Value> {
        let result = super::api_with_token(
            "graphql",
            Some(payload),
            "POST",
            false,
            Some(&self.token),
            None,
        )?
        .ok_or_else(|| anyhow!("GitHub returned no conversation response"))?;
        if result.get("errors").is_some() {
            bail!("GitHub couldn't read or update the conversation");
        }
        Ok(result)
    }

    /// Read only the consecutive replies at the end of this conversation
    ///
    /// Human replies and replies from another bot stop the scan
    pub(crate) fn response_tail(&self, number: u64, bot: &str) -> Result<Vec<Value>> {
        if number == 0 {
            bail!("issue or pull request number must be positive");
        }
        let (owner, name) = self.repo().split_once('/').expect("validated repository");
        let mut before = Value::Null;
        let mut tail = Vec::new();
        for _ in 0..31 {
            let mut result = self.graphql(&json!({
                "query": QUERY,
                "variables": {"owner": owner, "name": name, "number": number, "before": before},
            }))?;
            let page = &mut result["data"]["repository"]["issueOrPullRequest"]["replies"];
            let rows = std::mem::take(
                page["nodes"]
                    .as_array_mut()
                    .ok_or_else(|| anyhow!("GitHub omitted conversation replies"))?,
            );
            let empty = rows.is_empty();
            for row in rows.into_iter().rev() {
                if !own_response(&row, bot) {
                    return Ok(tail);
                }
                response_id(&row)?;
                tail.push(row);
            }
            match page["pageInfo"]["hasPreviousPage"].as_bool() {
                Some(false) => return Ok(tail),
                Some(true) => {
                    let cursor = page["pageInfo"]["startCursor"]
                        .as_str()
                        .filter(|cursor| !cursor.is_empty())
                        .ok_or_else(|| anyhow!("GitHub omitted the conversation cursor"))?;
                    if before == cursor || empty {
                        bail!("GitHub returned an invalid conversation page");
                    }
                    before = json!(cursor);
                }
                None => bail!("GitHub omitted conversation pagination"),
            }
        }
        bail!("consecutive Koelu replies exceed the cleanup limit; tidy the conversation manually")
    }

    /// Publish the current response before collapsing any older replies
    pub(crate) fn publish_response(
        &self,
        number: u64,
        body: &str,
        bot: &str,
        response: Response<'_>,
        tail: &[Value],
    ) -> Result<Value> {
        write_response(number, body, bot, response, tail, |path, body, method| {
            if path == "graphql" {
                self.graphql(body.expect("GraphQL payload"))
            } else {
                self.api(path, body, method)
            }
        })
    }

    pub(crate) fn reply(&self, number: u64, body: &str, bot: &str) -> Result<Value> {
        let tail = self.response_tail(number, bot)?;
        self.publish_response(number, body, bot, Response::Comment, &tail)
    }

    /// Keep inline replies in their original review thread
    pub(crate) fn thread_reply(
        &self,
        number: u64,
        thread: u64,
        body: &str,
        bot: &str,
        replies: &[Value],
    ) -> Result<Value> {
        let tail = replies
            .iter()
            .rev()
            .filter(|reply| reply["id"] == thread || reply["in_reply_to_id"] == thread)
            .take_while(|reply| {
                reply["user"]["type"] == "Bot"
                    && reply["user"]["login"]
                        .as_str()
                        .is_some_and(|login| login.eq_ignore_ascii_case(bot))
            })
            .map(|reply| {
                json!({
                    "__typename": "PullRequestReviewComment", "fullDatabaseId": reply["id"],
                    "id": reply["node_id"], "body": reply["body"],
                    "author": {"login": reply["user"]["login"], "__typename": "Bot"},
                })
            })
            .collect::<Vec<_>>();
        self.publish_response(number, body, bot, Response::Thread { thread }, &tail)
    }
}

fn own_response(row: &Value, bot: &str) -> bool {
    row["author"]["__typename"] == "Bot"
        && row["author"]["login"].as_str().is_some_and(|login| {
            login.eq_ignore_ascii_case(bot)
                || bot
                    .strip_suffix("[bot]")
                    .is_some_and(|slug| login.eq_ignore_ascii_case(slug))
        })
}

pub(crate) fn response_id(row: &Value) -> Result<u64> {
    row["fullDatabaseId"]
        .as_u64()
        .or_else(|| row["fullDatabaseId"].as_str()?.parse().ok())
        .filter(|id| *id > 0)
        .ok_or_else(|| anyhow!("GitHub returned an invalid response ID"))
}

fn write_response(
    number: u64,
    body: &str,
    bot: &str,
    response: Response<'_>,
    tail: &[Value],
    mut api: impl FnMut(&str, Option<&Value>, &str) -> Result<Value>,
) -> Result<Value> {
    if tail.iter().any(|row| !own_response(row, bot)) {
        bail!("only this App's consecutive replies can be updated");
    }
    let mut previous = tail.first().filter(|row| response.can_edit(row));
    let fresh = body;
    let mut body = fresh.to_owned();
    if let Some(previous) = previous {
        // Keep request receipts when the visible answer changes
        let receipt = |line: &&str| {
            (line.starts_with("<!-- koelu:") || line.starts_with("<!-- rady:mention:"))
                && line.ends_with(" -->")
        };
        let mut receipts = body
            .lines()
            .filter(receipt)
            .map(str::to_owned)
            .collect::<HashSet<_>>();
        for marker in previous["body"]
            .as_str()
            .unwrap_or_default()
            .lines()
            .filter(receipt)
        {
            if receipts.insert(marker.to_owned()) {
                body.push('\n');
                body.push_str(marker);
            }
        }
    }
    if body.len() > 60_000 && previous.is_some() {
        previous = None;
        body = fresh.to_owned();
    }
    if body.len() > 60_000 {
        bail!("the response exceeds the comment limit");
    }
    let (path, method, payload) = match previous {
        Some(row) if row["__typename"] == "IssueComment" => (
            format!("issues/comments/{}", response_id(row)?),
            "PATCH",
            json!({"body": body}),
        ),
        Some(row) if row["__typename"] == "PullRequestReviewComment" => (
            format!("pulls/comments/{}", response_id(row)?),
            "PATCH",
            json!({"body": body}),
        ),
        Some(row) => (
            format!("pulls/{number}/reviews/{}", response_id(row)?),
            "PUT",
            json!({"body": body}),
        ),
        None => match response {
            Response::Comment => (
                format!("issues/{number}/comments"),
                "POST",
                json!({"body": body}),
            ),
            Response::Review { head, event } => (
                format!("pulls/{number}/reviews"),
                "POST",
                json!({"commit_id": head, "event": event, "body": body}),
            ),
            Response::Thread { thread } => (
                format!("pulls/{number}/comments/{thread}/replies"),
                "POST",
                json!({"body": body}),
            ),
        },
    };
    let published = api(&path, Some(&payload), method)?;
    for older in tail.iter().skip(usize::from(previous.is_some())) {
        if older["isMinimized"] == true {
            continue;
        }
        if older["viewerCanMinimize"] == false {
            bail!("GitHub won't let Koelu collapse an older reply");
        }
        let id = older["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow!("GitHub omitted the older reply's node ID"))?;
        let minimized = api(
            "graphql",
            Some(&json!({
                "query": "mutation($id: ID!) { minimizeComment(input: {subjectId: $id, classifier: OUTDATED}) { minimizedComment { isMinimized } } }",
                "variables": {"id": id},
            })),
            "POST",
        )?;
        if minimized["data"]["minimizeComment"]["minimizedComment"]["isMinimized"] != true {
            bail!("GitHub didn't collapse the older reply");
        }
    }
    Ok(published)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(id: u64) -> Value {
        json!({"__typename": "IssueComment", "fullDatabaseId": id.to_string(), "id": format!("node-{id}"),
            "author": {"login": "koelu[bot]", "__typename": "Bot"}, "body": format!("<!-- koelu:mention:{id} -->\nOld answer"),
            "isMinimized": false, "viewerCanMinimize": true})
    }

    #[test]
    fn newest_reply_is_edited_before_older_replies_are_collapsed() -> Result<()> {
        let tail = [comment(6_000_000_002), comment(1)];
        let mut calls = Vec::new();
        write_response(
            7,
            "Latest answer",
            "koelu[bot]",
            Response::Comment,
            &tail,
            |path, body, method| {
                calls.push((path.to_owned(), method.to_owned(), body.cloned()));
                Ok(
                    json!({"data": {"minimizeComment": {"minimizedComment": {"isMinimized": true}}}}),
                )
            },
        )?;
        assert_eq!(
            (&*calls[0].0, &*calls[0].1),
            ("issues/comments/6000000002", "PATCH")
        );
        assert_eq!(
            calls[0].2.as_ref().unwrap()["body"],
            "Latest answer\n<!-- koelu:mention:6000000002 -->"
        );
        assert_eq!(calls[1].2.as_ref().unwrap()["variables"]["id"], "node-1");
        Ok(())
    }

    #[test]
    fn empty_tail_posts_and_other_authors_cannot_be_changed() -> Result<()> {
        write_response(
            7,
            "Hello",
            "koelu[bot]",
            Response::Comment,
            &[],
            |path, _, method| {
                assert_eq!((path, method), ("issues/7/comments", "POST"));
                Ok(json!({"id": 9}))
            },
        )?;
        let mut foreign = comment(1);
        for author in [
            json!({"login": "maintainer", "__typename": "User"}),
            json!({"login": "other[bot]", "__typename": "Bot"}),
        ] {
            foreign["author"] = author;
            assert!(
                write_response(
                    7,
                    "Hello",
                    "koelu[bot]",
                    Response::Comment,
                    &[foreign.clone()],
                    |_, _, _| { panic!("another author must never be changed") }
                )
                .is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn graphql_and_rest_names_identify_the_same_app() {
        let mut reply = comment(1);
        assert!(own_response(&reply, "koelu[bot]"));
        reply["author"]["login"] = json!("koelu");
        assert!(own_response(&reply, "koelu[bot]"));
        reply["author"]["__typename"] = json!("User");
        assert!(!own_response(&reply, "koelu[bot]"));
    }

    #[test]
    fn failed_update_preserves_all_older_replies() {
        let mut calls = 0;
        assert!(
            write_response(
                7,
                "Latest",
                "koelu[bot]",
                Response::Comment,
                &[comment(2), comment(1)],
                |_, _, _| {
                    calls += 1;
                    Err(anyhow!("GitHub unavailable"))
                }
            )
            .is_err()
        );
        assert_eq!(calls, 1);
    }

    #[test]
    fn full_receipts_remain_in_an_older_collapsed_reply() -> Result<()> {
        let mut previous = comment(1);
        previous["body"] = json!(
            (0..2_200)
                .map(|id| format!("<!-- koelu:mention:{id} -->\n"))
                .collect::<String>()
        );
        let fresh = format!("<!-- koelu:mention:2 -->\n{}", "answer".repeat(1_000));
        let mut calls = Vec::new();
        write_response(
            7,
            &fresh,
            "koelu[bot]",
            Response::Comment,
            &[previous],
            |path, body, method| {
                calls.push((path.to_owned(), method.to_owned(), body.cloned()));
                Ok(
                    json!({"data": {"minimizeComment": {"minimizedComment": {"isMinimized": true}}}}),
                )
            },
        )?;
        assert_eq!((&*calls[0].0, &*calls[0].1), ("issues/7/comments", "POST"));
        assert_eq!(calls[0].2.as_ref().unwrap()["body"], fresh);
        assert_eq!(calls[1].2.as_ref().unwrap()["variables"]["id"], "node-1");
        Ok(())
    }

    #[test]
    fn review_edits_preserve_the_commit_and_approval() {
        let review = json!({"__typename": "PullRequestReview", "state": "APPROVED", "commit": {"oid": "abc"}});
        assert!(
            Response::Review {
                head: "abc",
                event: "APPROVE"
            }
            .can_edit(&review)
        );
        assert!(
            !Response::Review {
                head: "def",
                event: "APPROVE"
            }
            .can_edit(&review)
        );
        assert!(
            !Response::Review {
                head: "abc",
                event: "COMMENT"
            }
            .can_edit(&review)
        );
        assert!(!Response::Comment.can_edit(&review));
    }
}
