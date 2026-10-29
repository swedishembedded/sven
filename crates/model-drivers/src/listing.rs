// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agents aware of what
// their model endpoints really serve, for its clients. If your team needs
// expertise in multi-provider LLM gateways then you can procure our services
// by sending an email to info@swedishembedded.com.
//! What every driver's `list_models` shares: asking the endpoint, and keeping
//! the answer.
//!
//! A listing that fails is an error naming the provider. It is never answered
//! from invented entries; the catalog cache keeps the last good answer.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use serde_json::Value;
use sven_model::catalog::{self, ModelCatalogEntry};
use sven_model::ModelProvider;

#[cfg(not(target_arch = "wasm32"))]
use crate::api_key::resolve_api_key;
#[cfg(not(target_arch = "wasm32"))]
use crate::discovery::{effective_base_url, is_local_endpoint};
use crate::ModelConfig;

/// How long an endpoint may take to list its models.
const LIST_TIMEOUT: Duration = Duration::from_secs(15);

/// Send `req` (a model-list `GET`) and return its JSON body.
///
/// # Errors
/// The endpoint did not answer, answered a non-success status (the key is not
/// echoed) or answered something that is not JSON.
pub(crate) async fn get_json(
    req: reqwest::RequestBuilder,
    provider: &str,
) -> anyhow::Result<Value> {
    let resp = req
        .timeout(LIST_TIMEOUT)
        .send()
        .await
        .with_context(|| format!("{provider}: the model list request failed"))?;
    let status = resp.status();
    if !status.is_success() {
        bail!(
            "{provider} answered HTTP {} to its model list",
            status.as_u16()
        );
    }
    resp.json()
        .await
        .with_context(|| format!("{provider}: the model list is not JSON"))
}

/// Record `entries` as what `provider` serves and return them ordered by id.
pub(crate) fn record(
    provider: &str,
    mut entries: Vec<ModelCatalogEntry>,
) -> Vec<ModelCatalogEntry> {
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    catalog::cache_update(provider, entries.clone());
    entries
}

/// Ask a hosted endpoint for its models in the background when the disk cache
/// of its provider is absent or older than a day, so limits and prices are
/// known without a listing having been requested.
///
/// Does nothing without a running tokio runtime or a key, nor for an endpoint
/// on this machine or network: those answer a live probe at start-up instead,
/// and a list request to them is not free of side effects for a test or a
/// server that counts requests. A failed refresh is logged and leaves the
/// cache as it was.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn refresh_when_stale(provider: &Arc<dyn ModelProvider>, cfg: &ModelConfig) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let hosted = effective_base_url(cfg).is_some_and(|url| !is_local_endpoint(&url));
    if !hosted || resolve_api_key(cfg).is_none() || !catalog::is_cache_stale(&cfg.provider) {
        return;
    }
    let provider = Arc::clone(provider);
    runtime.spawn(async move {
        if let Err(e) = provider.list_models().await {
            tracing::debug!(provider = provider.name(), error = %e, "model list refresh failed");
        }
    });
}

/// What a probed start does about a hosted endpoint's model list.
#[derive(Debug, PartialEq, Eq)]
enum StartListing {
    /// Nothing: a local endpoint (probed live instead) or no key to ask with.
    Skip,
    /// The model is not in the catalog yet: ask now and wait, because what
    /// it accepts (images, audio), its limits and its price are read from
    /// that answer, and a guess would refuse an image the model can see.
    Await,
    /// The model is known: refresh a stale list in the background.
    Background,
}

fn start_listing(hosted: bool, keyed: bool, model_known: bool) -> StartListing {
    match (hosted && keyed, model_known) {
        (false, _) => StartListing::Skip,
        (true, false) => StartListing::Await,
        (true, true) => StartListing::Background,
    }
}

/// The model list a probed start needs: awaited when `cfg`'s model is not in
/// the catalog yet, refreshed in the background when it is stale
/// ([`refresh_when_stale`]). An awaited listing that fails is logged and
/// leaves the model unknown (text only, no price), as before.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn settle_for_start(provider: &Arc<dyn ModelProvider>, cfg: &ModelConfig) {
    let hosted = effective_base_url(cfg).is_some_and(|url| !is_local_endpoint(&url));
    let known = catalog::lookup(&cfg.provider, &cfg.name).is_some();
    match start_listing(hosted, resolve_api_key(cfg).is_some(), known) {
        StartListing::Skip => {}
        StartListing::Background => refresh_when_stale(provider, cfg),
        StartListing::Await => {
            if let Err(e) = provider.list_models().await {
                tracing::warn!(provider = provider.name(), error = %e, "model list failed; the model's modalities and price stay unknown");
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) async fn settle_for_start(_provider: &Arc<dyn ModelProvider>, _cfg: &ModelConfig) {}

/// Ask `cfg`'s endpoint for its models in the background, whatever the age of
/// its cache: what a model menu does each time it opens, so the next look
/// at it is current. Wakes [`listings_changed`] once the new list is
/// recorded. Does nothing without a running tokio runtime, or for an
/// endpoint that cannot be built (no key, no base URL); a failed listing is
/// logged and leaves the cache as it was.
#[cfg(not(target_arch = "wasm32"))]
pub fn refresh(cfg: &ModelConfig) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let Ok((provider, _, _)) = crate::build_inner(cfg) else {
        return;
    };
    runtime.spawn(async move {
        match provider.list_models().await {
            Ok(_) => listings_changed().notify_one(),
            Err(e) => {
                tracing::debug!(provider = provider.name(), error = %e, "model list refresh failed")
            }
        }
    });
}

/// Woken after a [`refresh`] records a provider's new model list, so a menu
/// showing the cache can show the new models without waiting for a key.
/// One waiter is woken per refresh (a missed wake is kept for the next wait).
#[cfg(not(target_arch = "wasm32"))]
pub fn listings_changed() -> &'static tokio::sync::Notify {
    static CHANGED: std::sync::OnceLock<tokio::sync::Notify> = std::sync::OnceLock::new();
    CHANGED.get_or_init(tokio::sync::Notify::new)
}

/// A browser has no on-disk catalog to refresh.
#[cfg(target_arch = "wasm32")]
pub(crate) fn refresh_when_stale(_provider: &Arc<dyn ModelProvider>, _cfg: &ModelConfig) {}

#[cfg(test)]
mod tests {
    use super::{start_listing, StartListing};

    #[test]
    fn an_unknown_hosted_model_is_listed_before_use() {
        assert_eq!(start_listing(true, true, false), StartListing::Await);
        assert_eq!(start_listing(true, true, true), StartListing::Background);
        assert_eq!(start_listing(false, true, false), StartListing::Skip);
        assert_eq!(start_listing(true, false, false), StartListing::Skip);
    }
}
