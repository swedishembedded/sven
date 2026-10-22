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

use crate::app::App;

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
