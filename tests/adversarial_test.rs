// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Adversarial integration tests for sven.
//!
//! These tests exercise system-level concerns:
//!   * Agent loop behaviour under hostile/boundary conditions (Category 7)
//!   * Workflow parsing under adversarial inputs (Category 6 continuations)
//!   * Tool policy enforcement under edge-case commands (Category 8 continuations)
//!   * Config loading under boundary inputs (Category 5 continuations)

use std::sync::Arc;

use sven_config::{AgentConfig, AgentMode, Config};
use sven_core::{Agent, AgentRuntimeContext};
use sven_input::{parse_conversation, parse_workflow};
use sven_model::MockProvider;
use sven_tools::{events::ToolEvent, ToolRegistry};
use tokio::sync::{mpsc, Mutex};

// ── Agent construction helper ─────────────────────────────────────────────────

fn adversarial_agent(mode: AgentMode, max_tool_rounds: u32) -> Agent {
    let model: Arc<dyn sven_model::ModelProvider> = Arc::new(MockProvider);
    let tools = Arc::new(ToolRegistry::default());
    let mut agent_cfg = AgentConfig::default();
    agent_cfg.max_tool_rounds = max_tool_rounds;
    let config = Arc::new(agent_cfg);
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

// ── Category 7: Concurrency and resource limits ───────────────────────────────

#[tokio::test]
async fn adversarial_agent_max_tool_rounds_one_completes() {
    // max_tool_rounds=1 means only one model call is allowed;
    // the agent must return after that single call without hanging.
    let mut agent = adversarial_agent(AgentMode::Agent, 1);
    let (tx, mut rx) = mpsc::channel(64);
    agent.submit("hello", tx).await.unwrap();
    // Drain all events; must complete.
    while rx.try_recv().is_ok() {}
}

#[tokio::test]
async fn adversarial_agent_zero_max_tool_rounds_does_not_panic() {
    let mut agent = adversarial_agent(AgentMode::Agent, 0);
    let (tx, mut rx) = mpsc::channel(64);
    let result = agent.submit("hello", tx).await;
    while rx.try_recv().is_ok() {}
    // Must not panic; result may be Ok or Err.
    let _ = result;
}

#[tokio::test]
async fn adversarial_agent_empty_prompt_does_not_panic() {
    let mut agent = adversarial_agent(AgentMode::Agent, 10);
    let (tx, mut rx) = mpsc::channel(64);
    let result = agent.submit("", tx).await;
    while rx.try_recv().is_ok() {}
    let _ = result;
}

#[tokio::test]
async fn adversarial_agent_very_long_prompt_does_not_panic() {
    let mut agent = adversarial_agent(AgentMode::Agent, 5);
    let (tx, mut rx) = mpsc::channel(64);
    let long_prompt = "word ".repeat(50_000);
    let result = agent.submit(&long_prompt, tx).await;
    while rx.try_recv().is_ok() {}
    let _ = result;
}

#[tokio::test]
async fn adversarial_agent_unicode_prompt_does_not_panic() {
    let mut agent = adversarial_agent(AgentMode::Agent, 5);
    let (tx, mut rx) = mpsc::channel(64);
    // RTL override, zero-width joiners, multi-byte sequences
    let unicode_prompt = "日本語 café \u{202E}RTL\u{200D}ZWJ こんにちは 🎉";
    let result = agent.submit(unicode_prompt, tx).await;
    while rx.try_recv().is_ok() {}
    let _ = result;
}

#[tokio::test]
async fn adversarial_agent_concurrent_submissions_do_not_panic() {
    // Two agents running concurrently on the same thread pool.
    let mut a1 = adversarial_agent(AgentMode::Agent, 3);
    let mut a2 = adversarial_agent(AgentMode::Research, 3);
    let (tx1, mut rx1) = mpsc::channel(64);
    let (tx2, mut rx2) = mpsc::channel(64);
    let r1 = a1.submit("prompt one", tx1);
    let r2 = a2.submit("prompt two", tx2);
    let (res1, res2) = tokio::join!(r1, r2);
    while rx1.try_recv().is_ok() {}
    while rx2.try_recv().is_ok() {}
    let _ = res1;
    let _ = res2;
}

// ── Category 6 continued: Workflow parsing adversarial ────────────────────────

#[test]
fn adversarial_workflow_empty_input_does_not_panic() {
    let w = parse_workflow("");
    // An empty document should produce a fallback single step.
    let _ = w;
}

#[test]
fn adversarial_workflow_only_whitespace_does_not_panic() {
    let w = parse_workflow("   \t\n\r\n  ");
    let _ = w;
}

#[test]
fn adversarial_workflow_1mb_single_line_does_not_panic() {
    let big_line = "x".repeat(1_000_000);
    let w = parse_workflow(&big_line);
    assert_eq!(w.steps.len(), 1);
}

#[test]
fn adversarial_workflow_10000_steps_does_not_panic() {
    let many_steps: String = (0..10_000)
        .map(|i| format!("## Step {i}\nDo something in step {i}.\n\n"))
        .collect();
    let w = parse_workflow(&many_steps);
    assert_eq!(w.steps.len(), 10_000);
}

#[test]
fn adversarial_workflow_mixed_line_endings_does_not_panic() {
    let mixed = "## Step one\r\nDo this.\r\n\n## Step two\nDo that.\r\n";
    let w = parse_workflow(mixed);
    assert!(!w.steps.is_empty());
}

#[test]
fn adversarial_workflow_code_fence_inside_code_fence_does_not_panic() {
    let md = "## Step\n```rust\n```python\nprint('nested')\n```\nprintln!(\"outer\");\n```\n";
    let w = parse_workflow(md);
    let _ = w;
}

#[test]
fn adversarial_workflow_unicode_step_labels_do_not_panic() {
    let md = "## 日本語のステップ\nDo something.\n\n## Café Step\nAnother thing.\n";
    let w = parse_workflow(md);
    assert!(!w.steps.is_empty());
}

// ── Category 6 continued: Conversation parsing adversarial ───────────────────

#[test]
fn adversarial_conversation_empty_does_not_panic() {
    let turns = parse_conversation("");
    let _ = turns;
}

#[test]
fn adversarial_conversation_only_user_sections_does_not_panic() {
    let md = "## User\nhello\n\n## User\nhello again\n";
    let turns = parse_conversation(md);
    let _ = turns;
}

#[test]
fn adversarial_conversation_missing_sven_section_does_not_panic() {
    let md = "## User\nhello\n";
    let turns = parse_conversation(md);
    let _ = turns;
}

#[test]
fn adversarial_conversation_very_long_message_does_not_panic() {
    let long_msg = "x".repeat(1_000_000);
    let md = format!("## User\n{long_msg}\n\n## Sven\nresponse\n");
    let turns = parse_conversation(&md);
    let _ = turns;
}

// ── Category 5 continued: Config adversarial at integration level ─────────────

#[test]
fn adversarial_config_default_is_valid_and_complete() {
    let cfg = Config::default();
    assert!(!cfg.model.provider.is_empty());
    assert!(!cfg.model.name.is_empty());
    assert!(cfg.agent.max_tool_rounds > 0);
}

#[test]
fn adversarial_config_zero_max_tool_rounds_accepted() {
    let mut cfg = AgentConfig::default();
    cfg.max_tool_rounds = 0;
    // Zero rounds is unusual but should not panic when building an agent.
    let agent = adversarial_agent(AgentMode::Agent, cfg.max_tool_rounds);
    drop(agent);
}

#[test]
fn adversarial_config_u32_max_tool_rounds_accepted() {
    let agent = adversarial_agent(AgentMode::Agent, u32::MAX);
    drop(agent);
}
