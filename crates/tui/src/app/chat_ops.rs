// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Chat helpers: syncing the Neovim buffer, saving history, and copying
//! segments and selections to the clipboard.

use sven_model::MessageContent;
use tracing::debug;

use crate::{
    app::{session_subagents, App},
    chat::{
        markdown::{format_conversation, parse_markdown_to_messages},
        segment::ChatSegment,
    },
    ui::width_utils::{col_to_byte_offset, display_width},
};

impl App {
    /// Re-render the chat pane: update the Neovim buffer (if active) and
    /// rebuild the ratatui display lines.
    pub(crate) async fn rerender_chat(&mut self) {
        if let Some(nvim_bridge) = &self.nvim.bridge {
            let content = format_conversation(
                self.chat.segments(),
                &self.chat.streaming_buffer,
                &self.chat.tool_args,
            );
            let mut bridge = nvim_bridge.lock().await;
            if let Err(e) = bridge.set_modifiable(true).await {
                tracing::error!("Failed to set buffer modifiable for update: {}", e);
            }
            if let Err(e) = bridge.set_buffer_content(&content).await {
                tracing::error!("Failed to update Neovim buffer: {}", e);
            }
            if self.agent.busy {
                if let Err(e) = bridge.set_modifiable(false).await {
                    tracing::error!("Failed to set buffer non-modifiable: {}", e);
                }
            }
        }
        self.build_display_from_segments();
        self.ui.search.update_matches(&self.chat.lines);
    }

    pub(crate) fn ascii(&self) -> bool {
        if std::env::var("SVEN_ASCII_BORDERS").as_deref() == Ok("1") {
            return true;
        }
        self.tui_config.ascii_borders
    }

    /// Adjust `input.scroll_offset` so the cursor row is within the visible
    /// window of the input pane.
    pub(crate) fn adjust_input_scroll(&mut self) {
        let w = self.layout.input_inner_width as usize;
        let h = self.layout.input_inner_height as usize;
        if w == 0 || h == 0 {
            return;
        }
        let wrap = crate::input_wrap::wrap_content(&self.input.buffer, w, self.input.cursor);
        let effective_wrap = if wrap.lines.len() > h && w > 1 {
            crate::input_wrap::wrap_content(&self.input.buffer, w - 1, self.input.cursor)
        } else {
            wrap
        };
        crate::input_wrap::adjust_scroll(
            effective_wrap.cursor_row,
            h,
            &mut self.input.scroll_offset,
        );
    }

    /// Adjust `edit.scroll_offset` so the cursor row is within the visible
    /// window when in inline edit mode.
    pub(crate) fn adjust_edit_scroll(&mut self) {
        let w = self.layout.input_inner_width as usize;
        let h = self.layout.input_inner_height as usize;
        if w == 0 || h == 0 {
            return;
        }
        let wrap = crate::input_wrap::wrap_content(&self.edit.buffer, w, self.edit.cursor);
        let effective_wrap = if wrap.lines.len() > h && w > 1 {
            crate::input_wrap::wrap_content(&self.edit.buffer, w - 1, self.edit.cursor)
        } else {
            wrap
        };
        crate::input_wrap::adjust_scroll(
            effective_wrap.cursor_row,
            h,
            &mut self.edit.scroll_offset,
        );
    }

    // ── History persistence ───────────────────────────────────────────────────

    /// Resolve (and, if this is the session's first save, assign) the ATIF
    /// session path: an explicit `--trace`/`--output-trace` path if one was
    /// given at startup, otherwise the canonical `session_dir()/<id>.json`
    /// path. Also flips a legacy entry's `is_legacy` off, since from this
    /// point on the session is backed by the new format (the original
    /// `.yaml` file, if any, is left untouched on disk).
    fn resolve_session_path(&mut self) -> std::path::PathBuf {
        if let Some(path) = self.session_path.clone() {
            return path;
        }
        let active_id = self.sessions.active_id.clone();
        let path = sven_session_store::session_path(active_id.as_str());
        self.session_path = Some(path.clone());
        if let Some(entry) = self.sessions.get_mut(&active_id) {
            entry.session_path = Some(path.clone());
            entry.is_legacy = false;
        }
        path
    }

