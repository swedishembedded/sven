// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The `decision` section of the configuration file.
//!
//! ```yaml
//! decision:
//!   enabled: false            # nothing is contacted unless this is true
//!   mode: auto                # auto | required | off
//!   provider: brain
//!   model: auto               # "auto", or a stable model id
//!   timeout_ms: 500
//!   warn_if_unavailable: true
//!   brain:
//!     url: http://127.0.0.1:8788
//!   features: {tool_routing: true, ...}
//!   policy: {mode: apply, minimum_model_quality: validated}
//! ```

use serde::{Deserialize, Serialize};
use sven_config::{default_true, Schema};

use crate::brain::{BrainSettings, DEFAULT_URL};
use crate::directory::{MinimumQuality, AUTO};

/// Decision deadline when the file names none.
pub const DEFAULT_TIMEOUT_MS: u64 = 500;
/// Longest deadline the provider accepts.
pub const MAX_TIMEOUT_MS: u64 = 600_000;
/// The input budget when neither the file nor the served model names a
/// limit: below the smallest context a decision model is trained for.
pub const FALLBACK_INPUT_TOKENS: u32 = 256;

/// What the session does about decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionMode {
    /// Use a decision model when one is usable; when it is not, behave
    /// exactly as before decisions existed.
    Auto,
    /// Refuse to start optimised execution without a suitable model.
    Required,
    /// Never contact anything.
    Off,
}

/// Where decisions come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// A Brain server, reached over HTTP.
    Brain,
}

/// How a verdict is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyMode {
    /// An accepted verdict changes behaviour.
    Apply,
    /// Decisions are asked and recorded in the trajectory, and behaviour is
    /// always the legacy behaviour: the way to see what a model would do
    /// before letting it.
    Shadow,
}

/// `decision.brain`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrainConfig {
    /// The server's base URL; where `brain serve --openai` listens by default.
    pub url: String,
    /// Bearer token, when the server was started with a key. `${VAR}` is
    /// expanded when the file is loaded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Most decisions in flight at once.
    pub max_concurrent_requests: usize,
    /// Most decisions the per-process cache holds.
    pub cache_entries: usize,
}

impl Default for BrainConfig {
    fn default() -> Self {
        let defaults = BrainSettings::new(DEFAULT_URL);
        Self {
            url: defaults.base_url,
            api_key: None,
            max_concurrent_requests: defaults.max_concurrent_requests,
            cache_entries: defaults.cache_entries,
        }
    }
}

/// A decision point that may ask a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    ContextSelection,
    ToolRouting,
    SkillSelection,
    Delegation,
    ModelRouting,
    CacheRouting,
    PermissionAdvisory,
}

impl Feature {
    /// Every feature, in the order the configuration lists them.
    pub const ALL: [Feature; 7] = [
        Self::ContextSelection,
        Self::ToolRouting,
        Self::SkillSelection,
        Self::Delegation,
        Self::ModelRouting,
        Self::CacheRouting,
        Self::PermissionAdvisory,
    ];

    /// The key of the flag in `decision.features`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::ContextSelection => "context_selection",
            Self::ToolRouting => "tool_routing",
            Self::SkillSelection => "skill_selection",
            Self::Delegation => "delegation",
            Self::ModelRouting => "model_routing",
            Self::CacheRouting => "cache_routing",
            Self::PermissionAdvisory => "permission_advisory",
        }
    }

    /// What asks the model for this feature, or `None` when no decision
    /// point does yet and the flag changes nothing.
    #[must_use]
    pub fn decision_point(self) -> Option<&'static str> {
        match self {
            Self::ToolRouting => Some("re-ranking of ambiguous `tools search` results"),
            _ => None,
        }
    }
}

/// `decision.features`: which decision points may ask a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FeatureFlags {
    pub context_selection: bool,
    /// Re-ranks ambiguous `tools search` results.
    pub tool_routing: bool,
    pub skill_selection: bool,
    pub delegation: bool,
    pub model_routing: bool,
    pub cache_routing: bool,
    pub permission_advisory: bool,
}

impl FeatureFlags {
    /// Whether the file allows `feature` to ask a model.
    #[must_use]
    pub fn allows(&self, feature: Feature) -> bool {
        match feature {
            Feature::ContextSelection => self.context_selection,
            Feature::ToolRouting => self.tool_routing,
            Feature::SkillSelection => self.skill_selection,
            Feature::Delegation => self.delegation,
            Feature::ModelRouting => self.model_routing,
            Feature::CacheRouting => self.cache_routing,
            Feature::PermissionAdvisory => self.permission_advisory,
        }
    }
}

impl Default for FeatureFlags {
    fn default() -> Self {
        Self {
            context_selection: true,
            tool_routing: true,
            skill_selection: true,
            delegation: true,
            model_routing: false,
            cache_routing: true,
            permission_advisory: false,
        }
    }
}

