// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec tests for a conversation shown from elsewhere (`sven rc attach
//! --tui`): what its agent reports - a message the person sent from another
//! client, the answer streaming in with its tool calls - is drawn as if the
//! conversation ran here.

use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use sven_machines::AgentEvent;
use sven_vocab::ToolCall;

use crate::app::App;

const W: u16 = 100;
const H: u16 = 30;

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

async fn event(app: &mut App, event: AgentEvent) {
    let sid = app.sessions.active_id.clone();
    app.handle_agent_event(sid, event).await;
}

#[tokio::test]
async fn a_message_sent_elsewhere_and_its_answer_are_drawn_as_if_they_ran_here() {
    let (mut app, _rx) = App::for_testing();
    event(
        &mut app,
        AgentEvent::UserMessage("which files are here?".into()),
    )
    .await;
    assert!(app.agent.busy, "an answer follows a message sent elsewhere");
    event(&mut app, AgentEvent::TextDelta("Looking.".into())).await;
    event(&mut app, AgentEvent::TextComplete("Looking.".into())).await;
    event(
        &mut app,
        AgentEvent::ToolCallStarted(ToolCall {
            id: "c1".into(),
            name: "list_dir".into(),
            args: serde_json::json!({"path": "."}),
        }),
    )
    .await;
    event(
        &mut app,
        AgentEvent::ToolCallFinished {
            call_id: "c1".into(),
            tool_name: "list_dir".into(),
            output: "README.md".into(),
            is_error: false,
        },
    )
    .await;
    event(
        &mut app,
        AgentEvent::TextComplete("There is one file, README.md.".into()),
    )
    .await;
    event(&mut app, AgentEvent::TurnComplete).await;

    let drawn = screen(&mut app);
    for expected in [
        "which files are here?",
        "Looking.",
        "list_dir",
        "There is one file, README.md.",
    ] {
        assert!(
            drawn.contains(expected),
            "{expected:?} is not drawn:\n{drawn}"
        );
    }
    let asked = drawn.find("which files are here?").unwrap();
    let answered = drawn.find("There is one file").unwrap();
    assert!(
        asked < answered,
        "the message is drawn before its answer:\n{drawn}"
    );
    assert!(!app.agent.busy, "the answer ended");
}
