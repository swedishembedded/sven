// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! `sven doctor decisions`: what the decision runtime would do with this
//! configuration and this provider, found out by asking.

use std::time::Duration;

use sven_rt::Instant;

use serde::Serialize;

use crate::brain::BrainProvider;
use crate::config::{DecisionConfig, DecisionMode, Feature};
use crate::runtime::unavailable_text;
use crate::{
    Choice, ChoiceOption, DecisionProvider, DecisionRequest, ErrorClass, ModelInfo, Question,
    QuestionKind,
};

/// How long the doctor waits for the provider; a first decision may load
/// the model.
const PROBE_DEADLINE: Duration = Duration::from_secs(30);
/// Decisions asked to profile latency.
pub const PROBE_COUNT: usize = 3;

/// Whether decisions work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Status {
    Ready,
    Unavailable { class: ErrorClass, message: String },
}

/// The model decisions would go to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelSummary {
    pub id: String,
    pub backend: Option<String>,
    pub validated: bool,
    pub validation_profile: Option<String>,
    pub question_types: Vec<String>,
    /// What the model says it holds.
    pub input_limit_tokens: Option<u32>,
    /// What one decision may send: the configured cap within that limit.
    pub input_budget_tokens: u32,
}

/// Wall time of the tiny probe decisions, in milliseconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LatencyProfile {
    /// The first call, which may include loading the model.
    pub first_ms: u64,
    /// The median of the calls after it.
    pub steady_median_ms: Option<u64>,
    pub max_ms: u64,
}

/// Whether a decision point can ask a model, and if not why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FeatureReport {
    pub name: &'static str,
    pub active: bool,
    pub reason: String,
}

/// Everything `sven doctor decisions` reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorReport {
    /// What the session does: `auto`, `required` or `off`.
    pub mode: &'static str,
    pub provider_url: String,
    pub requested_model: String,
    pub status: Status,
    pub model: Option<ModelSummary>,
    pub latency: Option<LatencyProfile>,
    pub features: Vec<FeatureReport>,
}

/// Asks the provider about the configured model and profiles it with a few
/// tiny decisions. Contacts the provider even when decisions are off: the
/// doctor is how to find out before turning them on.
pub async fn run_doctor(config: &DecisionConfig) -> DoctorReport {
    let mode = match config.effective_mode() {
        DecisionMode::Auto => "auto",
        DecisionMode::Required => "required",
        DecisionMode::Off => "off",
    };
    let mut report = DoctorReport {
        mode,
        provider_url: config.brain.url.clone(),
        requested_model: config.model.clone(),
        status: Status::Ready,
        model: None,
        latency: None,
        features: Vec::new(),
    };
    let provider = match config
        .validate()
        .and_then(|()| BrainProvider::new(config.brain_settings()))
    {
        Ok(provider) => provider,
        Err(message) => {
            report.status = Status::Unavailable {
                class: ErrorClass::InvalidRequest,
                message,
            };
            report.features = feature_reports(config, mode, None);
            return report;
        }
    };
    match provider.readiness(config.timeout_ms_or_probe()).await {
        Ok(model) => {
            report.model = Some(summarise(config, &model));
            report.latency = profile(&provider).await;
            if report.latency.is_none() {
                report.status = Status::Unavailable {
                    class: ErrorClass::Timeout,
                    message: "the model is ready but did not answer a probe decision".into(),
                };
            }
            report.features = feature_reports(config, mode, Some(&model));
        }
        Err(error) => {
            report.status = Status::Unavailable {
                class: error.class(),
                message: unavailable_text(config, &error),
            };
            report.features = feature_reports(config, mode, None);
        }
    }
    report
}

/// Summarises `model` as decisions would use it.
#[must_use]
pub fn summarise(config: &DecisionConfig, model: &ModelInfo) -> ModelSummary {
    ModelSummary {
        id: model.id.clone(),
        backend: model.backend.clone(),
        validated: model.validated,
        validation_profile: model.validation_profile.clone(),
        question_types: model
            .question_types
            .iter()
            .map(|k| k.name().to_string())
            .collect(),
        input_limit_tokens: model.max_input_tokens,
        input_budget_tokens: config.input_budget_tokens(model.max_input_tokens),
    }
}

