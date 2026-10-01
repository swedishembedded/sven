// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

//! Tool-call replay: re-execute recorded tool calls with fresh results.
//!
//! This is used by `--rerun-toolcalls` to replay all tool calls recorded in
//! a loaded ATIF trajectory (`--load-trace`/`--trace`), updating each
//! matching observation result in-place before seeding the agent. The
//! model's text and reasoning content are preserved so the re-run reflects
//! the original reasoning with updated tool outputs.

use std::sync::Arc;

use atif::{MessageBody, TraceStep};
use sven_tool_registry::{ToolCall, ToolRegistry};

/// Re-execute every tool call recorded across `steps` with fresh results.
///
/// For each `TraceStep` that carries `tool_calls`, runs each invocation
/// through `tools` and replaces the matching `observation.results` entry's
/// `content` (matched by `source_call_id == tool_call_id`) with the fresh
/// output. A tool call with no matching observation entry is still executed
/// (for its side effects) but does not count toward the returned total,
/// since there is nowhere to record its result.
///
/// Returns the number of tool calls that were replayed (i.e. had a matching
/// observation entry updated).
pub async fn replay_tool_calls(steps: &mut [TraceStep], tools: &Arc<ToolRegistry>) -> usize {
    let mut replayed = 0;

    for step in steps.iter_mut() {
        let Some(tool_calls) = step.tool_calls.clone() else {
            continue;
        };

        for call in &tool_calls {
            let tc = ToolCall {
                id: call.tool_call_id.clone(),
                name: call.function_name.clone(),
                args: call.arguments.clone(),
            };
            let output = tools.execute(&tc).await;

            let Some(observation) = step.observation.as_mut() else {
                continue;
            };
            let Some(entry) = observation
                .results
                .iter_mut()
                .find(|r| r.source_call_id.as_deref() == Some(call.tool_call_id.as_str()))
            else {
                continue;
            };
            entry.content = Some(MessageBody::text(output.content));
            replayed += 1;
        }
    }

    replayed
}

#[cfg(test)]
mod tests {
    use super::*;
    use atif::{ObservationEntry, StepObservation, StepOrigin, ToolInvocation};
    use std::sync::Arc;
    use sven_tool_registry::{ApprovalPolicy, Tool, ToolOutput, ToolRegistry};

    struct EchoTool;

    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn kernel_capability(&self) -> sven_tool_api::ToolCapability {
            sven_tool_api::ToolCapability::ReadFile
        }
        fn description(&self) -> &str {
            "echoes message"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn default_policy(&self) -> ApprovalPolicy {
            ApprovalPolicy::Auto
        }
        async fn execute(&self, call: &ToolCall) -> ToolOutput {
            let msg = call
                .args
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("(none)");
            ToolOutput::ok(&call.id, format!("echo: {msg}"))
        }
    }

    /// Build a single agent step with one tool call and a stale observation
    /// result awaiting replay.
    fn step_with_call(
        id: &str,
        name: &str,
        args: serde_json::Value,
        stale_result: &str,
    ) -> TraceStep {
        let mut step = TraceStep::new(1, StepOrigin::Agent, "");
        step.tool_calls = Some(vec![ToolInvocation::new(id, name).with_arguments(args)]);
        step.observation = Some(StepObservation::single(ObservationEntry::for_call(
            id,
            stale_result,
        )));
        step
    }

    fn observation_text(step: &TraceStep, call_id: &str) -> String {
        step.observation
            .as_ref()
            .expect("observation present")
            .results
            .iter()
            .find(|r| r.source_call_id.as_deref() == Some(call_id))
            .and_then(|r| r.content.as_ref())
            .and_then(|c| c.as_text())
            .unwrap_or("")
            .to_string()
    }

    #[tokio::test]
    async fn replays_tool_calls_and_updates_results() {
        let mut reg = ToolRegistry::new();
        reg.register(EchoTool);
        let reg = Arc::new(reg);

        let mut steps = vec![step_with_call(
            "call-1",
            "echo",
            serde_json::json!({"message":"hello"}),
            "old result",
        )];

        let count = replay_tool_calls(&mut steps, &reg).await;
        assert_eq!(count, 1);

        let text = observation_text(&steps[0], "call-1");
        assert!(
            text.contains("echo: hello"),
            "Expected fresh result, got: {text}"
        );
    }

    #[tokio::test]
    async fn handles_unknown_tool_gracefully() {
        let reg = Arc::new(ToolRegistry::new()); // empty registry

        let mut steps = vec![step_with_call(
            "call-x",
            "nonexistent",
            serde_json::json!({}),
            "stale",
        )];

        let count = replay_tool_calls(&mut steps, &reg).await;
        // Tool execution produces an error result; the observation entry is
        // still updated.
        assert_eq!(count, 1);
        let text = observation_text(&steps[0], "call-x");
        assert_ne!(
            text, "stale",
            "stale result should have been replaced by error output"
        );
    }

    #[tokio::test]
    async fn multiple_tool_calls_all_replayed() {
        let mut reg = ToolRegistry::new();
        reg.register(EchoTool);
        let reg = Arc::new(reg);

        let mut step = TraceStep::new(1, StepOrigin::Agent, "");
        step.tool_calls = Some(vec![
            ToolInvocation::new("c1", "echo").with_arguments(serde_json::json!({"message":"one"})),
            ToolInvocation::new("c2", "echo").with_arguments(serde_json::json!({"message":"two"})),
        ]);
        step.observation = Some(StepObservation {
            results: vec![
                ObservationEntry::for_call("c1", "stale-1"),
                ObservationEntry::for_call("c2", "stale-2"),
            ],
        });
        let mut steps = vec![step];

        let count = replay_tool_calls(&mut steps, &reg).await;
        assert_eq!(count, 2, "both tool calls should be replayed");

        let text1 = observation_text(&steps[0], "c1");
        assert!(
            text1.contains("echo: one"),
            "first result should be refreshed: {text1}"
        );
        let text2 = observation_text(&steps[0], "c2");
        assert!(
            text2.contains("echo: two"),
            "second result should be refreshed: {text2}"
        );
    }

    #[tokio::test]
    async fn non_tool_steps_preserved_unchanged() {
        let reg = Arc::new(ToolRegistry::new());
        let user_text = "please do something";
        let assistant_text = "I will use echo";

        let mut steps = vec![
            TraceStep::new(1, StepOrigin::User, user_text),
            TraceStep::new(2, StepOrigin::Agent, assistant_text),
        ];

        let count = replay_tool_calls(&mut steps, &reg).await;
        assert_eq!(count, 0);
        // Steps should be completely unchanged.
        assert_eq!(steps[0].message.as_text(), Some(user_text));
        assert_eq!(steps[1].message.as_text(), Some(assistant_text));
    }

    #[tokio::test]
    async fn tool_call_without_matching_observation_does_not_count() {
        let mut reg = ToolRegistry::new();
        reg.register(EchoTool);
        let reg = Arc::new(reg);

        // Tool call with no observation recorded at all.
        let mut step = TraceStep::new(1, StepOrigin::Agent, "");
        step.tool_calls = Some(vec![ToolInvocation::new("c-orphan", "echo")
            .with_arguments(serde_json::json!({"message":"hi"}))]);
        let mut steps = vec![step];

        let count = replay_tool_calls(&mut steps, &reg).await;
        // No observation entry to update → not counted.
        assert_eq!(count, 0);
    }
}
