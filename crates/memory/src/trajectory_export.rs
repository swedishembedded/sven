// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Exports sven's own recorded trajectories into brain's training-chat
//! format - the procedural half of continuous learning, alongside
//! [`crate::local_study`]'s declarative (document-fact) half.
//!
//! Every `assimilate_fact`-driven study trains the model to recall stated
//! facts. Nothing trains it to get better at completing tasks - and sven
//! already records exactly the data that would: every headless/CI run is
//! stamped, on conclusion, with a real outcome
//! (`sven_session_store::reward::apply_outcome_to_trajectory`,
//! `final_metrics.extra.reward`), and every step already carries the
//! trainable/context mask a fact-only curriculum never needed
//! (`atif::Trajectory::sft_steps`, `TraceStep::is_excluded_from_sft`).
//!
//! This module is the missing link, kept deliberately small: read the
//! trajectories a session already wrote, keep the ones whose stamped reward
//! says the task actually succeeded, and turn each into one row of brain's
//! `generic-messages-v2` JSONL - the exact format
//! `data::chat::ChatSample::from_jsonl` already parses, unchanged. No new
//! brain-side code is needed to consume this file.
//!
//! # Why a file, not a shared type
//!
//! sven and brain do not share a Cargo dependency - the same reason
//! [`crate::local_study`] shells out to a `brain` binary instead of linking
//! `brain-data`. The wire structs below are a hand-spelled mirror of
//! `brain::data::chat`'s `WireRecord`/`WireMessage`/`WireToolCall`/
//! `WireFunction`, not an import; keeping the two in step is a contract
//! documented at both ends, the same discipline `local_study`'s dataset/
//! report round trip already carries.
//!
//! # What this spike does NOT do
//!
//! It does not train anything. It proves the export mapping is sound and
//! produces data brain will actually accept - the decisive question before
//! designing a `TrajectoryCurriculum` at all, since a real trajectory's
//! shape (variable length, variable step count) is what
//! `rl::continual::Curriculum::shape()`'s "every cycle reports an identical
//! shape" invariant was never built for. That design question is next, not
//! this milestone's.
//!
//! Multimodal content (image segments) is dropped, not silently mis-encoded:
//! [`to_wire_record`] extracts only text segments. Extending to real
//! multimodal training rows is future work.
//!
//! Swedish Embedded AB implements solutions for turning an agent's own
//! successful task completions into training data for its clients. If your
//! team needs expertise closing this loop - imitation learning on an
//! agent's own verified-good trajectories, not hand-authored examples -
//! you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::path::{Path, PathBuf};

use atif::{ContentSegment, MessageBody, StepOrigin, Trajectory};
use serde::Serialize;
use sven_session_store::reward::trajectory_reward;

// ---------------------------------------------------------------------------
// The wire shape - a mirror of brain's `data::chat::ChatSample::from_jsonl`
// input, not an import. See the module doc for why.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct WireMessage {
    role: &'static str,
    content: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<WireToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    train: bool,
}

#[derive(Debug, Serialize)]
struct WireToolCall {
    id: Option<String>,
    r#type: &'static str,
    function: WireFunction,
}

#[derive(Debug, Serialize)]
struct WireFunction {
    name: String,
    /// A JSON-ENCODED STRING, never a nested object - brain's own
    /// `WireFunction::arguments` doc comment is explicit that this is the
    /// wire contract, not a stylistic choice.
    arguments: String,
}

#[derive(Debug, Serialize)]
struct WireRecord {
    messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<serde_json::Value>,
}

/// Counts from one export run, so a caller (a human, `sven trajectory
/// export`'s own CLI report) does not have to re-derive them from the
/// output file's line count.
#[derive(Debug, Default, Clone, Serialize)]
pub struct ExportSummary {
    /// Trajectory files found under the runs directory.
    pub scanned: usize,
    /// Skipped: no stamped reward at all (unconcluded, or never scored) -
    /// never treated as a zero.
    pub no_reward: usize,
    /// Skipped: a real reward, below `min_reward`.
    pub below_threshold: usize,
    /// Rows actually written to the output file.
    pub exported: usize,
}

