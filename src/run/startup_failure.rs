// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for unattended agent runs that
// always leave a record, for its clients. If your team needs expertise in
// auditable agent pipelines then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The record of a headless run that failed before a session existed.
//!
//! A run that reaches the runner writes its own trajectory whatever its end.
//! One that stops earlier (no API key, a malformed `network` section, an
//! unreadable input file) has no session to write one, so the caller that
//! passed `--output-trace` would find nothing. [`record`] writes a valid
//! trajectory at that path whose outcome is `startup_error` and which carries
//! the error and the prompt.

use std::path::Path;
use std::time::SystemTime;

use atif::{StepOrigin, TraceStep, Trajectory};
use sven_session_store::trace_session;

/// Writes the failure record to `path` unless the run itself wrote a
/// trajectory there since `started`. A write failure is reported, never
/// raised: the run's own error stays the one that matters.
pub(crate) fn record(
    path: &Path,
    error: &anyhow::Error,
    prompt: Option<&str>,
    started: SystemTime,
) {
    let written_by_run = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .is_ok_and(|modified| modified >= started);
    if written_by_run {
        return;
    }
    let mut trajectory = Trajectory::new(
        trace_session::ATIF_SCHEMA_VERSION,
        trace_session::default_agent_profile(),
    );
    trajectory.session_id = Some(trace_session::new_session_id());
    let mut steps = Vec::new();
    if let Some(prompt) = prompt.filter(|p| !p.is_empty()) {
        steps.push(TraceStep::new(1, StepOrigin::User, prompt));
    }
    steps.push(TraceStep::new(
        steps.len() as u64 + 1,
        StepOrigin::System,
        format!("the run failed before a session started: {error:#}"),
    ));
    trajectory.steps = steps;
    let metrics = trajectory
        .final_metrics
        .get_or_insert_with(Default::default);
    metrics.extra = Some(serde_json::json!({
        "outcome": "startup_error",
        "error": format!("{error:#}"),
    }));
    match atif::persist::write_trajectory_atomic(path, &trajectory, None) {
        Ok(()) => eprintln!("[sven:trace] Failure record written to {}", path.display()),
        Err(e) => eprintln!("[sven:warn] Failed to write trace {}: {e}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_start_leaves_a_valid_record_with_the_error_and_the_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.json");
        record(
            &path,
            &anyhow::anyhow!("No API key found"),
            Some("fix it"),
            SystemTime::UNIX_EPOCH,
        );
        let doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(doc["final_metrics"]["extra"]["outcome"], "startup_error");
        assert!(doc["final_metrics"]["extra"]["error"]
            .as_str()
            .unwrap()
            .contains("No API key"));
        assert_eq!(doc["steps"][0]["message"], "fix it");
    }

    #[test]
    fn a_trajectory_the_run_wrote_itself_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.json");
        std::fs::write(&path, "the run's own").unwrap();
        let before = SystemTime::now() - std::time::Duration::from_secs(60);
        record(&path, &anyhow::anyhow!("late failure"), None, before);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "the run's own");
    }
}
