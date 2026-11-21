// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Settling the `model:` section once the configuration is read: detecting
//! a model from the environment when none is configured, expanding a named
//! endpoint into its settings, and warning about token limits that cannot
//! hold.
//!
//! Swedish Embedded AB implements model-provider integration for its clients.
//! If your team needs expertise in connecting agents to hosted and local
//! language models then you can procure our services by sending an email to
//! info@swedishembedded.com.

use super::{ModelConfig, Providers};
use tracing::{debug, warn};

/// Settles the active model once the configuration is read, and warns about
/// limits that cannot hold.
///
/// With no `model:` section (`model_configured` false), the model is chosen
/// from what the environment offers: the sven server signed in to, then a
/// locally served brain, then
/// OpenRouter, Anthropic and OpenAI keys, in that order. A `model.provider`
/// naming one of `providers` is then expanded into that endpoint's settings
/// with its per-model overrides.
pub fn settle(model: &mut ModelConfig, providers: &Providers, model_configured: bool) {
    if !model_configured {
        detect_model(model);
    }
    expand_named_provider(model, providers);
    validate_token_limits(model, providers);
}

/// `provider/model` naming the active model so that another sven process
/// loading the same config resolves the same endpoint, key and limits.
///
/// A named `providers:` entry is expanded at load time, after which
/// `model.provider` holds only the driver id; handing a child process
/// `driver/model` would resolve the driver's defaults and silently drop the
/// entry's `base_url` and key. So the alias whose expansion produced the
/// active model (same driver, endpoint and key source) is named instead;
/// with no such entry the driver id is the right name.
#[must_use]
pub fn model_reference(model: &ModelConfig, providers: &Providers) -> String {
    // Several identical entries would all resolve the same; take the
    // smallest name so the reference does not depend on map order.
    let alias = providers
        .iter()
        .filter(|(_, entry)| {
            entry.name == model.provider
                && entry.base_url == model.base_url
                && entry.api_key_file == model.api_key_file
                && entry.api_key == model.api_key
        })
        .map(|(alias, _)| alias.as_str())
        .min();
    format!("{}/{}", alias.unwrap_or(&model.provider), model.name)
}

/// Chooses the model when the configuration names none, from what the
/// environment offers.
///
/// Priority: the sven server signed in to (`sven auth login`), with the
/// account's default model > brain (local) > OpenRouter > Anthropic >
/// OpenAI. A locally
/// running brain wins over hosted providers when detectable - getting started
/// on a brain-enabled machine should need zero config - but ONLY when one is
/// running, so a user with no brain server sees the stored-key choice alone.
/// Everything here is a sync, network-free check (files and a process
/// liveness test); whether brain is actually
/// reachable is verified when the provider is constructed
/// ([`crate::from_config_probed`]): a brain that does not answer yields to
/// the cloud choice recorded in [`ModelConfig::detection_fallback`], and with
/// no such choice fails loudly.
fn detect_model(model: &mut ModelConfig) {
    let cloud = detect_cloud_model(model);
    if crate::sven_service::signed_in().is_some() {
        // The account's default model, which only the server knows: the
        // empty name is resolved by `from_config_probed`, which asks it. The
        // cloud choice is kept for a server that does not answer.
        let reserve = cloud.then(|| Box::new(model.clone()));
        model.provider = crate::sven_service::PROVIDER.into();
        model.name = String::new();
        model.detection_fallback = reserve;
    } else if brain_is_locally_detectable() {
        // The cloud choice, when a key offers one, is kept in reserve: the
        // brain hint is only a record, and a server that stopped answering
        // must not strand a user whose cloud provider works. Probed at build time by
        // `from_config_probed`.
        let reserve = cloud.then(|| Box::new(model.clone()));
        model.provider = "brain".into();
        // Empty is a deliberate sentinel, not an oversight: the actual
        // resident model is whatever brain is currently serving, and that
        // requires a network call to discover - this runs while the
        // configuration loads, which is sync and must stay that way.
        // `from_config_probed` resolves the sentinel by asking brain's
        // `/v1/models` (the only model, else the first chat-capable one); a
        // user who wants a specific resident model names it via `--model` or
        // an explicit `model:` block, which bypasses this detection entirely.
        model.name = String::new();
        model.detection_fallback = reserve;
    }
    // With neither brain nor a key the defaults remain (openrouter/auto).
    // `from_config_probed` then looks for a local server on the default
    // ports and uses the model it serves; failing that, `from_config()`
    // produces a clear missing-key error when actually invoked.
}

/// Applies the hosted provider a stored key implies (`sven auth set`):
/// OpenRouter, then Anthropic, then OpenAI. Returns whether a key chose one.
///
/// The model names are deliberate: a hosted provider lists hundreds of
/// models, so no served model can stand in for the choice a key implies.
fn detect_cloud_model(model: &mut ModelConfig) -> bool {
    let stored = crate::credentials::providers();
    let has = |id: &str| stored.iter().any(|p| p == id);
    if has("openrouter") {
        // Keep the defaults: provider="openrouter", name="openrouter/auto".
    } else if has("anthropic") {
        model.provider = "anthropic".into();
        model.name = "claude-sonnet-4-6".into();
    } else if has("openai") {
        model.provider = "openai".into();
        model.name = "gpt-5.2".into();
    } else {
        return false;
    }
    true
}