/// Reads every `*.atif.json` trajectory in `runs_dir`, keeps the ones whose
/// [`trajectory_reward`] is `>= min_reward`, and writes one JSONL row per
/// kept trajectory to `out` (overwritten, not appended - a re-run is
/// idempotent over the same runs directory).
///
/// # Errors
///
/// A trajectory file that fails to parse as ATIF fails the whole export
/// loudly, naming the file - a spike over a hand-curated runs directory
/// should not silently drop a malformed one and report a smaller number
/// than the operator expects.
pub fn export_trajectories(runs_dir: &Path, min_reward: f64, out: &Path) -> anyhow::Result<ExportSummary> {
    let mut summary = ExportSummary::default();
    let mut lines = Vec::new();

    for path in trajectory_files(runs_dir)? {
        summary.scanned += 1;
        let text = std::fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        let trajectory: Trajectory = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("{}: not a valid ATIF trajectory: {e}", path.display()))?;

        let Some(reward) = trajectory_reward(&trajectory) else {
            summary.no_reward += 1;
            continue;
        };
        if reward < min_reward {
            summary.below_threshold += 1;
            continue;
        }

        lines.push(serde_json::to_string(&to_wire_record(&trajectory))?);
        summary.exported += 1;
    }

    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut body = lines.join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    std::fs::write(out, body).map_err(|e| anyhow::anyhow!("{}: {e}", out.display()))?;

    Ok(summary)
}

