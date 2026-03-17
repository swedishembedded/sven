// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Plain (non-Slint) data structs for cross-thread communication between the
//! async agent task and the Slint UI thread.

use slint::{Color, Model, ModelRc, SharedString, VecModel};

use crate::highlight::HighlightToken;
use crate::{ChatMessage, CodeLine, CodeToken, TextRun};

// ── Text-run inline formatting ────────────────────────────────────────────────

/// A single inline formatting run within a paragraph.
#[derive(Clone, Default)]
pub struct PlainTextRun {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub is_code: bool,
    pub is_link: bool,
    pub url: String,
}

impl PlainTextRun {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Default::default()
        }
    }

    pub fn to_slint(&self) -> TextRun {
        TextRun {
            text: SharedString::from(self.text.as_str()),
            bold: self.bold,
            italic: self.italic,
            is_code: self.is_code,
            is_link: self.is_link,
            url: SharedString::from(self.url.as_str()),
        }
    }
}

// ── Main plain message type ───────────────────────────────────────────────────

/// Cross-thread message representation (no Slint / Rc types).
#[derive(Clone, Default)]
pub struct PlainChatMessage {
    pub message_type: &'static str,
    pub content: String,
    pub role: &'static str,

    pub is_first_in_group: bool,
    pub is_error: bool,
    pub is_streaming: bool,
    pub is_expanded: bool,

    pub tool_name: String,
    pub tool_icon: String,
    pub tool_summary: String,
    pub tool_category: String,
    pub tool_fields_json: String,

    pub language: String,
    pub heading_level: i32,

    /// Syntax-highlighted lines for `code-block` type. Preserved across
    /// session switches so re-highlighting is not needed.
    pub code_lines: Vec<Vec<HighlightToken>>,

    /// Inline text runs for formatted `assistant` paragraphs.
    /// Empty means the `content` string is rendered as-is.
    pub text_runs: Vec<PlainTextRun>,

    /// Table cells for `table-row` type (tab-separated in `content` too).
    pub cells: Vec<String>,
}

impl PlainChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            message_type: "user",
            content: content.into(),
            role: "user",
            ..Default::default()
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            message_type: "system",
            content: content.into(),
            role: "system",
            ..Default::default()
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            message_type: "error",
            content: content.into(),
            role: "error",
            is_error: true,
            ..Default::default()
        }
    }

    /// Convert to a Slint `ChatMessage`, preserving `code_lines` and `text_runs`.
    pub fn to_slint(&self) -> ChatMessage {
        // Build code_lines model
        let code_lines_model: ModelRc<CodeLine> = if self.code_lines.is_empty() {
            ModelRc::new(VecModel::<CodeLine>::default())
        } else {
            let lines: Vec<CodeLine> = self
                .code_lines
                .iter()
                .map(|line| {
                    let tokens: Vec<CodeToken> = line
                        .iter()
                        .map(|(text, r, g, b)| CodeToken {
                            text: SharedString::from(text.as_str()),
                            color: Color::from_rgb_u8(*r, *g, *b),
                        })
                        .collect();
                    CodeLine {
                        tokens: ModelRc::new(VecModel::from(tokens)),
                    }
                })
                .collect();
            ModelRc::new(VecModel::from(lines))
        };

        // Build text_runs model
        let text_runs_model: ModelRc<TextRun> = if self.text_runs.is_empty() {
            ModelRc::new(VecModel::<TextRun>::default())
        } else {
            let runs: Vec<TextRun> = self.text_runs.iter().map(|r| r.to_slint()).collect();
            ModelRc::new(VecModel::from(runs))
        };

        // Build cells model
        let cells_model: ModelRc<SharedString> = if self.cells.is_empty() {
            ModelRc::new(VecModel::<SharedString>::default())
        } else {
            let cells: Vec<SharedString> = self
                .cells
                .iter()
                .map(|c| SharedString::from(c.as_str()))
                .collect();
            ModelRc::new(VecModel::from(cells))
        };

        ChatMessage {
            message_type: SharedString::from(self.message_type),
            content: SharedString::from(self.content.as_str()),
            role: SharedString::from(self.role),
            is_first_in_group: self.is_first_in_group,
            is_error: self.is_error,
            is_streaming: self.is_streaming,
            is_expanded: self.is_expanded,
            is_search_match: false,
            tool_name: SharedString::from(self.tool_name.as_str()),
            tool_icon: SharedString::from(self.tool_icon.as_str()),
            tool_summary: SharedString::from(self.tool_summary.as_str()),
            tool_category: SharedString::from(self.tool_category.as_str()),
            tool_fields_json: SharedString::from(self.tool_fields_json.as_str()),
            language: SharedString::from(self.language.as_str()),
            heading_level: self.heading_level,
            code_lines: code_lines_model,
            text_runs: text_runs_model,
            cells: cells_model,
        }
    }
}

/// Convert a Slint `ChatMessage` back to a `PlainChatMessage` for session persistence.
/// Preserves `code_lines` so syntax highlighting survives session switches.
pub fn slint_msg_to_plain(m: &ChatMessage) -> PlainChatMessage {
    let code_lines: Vec<Vec<HighlightToken>> = {
        let n = m.code_lines.row_count();
        if n == 0 {
            vec![]
        } else {
            (0..n)
                .filter_map(|li| m.code_lines.row_data(li))
                .map(|line| {
                    let tn = line.tokens.row_count();
                    (0..tn)
                        .filter_map(|ti| line.tokens.row_data(ti))
                        .map(|tok| {
                            let c = tok.color;
                            (tok.text.to_string(), c.red(), c.green(), c.blue())
                        })
                        .collect()
                })
                .collect()
        }
    };

    let cells: Vec<String> = {
        let n = m.cells.row_count();
        (0..n)
            .filter_map(|i| m.cells.row_data(i))
            .map(|s| s.to_string())
            .collect()
    };

    PlainChatMessage {
        message_type: match m.message_type.as_str() {
            "user" => "user",
            "assistant" => "assistant",
            "code-block" => "code-block",
            "heading" => "heading",
            "list-item" => "list-item",
            "block-quote" => "block-quote",
            "separator" => "separator",
            "inline-code" => "inline-code",
            "table-row" => "table-row",
            "thinking" => "thinking",
            "tool-call" => "tool-call",
            "tool-result" => "tool-result",
            "error" => "error",
            _ => "system",
        },
        content: m.content.to_string(),
        role: "user",
        is_first_in_group: m.is_first_in_group,
        is_error: m.is_error,
        is_expanded: m.is_expanded,
        tool_name: m.tool_name.to_string(),
        tool_icon: m.tool_icon.to_string(),
        tool_summary: m.tool_summary.to_string(),
        tool_category: m.tool_category.to_string(),
        tool_fields_json: m.tool_fields_json.to_string(),
        language: m.language.to_string(),
        heading_level: m.heading_level,
        code_lines,
        cells,
        ..Default::default()
    }
}

// ── Plain toast ───────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct PlainToast {
    pub message: String,
    pub level: &'static str,
}