/// Is a brain server running on this host? Network-free: it has published a
/// connection record that this user owns and whose process is alive
/// ([`crate::brain_local`]). Whether it answers is checked when the provider
/// is built ([`crate::from_config_probed`]).
fn brain_is_locally_detectable() -> bool {
    crate::brain_local::find(None).is_some()
}

/// If `model.provider` names one of `providers`, replace `model` with that
/// endpoint's settings and any per-model overrides registered for
/// `model.name`. This is what makes the provider-first layout work.
fn expand_named_provider(model: &mut ModelConfig, providers: &Providers) {
    if let Some(entry) = providers.get(&model.provider) {
        debug!(
            provider = %model.provider,
            model = %model.name,
            driver = %entry.name,
            "expanding named provider config"
        );
        let mut requested = std::mem::take(model);
        let driver_options = std::mem::take(&mut requested.driver_options);
        *model = entry.to_model_config(&requested.name);
        overlay_requested(model, &requested);
        merge_driver_options(&mut model.driver_options, driver_options);
    }
}

/// Lays every setting the `model:` section gave over the expanded endpoint's,
/// so nothing written next to `provider:` is lost to the expansion: a field
/// whose value differs from [`ModelConfig::default`] was set and wins; one
/// left at its default keeps the endpoint's. `provider` and `name` are the
/// expansion's own, `driver_options` are merged key by key
/// ([`merge_driver_options`]), and the process-only fields the file format
/// skips (`session`, `detection_fallback`) are carried over as they are.
fn overlay_requested(model: &mut ModelConfig, requested: &ModelConfig) {
    let (Ok(serde_json::Value::Object(defaults)), Ok(serde_json::Value::Object(asked))) = (
        serde_json::to_value(ModelConfig::default()),
        serde_json::to_value(requested),
    ) else {
        return;
    };
    let Ok(serde_json::Value::Object(mut merged)) = serde_json::to_value(&*model) else {
        return;
    };
    for (key, value) in asked {
        if matches!(key.as_str(), "provider" | "name" | "driver_options") {
            continue;
        }
        if defaults.get(&key) != Some(&value) {
            merged.insert(key, value);
        }
    }
    let session = model.session.take().or_else(|| requested.session.clone());
    let fallback = model
        .detection_fallback
        .take()
        .or_else(|| requested.detection_fallback.clone());
    if let Ok(settled) = serde_json::from_value::<ModelConfig>(serde_json::Value::Object(merged)) {
        *model = settled;
    }
    model.session = session;
    model.detection_fallback = fallback;
}

/// Lays the options a `model:` section asked for over those of its endpoint,
/// key by key, so a request setting written next to `provider:` is not lost
/// to the expansion. A non-object value replaces the endpoint's options.
fn merge_driver_options(base: &mut serde_json::Value, requested: serde_json::Value) {
    match (base.as_object_mut(), requested) {
        (_, serde_json::Value::Null) => {}
        (Some(base), serde_json::Value::Object(requested)) => base.extend(requested),
        (_, requested) => *base = requested,
    }
}

/// Warns when a total context window is smaller than the input and output
/// limits it must hold, for the active model and every listed model.
fn validate_token_limits(model: &ModelConfig, providers: &Providers) {
    validate_model_params_token_limits(
        model.max_tokens,
        model.max_input_tokens,
        model.max_output_tokens,
        "model",
    );
    for (alias, entry) in providers {
        for (model_name, params) in &entry.models {
            let effective_max_tokens = params.max_tokens.or(entry.max_tokens);
            validate_model_params_token_limits(
                effective_max_tokens,
                params.max_input_tokens,
                params.max_output_tokens,
                &format!("providers.{alias}.models.{model_name}"),
            );
        }
    }
}

/// Warn when `max_tokens` (total context) is less than the sum of the
/// optional `max_input_tokens` and `max_output_tokens` limits.
///
/// The constraint is:
///   `max_tokens >= max_input_tokens + max_output_tokens`
fn validate_model_params_token_limits(
    max_tokens: Option<u32>,
    max_input_tokens: Option<u32>,
    max_output_tokens: Option<u32>,
    path: &str,
) {
    if let (Some(total), Some(input), Some(output)) =
        (max_tokens, max_input_tokens, max_output_tokens)
    {
        let sum = input.saturating_add(output);
        if total < sum {
            warn!(
                path,
                max_tokens = total,
                max_input_tokens = input,
                max_output_tokens = output,
                sum_of_parts = sum,
                "max_tokens ({total}) is less than max_input_tokens + max_output_tokens \
                 ({input} + {output} = {sum}); total must be >= sum of its parts"
            );
        }
    } else if let (Some(total), None, Some(output)) =
        (max_tokens, max_input_tokens, max_output_tokens)
    {
        if total < output {
            warn!(
                path,
                max_tokens = total,
                max_output_tokens = output,
                "max_tokens ({total}) is less than max_output_tokens ({output}); \
                 total context must be >= output limit"
            );
        }
    } else if let (Some(total), Some(input), None) =
        (max_tokens, max_input_tokens, max_output_tokens)
    {
        if total < input {
            warn!(
                path,
                max_tokens = total,
                max_input_tokens = input,
                "max_tokens ({total}) is less than max_input_tokens ({input}); \
                 total context must be >= input limit"
            );
        }
    }
}