/// Every `*.atif.json` file directly under `dir`, sorted for a deterministic
/// output order across runs.
fn trajectory_files(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| anyhow::anyhow!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| anyhow::anyhow!("{}: {e}", dir.display()))?.path();
        let is_atif_json = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(".atif.json"));
        if is_atif_json {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

/// One trajectory's [`Trajectory::sft_steps`], mapped message-by-message.
///
/// `System`/`User` steps become masked context; `Agent` steps become the one
/// trainable role, with their tool calls carried along, followed by one
/// `"tool"` message per observation result the step produced - mirroring
/// `data::chat::ChatMessage::tool_result`'s own rule that a tool result is
/// never trainable and the template merges consecutive tool turns on its
/// own.
fn to_wire_record(trajectory: &Trajectory) -> WireRecord {
    let mut messages = Vec::new();
    for step in trajectory.sft_steps() {
        let content = text_of(&step.message);
        match step.source {
            StepOrigin::System => messages.push(WireMessage { role: "system", content, tool_calls: Vec::new(), tool_call_id: None, train: false }),
            StepOrigin::User => messages.push(WireMessage { role: "user", content, tool_calls: Vec::new(), tool_call_id: None, train: false }),
            StepOrigin::Agent => {
                let tool_calls = step
                    .tool_calls
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .map(|tc| WireToolCall {
                        id: Some(tc.tool_call_id.clone()),
                        r#type: "function",
                        function: WireFunction {
                            name: tc.function_name.clone(),
                            arguments: serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".to_string()),
                        },
                    })
                    .collect();
                messages.push(WireMessage {
                    role: "assistant",
                    content,
                    tool_calls,
                    tool_call_id: None,
                    // The one trainable role - exactly what `sft_steps`
                    // already gated this step's presence here on.
                    train: true,
                });
                if let Some(observation) = &step.observation {
                    for entry in &observation.results {
                        messages.push(WireMessage {
                            role: "tool",
                            content: entry.content.as_ref().map(text_of).unwrap_or_default(),
                            tool_calls: Vec::new(),
                            tool_call_id: entry.source_call_id.clone(),
                            train: false,
                        });
                    }
                }
            }
        }
    }
    WireRecord { messages, tools: Vec::new() }
}

/// The text form of a step/observation body. Multimodal segments keep only
/// their text runs - image segments are dropped, documented, not silent.
fn text_of(message: &MessageBody) -> String {
    match message {
        MessageBody::Text(s) => s.clone(),
        MessageBody::Segments(segments) => segments
            .iter()
            .filter_map(|seg| match seg {
                ContentSegment::Text { text } => Some(text.as_str()),
                ContentSegment::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use atif::{AgentProfile, StepObservation, ToolInvocation};

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sven-memory-trajectory-export-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn trajectory_with_reward(reward: Option<f64>) -> Trajectory {
        let mut t = Trajectory::new("ATIF-v1.7", AgentProfile::new("sven", "test"));
        t.steps.push(atif::TraceStep::new(1, StepOrigin::System, "be helpful"));
        t.steps.push(atif::TraceStep::new(2, StepOrigin::User, "list files"));
        let mut agent_step = atif::TraceStep::new(3, StepOrigin::Agent, "I'll list the files.");
        agent_step.tool_calls = Some(vec![ToolInvocation::new("call-1", "list_files").with_arguments(serde_json::json!({"dir": "."}))]);
        agent_step.observation = Some(StepObservation::single(atif::ObservationEntry::for_call("call-1", "a.txt\nb.txt")));
        t.steps.push(agent_step);
        if let Some(r) = reward {
            sven_session_store::reward::apply_outcome_to_trajectory(
                &mut t,
                &sven_session_store::reward::SessionOutcome::Scored(
                    sven_session_store::reward::SessionReward { reward: r, outcome: "verified_success", tool_calls: 1, tool_errors: 0 },
                ),
            );
        }
        t
    }

    fn write(dir: &Path, name: &str, t: &Trajectory) {
        std::fs::write(dir.join(name), serde_json::to_string(t).unwrap()).unwrap();
    }

    /// The core mapping: system/user become masked context, the agent turn
    /// becomes the one trainable message with its tool call carried as a
    /// JSON-ENCODED STRING argument (brain's wire contract), and the tool
    /// result that answers it becomes an untrainable `"tool"` message tied
    /// back by `tool_call_id`.
    #[test]
    fn a_rewarded_trajectory_exports_one_row_with_the_agent_turn_trainable() {
        let dir = tmp("basic");
        write(&dir, "run-1.atif.json", &trajectory_with_reward(Some(1.0)));
        let out = dir.join("out.jsonl");

        let summary = export_trajectories(&dir, 0.5, &out).expect("export runs");
        assert_eq!((summary.scanned, summary.exported, summary.no_reward, summary.below_threshold), (1, 1, 0, 0));

        let text = std::fs::read_to_string(&out).unwrap();
        let row: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        let messages = row["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 4, "system, user, assistant, tool");
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["train"], false);
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["train"], true);
        assert_eq!(messages[2]["tool_calls"][0]["function"]["name"], "list_files");
        // Arguments MUST be a JSON string, not a nested object - the exact
        // regression brain's own WireFunction doc comment guards against.
        assert!(messages[2]["tool_calls"][0]["function"]["arguments"].is_string());
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["train"], false);
        assert_eq!(messages[3]["tool_call_id"], "call-1");
    }

    /// A trajectory nobody ever scored must not be silently treated as a
    /// zero-reward failure - it is excluded, and the summary says why.
    #[test]
    fn an_unscored_trajectory_is_excluded_not_treated_as_zero() {
        let dir = tmp("unscored");
        write(&dir, "run-1.atif.json", &trajectory_with_reward(None));
        let out = dir.join("out.jsonl");

        let summary = export_trajectories(&dir, 0.0, &out).expect("export runs");
        assert_eq!((summary.scanned, summary.exported, summary.no_reward), (1, 0, 1));
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "");
    }

    /// A real reward below the threshold is excluded distinctly from an
    /// absent one - the two summary counters must not collapse together.
    #[test]
    fn a_below_threshold_reward_is_excluded_and_counted_separately_from_unscored() {
        let dir = tmp("below");
        write(&dir, "run-1.atif.json", &trajectory_with_reward(Some(0.1)));
        let out = dir.join("out.jsonl");

        let summary = export_trajectories(&dir, 0.9, &out).expect("export runs");
        assert_eq!((summary.exported, summary.no_reward, summary.below_threshold), (0, 0, 1));
    }

    /// Re-running an export over the same runs directory must not double
    /// the output - the file is overwritten, not appended to.
    #[test]
    fn a_repeated_export_overwrites_rather_than_appends() {
        let dir = tmp("repeat");
        write(&dir, "run-1.atif.json", &trajectory_with_reward(Some(1.0)));
        let out = dir.join("out.jsonl");

        export_trajectories(&dir, 0.5, &out).expect("first export");
        export_trajectories(&dir, 0.5, &out).expect("second export");
        assert_eq!(std::fs::read_to_string(&out).unwrap().lines().count(), 1);
    }
}
