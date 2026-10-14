// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for making agent setups that fail
// early and explain why for its clients. If your team needs expertise in
// reliable LLM provider integration then you can procure our services by
// sending an email to info@swedishembedded.com.
//! Whether the configured model can be used, answered before a session
//! starts: the provider builds, a local server answers, it serves the model,
//! and the model can call tools.
//!
//! A hosted provider is never probed: the check would spend a request on the
//! user's account, and a key that exists says nothing the first request will
//! not.

use serde::Serialize;

use crate::config::ModelConfig;
use crate::discovery::{effective_base_url, is_local_endpoint};

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

/// `url` without credentials, query or fragment, for showing to a person.
fn shown(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(mut parsed) => {
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.set_query(None);
            parsed.set_fragment(None);
            parsed.to_string()
        }
        Err(_) => "(not a URL)".to_string(),
    }
}

/// Checks the model `cfg` selects, probing only a server on this host or a
/// private network.
pub async fn check_provider(cfg: &ModelConfig) -> ProviderReport {
    let mut checks = Vec::new();
    let provider = match crate::from_config_probed(cfg).await {
        Ok(provider) => provider,
        Err(e) => {
            checks.push(ProviderCheck::problem(
                Verdict::Fail,
                "provider",
                // The first line names the problem; the rest repeats the fix.
                e.to_string().lines().next().unwrap_or_default(),
                "run `sven list-providers`, then set the provider's key or choose another with --model",
            ));
            return ProviderReport { checks };
        }
    };
    let model = provider.model_name().to_string();
    checks.push(ProviderCheck::ok(
        "provider",
        format!("{}/{model}", provider.name()),
    ));

    let Some(base) = effective_base_url(cfg).filter(|b| is_local_endpoint(b)) else {
        return ProviderReport { checks };
    };
    let Some(served) = provider.discover().await.filter(|s| !s.is_empty()) else {
        checks.push(ProviderCheck::problem(
            Verdict::Fail,
            "endpoint",
            format!("{} did not answer, or serves no model", shown(&base)),
            "start the server and load a model, or correct base_url and the key",
        ));
        return ProviderReport { checks };
    };
    checks.push(ProviderCheck::ok(
        "endpoint",
        format!("{} serves {} model(s)", shown(&base), served.len()),
    ));

    let Some(entry) = served.iter().find(|m| m.id == model) else {
        let ids: Vec<&str> = served.iter().map(|m| m.id.as_str()).collect();
        checks.push(ProviderCheck::problem(
            Verdict::Fail,
            "model",
            format!("{model:?} is not served; the server has {}", ids.join(", ")),
            "name one of those with --model, or load the model on the server",
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
