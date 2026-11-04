// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that say plainly when they are done
// and when they are stuck for its clients. If your team needs expertise in
// supervised, long-running agent work then you can procure our services by
// sending an email to info@swedishembedded.com.
//! `update_goal` and `set_goal`: the model reports on the session's
//! standing goal, and sets one when the person asks it to.
//!
//! The tool only checks that a report is complete and acknowledges it. What
//! the report means is decided by the machine running the goal, which reads
//! the call as proposed once it has succeeded (`sven_machines::machines::
//! goal_flow`): a report of completion is believed only when no check is
//! set, and a blocker stops the goal only after it recurs. The tool never
//! changes the goal itself, so a report cannot resume a goal, change its
//! objective or remove its check.
//!
//! `set_goal` is how a person has the agent word the goal for them ("set the
//! best goal for this session"). The machine sets it when the reply ends,
//! and only in a turn the person started: during the goal's own turns the
//! model cannot change what it is working toward.

use async_trait::async_trait;
use serde_json::{json, Value};
use sven_hsm::ToolCapability;
use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{Tool, ToolCall, ToolDisplay, ToolOutput};

pub use sven_vocab::goal::{SET_GOAL_TOOL, UPDATE_GOAL_TOOL};

/// Reports that the goal is complete or blocked.
#[derive(Default)]
pub struct UpdateGoalTool;

#[async_trait]
impl Tool for UpdateGoalTool {
    fn name(&self) -> &str {
        UPDATE_GOAL_TOOL
    }

    fn summary(&self) -> &str {
        "Report that the session's standing goal is complete (with evidence) or blocked."
    }

    fn description(&self) -> &str {
        "Report on the standing goal the person set. status \"complete\": every requirement \
         is met and you checked it; give the evidence. If the goal has a check, the check \
         decides, not this report. status \"blocked\": something outside your control stops \
         you (a permission, a credential, a service, a decision only the person can make); say \
         what fails (blocker), the evidence, what is completed, what remains, what you need \
         (request) and what must be true to resume (resume_condition). The same blocker \
         reported on consecutive turns stops the goal, so first look for another way."
    }

    fn parameters_schema(&self) -> Value {
        let text = |about: &str| json!({ "type": "string", "description": about });
        json!({
            "type": "object",
            "properties": {
                "status": { "type": "string", "enum": ["complete", "blocked"] },
                "evidence": text("What shows it: test results, files, command output"),
                "blocker": text("The operation that fails and why (blocked)"),
                "completed": text("What is done toward the goal (blocked)"),
                "remaining": text("What is left to do (blocked)"),
                "verification": text("What was checked and how it turned out (blocked)"),
                "request": text("What the person has to do (blocked)"),
                "resume_condition": text("What must be true before work can go on (blocked)"),
            },
            "required": ["status"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    /// A report changes nothing outside the session.
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let given = |key: &str| {
            call.args
                .get(key)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.trim().is_empty())
        };
        match call.args.get("status").and_then(Value::as_str) {
            Some("complete") if given("evidence") => ToolOutput::ok(
                &call.id,
                "Recorded: you report the goal complete. If it has a check, the check decides.",
            ),
            Some("complete") => ToolOutput::err(
                &call.id,
                "`evidence` is required: say what shows it is complete",
            ),
            Some("blocked") => {
                let missing: Vec<&str> = ["blocker", "evidence", "request", "resume_condition"]
                    .into_iter()
                    .filter(|key| !given(key))
                    .collect();
                if missing.is_empty() {
                    ToolOutput::ok(
                        &call.id,
                        "Recorded: you report the goal blocked. If the same blocker stops the \
                         next turns too, the goal stops and the person is asked for help.",
                    )
                } else {
                    ToolOutput::err(
                        &call.id,
                        format!("a blocked report needs: {}", missing.join(", ")),
                    )
                }
            }
            _ => ToolOutput::err(&call.id, "`status` must be \"complete\" or \"blocked\""),
        }
    }
}

impl ToolDisplay for UpdateGoalTool {
    fn display_name(&self) -> &str {
        "Goal"
    }
    fn icon(&self) -> &str {
        "◎"
    }
    fn category(&self) -> &str {
        "system"
    }
    fn collapsed_summary(&self, args: &Value) -> String {
        args.get("status")
            .and_then(Value::as_str)
            .unwrap_or("report")
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn run(args: Value) -> ToolOutput {
        UpdateGoalTool
            .execute(&ToolCall {
                id: "c1".into(),
                name: UPDATE_GOAL_TOOL.into(),
                args,
            })
            .await
    }

    #[tokio::test]
    async fn a_report_must_say_what_shows_it() {
        assert!(
            !run(json!({"status": "complete", "evidence": "12 tests pass"}))
                .await
                .is_error
        );
        assert!(run(json!({"status": "complete"})).await.is_error);
        assert!(run(json!({"status": "done"})).await.is_error);
    }

    #[tokio::test]
    async fn a_blocked_report_must_name_what_the_person_has_to_do() {
        let partial = run(json!({"status": "blocked", "blocker": "no access"})).await;
        assert!(partial.is_error);
        assert!(partial.content.contains("request"), "{}", partial.content);
        let full = run(json!({
            "status": "blocked",
            "blocker": "Permission denied on /srv",
            "evidence": "EACCES",
            "request": "grant write access",
            "resume_condition": "/srv is writable",
        }))
        .await;
        assert!(!full.is_error);
    }

    #[test]
    fn the_summary_fits_the_deferred_listing() {
        assert!(UpdateGoalTool.summary().len() <= 100);
    }
}

/// Sets the session's standing goal, when the person asked for it.
#[derive(Default)]
pub struct SetGoalTool;

#[async_trait]
impl Tool for SetGoalTool {
    fn name(&self) -> &str {
        SET_GOAL_TOOL
    }

    fn summary(&self) -> &str {
        "Set the session's standing goal when the person asks you to set or word one."
    }

    fn description(&self) -> &str {
        "Set the standing goal the session then keeps working toward, turn after turn, until \
         it is met. Use it only when the person asked you to set, word or change the goal; \
         never to give yourself one. Word the objective so that it says what done looks like. \
         Give `check` (a command and its arguments) when a command can decide it, such as a \
         test run. The goal is set when this reply ends; it cannot be changed from inside the \
         goal's own turns."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "objective": { "type": "string", "description": "What to achieve, worded so that done is recognisable" },
                "max_turns": { "type": "integer", "minimum": 1, "maximum": sven_vocab::goal::MAX_TURNS_LIMIT },
                "check": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "A command that passes when the goal is met, as a program and its arguments"
                },
            },
            "required": ["objective"],
            "additionalProperties": false
        })
    }

    /// A person who approves calls approves this one too.
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Ask
    }

    /// Nothing outside the session changes.
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let objective = call
            .args
            .get("objective")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if objective.trim().is_empty() {
            return ToolOutput::err(&call.id, "`objective` is required");
        }
        ToolOutput::ok(
            &call.id,
            "The goal is set when this reply ends, and the session then works toward it. End \
             your reply by saying what you set.",
        )
    }
}

impl ToolDisplay for SetGoalTool {
    fn display_name(&self) -> &str {
        "Set goal"
    }
    fn icon(&self) -> &str {
        "◎"
    }
    fn category(&self) -> &str {
        "system"
    }
    fn collapsed_summary(&self, args: &Value) -> String {
        args.get("objective")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .chars()
            .take(60)
            .collect()
    }
}
