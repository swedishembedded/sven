// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Job and schedule types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Opaque job identifier.
pub type JobId = Uuid;

/// Schedule definition for a job.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Schedule {
    /// Execute at a specific UTC timestamp.
    Once {
        /// ISO 8601 UTC datetime.
        at: DateTime<Utc>,
    },
    /// Execute every fixed interval.
    Interval {
        /// Duration string parseable by `humantime` (e.g. `"30m"`, `"1h"`, `"15m"`).
        every: String,
    },
    /// Execute according to a 5-field cron expression.
    ///
    /// Format: `"min hour dom month dow"` (UTC).
    /// Examples:
    /// - `"0 8 * * *"` — daily at 08:00 UTC
    /// - `"*/15 * * * *"` — every 15 minutes
    /// - `"0 9 * * 1"` — every Monday at 09:00 UTC
    Cron {
        /// 5-field cron expression.
        expr: String,
        /// Optional IANA timezone name (e.g. `"America/New_York"`).
        /// When absent, UTC is used.
        timezone: Option<String>,
    },
}

impl Schedule {
    /// Compute the next execution time relative to `now`.
    ///
    /// Returns `None` for `Once` schedules that have already passed.
    pub fn next_after(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            Schedule::Once { at } => {
                if *at > now {
                    Some(*at)
                } else {
                    None
                }
            }
            Schedule::Interval { every } => {
                let dur = humantime::parse_duration(every).ok()?;
                let secs = dur.as_secs();
                Some(now + chrono::Duration::seconds(secs as i64))
            }
            Schedule::Cron { expr, .. } => {
                // The `cron` crate requires a 7-field expression:
                // `sec min hour dom month dow year`.
                // We accept the conventional 5-field format `min hour dom month dow`
                // and expand it automatically.
                let expanded = expand_cron_expr(expr);
                let cron: cron::Schedule = expanded.parse().ok()?;
                cron.after(&now).next()
            }
        }
    }
}

/// A scheduled agent job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    /// Unique identifier.
    pub id: JobId,
    /// Human-readable name.
    pub name: String,
    /// Schedule definition.
    pub schedule: Schedule,
    /// Prompt sent to the agent when this job fires.
    pub prompt: String,
    /// Optional channel to deliver the agent's response to.
    /// Format: `"telegram:<recipient_id>"`, `"discord:<channel_id>"`, etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deliver_to: Option<String>,
    /// When true, the job runs in an isolated agent session rather than
    /// the main persistent session.
    #[serde(default)]
    pub isolated: bool,
    /// Whether this job is active. Disabled jobs are stored but not executed.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// UTC timestamp of the last execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run: Option<DateTime<Utc>>,
    /// UTC timestamp of the next scheduled execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_run: Option<DateTime<Utc>>,
}

fn default_true() -> bool {
    true
}

/// Expand a conventional 5-field cron expression (`min hour dom month dow`)
/// to the 7-field format required by the `cron` crate (`sec min hour dom month dow year`).
///
/// If the expression already has 6 or 7 fields it is returned unchanged.
fn expand_cron_expr(expr: &str) -> String {
    let fields: Vec<&str> = expr.split_whitespace().collect();
    match fields.len() {
        5 => format!("0 {} *", fields.join(" ")),
        6 => format!("{} *", expr.trim()),
        _ => expr.to_string(),
    }
}

impl Job {
    /// Create a new enabled job.
    pub fn new(name: impl Into<String>, schedule: Schedule, prompt: impl Into<String>) -> Self {
        let now = Utc::now();
        let next_run = schedule.next_after(now);
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            schedule,
            prompt: prompt.into(),
            deliver_to: None,
            isolated: false,
            enabled: true,
            last_run: None,
            next_run,
        }
    }

    /// Advance the job's schedule after a successful execution.
    pub fn advance(&mut self) {
        let now = Utc::now();
        self.last_run = Some(now);
        self.next_run = self.schedule.next_after(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_schedule_returns_next() {
        let now = Utc::now();
        let s = Schedule::Interval {
            every: "30m".to_string(),
        };
        let next = s.next_after(now).unwrap();
        assert!(next > now);
        let diff = next - now;
        assert!(diff.num_minutes() >= 29 && diff.num_minutes() <= 31);
    }

    #[test]
    fn cron_schedule_parses() {
        let now = Utc::now();
        let s = Schedule::Cron {
            expr: "0 8 * * *".to_string(),
            timezone: None,
        };
        let next = s.next_after(now);
        assert!(next.is_some());
    }

    #[test]
    fn once_in_past_returns_none() {
        let now = Utc::now();
        let past = now - chrono::Duration::hours(1);
        let s = Schedule::Once { at: past };
        assert!(s.next_after(now).is_none());
    }

    #[test]
    fn once_in_future_returns_some() {
        let now = Utc::now();
        let future = now + chrono::Duration::hours(1);
        let s = Schedule::Once { at: future };
        assert!(s.next_after(now).is_some());
    }

    #[test]
    fn job_advance_updates_last_run() {
        let mut job = Job::new(
            "test",
            Schedule::Interval {
                every: "1h".to_string(),
            },
            "do something",
        );
        assert!(job.last_run.is_none());
        job.advance();
        assert!(job.last_run.is_some());
    }
}
