//! Run agent tools with bounded output and explicit token budgets

pub mod context;
mod evaluate;
mod harness;
mod laya;
mod process;
pub mod routing;
pub mod session;

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

pub use evaluate::{evaluate, evaluate_cancellable, worker_message};
pub use harness::{
    AgentCommand, command, executable, model_environment, run, run_cancellable, split_command,
    which,
};
pub use process::{MAX_OUTPUT, ProcessOutput, execute, safe_environment};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
/// The installed tool that will run a task
pub enum Harness {
    Codex,
    Claude,
    Command,
}

impl Harness {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Command => "command",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct UsageRecord {
    pub harness: String,
    pub known: bool,
    #[serde(flatten)]
    pub values: BTreeMap<String, u64>,
}

#[derive(Debug, Default)]
/// Token totals retained across calls so retries share the same budget
pub struct Usage {
    max_tokens: Option<u64>,
    records: Vec<UsageRecord>,
    missing: bool,
}

impl Usage {
    /// Start a budget shared by every call in a run
    ///
    /// Reaching the limit blocks the next call
    /// A missing usage total also blocks budgeted work
    ///
    /// ```
    /// use drukal::agent::{Harness, Usage};
    /// use std::collections::BTreeMap;
    ///
    /// # fn main() -> drukal::Result<()> {
    /// let mut usage = Usage::new(Some(10))?;
    /// usage.before_call()?;
    /// usage.record(Harness::Command, Some(BTreeMap::from([
    ///     ("total_tokens".into(), 10),
    /// ])))?;
    /// assert_eq!(usage.total_tokens(), 10);
    /// assert!(usage.before_call().is_err());
    /// # Ok(())
    /// # }
    /// ```
    pub fn new(max_tokens: Option<u64>) -> Result<Self> {
        if max_tokens == Some(0) {
            bail!("token budget must be a positive whole number");
        }
        Ok(Self {
            max_tokens,
            records: Vec::new(),
            missing: false,
        })
    }

    #[must_use]
    /// Return the known total without allowing large counters to wrap
    pub fn total_tokens(&self) -> u64 {
        self.records
            .iter()
            .filter_map(|record| record.values.get("total_tokens"))
            .copied()
            .fold(0, u64::saturating_add)
    }

    /// Refuse another call when usage is missing or the budget is spent
    pub fn before_call(&self) -> Result<()> {
        let Some(maximum) = self.max_tokens else {
            return Ok(());
        };
        if self.missing {
            bail!("token usage is unavailable, so Drukal cannot enforce the budget");
        }
        if self.total_tokens() >= maximum {
            bail!("token budget is exhausted");
        }
        Ok(())
    }

    /// Retain a call’s usage even when it exceeds the budget
    ///
    /// A map without `total_tokens` counts as unknown usage
    /// Budgeted runs return an error rather than treating that call as free
    pub fn record(
        &mut self,
        harness: Harness,
        values: Option<BTreeMap<String, u64>>,
    ) -> Result<()> {
        let known = values
            .as_ref()
            .is_some_and(|values| values.contains_key("total_tokens"));
        if !known {
            self.missing = true;
        }
        self.records.push(UsageRecord {
            harness: harness.as_str().to_owned(),
            known,
            values: values.unwrap_or_default(),
        });
        if !known && self.max_tokens.is_some() {
            bail!("token usage is unavailable, so Drukal cannot enforce the budget");
        }
        if self.max_tokens.is_some_and(|maximum| {
            self.records
                .iter()
                .filter_map(|record| record.values.get("total_tokens"))
                .try_fold(0_u64, |total, count| total.checked_add(*count))
                .is_none_or(|total| total > maximum)
        }) {
            bail!("token budget was exceeded");
        }
        Ok(())
    }

    pub fn record_unknown(&mut self, harness: Harness) {
        let _ = self.record(harness, None);
    }

    #[must_use]
    pub fn complete(&self, agents: usize) -> bool {
        !self.missing && agents == 1
    }

    #[must_use]
    pub fn records(&self) -> &[UsageRecord] {
        &self.records
    }

    #[must_use]
    pub const fn maximum(&self) -> Option<u64> {
        self.max_tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_budget_fails_closed() -> Result<()> {
        let mut usage = Usage::new(Some(4))?;
        usage
            .record(
                Harness::Command,
                Some(BTreeMap::from([("total_tokens".to_owned(), 5)])),
            )
            .expect_err("budget must reject overshoot");
        assert_eq!(usage.total_tokens(), 5);
        Ok(())
    }

    #[test]
    fn exact_budget_stops_the_next_call() -> Result<()> {
        let mut usage = Usage::new(Some(4))?;
        usage.before_call()?;
        usage.record(
            Harness::Command,
            Some(BTreeMap::from([("total_tokens".into(), 4)])),
        )?;
        assert_eq!(usage.total_tokens(), 4);
        assert!(
            usage
                .before_call()
                .unwrap_err()
                .to_string()
                .contains("exhausted")
        );
        Ok(())
    }

    #[test]
    fn missing_total_cannot_bypass_a_budget() -> Result<()> {
        let mut usage = Usage::new(Some(4))?;
        let error = usage
            .record(Harness::Command, Some(BTreeMap::new()))
            .unwrap_err();
        assert!(error.to_string().contains("unavailable"));
        assert!(!usage.complete(1));
        assert!(!usage.records()[0].known);
        assert!(usage.before_call().is_err());
        Ok(())
    }

    #[test]
    fn token_totals_do_not_wrap() -> Result<()> {
        let mut usage = Usage::new(None)?;
        for total in [u64::MAX, 1] {
            usage.record(
                Harness::Command,
                Some(BTreeMap::from([("total_tokens".into(), total)])),
            )?;
        }
        assert_eq!(usage.total_tokens(), u64::MAX);
        Ok(())
    }

    #[test]
    fn the_largest_budget_still_rejects_an_overflow() -> Result<()> {
        let mut usage = Usage::new(Some(u64::MAX))?;
        usage.record(
            Harness::Command,
            Some(BTreeMap::from([("total_tokens".into(), u64::MAX - 1)])),
        )?;
        usage.before_call()?;
        let error = usage
            .record(
                Harness::Command,
                Some(BTreeMap::from([("total_tokens".into(), 2)])),
            )
            .unwrap_err();
        assert!(error.to_string().contains("exceeded"));
        assert_eq!(usage.total_tokens(), u64::MAX);
        assert_eq!(usage.records().len(), 2);
        assert!(usage.before_call().is_err());
        Ok(())
    }
}
