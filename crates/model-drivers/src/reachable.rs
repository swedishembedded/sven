// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for running agents across local
// and hosted models at once, with every usable model one name away, for its
// clients. If your team needs expertise in multi-provider LLM setups then you
// can procure our services by sending an email to info@swedishembedded.com.
//! Every model this user can use right now, and the name `--model` takes for
//! each.
//!
//! The sources are the local servers that answer (a brain server found from
//! its connection record, and ollama, vLLM and LM Studio on their default
//! ports), every `providers:` entry, and every hosted provider with a stored
//! key (`sven auth set`). Each is asked for its model list; one that does not
//! answer, or refuses its key, is reported as a problem rather than dropped.

use std::time::Duration;

use serde::Serialize;
use sven_model::{registry, ServedModel, ServedModels};

use crate::api_key::resolve_api_key;
use crate::config::{ModelConfig, Providers};
use crate::discovery::{
    effective_base_url, fetch_served_models, is_local_endpoint, parse_served_models,
};

/// How long a hosted provider may take to list its models.
const HOSTED_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a local server may take to list its models.
const LOCAL_TIMEOUT: Duration = Duration::from_secs(2);

/// Local servers asked even when nothing names them, by driver id.
const LOCAL_DRIVERS: [&str; 4] = ["brain", "ollama", "vllm", "lmstudio"];

/// Drivers whose model list is not the OpenAI-shaped `GET /models` this
/// module reads, so they are not asked.
const UNLISTED_DRIVERS: [&str; 6] = ["google", "aws", "cohere", "azure", "dbus", "mock"];

/// One model that can be used now.
#[derive(Clone, Debug, Serialize)]
pub struct ReachableModel {
    /// What `--model` takes for it: `<provider or entry>/<model id>`.
    pub reference: String,
    /// The driver id or `providers:` entry it is reached through.
    pub source: String,
    /// The model id its server uses.
    pub id: String,
    /// The context window the server reports, when it reports one.
    pub context_length: Option<u32>,
    /// The output cap the server reports, when it reports one.
    pub max_output_tokens: Option<u32>,
    /// Whether the server says the model can call tools; `None` when it does
    /// not say.
    pub tools: Option<bool>,
}

/// A source that was asked and could not say.
#[derive(Clone, Debug, Serialize)]
pub struct SourceProblem {
    /// The driver id or `providers:` entry.
    pub source: String,
    /// What went wrong.
    pub problem: String,
}

/// What [`reachable_models`] found.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Reachable {
    /// Every usable model, local sources first.
    pub models: Vec<ReachableModel>,
    /// Every source that was asked and did not answer with a list.
    pub problems: Vec<SourceProblem>,
}

/// Ask every source this user can reach for its models.
pub async fn reachable_models(providers: &Providers) -> Reachable {
    let mut sources: Vec<(String, ModelConfig, bool)> = Vec::new();
    for id in LOCAL_DRIVERS {
        let cfg = ModelConfig {
            provider: id.to_string(),
            name: String::new(),
            ..ModelConfig::default()
        };
        // Unconfigured local servers are only reported when they answer.
        sources.push((id.to_string(), cfg, false));
    }
    let mut aliases: Vec<&String> = providers.keys().collect();
    aliases.sort();
    for alias in aliases {
        sources.push((alias.clone(), providers[alias].to_model_config(""), true));
    }
    for id in crate::credentials::providers() {
        if !LOCAL_DRIVERS.contains(&id.as_str()) && registry::get_driver(&id).is_some() {
            let cfg = ModelConfig {
                provider: id.clone(),
                name: String::new(),
                ..ModelConfig::default()
            };
            sources.push((id, cfg, true));
        }
    }
    let asked = sources.into_iter().map(|(source, cfg, report)| async move {
        let listed = served_by(&cfg).await;
        (source, report, listed)
    });
    let mut found = Reachable::default();
    for (source, report, listed) in futures::future::join_all(asked).await {
        match listed {
            Ok(models) => found
                .models
                .extend(models.into_iter().map(|m| ReachableModel {
                    reference: format!("{source}/{}", m.id),
                    source: source.clone(),
                    id: m.id,
                    context_length: m.context_length,
                    max_output_tokens: m.max_output_tokens,
                    tools: m.tools,
                })),
            Err(problem) if report => found.problems.push(SourceProblem { source, problem }),
            Err(_) => {}
        }
    }
    found
}

/// The models the endpoint `cfg` names serves, asked with its key. Errors
/// say what a person needs to know: it did not answer, it refused the key,
/// or its models cannot be listed this way.
pub(crate) async fn served_by(cfg: &ModelConfig) -> Result<ServedModels, String> {
    if UNLISTED_DRIVERS.contains(&cfg.provider.as_str()) {
        return Err(format!(
            "{} does not list its models this way",
            cfg.provider
        ));
    }
    let base =
        effective_base_url(cfg).ok_or_else(|| format!("{} has no base_url", cfg.provider))?;
    let local = is_local_endpoint(&base);
    let key = resolve_api_key(cfg);
    if !local && key.is_none() {
        return Err(format!(
            "no key for {}; store one with `sven auth set {}`",
            cfg.provider, cfg.provider
        ));
    }
    let url = format!("{}/models", base.trim_end_matches('/'));
    let mut req = crate::build_http_client().get(&url);
    if let Some(key) = &key {
        req = if cfg.provider == "anthropic" {
            req.header("x-api-key", key)
                .header("anthropic-version", "2023-06-01")
        } else {
            req.bearer_auth(key)
        };
    }
    if local {
        // A local server either answers at once or is not running.
        return fetch_served_models(req, LOCAL_TIMEOUT)
            .await
            .filter(|m| !m.is_empty())
            .ok_or_else(|| format!("{} did not answer with a model list", shown(&base)));
    }
    let resp = req
        .timeout(HOSTED_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("{} did not answer ({e})", shown(&base)))?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(format!(
            "{} refused the key (HTTP {})",
            cfg.provider,
            status.as_u16()
        ));
    }
    if !status.is_success() {
        return Err(format!(
            "{} answered HTTP {} to its model list",
            shown(&base),
            status.as_u16()
        ));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("{}: {e}", shown(&base)))?;
    parse_served_models(&body).ok_or_else(|| {
        format!(
            "{} answered something that is not a model list",
            shown(&base)
        )
    })
}

/// `url` without credentials, query or fragment, for showing to a person.
pub(crate) fn shown(url: &str) -> String {
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

/// The served model `id`, when `models` has it.
pub(crate) fn find<'a>(models: &'a [ServedModel], id: &str) -> Option<&'a ServedModel> {
    models.iter().find(|m| m.id == id)
}