    /// Build the ATIF [`atif::Trajectory`] for the active session, embedding
    /// any subagent children (see `session_subagents::build_active_trajectory`).
    fn active_session_trajectory(&self) -> atif::Trajectory {
        session_subagents::build_active_trajectory(
            &self.sessions,
            &self.chat,
            &self.chat_title,
            Some(self.session.model_display.clone()),
            Some(self.session.mode.to_string()),
        )
    }

    /// `true` iff the active chat has anything worth persisting (mirrors the
    /// old `records.is_empty()` early-return: display-only segments like
    /// `TodoUpdate`/`CollabEvent`/`Error` never count).
    fn active_chat_has_persistable_content(&self) -> bool {
        self.chat.segments().iter().any(|seg| {
            matches!(
                seg,
                ChatSegment::Message(_)
                    | ChatSegment::Thinking { .. }
                    | ChatSegment::ContextCompacted { .. }
            )
        })
    }

    /// Synchronous variant of `save_history_async` for use at clean exit.
    ///
    /// Called just before `run()` returns so that any messages typed in the
    /// current session are written to disk even if the tokio runtime is about
    /// to drop (which would cancel any pending `tokio::spawn` write tasks).
    pub(crate) fn save_history_sync(&mut self) {
        if !self.active_chat_has_persistable_content() {
            return;
        }
        let path = self.resolve_session_path();
        let trajectory = self.active_session_trajectory();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = atif::persist::write_trajectory_atomic(&path, &trajectory, None) {
            tracing::debug!("failed to save session trajectory on exit: {e}");
        }
    }

    pub(crate) fn save_history_async(&mut self) {
        if !self.active_chat_has_persistable_content() {
            return;
        }

        let path = self.resolve_session_path();
        let trajectory = self.active_session_trajectory();
        tokio::spawn(async move {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = atif::persist::write_trajectory_atomic(&path, &trajectory, None) {
                tracing::debug!("failed to save session trajectory: {e}");
            }
        });
    }

    // ── Neovim sync ───────────────────────────────────────────────────────────

    pub(crate) async fn sync_nvim_buffer_to_segments(&mut self) {
        let content = if let Some(nvim_bridge) = &self.nvim.bridge {
            let bridge = nvim_bridge.lock().await;
            bridge.get_buffer_content().await.ok()
        } else {
            return;
        };
        if let Some(content) = content {
            match parse_markdown_to_messages(&content) {
                Ok(messages) if !messages.is_empty() => {
                    self.chat
                        .set_segments(messages.iter().cloned().map(ChatSegment::Message).collect());
                    self.chat.tool_args.clear();
                    for m in &messages {
                        if let MessageContent::ToolCall {
                            tool_call_id,
                            function,
                        } = &m.content
                        {
                            self.chat
                                .tool_args
                                .insert(tool_call_id.clone(), function.name.clone());
                        }
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    debug!("sync_nvim_buffer_to_segments: parse error - keeping existing: {e}");
                }
            }
        }
    }

    pub(crate) async fn nvim_scroll_to_bottom(&self) {
        if let Some(nvim_bridge) = &self.nvim.bridge {
            let mut bridge = nvim_bridge.lock().await;
            let _ = bridge.send_input("G").await;
        }
    }

    // ── Clipboard copy ────────────────────────────────────────────────────────

    /// Copy the text content of a segment to the terminal clipboard via OSC 52.
    pub(crate) fn copy_segment_to_clipboard(&self, seg_idx: usize) -> bool {
        if let Some(seg) = self.chat.segments().get(seg_idx) {
            let text = extract_segment_text(seg, &self.chat.tool_args);
            if !text.is_empty() {
                osc52_copy(&text);
                return true;
            }
        }
        false
    }

    /// Copy all chat content to the clipboard via OSC 52.
    pub(crate) fn copy_all_to_clipboard(&self) -> bool {
        if self.chat.segments().is_empty() {
            return false;
        }
        let text = format_conversation(self.chat.segments(), "", &self.chat.tool_args);
        osc52_copy(&text);
        true
    }
}

// ── Clipboard via OSC 52 ──────────────────────────────────────────────────────

/// Copy `text` to the terminal clipboard using the OSC 52 escape sequence.
/// Works in most modern terminals (kitty, alacritty, tmux with allow-passthrough,
/// iTerm2, foot, wezterm) without any native library dependency.
fn osc52_copy(text: &str) {
    use std::io::Write;
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, text);
    let seq = format!("\x1b]52;c;{encoded}\x07");
    let _ = std::io::stdout().write_all(seq.as_bytes());
    let _ = std::io::stdout().flush();
}

