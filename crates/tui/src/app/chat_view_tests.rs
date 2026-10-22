// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec tests for the chat view: scrolling, expansion and the reader's
//! position, driven through `App` the way key and agent events drive it.
//!
//! Swedish Embedded AB implements solutions for agent interfaces that stay
//! readable under large tool output for its clients. If your team needs
//! expertise in terminal user interfaces for autonomous agents then you can
//! procure our services by sending an email to info@swedishembedded.com.

use ratatui::text::Line;

use sven_model::MessageContent;

use crate::{app::App, chat::segment::ChatSegment};

fn app_with_lines(n: usize, chat_height: u16) -> App {
    let (mut app, _rx) = App::for_testing();
    app.chat.lines = (0..n).map(|i| Line::from(format!("line {i}"))).collect();
    app.layout.chat_height = chat_height;
    app
}

#[test]
fn a_chat_longer_than_65535_lines_scrolls_to_its_real_bottom() {
    let mut app = app_with_lines(70_000, 20);
    app.chat.auto_scroll = true;
    app.scroll_to_bottom();
    assert_eq!(app.chat.scroll_offset, 69_980);

    app.scroll_up(5);
    assert_eq!(app.chat.scroll_offset, 69_975);
    app.scroll_down(5);
    assert_eq!(app.chat.scroll_offset, 69_980);
    assert!(
        app.chat.auto_scroll,
        "reaching the bottom resumes following"
    );
}

// ── Segment identity ──────────────────────────────────────────────────────────

fn tool_started(id: &str) -> sven_machines::AgentEvent {
    sven_machines::AgentEvent::ToolCallStarted(sven_vocab::ToolCall {
        id: id.into(),
        name: "read_file".into(),
        args: serde_json::json!({ "path": format!("{id}.rs") }),
    })
}

fn tool_finished(id: &str, output: &str) -> sven_machines::AgentEvent {
    sven_machines::AgentEvent::ToolCallFinished {
        call_id: id.into(),
        tool_name: "read_file".into(),
        output: output.into(),
        is_error: false,
    }
}

/// Index of the tool call (not its result) with `call_id`.
fn call_index(app: &App, call_id: &str) -> usize {
    app.chat
        .segments()
        .iter()
        .position(|s| match s {
            ChatSegment::Message(m) => matches!(
                &m.content,
                MessageContent::ToolCall { tool_call_id, .. } if tool_call_id == call_id
            ),
            _ => false,
        })
        .expect("tool call present")
}

fn expand_level_of_call(app: &App, call_id: &str) -> u8 {
    let idx = call_index(app, call_id);
    app.chat.expand_level(idx)
}

#[tokio::test]
async fn a_result_inserted_above_keeps_the_highlight_and_expansion_on_their_segment() {
    let (mut app, _rx) = App::for_testing();
    let sid = app.sessions.active_id.clone();
    app.handle_agent_event(sid.clone(), tool_started("a")).await;
    app.handle_agent_event(sid.clone(), tool_started("b")).await;

    let b = call_index(&app, "b");
    app.chat.focused_segment = Some(b);
    app.chat.set_expand_level(b, 1);

    // `a` finishes: its result lands between `a` and `b`.
    app.handle_agent_event(sid, tool_finished("a", "fn a() {}"))
        .await;

    let focused = app
        .chat
        .focused_segment
        .expect("a segment stays highlighted");
    assert_eq!(focused, call_index(&app, "b"), "the highlight follows `b`");
    assert_eq!(expand_level_of_call(&app, "b"), 1, "`b` stays expanded");
    assert_eq!(expand_level_of_call(&app, "a"), 0, "`a` stays collapsed");
}
