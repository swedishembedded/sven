// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
/// Integration tests for sven's core logic using the mock model provider.
use std::sync::Arc;

use sven_config::{AgentConfig, AgentMode, Config};
use sven_core::{Agent, AgentRuntimeContext};
use sven_input::{parse_conversation, parse_workflow, serialize_conversation_turn};
use sven_model::{Message, MockProvider, Role};
use sven_tools::{events::ToolEvent, ToolRegistry};
use tokio::sync::{mpsc, Mutex};

fn mock_agent(mode: AgentMode) -> Agent {
    let model: Arc<dyn sven_model::ModelProvider> = Arc::new(MockProvider);
    let tools = Arc::new(ToolRegistry::default());
    let config = Arc::new(AgentConfig::default());
    let mode_lock = Arc::new(Mutex::new(mode));
    let (_tx, tool_event_rx) = mpsc::channel::<ToolEvent>(64);
    Agent::new(
        model,
        tools,
        config,
        AgentRuntimeContext::default(),
        mode_lock,
        tool_event_rx,
        128_000,
    )
}

#[tokio::test]
async fn agent_returns_mock_response() {
    let mut agent = mock_agent(AgentMode::Agent);
    let (tx, mut rx) = mpsc::channel(64);
    agent.submit("hello", tx).await.unwrap();

    let mut got_text = false;
    while let Ok(event) = rx.try_recv() {
        if let sven_core::AgentEvent::TextDelta(t) = event {
            assert!(t.contains("MOCK"));
            got_text = true;
        }
    }
    assert!(got_text, "expected at least one TextDelta event");
}

#[test]
fn workflow_parsing_single_step_fallback() {
    // Plain text with no H2 headings → single fallback step
    let w = parse_workflow("Do something useful.");
    assert_eq!(w.steps.len(), 1);
}

#[test]
fn workflow_parsing_multiple_h2() {
    let md = "## First\nContent one.\n\n## Second\nContent two.";
    let mut w = parse_workflow(md);
    assert_eq!(w.steps.len(), 2);
    let s = w.steps.pop().unwrap();
    assert_eq!(s.label.as_deref(), Some("First"));
}

#[test]
fn workflow_parsing_preamble_goes_to_system_prompt() {
    // Preamble text is NOT a step; it becomes system_prompt_append
    let md = "Intro text.\n\n## Step A\nDo this.";
    let mut w = parse_workflow(md);
    assert_eq!(w.steps.len(), 1, "preamble must not become a step");
    assert!(w
        .system_prompt_append
        .as_deref()
        .map(|s| s.contains("Intro"))
        .unwrap_or(false));
    assert_eq!(w.steps.pop().unwrap().label.as_deref(), Some("Step A"));
}

#[test]
fn workflow_parsing_h1_is_title_not_step() {
    let md = "# My Project\n\nContext text.\n\n## Do work\nThe task.";
    let w = parse_workflow(md);
    assert_eq!(w.title.as_deref(), Some("My Project"));
    assert_eq!(w.steps.len(), 1, "H1 must not create a step");
}

#[test]
fn config_defaults_are_valid() {
    let cfg = Config::default();
    assert_eq!(cfg.model.provider, "openai");
    assert!(cfg.agent.max_tool_rounds > 0);
    assert!(cfg.agent.compaction_threshold > 0.0);
}

#[test]
fn tool_policy_auto_approve() {
    use sven_config::ToolsConfig;
    use sven_tools::{ApprovalPolicy, ToolPolicy};

    let cfg = ToolsConfig::default();
    let policy = ToolPolicy::from_config(&cfg);
    assert_eq!(policy.decide("cat /etc/hosts"), ApprovalPolicy::Auto);
    assert_eq!(policy.decide("ls /tmp"), ApprovalPolicy::Auto);
}

#[test]
fn tool_policy_deny() {
    use sven_config::ToolsConfig;
    use sven_tools::{ApprovalPolicy, ToolPolicy};

    let cfg = ToolsConfig {
        deny_patterns: vec!["rm -rf /*".into()],
        ..ToolsConfig::default()
    };
    let policy = ToolPolicy::from_config(&cfg);
    assert_eq!(policy.decide("rm -rf /*"), ApprovalPolicy::Deny);
}

#[tokio::test]
async fn shell_tool_executes_echo() {
    use sven_tools::Tool;
    use sven_tools::{ShellTool, ToolCall};

    let tool = ShellTool::default();
    let call = ToolCall {
        id: "1".into(),
        name: "shell".into(),
        args: serde_json::json!({ "shell_command": "echo hello_world" }),
    };
    let output = tool.execute(&call).await;
    assert!(!output.is_error);
    assert!(output.content.contains("hello_world"));
}