// ── Text extraction for clipboard ────────────────────────────────────────────

fn extract_segment_text(
    seg: &ChatSegment,
    tool_args: &std::collections::HashMap<String, String>,
) -> String {
    match seg {
        ChatSegment::Message(m) => match &m.content {
            MessageContent::Text(t) => t.clone(),
            MessageContent::ToolCall { function, .. } => {
                let pretty = serde_json::from_str::<serde_json::Value>(&function.arguments)
                    .and_then(|v| serde_json::to_string_pretty(&v))
                    .unwrap_or_else(|_| function.arguments.clone());
                format!("Tool: {}\n{}", function.name, pretty)
            }
            MessageContent::ToolResult {
                tool_call_id,
                content,
            } => {
                let name = tool_args
                    .get(tool_call_id)
                    .map(|s| s.as_str())
                    .unwrap_or("tool");
                format!("Result: {name}\n{content}")
            }
            _ => String::new(),
        },
        ChatSegment::Thinking { content } => content.clone(),
        ChatSegment::Error(msg) => format!("Error: {msg}"),
        _ => String::new(),
    }
}

// ── Mouse selection clipboard copy ───────────────────────────────────────────

impl App {
    /// Copy the currently selected text (from a mouse drag selection) to the
    /// terminal clipboard via OSC 52.  Shows a toast on success.
    pub(crate) fn copy_selection_to_clipboard(&mut self) {
        let Some((s_line, s_col, e_line, e_col)) = self.chat.normalized_selection() else {
            return;
        };
        let text = extract_selection_text(&self.chat.lines, s_line, s_col, e_line, e_col);
        if !text.is_empty() {
            osc52_copy(&text);
            self.ui
                .push_toast(crate::app::ui_state::Toast::info("Selection copied"));
        }
    }
}

/// Extract visible text from a range of rendered chat lines, respecting column
/// boundaries for the first and last line of the selection.
///
/// `start_col` and `end_col` are **display columns** (as reported by hit-testing),
/// not character or byte indices.  This function converts them to byte offsets
/// using cumulative unicode display width so that wide characters (emoji, CJK,
/// special symbols) are handled correctly.
fn extract_selection_text(
    lines: &crate::markdown::StyledLines,
    start_line: usize,
    start_col: u16,
    end_line: usize,
    end_col: u16,
) -> String {
    let line_count = lines.len();
    if line_count == 0 {
        return String::new();
    }
    let e_line = end_line.min(line_count - 1);
    let mut result = String::new();
    #[allow(clippy::needless_range_loop)]
    for abs_line in start_line..=e_line {
        let line = &lines[abs_line];
        let line_text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        let line_width = display_width(&line_text);
        let from_display_col = if abs_line == start_line {
            start_col as usize
        } else {
            0
        };
        let to_display_col = if abs_line == end_line {
            (end_col as usize).min(line_width)
        } else {
            line_width
        };
        // Convert display columns to byte offsets.
        let from_byte = col_to_byte_offset(&line_text, from_display_col);
        let to_byte = col_to_byte_offset(&line_text, to_display_col);
        let extracted = &line_text[from_byte..to_byte];
        if abs_line > start_line {
            result.push('\n');
        }
        result.push_str(extracted);
    }
    // Trim trailing whitespace per line, then overall trailing newlines.
    let trimmed: Vec<String> = result.lines().map(|l| l.trim_end().to_string()).collect();
    trimmed.join("\n").trim_end().to_string()
}
