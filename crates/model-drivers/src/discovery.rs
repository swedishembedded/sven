// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for local model serving for its
// clients: endpoints that report what they serve, adapters that change under a
// running agent, and zero-config start-up. If your team needs expertise in
// self-hosted LLM infrastructure then you can procure our services by sending
// an email to info@swedishembedded.com.
//! Discovery of what a local OpenAI-compatible server is serving.
//!
//! Sven names no model for a local server. It asks the server: `GET /models`
//! lists the served ids and, on servers that report them, each model's context
//! length, output cap, tool-calling capability and applied adapter. Static
//! catalog entries are only fallback metadata for what a server leaves out.
//!
//! Every call here is bounded by [`DISCOVERY_TIMEOUT`] and degrades to "not
//! known" on failure: discovery never fails or delays a turn.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use sven_model::{registry, AdapterRef, ModelChange, ModelProvider, ServedModel, ServedModels};

use crate::api_key::resolve_api_key;
use crate::{build_http_client, ModelConfig};

/// Upper bound for one discovery request against a configured server.
pub(crate) const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);

/// Upper bound for one probe of a default local endpoint nobody configured.
/// Short because most candidates are not running.
pub(crate) const ZERO_CONFIG_TIMEOUT: Duration = Duration::from_secs(1);

/// How long a probe result is trusted before the next turn refreshes it in
/// the background.
pub(crate) const REFRESH_TTL: Duration = Duration::from_secs(45);

/// Local drivers probed when nothing is configured, in priority order. Their
/// URLs come from the driver registry.
const ZERO_CONFIG_DRIVERS: [&str; 4] = ["brain", "ollama", "vllm", "lmstudio"];

// ── Parsing ───────────────────────────────────────────────────────────────────

fn token_count(v: &Value) -> Option<u32> {
    v.as_u64().map(|n| u32::try_from(n).unwrap_or(u32::MAX))
}

/// Capability names that mean "can call tools".
const TOOL_CAPABILITIES: [&str; 4] = ["tools", "tool_use", "tool_calls", "function_calling"];

fn parse_served_model(entry: &Value) -> Option<ServedModel> {
    let id = entry.get("id")?.as_str()?.to_string();
    let capabilities = entry
        .get("capabilities")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|c| c.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        });
    let tools = capabilities
        .as_ref()
        .map(|caps| caps.iter().any(|c| TOOL_CAPABILITIES.contains(&c.as_str())));
    let adapter = entry.get("adapter").and_then(|a| {
        Some(AdapterRef {
            id: a.get("id")?.as_str()?.to_string(),
            digest: a.get("digest")?.as_str()?.to_string(),
        })
    });
    Some(ServedModel {
        id,
        // vLLM reports `max_model_len`, Groq-style hosts `context_window`;
        // brain and OpenRouter `context_length`.
        context_length: entry
            .get("context_length")
            .or_else(|| entry.get("max_model_len"))
            .or_else(|| entry.get("context_window"))
            .and_then(token_count),
        max_output_tokens: entry.get("max_output_tokens").and_then(token_count),
        tools,
        capabilities,
        adapter,
    })
}

/// Parse an OpenAI-shaped `GET /models` body. Every field beyond `id` is
/// optional so servers that predate a field keep working. `None` when the body
/// is not a model list at all.
pub(crate) fn parse_served_models(body: &Value) -> Option<ServedModels> {
    let data = body.get("data")?.as_array()?;
    Some(data.iter().filter_map(parse_served_model).collect())
}

/// Send `req` (a `GET /models`) and parse the answer within `timeout`.
pub(crate) async fn fetch_served_models(
    req: reqwest::RequestBuilder,
    timeout: Duration,
) -> Option<ServedModels> {
    let resp = req.timeout(timeout).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    parse_served_models(&resp.json::<Value>().await.ok()?)
}

// ── Choosing ──────────────────────────────────────────────────────────────────

/// The model to use when the configuration names none: the first chat-capable
/// entry, where "chat-capable" means the server reports a context length (a
/// server that lists image or speech models beside its language models only
/// gives the latter one). A server that reports no context lengths at all is
/// not filtered.
pub(crate) fn choose_model(models: &[ServedModel]) -> Option<&ServedModel> {
    models
        .iter()
        .find(|m| m.context_length.is_some())
        .or_else(|| models.first())
}

// ── Locality ──────────────────────────────────────────────────────────────────

