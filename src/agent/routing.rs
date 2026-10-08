//! Choose a model from task scope before starting expensive work
//!
//! Larger changes and failed checks can raise the tier
//! An explicit model choice always wins

use anyhow::bail;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Result;

use super::laya;

const MAX_CHOICES: usize = 8;
const MAX_CHOICE_CHARS: usize = 200;
const MAX_TEXT_BYTES: usize = 16_000;

/// Keep the order of a comma separated model list
///
/// Validation happens when a model is chosen so empty entries stay visible
pub fn parse_model_choices(raw: &str) -> Vec<String> {
    if raw.trim().is_empty() {
        Vec::new()
    } else {
        raw.split(',')
            .map(|model| model.trim().to_owned())
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
/// How much model capacity the supplied task appears to need
pub enum Tier {
    Fast,
    Balanced,
    Deep,
}

/// The minimum execution context a request needs
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    ReadOnly,
    Write,
    Ambiguous,
}

/// Conservatively classify a request before allocating a workspace
///
/// Questions about a fix remain questions
/// Mixed requests that ask for an action need a write workflow
///
/// ```
/// use drukal::routing::{classify_request, Intent};
///
/// assert_eq!(classify_request("How do I fix this failure?"), Intent::ReadOnly);
/// assert_eq!(classify_request("Explain and fix this failure"), Intent::Write);
/// assert_eq!(classify_request("Take a look"), Intent::Ambiguous);
/// ```
pub fn classify_request(request: &str) -> Intent {
    const WRITE_VERBS: &[&str] = &[
        "add",
        "apply",
        "bump",
        "change",
        "commit",
        "create",
        "delete",
        "deploy",
        "edit",
        "fix",
        "implement",
        "install",
        "merge",
        "modify",
        "publish",
        "push",
        "refactor",
        "release",
        "remove",
        "rename",
        "revert",
        "run",
        "update",
        "upgrade",
        "write",
    ];
    const READ_ONLY_WORDS: &[&str] = &[
        "compare",
        "describe",
        "difference",
        "diff",
        "does",
        "explain",
        "how",
        "is",
        "status",
        "summarise",
        "summarize",
        "what",
        "when",
        "where",
        "which",
        "who",
        "why",
    ];

    if read_only_question(request) {
        Intent::ReadOnly
    } else if contains_ascii_word(request, WRITE_VERBS) {
        Intent::Write
    } else if contains_ascii_word(request, READ_ONLY_WORDS) {
        Intent::ReadOnly
    } else {
        Intent::Ambiguous
    }
}

pub fn classify_request_with_laya(request: &str) -> Intent {
    let intent = classify_request(request);
    if intent == Intent::Ambiguous {
        laya::classify_intent(request).unwrap_or(intent)
    } else {
        intent
    }
}

fn read_only_question(text: &str) -> bool {
    let mut words = text.as_bytes()[..text.len().min(MAX_TEXT_BYTES)]
        .split(|byte| !byte.is_ascii_alphabetic())
        .filter(|word| !word.is_empty());
    let Some(first) = words.next() else {
        return false;
    };
    let second = words.next().unwrap_or_default();
    let third = words.next().unwrap_or_default();
    if first.eq_ignore_ascii_case(b"how") {
        return second.eq_ignore_ascii_case(b"to")
            || ([b"do".as_slice(), b"can", b"should", b"would"]
                .iter()
                .any(|word| second.eq_ignore_ascii_case(word))
                && [b"i".as_slice(), b"we"]
                    .iter()
                    .any(|word| third.eq_ignore_ascii_case(word)));
    }
    [
        b"what".as_slice(),
        b"why",
        b"where",
        b"when",
        b"which",
        b"who",
    ]
    .iter()
    .any(|word| first.eq_ignore_ascii_case(word))
        && [b"is".as_slice(), b"are", b"does", b"did", b"was", b"were"]
            .iter()
            .any(|word| second.eq_ignore_ascii_case(word))
}

/// Raise the tier for larger changes, missing diffs or failed checks
///
/// This uses supplied task data and does not contact a model provider
///
/// ```
/// use drukal::routing::{select, Tier};
/// use serde_json::json;
///
/// assert_eq!(select(&json!({"request": "Summarize this"})), Tier::Fast);
/// assert_eq!(select(&json!({"files": [{"additions": 2}]})), Tier::Balanced);
/// assert_eq!(select(&json!({"complete_diff": false})), Tier::Deep);
/// ```
pub fn select(evidence: &Value) -> Tier {
    let (files, changes, patch_bytes) = scopes(evidence)
        .filter_map(|scope| scope["files"].as_array())
        .fold(
            (0_usize, 0_u64, 0_usize),
            |(count, changes, bytes), files| {
                let (changes, bytes) =
                    files
                        .iter()
                        .take(64)
                        .fold((changes, bytes), |(changes, bytes), file| {
                            (
                                changes
                                    .saturating_add(file["additions"].as_u64().unwrap_or_default())
                                    .saturating_add(file["deletions"].as_u64().unwrap_or_default()),
                                bytes.saturating_add(file["patch"].as_str().map_or(0, str::len)),
                            )
                        });
                (count.saturating_add(files.len()), changes, bytes)
            },
        );
    let text_bytes = scopes(evidence)
        .flat_map(|scope| {
            ["task", "request", "title", "description", "body"]
                .into_iter()
                .filter_map(move |name| scope[name].as_str())
        })
        .map(str::len)
        .sum::<usize>();
    let sensitive = scopes(evidence).any(|scope| {
        ["task", "request", "title", "description", "body"]
            .into_iter()
            .filter_map(|name| scope[name].as_str())
            .any(|text| {
                [
                    "security",
                    "vulnerability",
                    "cve-",
                    "secret",
                    "credential",
                    "merge conflict",
                    "conflict",
                ]
                .into_iter()
                .any(|term| contains_ascii(text, term))
            })
    });
    let failed_checks = scopes(evidence).any(|scope| {
        scope["checks"].as_array().is_some_and(|checks| {
            checks.iter().take(128).any(|check| {
                matches!(
                    check["state"].as_str(),
                    Some(
                        "failure"
                            | "cancelled"
                            | "timed_out"
                            | "action_required"
                            | "startup_failure"
                    )
                )
            })
        })
    });

    if sensitive
        || failed_checks
        || scopes(evidence).any(|scope| scope["complete_diff"].as_bool() == Some(false))
        || files > 12
        || changes > 1_500
        || patch_bytes > 20_000
        || text_bytes > 4_000
    {
        Tier::Deep
    } else if files > 0 || changes > 0 || patch_bytes > 0 || text_bytes > 600 {
        Tier::Balanced
    } else {
        Tier::Fast
    }
}

pub fn select_with_laya(evidence: &Value) -> Tier {
    let tier = select(evidence);
    if tier == Tier::Deep {
        tier
    } else {
        laya::select_tier(evidence).map_or(tier, |decision| tier.max(decision))
    }
}

fn scopes(evidence: &Value) -> impl Iterator<Item = &Value> {
    std::iter::once(evidence)
        .chain(evidence.get("issue"))
        .chain(evidence.get("pull_request"))
}

/// Pick the first, middle or last entry in a fast to deep model list
///
/// An explicit model overrides the list
/// Without one, empty or repeated choices are rejected
///
/// ```
/// use drukal::routing::{model_choice, Tier};
///
/// let choices = ["quick", "general", "careful"].map(str::to_owned);
/// assert_eq!(model_choice(None, &choices, Tier::Deep)?, Some("careful"));
/// assert_eq!(model_choice(Some("chosen"), &choices, Tier::Fast)?, Some("chosen"));
/// assert!(model_choice(None, &["".into()], Tier::Fast).is_err());
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn model_choice<'a>(
    explicit: Option<&'a str>,
    choices: &'a [String],
    tier: Tier,
) -> Result<Option<&'a str>> {
    if let Some(model) = explicit {
        if model.trim().is_empty() || model.chars().count() > MAX_CHOICE_CHARS {
            bail!("model must be nonempty and at most {MAX_CHOICE_CHARS} characters");
        }
        return Ok(Some(model));
    }
    if choices.len() > MAX_CHOICES {
        bail!("at most {MAX_CHOICES} model choices are supported");
    }
    for (index, choice) in choices.iter().enumerate() {
        if choice.trim().is_empty() || choice.chars().count() > MAX_CHOICE_CHARS {
            bail!(
                "model choice {} must be nonempty and at most {MAX_CHOICE_CHARS} characters",
                index + 1
            );
        }
        if choices[..index].iter().any(|previous| previous == choice) {
            bail!("model choices must be distinct");
        }
    }
    let choice = match tier {
        Tier::Fast => choices.first(),
        Tier::Balanced => choices.get(choices.len() / 2),
        Tier::Deep => choices.last(),
    };
    Ok(choice.map(String::as_str))
}