async fn profile(provider: &BrainProvider) -> Option<LatencyProfile> {
    let mut samples = Vec::with_capacity(PROBE_COUNT);
    for n in 0..PROBE_COUNT {
        // A different state each time keeps the cache out of the measurement.
        let request = DecisionRequest {
            state: format!("probe {n}: which letter comes first"),
            questions: vec![Question::Choice(Choice {
                id: "probe".into(),
                instruction: "Which letter comes first in the alphabet?".into(),
                options: ["a", "b"]
                    .map(|id| ChoiceOption {
                        id: id.into(),
                        description: None,
                    })
                    .into(),
            })],
            deadline: PROBE_DEADLINE,
        };
        let started = Instant::now();
        provider.decide(&request).await.ok()?;
        samples.push(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
    }
    let mut steady: Vec<u64> = samples[1..].to_vec();
    steady.sort_unstable();
    Some(LatencyProfile {
        first_ms: samples[0],
        steady_median_ms: steady.get(steady.len() / 2).copied(),
        max_ms: samples.iter().copied().max().unwrap_or(0),
    })
}

fn feature_reports(
    config: &DecisionConfig,
    mode: &str,
    model: Option<&ModelInfo>,
) -> Vec<FeatureReport> {
    Feature::ALL
        .into_iter()
        .map(|feature| {
            let (active, reason) = if !config.features.allows(feature) {
                (
                    false,
                    format!("decision.features.{} is false", feature.name()),
                )
            } else if feature.decision_point().is_none() {
                (false, "no decision point uses this flag yet".to_string())
            } else if mode == "off" {
                (
                    false,
                    "decisions are off (decision.enabled is false or mode is off)".into(),
                )
            } else if model.is_none() {
                (false, "no usable decision model".to_string())
            } else if model.is_some_and(|m| !m.question_types.contains(&QuestionKind::Choice)) {
                (
                    false,
                    "the model does not answer choice questions".to_string(),
                )
            } else {
                (
                    true,
                    feature.decision_point().unwrap_or_default().to_string(),
                )
            };
            FeatureReport {
                name: feature.name(),
                active,
                reason,
            }
        })
        .collect()
}

impl DecisionConfig {
    /// The deadline for finding the model out: the decision deadline, but
    /// never less than a couple of seconds, because a doctor that gives up
    /// before a cold provider answers diagnoses nothing.
    fn timeout_ms_or_probe(&self) -> Duration {
        Duration::from_millis(self.timeout_ms.max(2_000))
    }
}

impl DoctorReport {
    /// Whether decisions work.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.status == Status::Ready
    }

    /// The report as text for a terminal.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!(
            "decisions: mode {} | provider {} | requested model {}\n",
            self.mode, self.provider_url, self.requested_model
        );
        match &self.status {
            Status::Ready => out.push_str("status: ready\n"),
            Status::Unavailable { class, message } => {
                out.push_str(&format!(
                    "status: unavailable ({})\n  {message}\n",
                    class.name()
                ));
            }
        }
        if let Some(m) = &self.model {
            out.push_str(&format!(
                "model: {} ({})\n  validation: {}\n  question types: {}\n  input limit: {} tokens (one decision sends at most {})\n",
                m.id,
                m.backend.as_deref().unwrap_or("unknown backend"),
                match (&m.validated, &m.validation_profile) {
                    (true, Some(profile)) => format!("validated on {profile}"),
                    (true, None) => "validated".to_string(),
                    (false, _) => "not validated".to_string(),
                },
                m.question_types.join(", "),
                m.input_limit_tokens.map_or("unreported".to_string(), |n| n.to_string()),
                m.input_budget_tokens,
            ));
        }
        if let Some(l) = &self.latency {
            out.push_str(&format!(
                "latency: first {} ms, steady median {}, max {} ms ({PROBE_COUNT} probe decisions)\n",
                l.first_ms,
                l.steady_median_ms.map_or("n/a".to_string(), |ms| format!("{ms} ms")),
                l.max_ms,
            ));
        }
        out.push_str("features:\n");
        for f in &self.features {
            out.push_str(&format!(
                "  {:<20} {:<8} {}\n",
                f.name,
                if f.active { "active" } else { "inactive" },
                f.reason
            ));
        }
        out
    }
}

/// `sven decisions models`: the provider's models as text.
#[must_use]
pub fn render_models(config: &DecisionConfig, models: &[ModelInfo]) -> String {
    if models.is_empty() {
        return "the provider knows no decision model\n".to_string();
    }
    let mut out = String::new();
    for m in models {
        let selected = if m.id == config.model {
            " (configured)"
        } else {
            ""
        };
        out.push_str(&format!(
            "{}{selected}\n  state: {}{}\n  validation: {}\n  question types: {}\n  input limit: {}\n",
            m.id,
            m.state.name(),
            m.hint
                .as_deref()
                .or(m.detail.as_deref())
                .map_or(String::new(), |h| format!(" - {h}")),
            match (m.validated, &m.validation_profile) {
                (true, Some(p)) => format!("validated on {p}"),
                (true, None) => "validated".to_string(),
                (false, _) => "not validated".to_string(),
            },
            m.question_types
                .iter()
                .map(|k| k.name())
                .collect::<Vec<_>>()
                .join(", "),
            m.max_input_tokens
                .map_or("unreported".to_string(), |n| format!("{n} tokens")),
        ));
    }
    out
}
