// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The decision runtime of one session: what the configuration allows, the
//! provider that answers, and the single diagnostic a session gives when
//! decisions are unavailable.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::brain::BrainProvider;
use crate::config::{DecisionConfig, DecisionMode, Feature, PolicyMode};
use crate::telemetry::DecisionTelemetry;
use crate::DecisionError;

/// The decision state of a session. Built from the configuration; contacts
/// nothing until a decision is asked for, and never when decisions are off.
pub struct DecisionRuntime {
    config: DecisionConfig,
    provider: Option<Arc<BrainProvider>>,
    diagnosed: AtomicBool,
    telemetry: Arc<DecisionTelemetry>,
}

impl DecisionRuntime {
    /// The runtime `config` describes.
    ///
    /// # Errors
    ///
    /// The first value of the section that is out of range.
    pub fn new(config: DecisionConfig) -> Result<Self, String> {
        config.validate()?;
        let provider = match config.effective_mode() {
            DecisionMode::Off => None,
            DecisionMode::Auto | DecisionMode::Required => {
                Some(Arc::new(BrainProvider::new(config.brain_settings())?))
            }
        };
        Ok(Self {
            config,
            provider,
            diagnosed: AtomicBool::new(false),
            telemetry: Arc::default(),
        })
    }

    /// A runtime that never contacts anything.
    #[must_use]
    pub fn off() -> Self {
        Self {
            config: DecisionConfig::default(),
            provider: None,
            diagnosed: AtomicBool::new(false),
            telemetry: Arc::default(),
        }
    }

    /// The section this runtime was built from.
    #[must_use]
    pub fn config(&self) -> &DecisionConfig {
        &self.config
    }

    /// The provider, unless decisions are off.
    #[must_use]
    pub fn provider(&self) -> Option<&Arc<BrainProvider>> {
        self.provider.as_ref()
    }

    /// Whether `feature` may ask a model: decisions are on and the file allows it.
    #[must_use]
    pub fn feature_enabled(&self, feature: Feature) -> bool {
        self.provider.is_some() && self.config.features.allows(feature)
    }

    /// Whether verdicts are only recorded: behaviour stays the legacy
    /// behaviour.
    #[must_use]
    pub fn shadow(&self) -> bool {
        self.config.policy.mode == PolicyMode::Shadow
    }

    /// The session's account of what decisions cost and became of.
    #[must_use]
    pub fn telemetry(&self) -> &Arc<DecisionTelemetry> {
        &self.telemetry
    }

    /// How long one decision may take.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.config.timeout_ms)
    }

    /// The text of the session's one diagnostic for `error`, the first time
    /// this is called; `None` afterwards, and always when
    /// `warn_if_unavailable` is false. A session says decisions are
    /// unavailable once, not once per decision.
    #[must_use]
    pub fn diagnose(&self, error: &DecisionError) -> Option<String> {
        let first = !self.diagnosed.swap(true, Ordering::SeqCst);
        (first && self.config.warn_if_unavailable).then(|| {
            format!(
                "Decisions unavailable, using the built-in behaviour: {}",
                unavailable_text(&self.config, error)
            )
        })
    }

    /// In `required` mode, checks that a suitable model is ready before
    /// anything that depends on one starts. Without a decision point that
    /// is enabled there is nothing to wait for. Loads no model.
    ///
    /// # Errors
    ///
    /// A message naming why no model can be used and the remedy.
    pub async fn require_ready(&self) -> Result<(), String> {
        let needed = Feature::ALL
            .into_iter()
            .any(|f| f.decision_point().is_some() && self.feature_enabled(f));
        let Some(provider) = self
            .provider
            .as_ref()
            .filter(|_| needed && self.config.effective_mode() == DecisionMode::Required)
        else {
            return Ok(());
        };
        provider
            .readiness(self.timeout())
            .await
            .map(drop)
            .map_err(|e| {
                format!(
                    "decision.mode is required but no suitable decision model is available: {}",
                    unavailable_text(&self.config, &e)
                )
            })
    }
}

/// What is wrong and exactly what to do about it, for each way decisions can
/// be unavailable. The model id in a remedy is always one the provider
/// itself reported.
#[must_use]
pub fn unavailable_text(config: &DecisionConfig, error: &DecisionError) -> String {
    match error {
        DecisionError::Unreachable(detail) => format!(
            "the decision provider at {} is unreachable ({detail}). Start it (`brain serve \
             --openai`), correct decision.brain.url, or set decision.enabled: false.",
            config.brain.url
        ),
        DecisionError::ModelNotInstalled { model, detail } => {
            let remedy = if detail.contains("brain pull") {
                String::new()
            } else {
                " Install a validated decision model on the provider, or name an installed \
                 one in decision.model (`sven decisions models` lists them)."
                    .to_string()
            };
            format!("decision model '{model}' is not installed ({detail}).{remedy}")
        }
        DecisionError::FailedToLoad { model, detail } => format!(
            "decision model '{model}' failed to load ({detail}). Check the model on the \
             provider, or name another in decision.model (`sven decisions models` lists them)."
        ),
        DecisionError::UnsupportedCapability(detail) => format!(
            "the decision model cannot be used ({detail}). `sven doctor decisions` shows what \
             it supports."
        ),
        DecisionError::Timeout { .. } => format!(
            "the decision model did not answer within {} ms; the first decision may be loading \
             the model. Raise decision.timeout_ms, or run `sven doctor decisions`.",
            config.timeout_ms
        ),
        DecisionError::InvalidResponse(detail) => format!(
            "the provider answered with something that is not a decision ({detail}); the \
             provider and sven may speak different versions. Run `sven doctor decisions`."
        ),
        DecisionError::InvalidRequest(detail) => {
            format!("sven sent a request the provider cannot accept ({detail}); this is a defect.")
        }
    }
}
