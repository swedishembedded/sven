// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Shared tool-call vocabulary.
//!
//! [`ToolCall`], [`ToolOutput`]/[`ToolOutputPart`], [`ToolSchema`], and
//! [`OutputCategory`] are pure data types with no behavior — no execution,
//! no registry, no permission policy. They exist here, below `sven-tools`,
//! so that crates which only need to *name* a tool call or its result (a
//! wire protocol, a control protocol) don't have to depend on the full
//! tool-implementation crate to do it.
//!
//! `sven-tools` re-exports all of these at its crate root, so existing
//! `sven_tools::ToolCall` (etc.) call sites are unaffected by where the
//! types are actually defined.

use serde_json::Value;

/// A single tool invocation requested by the model.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// Opaque identifier returned by the model (forwarded verbatim)
    pub id: String,
    pub name: String,
    /// Parsed JSON arguments
    pub args: Value,
}

/// A single content item in a rich tool output.
///
/// Most tools produce only `Text`.  Multimodal tools (e.g. `read_image`,
/// `attach_file`) may produce a mix of `Text`, `Image`, and `Audio` items.
#[derive(Debug, Clone)]
pub enum ToolOutputPart {
    /// Plain UTF-8 text.
    Text(String),
    /// Base64 data URL: `data:<mime>;base64,<b64>`.
    Image(String),
    /// Base64 audio data URL: `data:audio/wav;base64,<b64>`.
    Audio(String),
}

/// The result of executing a tool.
///
/// ## Backward compatibility
/// `content` is always the plain-text representation of the output (the
/// concatenation of all `Text` parts).  Existing tools and tests that only
/// access `content` continue to work unchanged.
///
/// ## Image support
/// Tools that return images populate `parts` with a mix of [`ToolOutputPart::Text`]
/// and [`ToolOutputPart::Image`] items.  The agent maps these into the
/// appropriate `sven_model::ToolResultContent` variant when building the
/// conversation history.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub call_id: String,
    /// Plain-text content - concatenation of all Text parts.
    /// Always set; always readable.  Backward-compatible field.
    pub content: String,
    /// Structured parts (text and/or images).  For tools that only return
    /// text this contains exactly one `Text` part mirroring `content`.
    pub parts: Vec<ToolOutputPart>,
    /// If true, the tool execution failed non-fatally (returned error message).
    pub is_error: bool,
}

impl ToolOutput {
    /// Successful plain-text result.
    pub fn ok(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        let text = content.into();
        let call_id = call_id.into();
        Self {
            call_id,
            content: text.clone(),
            parts: vec![ToolOutputPart::Text(text)],
            is_error: false,
        }
    }

    /// Error result containing a plain-text error message.
    pub fn err(call_id: impl Into<String>, msg: impl Into<String>) -> Self {
        let text = msg.into();
        let call_id = call_id.into();
        Self {
            call_id,
            content: text.clone(),
            parts: vec![ToolOutputPart::Text(text)],
            is_error: true,
        }
    }

    /// Result with arbitrary parts (text and/or images).
    ///
    /// `content` is set to the concatenation of all Text parts.
    pub fn with_parts(call_id: impl Into<String>, parts: Vec<ToolOutputPart>) -> Self {
        let text = parts
            .iter()
            .filter_map(|p| match p {
                ToolOutputPart::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        Self {
            call_id: call_id.into(),
            content: text,
            parts,
            is_error: false,
        }
    }

    /// Return `true` if this output contains at least one image part.
    pub fn has_images(&self) -> bool {
        self.parts
            .iter()
            .any(|p| matches!(p, ToolOutputPart::Image(_)))
    }

    /// Return `true` if this output contains at least one audio part.
    pub fn has_audio(&self) -> bool {
        self.parts
            .iter()
            .any(|p| matches!(p, ToolOutputPart::Audio(_)))
    }
}

/// Describes the shape of a tool's text output for context-aware truncation.
///
/// When a tool result exceeds the configured token cap, `sven-core` uses
/// this category to pick the right extraction strategy.  Each tool declares
/// its own category; `sven-core` never hard-codes tool names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputCategory {
    /// Terminal / process output: keep the first 60 + last 40 lines so both
    /// the command preamble and the final result are visible.
    /// Suitable for: shell, run_terminal_command, gdb commands.
    HeadTail,
    /// Ordered match list: keep the leading matches so the model sees the
    /// highest-relevance results first.
    /// Suitable for: grep, search_codebase, read_lints.
    MatchList,
    /// File content: keep a head and tail window with a separator so the
    /// model sees both the top of the file (imports, declarations) and the
    /// end (recent changes).
    /// Suitable for: read_file, fs read operations.
    FileContent,
    /// Generic text: hard-truncate at the character boundary.
    /// Used for all tools that do not fit the categories above.
    #[default]
    Generic,
}

/// A tool schema - mirrors `sven_model::ToolSchema` but keeps this crate
/// independent from the model-provider crate.
#[derive(Debug, Clone)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    /// Whether this tool comes from an external MCP server.
    ///
    /// MCP tools are placed after core tools in the prompt and get their own
    /// Anthropic cache breakpoint (BP2) so that toggling servers only
    /// invalidates the MCP section, not the stable core tools section (BP1).
    pub is_mcp: bool,
}
