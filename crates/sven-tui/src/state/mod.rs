// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Session-level state: model and mode transitions.
//!
//! `SessionState` is the single source of truth for the active model and mode.
//! All transitions go through its methods; the fields themselves are `pub` for
//! read access only (no direct mutation outside this module is expected).

use sven_config::{AgentMode, ModelConfig};

/// Unified session state: active model/mode plus staged transitions.
#[derive(Clone)]
pub struct SessionState {
    /// Config for the currently active model (what the agent will use).
    pub model_cfg: ModelConfig,
    /// Cached `"{provider}/{name}"` string for status-bar display.
    /// Updated immediately when `/model` is issued so the bar reflects the
    /// switch without waiting for the next message to be sent.
    pub model_display: String,
    /// Model staged for the next message (set by `/model` command).
    ///
    /// `model_display` and `model_cfg` are updated immediately so the status
    /// bar and completion context reflect the choice right away.  This field
    /// retains the config so `consume_staged()` can forward it to the agent.
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

    /// Switch the active model.
    ///
    /// Updates `model_display` and `model_cfg` immediately so the status bar
    /// and completion list reflect the new model right away.  The config is
    /// also stored in `staged_model` so `consume_staged()` can forward it to
    /// the background agent task with the next submitted message.
    pub fn stage_model(&mut self, cfg: ModelConfig) {
        self.model_display = format!("{}/{}", cfg.provider, cfg.name);
        self.model_cfg = cfg.clone();
        self.staged_model = Some(cfg);
    }

    /// Switch the active mode.
    ///
    /// Updates `mode` immediately for display and stores in `staged_mode` so
    /// `consume_staged()` can forward it to the agent with the next message.
    pub fn stage_mode(&mut self, mode: AgentMode) {
        self.mode = mode;
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

    /// Consume any staged overrides and return them for embedding into the
    /// next `QueuedMessage`.
    ///
    /// `model_display`, `model_cfg`, and `mode` are already up to date (set
    /// by `stage_model` / `stage_mode`), so this only clears the staged fields
    /// and returns the configs for the agent task.
    pub fn consume_staged(&mut self) -> (Option<ModelConfig>, Option<AgentMode>) {
        (self.staged_model.take(), self.staged_mode.take())
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
    fn stage_model_updates_display_and_cfg_immediately() {
        let mut s = SessionState::new(mock_cfg("openai", "gpt-4o"), AgentMode::Agent);
        s.stage_model(mock_cfg("anthropic", "claude-opus-4-6"));
        // Both model_cfg and model_display updated immediately
        assert_eq!(s.model_cfg.provider, "anthropic");
        assert_eq!(s.model_display, "anthropic/claude-opus-4-6");
        // staged_model set so agent gets it on next message
        assert_eq!(s.staged_model.as_ref().unwrap().provider, "anthropic");
    }

    #[test]
    fn stage_mode_updates_mode_immediately() {
        let mut s = SessionState::new(mock_cfg("openai", "gpt-4o"), AgentMode::Agent);
        s.stage_mode(AgentMode::Research);
        assert_eq!(s.mode, AgentMode::Research);
        assert_eq!(s.staged_mode, Some(AgentMode::Research));
    }

    #[test]
    fn consume_staged_clears_staged_and_returns_configs() {
        let mut s = SessionState::new(mock_cfg("openai", "gpt-4o"), AgentMode::Agent);
        s.stage_model(mock_cfg("anthropic", "claude-opus-4-6"));
        s.stage_mode(AgentMode::Research);

        // display/mode already updated by stage_*
        assert_eq!(s.model_display, "anthropic/claude-opus-4-6");
        assert_eq!(s.mode, AgentMode::Research);

        let (model, mode) = s.consume_staged();

        // staged fields cleared
        assert!(s.staged_model.is_none());
        assert!(s.staged_mode.is_none());
        // returned values carry the configs for the agent
        assert_eq!(model.unwrap().provider, "anthropic");
        assert_eq!(mode, Some(AgentMode::Research));
        // display/mode unchanged after consume
        assert_eq!(s.model_display, "anthropic/claude-opus-4-6");
        assert_eq!(s.mode, AgentMode::Research);
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