fn contains_ascii(text: &str, term: &str) -> bool {
    text.as_bytes()
        .get(..text.len().min(MAX_TEXT_BYTES))
        .unwrap_or_default()
        .windows(term.len())
        .any(|part| part.eq_ignore_ascii_case(term.as_bytes()))
}

fn contains_ascii_word(text: &str, terms: &[&str]) -> bool {
    let text = &text.as_bytes()[..text.len().min(MAX_TEXT_BYTES)];
    text.split(|byte| !byte.is_ascii_alphabetic()).any(|word| {
        terms
            .iter()
            .any(|term| word.eq_ignore_ascii_case(term.as_bytes()))
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn model_choices_preserve_requested_order() {
        assert_eq!(
            parse_model_choices("fast, balanced, deep"),
            ["fast", "balanced", "deep"]
        );
        assert!(parse_model_choices(" ").is_empty());
        assert_eq!(parse_model_choices("fast,,deep"), ["fast", "", "deep"]);
    }

    #[test]
    fn questions_about_fixes_do_not_request_edits() {
        for request in [
            "How do I fix this failure?",
            "Why is this pull request blocked?",
            "What is included in 0.7.0?",
        ] {
            assert_eq!(classify_request(request), Intent::ReadOnly, "{request}");
        }
        assert_eq!(
            classify_request("Can you explain and fix this failure?"),
            Intent::Write
        );
        assert_eq!(
            classify_request("Take a look at the repository"),
            Intent::Ambiguous
        );
    }

    #[test]
    fn each_tier_uses_its_place_in_the_model_list() -> Result<()> {
        let choices = ["fast", "balanced", "deep"].map(str::to_owned);
        for (tier, expected) in [
            (Tier::Fast, "fast"),
            (Tier::Balanced, "balanced"),
            (Tier::Deep, "deep"),
        ] {
            assert_eq!(model_choice(None, &choices, tier)?, Some(expected));
            assert_eq!(model_choice(None, &[], tier)?, None);
            assert_eq!(model_choice(None, &["only".into()], tier)?, Some("only"));
        }
        Ok(())
    }

    #[test]
    fn failed_checks_and_incomplete_diffs_need_a_deep_review() {
        for evidence in [
            json!({"pull_request": {"checks": [{"state": "failure"}]}}),
            json!({"issue": {"complete_diff": false}}),
            json!({"title": "Fix security vulnerability"}),
        ] {
            assert_eq!(select(&evidence), Tier::Deep, "{evidence}");
        }
        assert_eq!(select(&json!({"request": "Summarize this"})), Tier::Fast);
        assert_eq!(
            select(&json!({"files": [{"additions": 2}]})),
            Tier::Balanced
        );
    }

    #[test]
    fn model_lists_have_count_and_text_limits() {
        assert!(model_choice(None, &["x".repeat(MAX_CHOICE_CHARS)], Tier::Fast).is_ok());
        assert!(model_choice(None, &["x".repeat(MAX_CHOICE_CHARS + 1)], Tier::Fast).is_err());
        let choices = (0..MAX_CHOICES)
            .map(|index| format!("model-{index}"))
            .collect::<Vec<_>>();
        assert_eq!(
            model_choice(None, &choices, Tier::Deep).unwrap(),
            Some("model-7")
        );
        let mut too_many = choices;
        too_many.push("extra".into());
        assert!(model_choice(None, &too_many, Tier::Fast).is_err());
    }

    #[test]
    fn explicit_model_wins_and_invalid_choices_fail() {
        let choices = vec!["fast".to_owned(), "fast".to_owned()];
        assert_eq!(
            model_choice(Some("fixed"), &choices, Tier::Deep)
                .unwrap()
                .unwrap(),
            "fixed"
        );
        assert!(model_choice(None, &choices, Tier::Fast).is_err());
        assert!(model_choice(None, &[" ".to_owned()], Tier::Fast).is_err());
    }
}
