// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Plain (non-Slint) data structs for cross-thread communication between the
//! async agent task and the Slint UI thread.

use slint::{Color, Model, ModelRc, SharedString, VecModel};

use crate::highlight::HighlightToken;
use crate::{ChatMessage, CodeLine, CodeToken, MdBlock, RichLine, TextRun};

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

// ── Plain markdown block (for sub-blocks inside thinking / tool results) ──────

/// A single rendered markdown block, usable inside `ThinkingBubble` or
/// `ToolCallBubble` without requiring a recursive `ChatMessage` type.
#[derive(Clone, Default)]
pub struct PlainMdBlock {
    /// Block kind: "paragraph"|"code-block"|"heading"|"list-item"|"block-quote"|"separator"|"table-row"
    pub kind: &'static str,
    pub content: String,
    pub language: String,
    pub heading_level: i32,
    pub is_ordered: bool,
    pub code_lines: Vec<Vec<HighlightToken>>,
    pub rich_lines: Vec<Vec<PlainTextRun>>,
    pub cells: Vec<String>,
}

impl PlainMdBlock {
    pub fn to_slint(&self) -> MdBlock {
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

        let rich_lines_model: ModelRc<RichLine> = if self.rich_lines.is_empty() {
            ModelRc::new(VecModel::<RichLine>::default())
        } else {
            let lines: Vec<RichLine> = self
                .rich_lines
                .iter()
                .map(|line| {
                    let runs: Vec<TextRun> = line.iter().map(|r| r.to_slint()).collect();
                    RichLine {
                        runs: ModelRc::new(VecModel::from(runs)),
                    }
                })
                .collect();
            ModelRc::new(VecModel::from(lines))
        };

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

        MdBlock {
            kind: SharedString::from(self.kind),
            content: SharedString::from(self.content.as_str()),
            language: SharedString::from(self.language.as_str()),
            heading_level: self.heading_level,
            is_ordered: self.is_ordered,
            code_lines: code_lines_model,
            rich_lines: rich_lines_model,
            cells: cells_model,
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
    /// True when the list item belongs to an ordered (numbered) list.
    pub is_ordered_list: bool,

    /// Syntax-highlighted lines for `code-block` type. Preserved across
    /// session switches so re-highlighting is not needed.
    pub code_lines: Vec<Vec<HighlightToken>>,

    /// Inline text runs for formatted `assistant` paragraphs.
    /// Empty means the `content` string is rendered as-is.
    pub text_runs: Vec<PlainTextRun>,

    /// Pre-wrapped rich text lines: each entry is one visual line of runs.
    /// Populated for paragraphs, list items, and blockquotes.
    /// When non-empty, renders as VerticalLayout of HorizontalLayouts.
    pub rich_lines: Vec<Vec<PlainTextRun>>,

    /// Table cells for `table-row` type (tab-separated in `content` too).
    pub cells: Vec<String>,

    /// Result content from the associated tool call (for tool-call messages).
    pub tool_result_content: String,

    /// Whether the tool result is an error.
    pub tool_result_is_error: bool,

    /// First line of thinking content for the collapsed preview.
    pub thinking_preview: String,

    /// Parsed markdown sub-blocks for thinking and tool-result content.
    /// Populated after parsing so the Slint side can render them as markdown.
    pub sub_blocks: Vec<PlainMdBlock>,

    /// Parsed markdown blocks for the tool result content (used in ToolCallBubble).
    pub tool_result_blocks: Vec<PlainMdBlock>,
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

        // Build rich_lines model
        let rich_lines_model: ModelRc<RichLine> = if self.rich_lines.is_empty() {
            ModelRc::new(VecModel::<RichLine>::default())
        } else {
            let lines: Vec<RichLine> = self
                .rich_lines
                .iter()
                .map(|line| {
                    let runs: Vec<TextRun> = line.iter().map(|r| r.to_slint()).collect();
                    RichLine {
                        runs: ModelRc::new(VecModel::from(runs)),
                    }
                })
                .collect();
            ModelRc::new(VecModel::from(lines))
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
            tool_result_content: SharedString::from(self.tool_result_content.as_str()),
            tool_result_is_error: self.tool_result_is_error,
            thinking_preview: SharedString::from(self.thinking_preview.as_str()),
            language: SharedString::from(self.language.as_str()),
            heading_level: self.heading_level,
            is_ordered_list: self.is_ordered_list,
            code_lines: code_lines_model,
            text_runs: text_runs_model,
            rich_lines: rich_lines_model,
            cells: cells_model,
            sub_blocks: ModelRc::new(VecModel::from(
                self.sub_blocks
                    .iter()
                    .map(|b| b.to_slint())
                    .collect::<Vec<_>>(),
            )),
            tool_result_blocks: ModelRc::new(VecModel::from(
                self.tool_result_blocks
                    .iter()
                    .map(|b| b.to_slint())
                    .collect::<Vec<_>>(),
            )),
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
        role: match m.role.as_str() {
            "user" => "user",
            "assistant" => "assistant",
            "thinking" => "thinking",
            "tool" => "tool",
            _ => "assistant",
        },
        is_first_in_group: m.is_first_in_group,
        is_error: m.is_error,
        is_expanded: m.is_expanded,
        tool_name: m.tool_name.to_string(),
        tool_icon: m.tool_icon.to_string(),
        tool_summary: m.tool_summary.to_string(),
        tool_category: m.tool_category.to_string(),
        tool_fields_json: m.tool_fields_json.to_string(),
        tool_result_content: m.tool_result_content.to_string(),
        tool_result_is_error: m.tool_result_is_error,
        thinking_preview: m.thinking_preview.to_string(),
        language: m.language.to_string(),
        heading_level: m.heading_level,
        is_ordered_list: m.is_ordered_list,
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
