/// Integration tests for sven's core logic using the mock model provider.
use std::sync::Arc;

use sven_config::{AgentConfig, AgentMode, Config};
use sven_core::Agent;
use sven_input::parse_markdown_steps;
use sven_model::MockProvider;
use sven_tools::ToolRegistry;
use tokio::sync::mpsc;

fn mock_agent(mode: AgentMode) -> Agent {
    let model: Arc<dyn sven_model::ModelProvider> = Arc::new(MockProvider::default());
    let tools = Arc::new(ToolRegistry::default());
    let config = Arc::new(AgentConfig::default());
    Agent::new(model, tools, config, mode, 128_000)
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
fn markdown_step_parsing_single_step() {
    let q = parse_markdown_steps("Do something useful.");
    assert_eq!(q.len(), 1);
}

#[test]
fn markdown_step_parsing_multiple_h2() {
    let md = "## First\nContent one.\n\n## Second\nContent two.";
    let mut q = parse_markdown_steps(md);
    assert_eq!(q.len(), 2);
    let s = q.pop().unwrap();
    assert_eq!(s.label.as_deref(), Some("First"));
}

#[test]
fn markdown_step_parsing_preamble() {
    let md = "Intro text.\n\n## Step A\nDo this.";
    let mut q = parse_markdown_steps(md);
    assert_eq!(q.len(), 2);
    let first = q.pop().unwrap();
    assert!(first.label.is_none());
    assert!(first.content.contains("Intro"));
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
    use sven_tools::{ToolCall, ShellTool};
    use sven_tools::Tool;

    let tool = ShellTool::default();
    let call = ToolCall {
        id: "1".into(),
        name: "shell".into(),
        args: serde_json::json!({ "command": "echo hello_world" }),
    };
    let output = tool.execute(&call).await;
    assert!(!output.is_error);
    assert!(output.content.contains("hello_world"));
}

#[tokio::test]
async fn fs_tool_write_read_roundtrip() {
    use sven_tools::{FsTool, Tool, ToolCall};

    let tool = FsTool;
    let path = format!("/tmp/sven_test_{}.txt", uuid::Uuid::new_v4());

    let write_call = ToolCall {
        id: "w1".into(),
        name: "fs".into(),
        args: serde_json::json!({ "operation": "write", "path": path, "content": "roundtrip" }),
    };
    let wo = tool.execute(&write_call).await;
    assert!(!wo.is_error, "write failed: {}", wo.content);

    let read_call = ToolCall {
        id: "r1".into(),
        name: "fs".into(),
        args: serde_json::json!({ "operation": "read", "path": path }),
    };
    let ro = tool.execute(&read_call).await;
    assert!(!ro.is_error);
    assert_eq!(ro.content.trim(), "roundtrip");

    let _ = std::fs::remove_file(&path);
}
