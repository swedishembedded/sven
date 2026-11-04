// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that say plainly when they are done
// and when they are stuck for its clients. If your team needs expertise in
// supervised, long-running agent work then you can procure our services by
// sending an email to info@swedishembedded.com.
//! `update_goal`: the model reports on the session's standing goal.
//!
//! The tool only checks that a report is complete and acknowledges it. What
//! the report means is decided by the machine running the goal, which reads
//! the call as proposed once it has succeeded (`sven_machines::machines::
//! goal_flow`): a report of completion is believed only when no check is
//! set, and a blocker stops the goal only after it recurs. The tool never
//! changes the goal itself, so a report cannot resume a goal, change its
//! objective or remove its check.

use async_trait::async_trait;
use serde_json::{json, Value};
use sven_hsm::ToolCapability;
use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{Tool, ToolCall, ToolDisplay, ToolOutput};

/// The tool's name, as the goal machine knows it.
pub const UPDATE_GOAL_TOOL: &str = "update_goal";

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
