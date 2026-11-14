// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Input box and inline-edit state, including message history and attachments.

use std::path::PathBuf;

use sven_vocab::message::MAX_MESSAGE_BYTES;

// ── InputAttachment ───────────────────────────────────────────────────────────

/// What an attachment is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    /// An image file the person pasted the path of.
    Image,
    /// Text too long to send as the message's own ([`MAX_MESSAGE_BYTES`]),
    /// kept in a file of its own: its size in bytes.
    Text { bytes: usize },
}

/// A file attached to the current message: an image pasted by path, or text
/// too long for the message itself. The agent receives the path.
#[derive(Debug, Clone)]
pub struct InputAttachment {
    pub path: PathBuf,
    pub kind: AttachmentKind,
}

impl InputAttachment {
    /// An image file.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            kind: AttachmentKind::Image,
        }
    }

    /// Text of `bytes` bytes, kept at `path`.
    pub fn text(path: PathBuf, bytes: usize) -> Self {
        Self {
            path,
            kind: AttachmentKind::Text { bytes },
        }
    }

    /// Short display name (just the file name, not the full path).
    pub fn display_name(&self) -> &str {
        self.path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("(file)")
    }

    /// Icon/prefix character for display.
    pub fn icon(&self, ascii: bool) -> &'static str {
        match (self.kind, ascii) {
            (AttachmentKind::Image, true) => "[img] ",
            (AttachmentKind::Image, false) => "🖼  ",
            (AttachmentKind::Text { .. }, true) => "[txt] ",
            (AttachmentKind::Text { .. }, false) => "📄 ",
        }
    }

    /// Text injected into the submitted message (the agent receives the path).
    pub fn to_message_text(&self) -> String {
        match self.kind {
            AttachmentKind::Image => format!("[Image: {}]", self.path.display()),
            AttachmentKind::Text { bytes } => format!(
                "[Pasted text, {} KB, in {}: read it in full before acting on it]",
                bytes.div_ceil(1024),
                self.path.display()
            ),
        }
    }

    /// Full path as a string for display in a compact form.
    pub fn full_path(&self) -> String {
        self.path.display().to_string()
    }
}

/// Where text too long for a message is kept: sven's data directory.
pub(crate) fn pastes_dir() -> PathBuf {
    sven_roots::data_dir()
        .unwrap_or_else(sven_roots::temp_dir)
        .join("sven")
        .join("pastes")
}

