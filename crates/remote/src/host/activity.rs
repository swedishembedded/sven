// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements solutions for running agent sessions on a
// person's own machine for a server for its clients. If your team needs
// expertise in remotely driven agents then you can procure our services by
// sending an email to info@swedishembedded.com.

//! What a session's host prints about its work, one line each: enough to
//! tell from the log alone what a turn did and why it ended. A call is
//! named with the arguments that say what it was asked (the model, the path,
//! the command), a failed call with how long it ran and its reason, a slow
//! call with how long it took, and a turn with how long it ran and how it
//! ended; a message held behind a turn says what that turn is doing.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::printable::{call_summary, printable};

/// A call that took at least this long is logged when it finishes well too.
const SLOW_CALL: Duration = Duration::from_secs(5);

/// The longest reason a failed call's line carries.
const REASON_CHARS: usize = 200;

/// The turn of a session and the calls running in it.
#[derive(Debug, Default)]
pub(super) struct Activity {
    turn_started: Option<Instant>,
    calls: HashMap<String, (String, Instant)>,
}

impl Activity {
    /// A turn starts now.
    pub(super) fn turn_started(&mut self, now: Instant) {
        self.turn_started = Some(now);
        self.calls.clear();
    }

    /// The line for a call starting: its name and what it was asked.
    pub(super) fn call_started(
        &mut self,
        id: &str,
        name: &str,
        args: &serde_json::Value,
        now: Instant,
    ) -> String {
        self.calls.insert(id.to_string(), (name.to_string(), now));
        call_summary(name, args)
    }

    /// The line for a call finishing, if it says something: a failure, with
    /// how long it ran and its reason, or a slow success.
    pub(super) fn call_finished(
        &mut self,
        id: &str,
        name: &str,
        output: &str,
        is_error: bool,
        now: Instant,
    ) -> Option<String> {
        let took = self
            .calls
            .remove(id)
            .map(|(_, at)| now.saturating_duration_since(at));
        let after = took
            .map(|d| format!(" after {}", duration(d)))
            .unwrap_or_default();
        if is_error {
            return Some(format!(
                "{} failed{after}: {}",
                printable(name),
                reason(output)
            ));
        }
        took.filter(|d| *d >= SLOW_CALL)
            .map(|d| format!("{} finished in {}", printable(name), duration(d)))
    }

    /// The line for the turn ending: how it ended and how long it ran.
    pub(super) fn turn_ended(&mut self, stopped: bool, now: Instant) -> String {
        let ran = self
            .turn_started
            .take()
            .map(|at| duration(now.saturating_duration_since(at)));
        self.calls.clear();
        match (stopped, ran) {
            (false, Some(ran)) => format!("done in {ran}"),
            (true, Some(ran)) => format!("stopped after {ran}"),
            (false, None) => "done".to_string(),
            (true, None) => "stopped".to_string(),
        }
    }

    /// What the running turn is doing: how long it has run, and the calls
    /// it is waiting on.
    pub(super) fn running(&self, now: Instant) -> String {
        let ran = self
            .turn_started
            .map(|at| format!("running {}", duration(now.saturating_duration_since(at))))
            .unwrap_or_else(|| "running".to_string());
        let mut calls: Vec<(&String, &Instant)> =
            self.calls.values().map(|(name, at)| (name, at)).collect();
        calls.sort_by_key(|(_, at)| **at);
        if calls.is_empty() {
            return ran;
        }
        let waiting: Vec<String> = calls
            .iter()
            .map(|(name, at)| {
                format!(
                    "{} for {}",
                    printable(name),
                    duration(now.saturating_duration_since(**at))
                )
            })
            .collect();
        format!("{ran}, in {}", waiting.join(", "))
    }
}

/// The first line of a failure, bounded.
fn reason(output: &str) -> String {
    let line = output
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("no reason given");
    let line = printable(line).into_owned();
    if line.chars().count() <= REASON_CHARS {
        return line;
    }
    let cut: String = line.chars().take(REASON_CHARS).collect();
    format!("{cut}…")
}

/// A duration as a person reads it in a log: 850ms, 12s, 4m09s, 1h02m.
fn duration(d: Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0 => format!("{}ms", d.as_millis()),
        1..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m{:02}s", secs / 60, secs % 60),
        _ => format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_says_what_it_asked_why_a_call_failed_and_how_it_ended() {
        let start = Instant::now();
        let mut log = Activity::default();
        log.turn_started(start);
        let args = serde_json::json!({
            "model": "google/veo-3.1-fast",
            "operation": "video",
            "prompt": "a car falls from the sky and breaks apart over his head, and he says his line",
            "params": {"duration": 8},
        });
        let line = log.call_started("c1", "run_model", &args, start);
        assert!(line.starts_with("run_model {"), "{line}");
        assert!(
            line.contains("model: google/veo-3.1-fast") && line.contains("operation: video"),
            "{line}"
        );

        let held = log.running(start + Duration::from_secs(192));
        assert_eq!(held, "running 3m12s, in run_model for 3m12s");

        let failed = log
            .call_finished(
                "c1",
                "run_model",
                "HTTP 422 invalid_request: the model takes at most 8 seconds of audio\nmore",
                true,
                start + Duration::from_secs(249),
            )
            .unwrap();
        assert_eq!(failed, "run_model failed after 4m09s: HTTP 422 invalid_request: the model takes at most 8 seconds of audio");

        log.call_started(
            "c2",
            "read_file",
            &serde_json::json!({"path": "src/main.rs"}),
            start,
        );
        assert_eq!(
            log.call_finished(
                "c2",
                "read_file",
                "fn main",
                false,
                start + Duration::from_millis(300)
            ),
            None,
            "a quick success says nothing"
        );

        assert_eq!(
            log.turn_ended(false, start + Duration::from_secs(252)),
            "done in 4m12s"
        );
        log.turn_started(start);
        assert_eq!(
            log.turn_ended(true, start + Duration::from_secs(9)),
            "stopped after 9s"
        );
    }

    #[test]
    fn a_long_reason_is_cut_and_durations_read_as_a_person_reads_them() {
        assert!(reason(&"x".repeat(500)).ends_with('…'));
        assert_eq!(reason(""), "no reason given");
        assert_eq!(duration(Duration::from_millis(850)), "850ms");
        assert_eq!(duration(Duration::from_secs(3720)), "1h02m");
    }
}
