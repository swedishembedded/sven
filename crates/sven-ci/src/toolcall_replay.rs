// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT

//! Tool-call replay: re-execute recorded tool calls with fresh results.
//!
//! This is used by `--rerun-toolcalls` to replay all tool calls in a loaded
//! JSONL conversation, updating the tool-result messages in-place before
//! seeding the agent.  The model's text responses are preserved so that the
//! re-run reflects the original reasoning with updated tool outputs.

use std::sync::Arc;

use sven_input::ConversationRecord;
use sven_model::{Message, MessageContent, Role};
use sven_tools::{ToolCall, ToolRegistry};

/// Return true if `record` is a ToolResult message with the given call id.
fn is_tool_result_for(record: &ConversationRecord, id: &str) -> bool {
    if let ConversationRecord::Message(Message {
        role: Role::Tool,
        content: MessageContent::ToolResult { tool_call_id, .. },
    }) = record
    {
        tool_call_id == id
    } else {
        false
    }
}

/// Re-execute all tool calls in `records` with fresh results.
///
/// Iterates the records in order, finds each assistant `ToolCall` message, runs
/// the corresponding tool via `tools`, then locates the immediately following
/// `ToolResult` record for the same `tool_call_id` and replaces its content
/// with the new output.
///
/// Returns the number of tool calls that were replayed.
pub async fn replay_tool_calls(
    records: &mut [ConversationRecord],
    tools: &Arc<ToolRegistry>,
) -> usize {
    let mut replayed = 0;

    // Collect (index, tool_call_id, name, args) for all assistant ToolCall records
    // so we can mutate the slice afterwards without conflicting borrows.
    let call_sites: Vec<(usize, String, String, String)> = records
        .iter()
        .enumerate()
        .filter_map(|(i, record)| {
            if let ConversationRecord::Message(Message {
                role: Role::Assistant,
                content:
                    MessageContent::ToolCall {
                        tool_call_id,
                        function,
                    },
            }) = record
            {
                Some((
                    i,
                    tool_call_id.clone(),
                    function.name.clone(),
                    function.arguments.clone(),
                ))
            } else {
                None
            }
        })
        .collect();

    for (call_idx, tool_call_id, name, args_json) in call_sites {
        // Parse the stored JSON arguments.
        let args = serde_json::from_str::<serde_json::Value>(&args_json)
            .unwrap_or(serde_json::Value::Object(Default::default()));

        // Execute the tool call with fresh inputs.
        let tc = ToolCall {
            id: tool_call_id.clone(),
            name,
            args,
        };
        let output = tools.execute(&tc).await;

        // Find the matching ToolResult record after the call and update it in place.
        let after_call = &mut records[call_idx + 1..];
        let mut found = false;
        for slot in after_call.iter_mut() {
            if is_tool_result_for(slot, &tool_call_id) {
                *slot = ConversationRecord::Message(Message::tool_result(
                    &tool_call_id,
                    &output.content,
                ));
                found = true;
                break;
            }
        }
        if found {
            replayed += 1;
        }
    }

    replayed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use sven_model::{FunctionCall, Message, MessageContent, Role};
    use sven_tools::{ApprovalPolicy, Tool, ToolOutput, ToolRegistry};

    struct EchoTool;

    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
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

    #[tokio::test]
    async fn replays_tool_calls_and_updates_results() {
        let mut reg = ToolRegistry::new();
        reg.register(EchoTool);
        let reg = Arc::new(reg);

        let mut records = vec![
            ConversationRecord::Message(Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: "call-1".into(),
                    function: FunctionCall {
                        name: "echo".into(),
                        arguments: r#"{"message":"hello"}"#.into(),
                    },
                },
            }),
            // Stale result that should be replaced
            ConversationRecord::Message(Message::tool_result("call-1", "old result")),
        ];

        let count = replay_tool_calls(&mut records, &reg).await;
        assert_eq!(count, 1);

        if let ConversationRecord::Message(m) = &records[1] {
            match &m.content {
                MessageContent::ToolResult { content, .. } => {
                    let text = content.as_text().unwrap_or("").to_string();
                    assert!(
                        text.contains("echo: hello"),
                        "Expected fresh result, got: {text}"
                    );
                }
                _ => panic!("Expected ToolResult"),
            }
        } else {
            panic!("Expected Message record");
        }
    }

    #[tokio::test]
    async fn handles_unknown_tool_gracefully() {
        let reg = Arc::new(ToolRegistry::new()); // empty registry

        let mut records = vec![
            ConversationRecord::Message(Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: "call-x".into(),
                    function: FunctionCall {
                        name: "nonexistent".into(),
                        arguments: "{}".into(),
                    },
                },
            }),
            ConversationRecord::Message(Message::tool_result("call-x", "stale")),
        ];

        let count = replay_tool_calls(&mut records, &reg).await;
        // Tool execution produces an error result; the record is still updated.
        assert_eq!(count, 1);
        // The result record should now contain an error message (not the stale value)
        if let ConversationRecord::Message(m) = &records[1] {
            if let MessageContent::ToolResult { content, .. } = &m.content {
                let text = content.as_text().unwrap_or("");
                assert_ne!(
                    text, "stale",
                    "stale result should have been replaced by error output"
                );
            }
        }
    }

    #[tokio::test]
    async fn multiple_tool_calls_all_replayed() {
        let mut reg = ToolRegistry::new();
        reg.register(EchoTool);
        let reg = Arc::new(reg);

        let mut records = vec![
            ConversationRecord::Message(Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: "c1".into(),
                    function: FunctionCall {
                        name: "echo".into(),
                        arguments: r#"{"message":"one"}"#.into(),
                    },
                },
            }),
            ConversationRecord::Message(Message::tool_result("c1", "stale-1")),
            ConversationRecord::Message(Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: "c2".into(),
                    function: FunctionCall {
                        name: "echo".into(),
                        arguments: r#"{"message":"two"}"#.into(),
                    },
                },
            }),
            ConversationRecord::Message(Message::tool_result("c2", "stale-2")),
        ];

        let count = replay_tool_calls(&mut records, &reg).await;
        assert_eq!(count, 2, "both tool calls should be replayed");

        let text1 = if let ConversationRecord::Message(m) = &records[1] {
            if let MessageContent::ToolResult { content, .. } = &m.content {
                content.as_text().unwrap_or("").to_string()
            } else {
                panic!("expected ToolResult")
            }
        } else {
            panic!("expected Message")
        };
        assert!(
            text1.contains("echo: one"),
            "first result should be refreshed: {text1}"
        );

        let text2 = if let ConversationRecord::Message(m) = &records[3] {
            if let MessageContent::ToolResult { content, .. } = &m.content {
                content.as_text().unwrap_or("").to_string()
            } else {
                panic!("expected ToolResult")
            }
        } else {
            panic!("expected Message")
        };
        assert!(
            text2.contains("echo: two"),
            "second result should be refreshed: {text2}"
        );
    }

    #[tokio::test]
    async fn non_tool_records_preserved_unchanged() {
        let reg = Arc::new(ToolRegistry::new());
        let user_text = "please do something";
        let assistant_text = "I will use echo";

        let mut records = vec![
            ConversationRecord::Message(Message::user(user_text)),
            ConversationRecord::Message(Message::assistant(assistant_text)),
        ];

        let count = replay_tool_calls(&mut records, &reg).await;
        assert_eq!(count, 0);
        // Records should be completely unchanged
        assert!(
            matches!(&records[0], ConversationRecord::Message(m) if m.as_text() == Some(user_text))
        );
        assert!(
            matches!(&records[1], ConversationRecord::Message(m) if m.as_text() == Some(assistant_text))
        );
    }

    #[tokio::test]
    async fn tool_call_without_matching_result_does_not_count() {
        let mut reg = ToolRegistry::new();
        reg.register(EchoTool);
        let reg = Arc::new(reg);

        // Tool call with no following ToolResult record
        let mut records = vec![
            ConversationRecord::Message(Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: "c-orphan".into(),
                    function: FunctionCall {
                        name: "echo".into(),
                        arguments: r#"{"message":"hi"}"#.into(),
                    },
                },
            }),
            // No ToolResult follows
            ConversationRecord::Message(Message::user("follow-up")),
        ];

        let count = replay_tool_calls(&mut records, &reg).await;
        // No result record to update → not counted
        assert_eq!(count, 0);
    }
}