/// Is `url` a server on this host or a private network? Discovery that picks a
/// model on the user's behalf is only safe there: a hosted provider lists
/// hundreds of models and the first is not a sensible default.
pub(crate) fn is_local_endpoint(url: &str) -> bool {
    use std::net::IpAddr;
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private(),
        Ok(IpAddr::V6(ip)) => ip.is_loopback(),
        Err(_) => false,
    }
}

/// The base URL a config resolves to: its own, else the driver's default.
pub(crate) fn effective_base_url(cfg: &ModelConfig) -> Option<String> {
    cfg.base_url.clone().or_else(|| {
        registry::get_driver(&cfg.provider)
            .and_then(|m| m.default_base_url)
            .map(str::to_string)
    })
}

// ── Change detection ──────────────────────────────────────────────────────────

/// Compare the adapter seen on the previous probe with the one on this probe.
pub(crate) fn adapter_change(
    model: &str,
    previous: &Option<AdapterRef>,
    current: &Option<AdapterRef>,
) -> Option<ModelChange> {
    (previous != current).then(|| ModelChange {
        model: model.to_string(),
        from: previous.clone(),
        to: current.clone(),
        context_window: None,
    })
}

/// Does a server's error text say the request did not fit its context? Such
/// an error usually means the window changed under us (a different model or
/// adapter was loaded with a smaller one).
pub(crate) fn is_context_overflow(error_text: &str) -> bool {
    let t = error_text.to_ascii_lowercase();
    t.contains("context_length_exceeded")
        || t.contains("n_ctx")
        || (t.contains("context")
            && ["exceed", "too long", "too large", "maximum", "window"]
                .iter()
                .any(|w| t.contains(w)))
}

// ── Live state of a served model ──────────────────────────────────────────────

const TOOLS_UNKNOWN: u8 = 0;
const TOOLS_NO: u8 = 1;
const TOOLS_YES: u8 = 2;

/// What sven currently believes about the model it is bound to, refreshed from
/// the live server. Shared between the provider and its background refresh.
pub(crate) struct LiveState {
    model: String,
    /// Only a local server is re-probed during a session.
    refreshable: bool,
    /// Context window the server reported; 0 = unknown.
    context_window: AtomicU32,
    /// Output cap the server reported; 0 = unknown.
    max_output_tokens: AtomicU32,
    /// Tool support the server reported: [`TOOLS_UNKNOWN`], `NO` or `YES`.
    tools: AtomicU8,
    /// The adapter seen on the last answering probe; the outer `None` until
    /// there is one, so the first answer is a baseline and not a change.
    adapter: Mutex<Option<Option<AdapterRef>>>,
    /// A change seen and not yet taken by the session.
    pending_change: Mutex<Option<ModelChange>>,
    last_probe: Mutex<Option<Instant>>,
    in_flight: AtomicBool,
}

impl LiveState {
    pub(crate) fn new(model: &str, refreshable: bool) -> Arc<Self> {
        Arc::new(Self {
            model: model.to_string(),
            refreshable,
            context_window: AtomicU32::new(0),
            max_output_tokens: AtomicU32::new(0),
            tools: AtomicU8::new(TOOLS_UNKNOWN),
            adapter: Mutex::new(None),
            pending_change: Mutex::new(None),
            last_probe: Mutex::new(None),
            in_flight: AtomicBool::new(false),
        })
    }

    pub(crate) fn context_window(&self) -> Option<u32> {
        Some(self.context_window.load(Ordering::Relaxed)).filter(|&n| n > 0)
    }

    pub(crate) fn max_output_tokens(&self) -> Option<u32> {
        Some(self.max_output_tokens.load(Ordering::Relaxed)).filter(|&n| n > 0)
    }

    pub(crate) fn tool_support(&self) -> Option<bool> {
        match self.tools.load(Ordering::Relaxed) {
            TOOLS_NO => Some(false),
            TOOLS_YES => Some(true),
            _ => None,
        }
    }

    /// The adapter change seen since the last call, with the window the
    /// server reports now. Taking it clears it.
    pub(crate) fn take_change(&self) -> Option<ModelChange> {
        let mut change = self
            .pending_change
            .lock()
            .expect("pending change poisoned")
            .take()?;
        change.context_window = self.context_window();
        Some(change)
    }

    pub(crate) fn is_refreshable(&self) -> bool {
        self.refreshable
    }

