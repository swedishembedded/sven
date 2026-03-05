// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Input box and inline-edit state, including message history and attachments.

use std::path::PathBuf;

// ── InputAttachment ───────────────────────────────────────────────────────────

/// A file or image the user attached to the current message via paste.
#[derive(Debug, Clone)]
pub enum InputAttachment {
    File(PathBuf),
    Image(PathBuf),
}

impl InputAttachment {
    /// Short display name (just the file name, not the full path).
    pub fn display_name(&self) -> &str {
        match self {
            InputAttachment::File(p) | InputAttachment::Image(p) => {
                p.file_name().and_then(|n| n.to_str()).unwrap_or("(file)")
            }
        }
    }

    /// Icon/prefix character for display.
    pub fn icon(&self, ascii: bool) -> &'static str {
        match self {
            InputAttachment::Image(_) => {
                if ascii {
                    "[img] "
                } else {
                    "🖼  "
                }
            }
            InputAttachment::File(_) => {
                if ascii {
                    "[file] "
                } else {
                    "📎 "
                }
            }
        }
    }

    /// Text injected into the submitted message (the agent receives the path).
    pub fn to_message_text(&self) -> String {
        match self {
            InputAttachment::File(p) => format!("[File: {}]", p.display()),
            InputAttachment::Image(p) => format!("[Image: {}]", p.display()),
        }
    }

    /// Full path as a string for display in a compact form.
    pub fn full_path(&self) -> String {
        match self {
            InputAttachment::File(p) | InputAttachment::Image(p) => p.display().to_string(),
        }
    }
}

/// Returns `true` when the path has an image file extension.
pub fn is_image_path(path: &std::path::Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_lowercase())
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "tiff" | "tif" | "ico")
    )
}

// ── InputState ────────────────────────────────────────────────────────────────

/// Capacity of the per-session message history ring.
const HISTORY_CAP: usize = 100;

/// State for the normal message composition input box.
pub(crate) struct InputState {
    /// Raw UTF-8 text in the input box.
    pub buffer: String,
    /// Byte offset of the cursor inside `buffer`.
    pub cursor: usize,
    /// Index of the first visible wrapped line (for scrolling).
    pub scroll_offset: usize,
    /// Sent-message history (oldest first, newest last).
    pub history: Vec<String>,
    /// Current position while cycling through history (None = editing new).
    pub history_idx: Option<usize>,
    /// Draft text saved before entering history navigation.
    pub history_draft: Option<String>,
    /// Attached files/images for the current message.
    pub attachments: Vec<InputAttachment>,
}

impl InputState {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
            scroll_offset: 0,
            history: Vec::new(),
            history_idx: None,
            history_draft: None,
            attachments: Vec::new(),
        }
    }

    /// Push a newly-submitted message into the history ring.
    pub fn push_history(&mut self, text: &str) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        // Avoid consecutive duplicates.
        if self.history.last().map(|s| s.as_str()) == Some(text.as_str()) {
            return;
        }
        if self.history.len() >= HISTORY_CAP {
            self.history.remove(0);
        }
        self.history.push(text);
        self.history_idx = None;
        self.history_draft = None;
    }

    /// Navigate one step older in history; saves the current buffer as draft
    /// on first navigation. Returns the text to load, or `None` if already at
    /// the oldest entry.
    pub fn history_up(&mut self) -> Option<&str> {
        if self.history.is_empty() {
            return None;
        }
        match self.history_idx {
            None => {
                self.history_draft = Some(self.buffer.clone());
                let idx = self.history.len() - 1;
                self.history_idx = Some(idx);
                Some(self.history[idx].as_str())
            }
            Some(0) => None,
            Some(idx) => {
                let new_idx = idx - 1;
                self.history_idx = Some(new_idx);
                Some(self.history[new_idx].as_str())
            }
        }
    }

    /// Navigate one step newer in history; restores the draft when reaching
    /// the bottom.
    pub fn history_down(&mut self) -> Option<&str> {
        match self.history_idx {
            None => None,
            Some(idx) => {
                if idx + 1 >= self.history.len() {
                    self.history_idx = None;
                    Some(self.history_draft.as_deref().unwrap_or(""))
                } else {
                    let new_idx = idx + 1;
                    self.history_idx = Some(new_idx);
                    Some(self.history[new_idx].as_str())
                }
            }
        }
    }
}

// ── EditState ─────────────────────────────────────────────────────────────────

/// State for inline editing of a chat segment or a queued message.
pub(crate) struct EditState {
    /// Index of the chat segment currently being edited (`None` if not editing).
    pub message_index: Option<usize>,
    /// Index of the queued message currently being edited (`None` if not editing).
    pub queue_index: Option<usize>,
    /// Content of the edit buffer.
    pub buffer: String,
    /// Byte offset of the cursor inside `buffer`.
    pub cursor: usize,
    /// First visible wrapped line in the edit box.
    pub scroll_offset: usize,
    /// Original text saved for cancel / restore.
    pub original_text: Option<String>,
}

impl EditState {
    pub fn new() -> Self {
        Self {
            message_index: None,
            queue_index: None,
            buffer: String::new(),
            cursor: 0,
            scroll_offset: 0,
            original_text: None,
        }
    }

    /// True when any kind of inline edit is in progress.
    pub fn active(&self) -> bool {
        self.message_index.is_some() || self.queue_index.is_some()
    }

    /// Clear all edit state (cancel or confirm).
    pub fn clear(&mut self) {
        self.message_index = None;
        self.queue_index = None;
        self.buffer.clear();
        self.cursor = 0;
        self.scroll_offset = 0;
        self.original_text = None;
    }
}
