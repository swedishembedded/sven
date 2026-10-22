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

    // Two tools form a run; open it so `b` has a row of its own.
    app.chat.set_run_expanded(call_index(&app, "a"), true);
    let b = call_index(&app, "b");
    app.chat.focus_segment(b);
    app.chat.set_expand_level(b, 1);

    // `a` finishes: its result lands between `a` and `b`.
    app.handle_agent_event(sid, tool_finished("a", "fn a() {}"))
        .await;

    let focused = app
        .chat
        .focused_segment()
        .expect("a segment stays highlighted");
    assert_eq!(focused, call_index(&app, "b"), "the highlight follows `b`");
    assert_eq!(expand_level_of_call(&app, "b"), 1, "`b` stays expanded");
    assert_eq!(expand_level_of_call(&app, "a"), 0, "`a` stays collapsed");
}

// ── Thinking ──────────────────────────────────────────────────────────────────

fn text_of(app: &App) -> String {
    app.chat
        .lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn chat_with_thinking(show_thinking: bool) -> App {
    let (mut app, _rx) = App::for_testing();
    app.tui_config.show_thinking = show_thinking;
    let sid = app.sessions.active_id.clone();
    app.chat
        .push_segment(ChatSegment::Message(sven_model::Message::user("hi")));
    app.handle_agent_event(
        sid.clone(),
        sven_machines::AgentEvent::ThinkingDelta("weighing the options".into()),
    )
    .await;
    app.build_display_from_segments();
    app
}

#[tokio::test]
async fn thinking_is_hidden_by_default_and_the_status_says_thinking() {
    let mut app = chat_with_thinking(false).await;
    assert!(!text_of(&app).contains("weighing"), "{}", text_of(&app));
    assert_eq!(app.activity_label(), Some("Thinking"));

    let sid = app.sessions.active_id.clone();
    app.handle_agent_event(
        sid,
        sven_machines::AgentEvent::ThinkingComplete("weighing the options".into()),
    )
    .await;
    let thinking = app.chat.segments().len() - 1;
    assert!(
        matches!(app.chat.segments()[thinking], ChatSegment::Thinking { .. }),
        "the thinking is kept in the conversation"
    );
    let (start, end) = app.chat.segment_line_ranges[thinking];
    assert_eq!(start, end, "but it takes no lines in the chat");

    // The highlight never lands on a segment that shows nothing.
    app.chat.focus_segment(0);
    app.dispatch_action(crate::keys::Action::ChatHighlightDown)
        .await;
    assert_eq!(app.chat.focused_segment(), Some(0));
}

#[tokio::test]
async fn thinking_is_shown_when_configured() {
    let app = chat_with_thinking(true).await;
    assert!(text_of(&app).contains("weighing"), "{}", text_of(&app));
}

// ── Tool runs ─────────────────────────────────────────────────────────────────

use crate::keys::Action;

/// A chat where the user asked, the agent ran `tools` (each reading
/// `<id>.rs`, whose output is `output(id)`), then answered.
async fn chat_with_tools(tools: &[&str], output: impl Fn(&str) -> String) -> App {
    let (mut app, _rx) = App::for_testing();
    app.layout.chat_height = 20;
    let sid = app.sessions.active_id.clone();
    app.chat
        .push_segment(ChatSegment::Message(sven_model::Message::user("look")));
    for id in tools {
        app.handle_agent_event(sid.clone(), tool_started(id)).await;
    }
    for id in tools {
        app.handle_agent_event(sid.clone(), tool_finished(id, &output(id)))
            .await;
    }
    app.chat
        .push_segment(ChatSegment::Message(sven_model::Message::assistant(
            "all done",
        )));
    app.build_display_from_segments();
    app
}

async fn press(app: &mut App, action: Action) {
    app.dispatch_action(action).await;
}

#[tokio::test]
async fn consecutive_tools_are_one_collapsed_ran_n_tools_row() {
    let app = chat_with_tools(&["a", "b", "c"], |id| format!("fn {id}() {{}}")).await;
    let text = text_of(&app);
    assert!(text.contains("Ran 3 tools"), "{text}");
    assert!(!text.contains("a.rs"), "the tools are folded away: {text}");
    assert!(text.contains("all done"));
}

#[tokio::test]
async fn the_run_expands_to_its_tools_and_a_tool_expands_to_its_output() {
    let mut app = chat_with_tools(&["a", "b", "c"], |id| format!("fn {id}() {{}}")).await;
    app.ui.focus = crate::app::FocusPane::Chat;
    press(&mut app, Action::ChatHighlightDown).await; // the user's message
    press(&mut app, Action::ChatHighlightDown).await; // the run
    press(&mut app, Action::ToggleChatItem).await;
    let text = text_of(&app);
    for id in ["a", "b", "c"] {
        assert!(text.contains(&format!("{id}.rs")), "row for {id}: {text}");
    }
    assert!(!text.contains("fn a()"), "rows are summaries: {text}");

    press(&mut app, Action::ChatHighlightDown).await; // tool `a`
    press(&mut app, Action::ToggleChatItem).await;
    assert!(text_of(&app).contains("fn a()"), "{}", text_of(&app));

    // Collapsing from inside the run folds the whole run back up.
    press(&mut app, Action::CollapseChatItem).await; // tool `a`
    press(&mut app, Action::CollapseChatItem).await; // the run
    assert!(text_of(&app).contains("Ran 3 tools"));
    assert!(!text_of(&app).contains("b.rs"), "{}", text_of(&app));
}

#[tokio::test]
async fn an_expanded_tool_is_never_taller_than_the_chat_pane_and_opens_in_full() {
    let huge = |_: &str| {
        (0..100_000)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut app = chat_with_tools(&["a"], huge).await;
    app.ui.focus = crate::app::FocusPane::Chat;
    press(&mut app, Action::ChatHighlightDown).await;
    press(&mut app, Action::ChatHighlightDown).await; // the tool
    press(&mut app, Action::ToggleChatItem).await;

    let call = call_index(&app, "a");
    let (start, end) = app.chat.segment_line_ranges[call];
    assert!(end - start <= 20, "inline height {} > pane", end - start);
    assert!(text_of(&app).contains("more lines"), "{}", text_of(&app));

    let scroll = app.chat.scroll_offset;
    press(&mut app, Action::OpenChatItem).await;
    let viewer = app.ui.inspector.as_ref().expect("the viewer is open");
    assert!(viewer.pager.cloned_lines().len() >= 100_000);
    app.ui.inspector = None;
    assert_eq!(app.chat.scroll_offset, scroll, "the chat did not move");
    assert_eq!(app.chat.focused_segment(), Some(call));
}

#[tokio::test]
async fn enter_edits_a_message_and_expands_anything_else() {
    let mut app = chat_with_tools(&["a", "b"], |id| format!("fn {id}() {{}}")).await;
    app.ui.focus = crate::app::FocusPane::Chat;
    press(&mut app, Action::ChatHighlightDown).await; // the user's message
    press(&mut app, Action::ActivateChatItem).await;
    assert_eq!(app.edit.message_index, Some(0), "Enter edits the message");

    app.edit.clear();
    app.ui.focus = crate::app::FocusPane::Chat;
    press(&mut app, Action::ChatHighlightDown).await; // the run
    press(&mut app, Action::ActivateChatItem).await;
    assert!(text_of(&app).contains("b.rs"), "Enter expands the run");
}