    pub(crate) fn is_stale(&self, now: Instant) -> bool {
        self.last_probe
            .lock()
            .expect("probe clock poisoned")
            .is_none_or(|t| now.duration_since(t) >= REFRESH_TTL)
    }

    /// Probe `inner` now: update the context window and the applied adapter,
    /// and log an adapter change. A failed probe keeps what was known. Returns
    /// what the server served, when it answered.
    pub(crate) async fn refresh(&self, inner: &dyn ModelProvider) -> Option<ServedModels> {
        let window = tokio::time::timeout(DISCOVERY_TIMEOUT, inner.probe_context_window())
            .await
            .ok()
            .flatten();
        // Hosted providers list hundreds of models and have no adapters to
        // swap; only a local server is asked what it serves.
        let served = if self.refreshable {
            tokio::time::timeout(DISCOVERY_TIMEOUT, inner.discover())
                .await
                .ok()
                .flatten()
        } else {
            None
        };
        *self.last_probe.lock().expect("probe clock poisoned") = Some(Instant::now());

        let entry = served
            .as_ref()
            .and_then(|s| s.iter().find(|m| m.id == self.model));
        if let Some(n) = window.or(entry.and_then(|m| m.context_length)) {
            self.context_window.store(n, Ordering::Relaxed);
        }
        if let Some(m) = entry {
            self.max_output_tokens
                .store(m.max_output_tokens.unwrap_or(0), Ordering::Relaxed);
            self.tools.store(
                m.tools
                    .map_or(TOOLS_UNKNOWN, |t| if t { TOOLS_YES } else { TOOLS_NO }),
                Ordering::Relaxed,
            );
        }
        if served.is_some() {
            self.note_adapter(entry.and_then(|m| m.adapter.clone()));
        }
        served
    }

    /// Record the adapter an answering probe saw. A difference from the one
    /// before it is logged and queued for the session; several differences
    /// before the session looks coalesce into one, and a swap back to the
    /// adapter the session last heard of cancels out.
    fn note_adapter(&self, current: Option<AdapterRef>) {
        let mut known = self.adapter.lock().expect("adapter state poisoned");
        let Some(previous) = known.replace(current.clone()) else {
            return;
        };
        let Some(mut change) = adapter_change(&self.model, &previous, &current) else {
            return;
        };
        tracing::info!(
            model = %change.model,
            from = ?change.from,
            to = ?change.to,
            "served adapter changed"
        );
        let mut pending = self.pending_change.lock().expect("pending change poisoned");
        if let Some(earlier) = pending.take() {
            change.from = earlier.from;
        }
        *pending = (change.from != change.to).then_some(change);
    }

    /// Refresh in the background when the last probe is older than the TTL.
    /// Never blocks the caller; at most one refresh runs at a time.
    pub(crate) fn refresh_if_stale(self: &Arc<Self>, inner: &Arc<dyn ModelProvider>) {
        if !self.refreshable || !self.is_stale(Instant::now()) {
            return;
        }
        if self.in_flight.swap(true, Ordering::AcqRel) {
            return;
        }
        let (state, inner) = (Arc::clone(self), Arc::clone(inner));
        tokio::spawn(async move {
            state.refresh(&*inner).await;
            state.in_flight.store(false, Ordering::Release);
        });
    }
}

// ── Choosing a model when the configuration names none ────────────────────────

fn models_request(base_url: &str, key: Option<&str>) -> reqwest::RequestBuilder {
    let req = build_http_client().get(format!("{}/models", base_url.trim_end_matches('/')));
    match key {
        Some(k) => req.bearer_auth(k),
        None => req,
    }
}

fn log_choice(models: &[ServedModel], chosen: &ServedModel) {
    if models.len() > 1 {
        tracing::info!("{} models served, using {}", models.len(), chosen.id);
    }
}

/// Resolve an EMPTY `cfg.name` against a local server's `/models`.
///
/// One served model resolves to it; several resolve to the first chat-capable
/// one, with an info line. A named model is never touched, and neither is a
/// hosted provider (see [`is_local_endpoint`]). An unreachable or silent
/// server leaves the config as it was, so the request later fails with the
/// server's own error.
pub(crate) async fn resolve_empty_model_name(cfg: &ModelConfig) -> ModelConfig {
    if !cfg.name.is_empty() {
        return cfg.clone();
    }
    let Some(base) = effective_base_url(cfg).filter(|b| is_local_endpoint(b)) else {
        return cfg.clone();
    };
    let key = resolve_api_key(cfg);
    let req = models_request(&base, key.as_deref());
    let Some(models) = fetch_served_models(req, DISCOVERY_TIMEOUT).await else {
        return cfg.clone();
    };
    match choose_model(&models) {
        Some(chosen) => {
            log_choice(&models, chosen);
            ModelConfig {
                name: chosen.id.clone(),
                ..cfg.clone()
            }
        }
        None => cfg.clone(),
    }
}