/// `decision.policy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicySection {
    pub mode: PolicyMode,
    /// Most tokens of state one decision sends. Unset, the limit the served
    /// model reports; set, the smaller of the two.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_decision_input_tokens: Option<u32>,
    pub minimum_model_quality: MinimumQuality,
}

impl Default for PolicySection {
    fn default() -> Self {
        Self {
            mode: PolicyMode::Apply,
            max_decision_input_tokens: None,
            minimum_model_quality: MinimumQuality::Validated,
        }
    }
}

/// The `decision` section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DecisionConfig {
    /// Decisions are off, whatever `mode` says, unless this is set.
    pub enabled: bool,
    pub mode: DecisionMode,
    pub provider: ProviderKind,
    /// `auto` leaves the choice to the provider; otherwise a stable model id.
    pub model: String,
    /// How long one decision may take, in milliseconds.
    pub timeout_ms: u64,
    /// Say once per session that decisions are unavailable.
    #[serde(default = "default_true")]
    pub warn_if_unavailable: bool,
    pub brain: BrainConfig,
    pub features: FeatureFlags,
    pub policy: PolicySection,
}

impl Default for DecisionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: DecisionMode::Auto,
            provider: ProviderKind::Brain,
            model: AUTO.to_string(),
            timeout_ms: DEFAULT_TIMEOUT_MS,
            warn_if_unavailable: true,
            brain: BrainConfig::default(),
            features: FeatureFlags::default(),
            policy: PolicySection::default(),
        }
    }
}

impl DecisionConfig {
    /// The keys of the `decision` section.
    #[must_use]
    pub fn schema() -> Schema {
        Schema::fields([
            ("enabled", Schema::value()),
            ("mode", Schema::value()),
            ("provider", Schema::value()),
            ("model", Schema::value()),
            ("timeout_ms", Schema::value()),
            ("warn_if_unavailable", Schema::value()),
            (
                "brain",
                Schema::keys(&["url", "api_key", "max_concurrent_requests", "cache_entries"]),
            ),
            (
                "features",
                Schema::keys(&[
                    "context_selection",
                    "tool_routing",
                    "skill_selection",
                    "delegation",
                    "model_routing",
                    "cache_routing",
                    "permission_advisory",
                ]),
            ),
            (
                "policy",
                Schema::keys(&["mode", "max_decision_input_tokens", "minimum_model_quality"]),
            ),
        ])
    }

    /// What the session does: [`DecisionMode::Off`] unless `enabled`.
    #[must_use]
    pub fn effective_mode(&self) -> DecisionMode {
        if self.enabled {
            self.mode
        } else {
            DecisionMode::Off
        }
    }

    /// The tokens of state one decision may send: `max_decision_input_tokens`
    /// capped by the limit the served model reports, and
    /// [`FALLBACK_INPUT_TOKENS`] when neither is known.
    #[must_use]
    pub fn input_budget_tokens(&self, model_limit: Option<u32>) -> u32 {
        match (self.policy.max_decision_input_tokens, model_limit) {
            (Some(configured), Some(limit)) => configured.min(limit),
            (Some(configured), None) => configured,
            (None, Some(limit)) => limit,
            (None, None) => FALLBACK_INPUT_TOKENS,
        }
    }

    /// The transport settings this section describes.
    #[must_use]
    pub fn brain_settings(&self) -> BrainSettings {
        BrainSettings {
            base_url: self.brain.url.clone(),
            api_key: self.brain.api_key.clone(),
            model: self.model.clone(),
            minimum_quality: self.policy.minimum_model_quality,
            max_concurrent_requests: self.brain.max_concurrent_requests,
            cache_entries: self.brain.cache_entries,
        }
    }

    /// Checks every value against its range.
    ///
    /// # Errors
    ///
    /// A message naming the key and what it must be.
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_TIMEOUT_MS).contains(&self.timeout_ms) {
            return Err(format!(
                "decision.timeout_ms must be 1 to {MAX_TIMEOUT_MS}, got {}",
                self.timeout_ms
            ));
        }
        if self.model.trim().is_empty() {
            return Err("decision.model must be `auto` or a model id".into());
        }
        if !(self.brain.url.starts_with("http://") || self.brain.url.starts_with("https://")) {
            return Err(format!(
                "decision.brain.url must be an http:// or https:// URL, got {:?}",
                self.brain.url
            ));
        }
        if self.brain.max_concurrent_requests == 0 {
            return Err("decision.brain.max_concurrent_requests must be at least 1".into());
        }
        if self.brain.cache_entries == 0 {
            return Err("decision.brain.cache_entries must be at least 1".into());
        }
        if self.policy.max_decision_input_tokens == Some(0) {
            return Err("decision.policy.max_decision_input_tokens must be at least 1".into());
        }
        Ok(())
    }
}
