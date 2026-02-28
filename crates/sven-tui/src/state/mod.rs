// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Session-level state: model and mode transitions.
//!
//! Previously five parallel fields on `App` tracked the active model:
//! `effective_model_name`, `effective_model_cfg`, `pending_model_override`,
//! `pending_model_display`, and `pending_mode_override`.  Any mutation that
//! touched one but missed another caused a visible bug (wrong model in
//! completions, stale status-bar display, override reverting unexpectedly).
//!
//! `SessionState` is the single source of truth for all of these.  All
//! transitions go through its methods; the fields themselves are `pub` for
//! read access only (no direct mutation outside this module is expected).

use sven_config::{AgentMode, ModelConfig};

/// Unified session state: active model/mode plus staged transitions.
pub struct SessionState {
    /// Config for the currently active model (what the agent is using).
    pub model_cfg: ModelConfig,
    /// Cached `"{provider}/{name}"` string for status-bar display.
    pub model_display: String,
    /// Model staged for the next message (set by `/model` command).
    ///
    /// Also immediately reflected in `model_cfg` so that completions highlight
    /// the staged model as `(current)` without waiting for a message send.
    pub staged_model: Option<ModelConfig>,
    /// Mode staged for the next message (set by `/mode` command).
    pub staged_mode: Option<AgentMode>,
    /// Currently active agent mode.
    pub mode: AgentMode,
}

impl SessionState {
    pub fn new(model_cfg: ModelConfig, mode: AgentMode) -> Self {
        let model_display = format!("{}/{}", model_cfg.provider, model_cfg.name);
        Self {
            model_cfg,
            model_display,
            staged_model: None,
            staged_mode: None,
            mode,
        }
    }

    /// Stage a model to take effect with the next sent message.
    ///
    /// Also updates `model_cfg` immediately so completions reflect the staged
    /// model as the active one (avoids the need for a separate "completion context"
    /// field).
    pub fn stage_model(&mut self, cfg: ModelConfig) {
        self.model_cfg = cfg.clone();
        self.staged_model = Some(cfg);
    }

    /// Stage a mode to take effect with the next sent message.
    pub fn stage_mode(&mut self, mode: AgentMode) {
        self.staged_mode = Some(mode);
    }

    /// Apply a model switch immediately without staging.
    ///
    /// Used by the `SubmitBufferToAgent` path where a `/model` command inside
    /// the Neovim buffer takes effect right away.
    pub fn apply_model(&mut self, cfg: ModelConfig) {
        self.model_display = format!("{}/{}", cfg.provider, cfg.name);
        self.model_cfg = cfg;
        self.staged_model = None;
    }

    /// Apply a mode switch immediately without staging.
    pub fn apply_mode(&mut self, mode: AgentMode) {
        self.mode = mode;
        self.staged_mode = None;
    }

    /// Cycle the mode: Research → Plan → Agent → Research.
    pub fn cycle_mode(&mut self) {
        self.mode = match self.mode {
            AgentMode::Research => AgentMode::Plan,
            AgentMode::Plan => AgentMode::Agent,
            AgentMode::Agent => AgentMode::Research,
        };
    }

    /// Consume any staged overrides and promote the staged model to baseline.
    ///
    /// Called when a message is about to be sent.  Promotes the staged model
    /// to `model_display` (so the status bar reflects the switch before the
    /// agent turn completes), then returns the staged `(model, mode)` pair for
    /// embedding into the `QueuedMessage`.
    pub fn consume_staged(&mut self) -> (Option<ModelConfig>, Option<AgentMode>) {
        if let Some(ref cfg) = self.staged_model {
            self.model_display = format!("{}/{}", cfg.provider, cfg.name);
        }
        (self.staged_model.take(), self.staged_mode.take())
    }

    /// Label for the "next: …" hint shown in the status bar when a model
    /// override is staged but not yet sent.
    pub fn staged_model_label(&self) -> Option<String> {
        self.staged_model
            .as_ref()
            .map(|c| format!("{}/{}", c.provider, c.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_config::ModelConfig;

    fn mock_cfg(provider: &str, name: &str) -> ModelConfig {
        ModelConfig {
            provider: provider.to_string(),
            name: name.to_string(),
            ..ModelConfig::default()
        }
    }

    #[test]
    fn new_sets_display_from_cfg() {
        let s = SessionState::new(mock_cfg("openai", "gpt-4o"), AgentMode::Agent);
        assert_eq!(s.model_display, "openai/gpt-4o");
        assert_eq!(s.model_cfg.provider, "openai");
        assert!(s.staged_model.is_none());
        assert!(s.staged_mode.is_none());
    }

    #[test]
    fn stage_model_updates_model_cfg_for_completions() {
        let mut s = SessionState::new(mock_cfg("openai", "gpt-4o"), AgentMode::Agent);
        s.stage_model(mock_cfg("anthropic", "claude-opus-4-6"));
        // model_cfg updated immediately for completions
        assert_eq!(s.model_cfg.provider, "anthropic");
        // model_display NOT yet updated (still shows baseline)
        assert_eq!(s.model_display, "openai/gpt-4o");
        // staged_model set
        assert_eq!(s.staged_model.as_ref().unwrap().provider, "anthropic");
        // staged label visible for status bar "next: …" hint
        assert_eq!(
            s.staged_model_label().as_deref(),
            Some("anthropic/claude-opus-4-6")
        );
    }

    #[test]
    fn consume_staged_promotes_display_and_clears_staged() {
        let mut s = SessionState::new(mock_cfg("openai", "gpt-4o"), AgentMode::Agent);
        s.stage_model(mock_cfg("anthropic", "claude-opus-4-6"));
        s.stage_mode(AgentMode::Research);

        let (model, mode) = s.consume_staged();

        assert_eq!(s.model_display, "anthropic/claude-opus-4-6");
        assert!(s.staged_model.is_none());
        assert!(s.staged_mode.is_none());
        assert_eq!(model.unwrap().provider, "anthropic");
        assert_eq!(mode, Some(AgentMode::Research));
    }

    #[test]
    fn consume_staged_no_override_is_noop() {
        let mut s = SessionState::new(mock_cfg("openai", "gpt-4o"), AgentMode::Agent);
        let (model, mode) = s.consume_staged();
        assert!(model.is_none());
        assert!(mode.is_none());
        assert_eq!(s.model_display, "openai/gpt-4o");
    }

    #[test]
    fn apply_model_updates_display_immediately() {
        let mut s = SessionState::new(mock_cfg("openai", "gpt-4o"), AgentMode::Agent);
        s.stage_model(mock_cfg("anthropic", "claude-opus-4-6"));
        s.apply_model(mock_cfg("google", "gemini-2-5-pro"));
        assert_eq!(s.model_display, "google/gemini-2-5-pro");
        assert!(s.staged_model.is_none(), "apply_model should clear staged");
    }

    #[test]
    fn cycle_mode_wraps_correctly() {
        let mut s = SessionState::new(mock_cfg("openai", "gpt-4o"), AgentMode::Research);
        s.cycle_mode();
        assert_eq!(s.mode, AgentMode::Plan);
        s.cycle_mode();
        assert_eq!(s.mode, AgentMode::Agent);
        s.cycle_mode();
        assert_eq!(s.mode, AgentMode::Research);
    }
}
