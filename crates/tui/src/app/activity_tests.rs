// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec tests for the activity panel: the strip between the chat and the
//! input that shows what the session's subagents are doing. Driven through
//! terminal key events and checked on the drawn screen.
//!
//! Swedish Embedded AB implements solutions for supervising parallel
//! autonomous agents for its clients. If your team needs expertise in
//! terminal interfaces for agent runtimes then you can procure our services
//! by sending an email to info@swedishembedded.com.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use sven_machines::AgentEvent;
use sven_vocab::SubagentUpdate;

use crate::app::{App, FocusPane};

const W: u16 = 100;
const H: u16 = 30;

/// Draw the app the way the run loop does and return the screen as text.
fn screen(app: &mut App) -> String {
    let layout = app.layout_for(Rect::new(0, 0, W, H));
    app.cache_layout(&layout);
    let mut terminal = Terminal::new(TestBackend::new(W, H)).unwrap();
    terminal
        .draw(|f| app.view(f, &Vec::new(), 0, None))
        .unwrap();
    let buf = terminal.backend().buffer();
    (0..H)
        .map(|y| (0..W).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    app.handle_term_event(Event::Key(KeyEvent::new(code, mods)))
        .await;
}

async fn ctrl_w(app: &mut App, then: char) {
    key(app, KeyCode::Char('w'), KeyModifiers::CONTROL).await;
    key(app, KeyCode::Char(then), KeyModifiers::NONE).await;
}

async fn spawn(app: &mut App, handle: &str, description: &str) {
    let sid = app.sessions.active_id.clone();
    app.handle_agent_event(
        sid,
        AgentEvent::SubagentStarted {
            call_id: format!("call-{handle}"),
            handle_id: handle.into(),
            description: description.into(),
            prompt: format!("please {description}"),
        },
    )
    .await;
}

async fn update(app: &mut App, handle: &str, update: SubagentUpdate) {
    let sid = app.sessions.active_id.clone();
    app.handle_agent_event(
        sid,
        AgentEvent::SubagentEvent {
            call_id: format!("call-{handle}"),
            handle_id: handle.into(),
            update,
        },
    )
    .await;
}

fn tool(name: &str) -> SubagentUpdate {
    SubagentUpdate::ToolCallStarted {
        id: format!("{name}-1"),
        name: name.into(),
        args: serde_json::json!({}),
    }
}

async fn two_agents() -> App {
    let (mut app, _rx) = App::for_testing();
    spawn(&mut app, "buf_1", "review token validation").await;
    spawn(&mut app, "buf_2", "run the unit tests").await;
    update(&mut app, "buf_1", tool("read_file")).await;
    update(&mut app, "buf_2", tool("shell")).await;
    update(
        &mut app,
        "buf_2",
        SubagentUpdate::Finished {
            final_text: "all green".into(),
        },
    )
    .await;
    update(
        &mut app,
        "buf_2",
        SubagentUpdate::TokensUsed {
            input_tokens: 12_000,
            output_tokens: 300,
        },
    )
    .await;
    app
}

#[tokio::test]
async fn without_subagents_there_is_no_panel() {
    let (mut app, _rx) = App::for_testing();
    assert!(!screen(&mut app).contains("Agents"));
}

#[tokio::test]
async fn the_collapsed_panel_sums_up_the_session_s_subagents_in_one_line() {
    let mut app = two_agents().await;
    let text = screen(&mut app);
    let line = text
        .lines()
        .find(|l| l.contains("Agents"))
        .expect("the panel is drawn");
    assert!(line.contains("1 running"), "{line}");
    assert!(line.contains("1 done"), "{line}");
    assert!(
        !text.contains("review token validation"),
        "collapsed: {text}"
    );
}

#[tokio::test]
async fn ctrl_w_a_and_enter_open_the_panel_and_enter_on_an_agent_shows_its_session() {
    let mut app = two_agents().await;
    ctrl_w(&mut app, 'a').await;
    assert_eq!(app.ui.focus, FocusPane::Activity);

    key(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    let text = screen(&mut app);
    let row = |needle: &str| {
        text.lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no row for {needle}: {text}"))
            .to_string()
    };
    let reviewer = row("review token validation");
    assert!(reviewer.contains("read_file"), "current tool: {reviewer}");
    assert!(reviewer.contains("1 tool"), "tool count: {reviewer}");
    let tester = row("run the unit tests");
    assert!(tester.contains("12.3k tok"), "token count: {tester}");
    assert!(tester.contains("✓"), "finished: {tester}");

    // Down to the second agent, Enter: its session is shown.
    key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE).await;
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    let active = app.sessions.get(&app.sessions.active_id).unwrap();
    assert_eq!(active.buffer_handle.as_deref(), Some("buf_2"));
    assert!(screen(&mut app).contains("please run the unit tests"));
}

#[tokio::test]
async fn esc_folds_the_panel_and_gives_focus_back_to_the_input() {
    let mut app = two_agents().await;
    ctrl_w(&mut app, 'a').await;
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE).await;
    assert_eq!(app.ui.focus, FocusPane::Input);
    assert!(!screen(&mut app).contains("review token validation"));
}

#[tokio::test]
async fn an_open_panel_takes_at_most_two_fifths_of_the_screen_and_scrolls() {
    let (mut app, _rx) = App::for_testing();
    for i in 0..40 {
        spawn(&mut app, &format!("buf_{i}"), &format!("agent number {i}")).await;
    }
    ctrl_w(&mut app, 'a').await;
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE).await;
    let layout = app.layout_for(Rect::new(0, 0, W, H));
    assert!(layout.activity_pane.height <= H * 2 / 5, "{layout:?}");

    for _ in 0..39 {
        key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE).await;
    }
    let text = screen(&mut app);
    assert!(
        text.contains("agent number 39"),
        "the selection stays in view"
    );
    assert!(!text.contains("agent number 0 "), "{text}");
}

#[tokio::test]
async fn clicking_the_summary_opens_the_panel_and_clicking_an_agent_shows_its_session() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut app = two_agents().await;
    screen(&mut app);
    let click = |row: u16| {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row,
            modifiers: KeyModifiers::NONE,
        })
    };
    let top = app.layout.activity_pane.y;
    app.handle_term_event(click(top)).await;
    assert!(app.ui.activity.expanded);

    screen(&mut app);
    let top = app.layout.activity_pane.y;
    app.handle_term_event(click(top + 1)).await; // the first agent's row
    let active = app.sessions.get(&app.sessions.active_id).unwrap();
    assert_eq!(active.buffer_handle.as_deref(), Some("buf_1"));
}
