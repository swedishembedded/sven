// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Input box and inline-edit state.

/// State for the normal message composition input box.
pub(crate) struct InputState {
    /// Raw UTF-8 text in the input box.
    pub buffer: String,
    /// Byte offset of the cursor inside `buffer`.
    pub cursor: usize,
    /// Index of the first visible wrapped line (for scrolling).
    pub scroll_offset: usize,
}

impl InputState {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
            scroll_offset: 0,
        }
    }
}

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