/// Keeps `text` in a new file under `dir`, named for when it was pasted and
/// what it holds, and returns it as an attachment.
pub(crate) fn keep_pasted_text(
    dir: &std::path::Path,
    text: &str,
) -> std::io::Result<InputAttachment> {
    use std::hash::{Hash, Hasher};
    std::fs::create_dir_all(dir)?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let path = dir.join(format!("paste-{stamp}-{:016x}.md", hasher.finish()));
    std::fs::write(&path, text)?;
    Ok(InputAttachment::text(path, text.len()))
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

/// Inserts dictated `text` into `buffer` at the byte offset `cursor` and
/// moves the cursor past it.
///
/// The text is trimmed and joined to what precedes it with one space, unless
/// the cursor is at the start or already after whitespace. A `cursor` inside
/// a multibyte character is taken to be at the start of that character.
/// Empty text changes nothing.
pub(crate) fn insert_dictation(buffer: &mut String, cursor: &mut usize, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let mut at = (*cursor).min(buffer.len());
    while !buffer.is_char_boundary(at) {
        at -= 1;
    }
    let needs_space = buffer[..at]
        .chars()
        .next_back()
        .is_some_and(|c| !c.is_whitespace());
    let mut inserted = String::with_capacity(text.len() + 1);
    if needs_space {
        inserted.push(' ');
    }
    inserted.push_str(text);
    buffer.insert_str(at, &inserted);
    *cursor = at + inserted.len();
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

    /// Takes what the box holds as the message to send: its attachments'
    /// lines, then the typed text, and the typed text alone (for history).
    /// Typed text too long for a message (typed, not pasted) is kept as an
    /// attachment, as a long paste is. `None` when there is nothing to send.
    pub fn take_message(
        &mut self,
        pastes: &std::path::Path,
    ) -> std::io::Result<Option<(String, String)>> {
        let text = std::mem::take(&mut self.buffer).trim().to_string();
        self.cursor = 0;
        self.scroll_offset = 0;
        let typed = if text.len() > MAX_MESSAGE_BYTES {
            self.attachments.push(keep_pasted_text(pastes, &text)?);
            String::new()
        } else {
            text
        };
        let lines: Vec<String> = self
            .attachments
            .drain(..)
            .map(|a| a.to_message_text())
            .collect();
        let full = match (lines.is_empty(), typed.is_empty()) {
            (true, _) => typed.clone(),
            (false, true) => lines.join("\n"),
            (false, false) => format!("{}\n{typed}", lines.join("\n")),
        };
        Ok((!full.is_empty()).then_some((full, typed)))
    }

    /// Whether pasting `text` would make the message too long to send as
    /// text, so it is to be attached instead.
    pub fn paste_overflows(&self, text: &str) -> bool {
        self.buffer.len() + text.len() > MAX_MESSAGE_BYTES
    }

    /// Inserts dictated text at the cursor; see [`insert_dictation`].
    pub fn insert_dictation(&mut self, text: &str) {
        insert_dictation(&mut self.buffer, &mut self.cursor, text);
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

#[cfg(test)]
mod history_tests {
    use super::*;

    fn state_with(history: &[&str]) -> InputState {
        let mut s = InputState::new();
        for entry in history {
            s.push_history(entry);
        }
        s
    }

    /// The core shell-style guarantee this whole feature exists for:
    /// an unsubmitted draft survives a round trip through history and
    /// back, byte-for-byte.
    #[test]
    fn draft_survives_a_full_up_down_round_trip() {
        let mut s = state_with(&["first", "second"]);
        s.buffer = "unsubmitted draft".to_string();

        assert_eq!(s.history_up(), Some("second"));
        assert_eq!(s.history_up(), Some("first"));
        // Walk back down to the bottom - draft must come back exactly.
        assert_eq!(s.history_down(), Some("second"));
        assert_eq!(s.history_down(), Some("unsubmitted draft"));
        assert_eq!(s.history_idx, None, "back at the bottom = not navigating");
    }

    #[test]
    fn draft_is_captured_only_on_the_first_up_not_every_step() {
        let mut s = state_with(&["a", "b", "c"]);
        s.buffer = "draft".to_string();

        s.history_up(); // captures "draft"
        s.buffer = "would-be-lost-if-recaptured".to_string(); // in-place edit
        s.history_up(); // must NOT overwrite the saved draft
        assert_eq!(s.history_draft.as_deref(), Some("draft"));

        // Walking all the way back down still returns the ORIGINAL draft,
        // not the in-place edit made while mid-navigation.
        while s.history_idx.is_some() {
            s.history_down();
        }
        assert_eq!(s.buffer, "would-be-lost-if-recaptured"); // caller applies the return value
                                                             // (Confirms history_down's *return value* is the original draft;
                                                             // dispatch.rs is what copies it into `buffer` - see InputHistoryUp/
                                                             // InputHistoryDown in app/dispatch.rs.)
    }

    #[test]
    fn history_up_at_oldest_entry_returns_none() {
        let mut s = state_with(&["only"]);
        assert_eq!(s.history_up(), Some("only"));
        assert_eq!(s.history_up(), None, "already at the oldest entry");
    }

    #[test]
    fn history_down_without_navigating_returns_none() {
        let mut s = state_with(&["a"]);
        assert_eq!(s.history_down(), None, "not currently navigating");
    }

    #[test]
    fn empty_history_never_navigates() {
        let mut s = InputState::new();
        s.buffer = "draft".to_string();
        assert_eq!(s.history_up(), None);
        assert_eq!(
            s.history_draft, None,
            "must not capture a draft with nowhere to go"
        );
    }

    #[test]
    fn submitting_resets_navigation_and_drops_the_stale_draft() {
        let mut s = state_with(&["a", "b"]);
        s.buffer = "draft".to_string();
        s.history_up();
        assert!(s.history_idx.is_some());
        assert!(s.history_draft.is_some());

        s.push_history("newly submitted");
        assert_eq!(s.history_idx, None);
        assert_eq!(s.history_draft, None);
        assert_eq!(
            s.history.last().map(String::as_str),
            Some("newly submitted")
        );
    }

    #[test]
    fn push_history_skips_empty_and_consecutive_duplicate_entries() {
        let mut s = state_with(&["same"]);
        s.push_history("   "); // whitespace-only, must be skipped
        s.push_history("same"); // consecutive duplicate, must be skipped
        assert_eq!(s.history, vec!["same".to_string()]);
    }

    fn dictate(buffer: &str, cursor: usize, text: &str) -> (String, usize) {
        let (mut buffer, mut cursor) = (buffer.to_string(), cursor);
        insert_dictation(&mut buffer, &mut cursor, text);
        (buffer, cursor)
    }

    #[test]
    fn dictation_is_joined_with_one_space_and_moves_the_cursor_past_it() {
        assert_eq!(dictate("", 0, "hello"), ("hello".into(), 5));
        assert_eq!(dictate("fix", 3, " the bug "), ("fix the bug".into(), 11));
        assert_eq!(dictate("fix ", 4, "it"), ("fix it".into(), 6));
        assert_eq!(dictate("a\n", 2, "b"), ("a\nb".into(), 3));
        assert_eq!(
            dictate("x", 1, "  "),
            ("x".into(), 1),
            "blank text is nothing"
        );
    }

    #[test]
    fn dictation_lands_at_a_char_boundary_in_multibyte_text() {
        // "åäö" is 6 bytes; a cursor in the middle of 'ä' is moved back.
        assert_eq!(dictate("åäö", 3, "ok"), ("å ok\u{e4}\u{f6}".into(), 5));
        // Insert in the middle, before existing text.
        let (buffer, cursor) = dictate("héllo", 1, "världen");
        assert_eq!(buffer, "h världenéllo");
        assert_eq!(&buffer[..cursor], "h världen");
        // A cursor past the end is clamped.
        assert_eq!(dictate("ö", 99, "x"), ("ö x".into(), 4));
    }
}

#[cfg(test)]
mod long_message_tests {
    use super::*;

    #[test]
    fn a_paste_up_to_the_limit_goes_into_the_box_and_one_past_it_is_attached() {
        let mut state = InputState::new();
        state.buffer = "a".repeat(10);
        assert!(!state.paste_overflows(&"b".repeat(MAX_MESSAGE_BYTES - 10)));
        assert!(state.paste_overflows(&"b".repeat(MAX_MESSAGE_BYTES - 9)));
    }

    #[test]
    fn long_text_is_kept_as_a_file_the_message_names_for_the_agent_to_read() {
        let dir = tempfile::tempdir().unwrap();
        let plan = format!("# The plan\n{}", "step\n".repeat(30_000));
        let mut state = InputState::new();
        state
            .attachments
            .push(keep_pasted_text(dir.path(), &plan).unwrap());
        state.buffer = "execute this plan".into();

        let (full, typed) = state.take_message(dir.path()).unwrap().unwrap();

        let kept = &state_paths(dir.path())[0];
        assert_eq!(std::fs::read_to_string(kept).unwrap(), plan);
        assert_eq!(
            full,
            format!(
                "[Pasted text, {} KB, in {}: read it in full before acting on it]\nexecute this plan",
                plan.len().div_ceil(1024),
                kept.display()
            )
        );
        assert_eq!(typed, "execute this plan");
        assert!(state.attachments.is_empty() && state.buffer.is_empty());
    }

    #[test]
    fn typed_text_too_long_for_a_message_is_attached_as_a_paste_is() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = InputState::new();
        state.buffer = "x".repeat(MAX_MESSAGE_BYTES + 1);

        let (full, typed) = state.take_message(dir.path()).unwrap().unwrap();

        assert!(full.starts_with("[Pasted text, 101 KB, in "), "{full}");
        assert!(sven_vocab::message::fits_message(&full));
        assert_eq!(typed, "", "a file's worth of text is not kept in history");
    }

    #[test]
    fn an_empty_box_sends_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(InputState::new()
            .take_message(dir.path())
            .unwrap()
            .is_none());
    }

    fn state_paths(dir: &std::path::Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect()
    }
}
