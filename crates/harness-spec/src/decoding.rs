// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The sampling, thinking and timing settings of a model turn.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use serde::{Deserialize, Serialize};

/// Sampling, thinking and timing settings of a model turn. Every field is
/// optional: an unset one leaves the run's or the provider's setting. Each
/// holds a literal (a number or a boolean) or a template that is a single
/// hole over the run's inputs (`"{{ inputs.seed }}"`), resolved when the turn
/// starts. Only a version 2 document sets them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepDecoding {
    /// Softmax temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<serde_json::Value>,
    /// Nucleus-sampling mass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<serde_json::Value>,
    /// Number of highest-probability tokens kept before sampling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<serde_json::Value>,
    /// Sampling seed, for providers that honour one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<serde_json::Value>,
    /// Reasoning on or off, where the server has the switch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<serde_json::Value>,
    /// The most thinking tokens before the turn is cut off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<serde_json::Value>,
    /// The most output tokens one response may ask for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<serde_json::Value>,
    /// Seconds to wait for the server's first response before the turn fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_response_timeout: Option<serde_json::Value>,
    /// The most wall-clock seconds one turn may take, whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<serde_json::Value>,
}

impl StepDecoding {
    /// `true` when no setting is made.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The settings, by name.
    #[must_use]
    pub fn settings(&self) -> [(&'static str, &Option<serde_json::Value>); 9] {
        [
            ("temperature", &self.temperature),
            ("top_p", &self.top_p),
            ("top_k", &self.top_k),
            ("seed", &self.seed),
            ("thinking", &self.thinking),
            ("thinking_budget", &self.thinking_budget),
            ("max_output_tokens", &self.max_output_tokens),
            ("first_response_timeout", &self.first_response_timeout),
            ("deadline", &self.deadline),
        ]
    }

    /// These settings with `over`'s laid on top: each setting `over` makes
    /// wins.
    #[must_use]
    pub fn overlaid_by(&self, over: &Self) -> Self {
        let pick = |a: &Option<serde_json::Value>, b: &Option<serde_json::Value>| {
            b.clone().or_else(|| a.clone())
        };
        Self {
            temperature: pick(&self.temperature, &over.temperature),
            top_p: pick(&self.top_p, &over.top_p),
            top_k: pick(&self.top_k, &over.top_k),
            seed: pick(&self.seed, &over.seed),
            thinking: pick(&self.thinking, &over.thinking),
            thinking_budget: pick(&self.thinking_budget, &over.thinking_budget),
            max_output_tokens: pick(&self.max_output_tokens, &over.max_output_tokens),
            first_response_timeout: pick(
                &self.first_response_timeout,
                &over.first_response_timeout,
            ),
            deadline: pick(&self.deadline, &over.deadline),
        }
    }
}
