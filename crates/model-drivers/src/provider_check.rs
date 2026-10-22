// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for making agent setups that fail
// early and explain why for its clients. If your team needs expertise in
// reliable LLM provider integration then you can procure our services by
// sending an email to info@swedishembedded.com.
//! Whether the configured model can be used, answered before a session
//! starts: the provider builds, its endpoint answers with the key, it serves
//! the model, and the model can call tools.
//!
//! A hosted provider is asked only for its model list, which costs nothing
//! on the user's account; no completion is ever requested.

use serde::Serialize;

use crate::config::ModelConfig;
use crate::discovery::effective_base_url;
use crate::reachable::{find, served_by, shown};

/// How a check came out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Nothing to do.
    Ok,
    /// Usable, with a limit worth knowing.
    Warn,
    /// Not usable as configured.
    Fail,
}

/// One thing checked.
#[derive(Clone, Debug, Serialize)]
pub struct ProviderCheck {
    /// What was checked: `provider`, `endpoint`, `model` or `tools`.
    pub name: &'static str,
    /// How it came out.
    pub verdict: Verdict,
    /// What was found.
    pub detail: String,
    /// What to change, when it is not `ok`.
    pub fix: Option<String>,
}

impl ProviderCheck {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            verdict: Verdict::Ok,
            detail: detail.into(),
            fix: None,
        }
    }

    fn problem(
        verdict: Verdict,
        name: &'static str,
        detail: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self {
            name,
            verdict,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
}

/// What [`check_provider`] found.
#[derive(Clone, Debug, Serialize)]
pub struct ProviderReport {
    /// Every check made, in order; a failed one ends the list.
    pub checks: Vec<ProviderCheck>,
}

impl ProviderReport {
    /// Whether a session can use the model: no check failed.
    #[must_use]
    pub fn ready(&self) -> bool {
        self.checks.iter().all(|c| c.verdict != Verdict::Fail)
    }
}

/// Checks the model `cfg` selects. A provider whose model list cannot be
/// read the OpenAI way (Google, AWS, Azure, Cohere, D-Bus) stops after the
/// provider check.
pub async fn check_provider(cfg: &ModelConfig) -> ProviderReport {
    let mut checks = Vec::new();
    let cfg = &crate::resolve_probed(cfg).await;
    let provider = match crate::from_config(cfg) {
        Ok(provider) => provider,
        Err(e) => {
            let message = e.to_string();
            let mut lines = message.lines();
            let detail = lines.next().unwrap_or_default().to_string();
            let fix = lines.collect::<Vec<_>>().join(" ");
            checks.push(ProviderCheck::problem(
                Verdict::Fail,
                "provider",
                detail,
                if fix.trim().is_empty() {
                    "run `sven list-providers` and choose a model with --model".to_string()
                } else {
                    fix
                },
            ));
            return ProviderReport { checks };
        }
    };
    let model = provider.model_name().to_string();
    checks.push(ProviderCheck::ok(
        "provider",
        format!("{}/{model}", provider.name()),
    ));

    let base = effective_base_url(cfg)
        .map(|b| shown(&b))
        .unwrap_or_default();
    let served = match served_by(cfg).await {
        Ok(served) => served,
        Err(problem) if problem.contains("does not list its models") => {
            return ProviderReport { checks }
        }
        Err(problem) => {
            checks.push(ProviderCheck::problem(
                Verdict::Fail,
                "endpoint",
                problem,
                "start the server and load a model, or correct base_url and the key (`sven auth set <provider>`)",
            ));
            return ProviderReport { checks };
        }
    };
    checks.push(ProviderCheck::ok(
        "endpoint",
        format!("{base} serves {} model(s)", served.len()),
    ));

    let Some(entry) = find(&served, &model) else {
        let mut ids: Vec<&str> = served.iter().map(|m| m.id.as_str()).collect();
        let more = ids.len().saturating_sub(12);
        ids.truncate(12);
        let listed = if more > 0 {
            format!("{} and {more} more", ids.join(", "))
        } else {
            ids.join(", ")
        };
        checks.push(ProviderCheck::problem(
            Verdict::Fail,
            "model",
            format!("{model:?} is not served; the server has {listed}"),
            "name one of those with --model (`sven list-models` lists every one), or load the model on the server",
        ));
        return ProviderReport { checks };
    };
    checks.push(ProviderCheck::ok("model", format!("{model} is served")));
    checks.push(match entry.tools {
        Some(true) => ProviderCheck::ok("tools", "the server advertises tool calling"),
        Some(false) => ProviderCheck::problem(
            Verdict::Warn,
            "tools",
            "the server does not advertise tool calling for this model",
            "agent tasks that edit files or run commands need a model with tools",
        ),
        None => ProviderCheck::ok("tools", "the server does not say; assumed available"),
    });
    ProviderReport { checks }
}