#[tokio::test]
async fn fs_tool_write_read_roundtrip() {
    use sven_tools::{ReadFileTool, Tool, ToolCall, WriteTool};

    let path = format!("/tmp/sven_test_{}.txt", uuid::Uuid::new_v4());

    let write_call = ToolCall {
        id: "w1".into(),
        name: "write_file".into(),
        args: serde_json::json!({ "path": path, "text": "roundtrip", "append": false }),
    };
    let wo = WriteTool.execute(&write_call).await;
    assert!(!wo.is_error, "write failed: {}", wo.content);

    let read_call = ToolCall {
        id: "r1".into(),
        name: "read_file".into(),
        args: serde_json::json!({ "path": path }),
    };
    let ro = ReadFileTool.execute(&read_call).await;
    assert!(!ro.is_error);
    assert!(ro.content.contains("roundtrip"));

    let _ = std::fs::remove_file(&path);
}

// ── Conversation parsing integration tests ────────────────────────────────────

#[test]
fn conversation_parse_fixture_file() {
    let md = std::fs::read_to_string("tests/fixtures/conversation.md")
        .expect("conversation fixture must exist");
    let conv = parse_conversation(&md).expect("fixture must parse cleanly");
    // Fixture has title + 2 complete turns + 1 pending user section
    assert_eq!(conv.title.as_deref(), Some("Test Conversation"));
    assert_eq!(conv.history.len(), 2, "two complete messages in history");
    assert!(
        conv.pending_user_input.is_some(),
        "trailing ## User is pending"
    );
    assert_eq!(
        conv.pending_user_input.as_deref().unwrap().trim(),
        "What did you echo?"
    );
}

#[test]
fn conversation_parse_empty_file() {
    let conv = parse_conversation("").expect("empty file must parse");
    assert!(conv.history.is_empty());
    assert!(conv.pending_user_input.is_none());
}

#[test]
fn conversation_parse_only_user_section() {
    let md = "## User\nFirst task\n";
    let conv = parse_conversation(md).expect("must parse");
    assert!(conv.history.is_empty());
    assert_eq!(conv.pending_user_input.as_deref(), Some("First task"));
}

#[test]
fn conversation_parse_complete_exchange_no_pending() {
    let md = "## User\nTask\n\n## Sven\nDone\n";
    let conv = parse_conversation(md).expect("must parse");
    assert_eq!(conv.history.len(), 2);
    assert!(conv.pending_user_input.is_none());
}

#[test]
fn conversation_round_trip_text_only() {
    let messages = vec![
        Message::user("Do something"),
        Message::assistant("I did it"),
    ];
    let md = serialize_conversation_turn(&messages);
    let conv = parse_conversation(&md).expect("round-trip must parse");
    assert_eq!(conv.history.len(), 2);
    assert_eq!(conv.history[0].as_text(), Some("Do something"));
    assert_eq!(conv.history[1].as_text(), Some("I did it"));
}

#[test]
fn conversation_round_trip_with_tool_call() {
    use sven_model::{FunctionCall, MessageContent};
    let messages = vec![
        Message::user("Search"),
        Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: "call_99".into(),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"/tmp/x"}"#.into(),
                },
            },
        },
        Message::tool_result("call_99", "file contents"),
        Message::assistant("Found it"),
    ];
    let md = serialize_conversation_turn(&messages);

    assert!(md.contains("## Tool\n"), "tool section present");
    assert!(
        md.contains("## Tool Result\n"),
        "tool result section present"
    );
    assert!(md.contains("call_99"), "tool call id present");

    let conv = parse_conversation(&md).expect("round-trip parse");
    assert_eq!(conv.history.len(), 4);
    match &conv.history[1].content {
        MessageContent::ToolCall {
            tool_call_id,
            function,
        } => {
            assert_eq!(tool_call_id, "call_99");
            assert_eq!(function.name, "read_file");
        }
        _ => panic!("expected ToolCall"),
    }
    match &conv.history[2].content {
        MessageContent::ToolResult {
            tool_call_id,
            content,
        } => {
            assert_eq!(tool_call_id, "call_99");
            assert!(content.to_string().contains("file contents"));
        }
        _ => panic!("expected ToolResult"),
    }
}

#[test]
fn conversation_nested_code_block_preserved() {
    let md = concat!(
        "## User\nHow to write Rust?\n\n",
        "## Sven\nHere you go:\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\nDone.\n",
    );
    let conv = parse_conversation(md).expect("nested code block must not break parsing");
    assert_eq!(conv.history.len(), 2);
    let response = conv.history[1].as_text().unwrap();
    assert!(
        response.contains("fn main()"),
        "code block content preserved"
    );
}

#[test]
fn conversation_serialize_skips_system_messages() {
    let messages = vec![
        Message::system("You are a helpful assistant"),
        Message::user("Hi"),
        Message::assistant("Hello"),
    ];
    let md = serialize_conversation_turn(&messages);
    assert!(
        !md.contains("## System"),
        "system messages must not appear in file"
    );
    assert!(md.contains("## User"), "user message present");
    assert!(md.contains("## Sven"), "sven message present");
}
