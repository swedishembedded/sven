// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Shared types used by Sven frontends.

use std::path::PathBuf;

use sven_model_drivers::ModelConfig;
use sven_session_store::SessionId;
use sven_vocab::AgentMode;

use crate::segment::ChatSegment;

/// Specifies a model switch to take effect with a queued message.
#[derive(Debug, Clone)]
pub enum ModelDirective {
    SwitchTo(Box<ModelConfig>),
}

impl ModelDirective {
    pub fn into_model_config(self) -> ModelConfig {
        let ModelDirective::SwitchTo(c) = self;
        *c
    }

    /// Display label for UI (e.g. queue panel). Never panics.
    pub fn display_label(&self) -> String {
        let ModelDirective::SwitchTo(c) = self;
        format!("{}/{}", c.provider, c.name)
    }
}

/// A message waiting in the send queue, with optional per-message transitions.
#[derive(Debug, Clone)]
pub struct QueuedMessage {
    pub content: String,
    pub model_transition: Option<ModelDirective>,
    pub mode_transition: Option<AgentMode>,
}

impl QueuedMessage {
    pub fn plain(content: String) -> Self {
        Self {
            content,
            model_transition: None,
            mode_transition: None,
        }
    }
}

/// Options for launching any Sven frontend.
pub struct FrontendOptions {
    pub mode: AgentMode,
    pub initial_prompt: Option<String>,
    pub initial_history: Option<(Vec<ChatSegment>, PathBuf)>,
    pub model_override: Option<String>,
    /// Combined load+output ATIF trace path (`--trace`), or the output-only
    /// path (`--output-trace`).
    pub trace_path: Option<PathBuf>,
    /// Load-only ATIF trace path (`--load-trace`).
    pub load_trace_path: Option<PathBuf>,
    pub initial_queue: Vec<QueuedMessage>,
    /// Load an existing YAML chat document to resume the conversation.
    pub chat_path: Option<PathBuf>,
    /// Save the chat to this YAML path after each turn.
    pub output_chat_path: Option<PathBuf>,
}

impl Default for FrontendOptions {
    fn default() -> Self {
        Self {
            mode: AgentMode::Agent,
            initial_prompt: None,
            initial_history: None,
            model_override: None,
            trace_path: None,
            load_trace_path: None,
            initial_queue: vec![],
            chat_path: None,
            output_chat_path: None,
        }
    }
}

/// Metadata snapshot for a single chat session, usable by any frontend.
#[derive(Debug, Clone)]
pub struct SessionMeta {
    pub id: SessionId,
    pub parent_id: Option<SessionId>,
    pub title: String,
    pub busy: bool,
    pub current_tool: Option<String>,
    pub context_pct: u8,
    pub total_cost_usd: f64,
    pub total_output_tokens: u32,
    pub cache_hit_pct: u8,
    pub is_active: bool,
    pub depth: u16,
}