/// Settle a brain that configuration loading merely *detected* (a keys file
/// or `BRAIN_API_KEY`, no network check): when it answers with at least one
/// model it is kept, otherwise the cloud choice held in
/// [`ModelConfig::detection_fallback`] replaces it. A configuration without a
/// fallback - everything an operator wrote down - is returned unchanged.
pub(crate) async fn resolve_detected_brain(cfg: &ModelConfig) -> ModelConfig {
    let Some(fallback) = cfg.detection_fallback.as_deref() else {
        return cfg.clone();
    };
    let kept = ModelConfig {
        detection_fallback: None,
        ..cfg.clone()
    };
    let Some(base) = effective_base_url(&kept) else {
        return kept;
    };
    let req = models_request(&base, resolve_api_key(&kept).as_deref());
    let served = fetch_served_models(req, DISCOVERY_TIMEOUT).await;
    if served.is_some_and(|models| !models.is_empty()) {
        return kept;
    }
    tracing::warn!(
        "brain was detected locally but serves no model at {base}; using {}/{} instead",
        fallback.provider,
        fallback.name
    );
    fallback.clone()
}

/// A local endpoint that answered a zero-config probe.
#[derive(Debug)]
pub(crate) struct LocalEndpoint {
    pub driver: String,
    pub base_url: String,
    pub models: ServedModels,
}

/// Probe `candidates` (driver id, base URL, key) concurrently, each bounded by
/// `timeout`, and return the first in list order that serves a model. The
/// whole call takes about one timeout, however many candidates are down.
pub(crate) async fn probe_endpoints(
    candidates: Vec<(String, String, Option<String>)>,
    timeout: Duration,
) -> Option<LocalEndpoint> {
    let probes = candidates
        .into_iter()
        .map(|(driver, base_url, key)| async move {
            let models = fetch_served_models(models_request(&base_url, key.as_deref()), timeout)
                .await
                .filter(|m| !m.is_empty())?;
            Some(LocalEndpoint {
                driver,
                base_url,
                models,
            })
        });
    futures::future::join_all(probes)
        .await
        .into_iter()
        .flatten()
        .next()
}

/// Is this the configuration nothing selected? True for the built-in default
/// (OpenRouter's `openrouter/auto`) when no key for it exists anywhere: such a
/// config can only fail, so a local server that answers is a better outcome.
fn is_unconfigured_default(cfg: &ModelConfig) -> bool {
    let default = ModelConfig::default();
    cfg.provider == default.provider
        && cfg.name == default.name
        && cfg.base_url.is_none()
        && resolve_api_key(cfg).is_none()
}

/// Zero-config start: when nothing is configured, use the first default local
/// endpoint (brain, ollama, vllm, lmstudio) that answers, with the model it
/// serves. Any configuration that selects something is returned unchanged.
pub(crate) async fn resolve_unconfigured(cfg: &ModelConfig) -> ModelConfig {
    if !is_unconfigured_default(cfg) {
        return cfg.clone();
    }
    let brain_off = crate::config::brain_autodetect_disabled();
    let candidates = ZERO_CONFIG_DRIVERS
        .iter()
        .filter(|id| !(brain_off && **id == "brain"))
        .filter_map(|id| {
            let url = registry::get_driver(id)?.default_base_url?;
            let key = resolve_api_key(&ModelConfig {
                provider: (*id).to_string(),
                ..ModelConfig::default()
            });
            Some(((*id).to_string(), url.to_string(), key))
        })
        .collect();
    let Some(found) = probe_endpoints(candidates, ZERO_CONFIG_TIMEOUT).await else {
        return cfg.clone();
    };
    let chosen = choose_model(&found.models).expect("endpoints serve at least one model");
    log_choice(&found.models, chosen);
    tracing::info!(
        "using {} at {}, model {}",
        found.driver,
        found.base_url,
        chosen.id
    );
    ModelConfig {
        provider: found.driver,
        name: chosen.id.clone(),
        ..cfg.clone()
    }
}

#[cfg(test)]
mod tests;
