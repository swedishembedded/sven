// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Adversarial integration tests for sven.
//!
//! These tests exercise system-level concerns:
//!   * Workflow parsing under adversarial inputs (Category 6 continuations)
//!   * Tool policy enforcement under edge-case commands (Category 8 continuations)
//!   * Config loading under boundary inputs (Category 5 continuations)

use sven_config::{AgentConfig, Config};
use sven_session_store::{parse_conversation, parse_workflow};

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
    // Zero rounds is unusual but must be a valid config value.
    let cfg = AgentConfig {
        max_tool_rounds: 0,
        ..Default::default()
    };
    assert_eq!(cfg.max_tool_rounds, 0);
}

#[test]
fn adversarial_config_u32_max_tool_rounds_accepted() {
    let cfg = AgentConfig {
        max_tool_rounds: u32::MAX,
        ..Default::default()
    };
    assert_eq!(cfg.max_tool_rounds, u32::MAX);
}
