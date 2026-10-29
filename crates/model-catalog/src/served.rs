// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping model limits and prices
// faithful to what the serving endpoint reports, for its clients. If your team
// needs expertise in multi-provider LLM gateways then you can procure our
// services by sending an email to info@swedishembedded.com.
//! One entry of an OpenAI-shaped `GET /models` response read into a
//! [`ModelCatalogEntry`].
//!
//! Every field beyond `id` is optional, because servers differ in what they
//! report: what a server leaves out stays unknown here and is never filled in
//! from anywhere else.
//!
//! | entry field | read from |
//! |-------------|-----------|
//! | `context_window` | `context_length`, else `max_model_len`, else `context_window` |
//! | `max_output_tokens` | `top_provider.max_completion_tokens`, else `max_output_tokens` |
//! | `input_modalities` | `architecture.input_modalities`, else `capabilities` |
//! | `pricing` | `pricing` (see [`served_pricing_of`]) |

use serde_json::Value;

use crate::{CachePricing, InputModality, ModelCatalogEntry, Pricing};

impl ModelCatalogEntry {
    /// Read one `data[]` element of a `/models` response for `provider`.
    /// `None` when the element has no string `id`.
    #[must_use]
    pub fn from_served(provider: &str, item: &Value) -> Option<Self> {
        let id = item.get("id")?.as_str()?.to_string();
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(&id)
            .to_string();
        Some(Self {
            context_window: ["context_length", "max_model_len", "context_window"]
                .iter()
                .find_map(|key| token_count(item.get(*key))),
            max_output_tokens: token_count(item.pointer("/top_provider/max_completion_tokens"))
                .or_else(|| token_count(item.get("max_output_tokens"))),
            description: item
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            input_modalities: input_modalities_of(item),
            pricing: served_pricing_of(item),
            name,
            id,
            provider: provider.to_string(),
        })
    }
}

/// A positive token count; zero and non-numbers are "not reported".
#[must_use]
pub fn token_count(v: Option<&Value>) -> Option<u32> {
    let n = v?.as_u64().filter(|&n| n > 0)?;
    Some(u32::try_from(n).unwrap_or(u32::MAX))
}

fn input_modalities_of(item: &Value) -> Vec<InputModality> {
    let listed = |pointer: &str| item.pointer(pointer).and_then(Value::as_array);
    let names: Vec<&str> = listed("/architecture/input_modalities")
        .or_else(|| listed("/capabilities"))
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut out = vec![InputModality::Text];
    for (modality, spellings) in [
        (InputModality::Image, ["image", "vision"]),
        (InputModality::Audio, ["audio", "audio_input"]),
    ] {
        if names.iter().any(|n| spellings.contains(n)) {
            out.push(modality);
        }
    }
    out
}

/// The `pricing` object of a served model, converted to [`Pricing`].
///
/// The server states USD **per token** as decimal strings (numbers are
/// accepted too): `prompt` and `completion` are required, `input_cache_read`
/// and `input_cache_write` optional. Each is multiplied by one million to give
/// the per-million-token figures the cost code uses. A cache write that is not
/// stated is charged at the prompt price; a cache read that is not stated
/// means the endpoint serves no cache terms. No time-to-live is served, so
/// none is set.
///
/// `None` when a required figure is missing, unreadable, negative (OpenRouter
/// states `-1` for models it prices per request) or the terms are inconsistent:
/// the price is then unknown, never zero.
#[must_use]
pub fn served_pricing_of(item: &Value) -> Option<Pricing> {
    let pricing = item.get("pricing")?;
    let per_mtok = |key: &str| -> Option<f64> {
        let per_token = match pricing.get(key)? {
            Value::String(s) => s.trim().parse::<f64>().ok()?,
            Value::Number(n) => n.as_f64()?,
            _ => return None,
        };
        // Round to a millionth of the unit so a decimal such as 0.00000014
        // becomes 0.14 rather than 0.13999999999999999.
        per_token
            .is_finite()
            .then(|| (per_token * 1e12).round() / 1e6)
    };
    let input = per_mtok("prompt")?;
    let cache = per_mtok("input_cache_read").map(|read| CachePricing {
        read_per_mtok: read,
        write_per_mtok: per_mtok("input_cache_write").unwrap_or(input),
        ttl_secs: None,
    });
    let priced = Pricing {
        input_per_mtok: input,
        output_per_mtok: per_mtok("completion")?,
        cache,
    };
    priced.validate().ok().map(|()| priced)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn an_openrouter_entry_yields_limits_modalities_and_prices() {
        let item = json!({
            "id": "deepseek/deepseek-chat",
            "name": "DeepSeek Chat",
            "description": "General chat",
            "context_length": 163840,
            "architecture": { "input_modalities": ["text", "image"] },
            "top_provider": { "max_completion_tokens": 32768 },
            "pricing": {
                "prompt": "0.00000014",
                "completion": "0.00000042",
                "input_cache_read": "0.000000014",
                "input_cache_write": "0.00000018"
            }
        });
        let e = ModelCatalogEntry::from_served("openrouter", &item).unwrap();
        assert_eq!(e.provider, "openrouter");
        assert_eq!(e.name, "DeepSeek Chat");
        assert_eq!(e.context_window, Some(163_840));
        assert_eq!(e.max_output_tokens, Some(32_768));
        assert!(e.supports_images());
        let p = e.pricing.unwrap();
        assert_eq!(p.input_per_mtok, 0.14);
        assert_eq!(p.output_per_mtok, 0.42);
        let cache = p.cache.unwrap();
        assert_eq!(cache.read_per_mtok, 0.014);
        assert_eq!(cache.write_per_mtok, 0.18);
        assert_eq!(cache.ttl_secs, None);
    }

    #[test]
    fn a_bare_id_leaves_everything_else_unknown() {
        let e = ModelCatalogEntry::from_served("groq", &json!({"id": "llama"})).unwrap();
        assert_eq!(e.name, "llama");
        assert_eq!(e.context_window, None);
        assert_eq!(e.max_output_tokens, None);
        assert_eq!(e.pricing, None);
        assert_eq!(e.input_modalities, vec![InputModality::Text]);
    }

    #[test]
    fn other_servers_spellings_of_the_window_are_read() {
        for (key, value) in [("max_model_len", 8192), ("context_window", 32768)] {
            let e = ModelCatalogEntry::from_served("p", &json!({"id": "m", key: value})).unwrap();
            assert_eq!(e.context_window, Some(value as u32), "{key}");
        }
    }

    #[test]
    fn capabilities_declare_modalities_when_architecture_is_absent() {
        let item = json!({"id": "m", "capabilities": ["completion", "vision", "audio"]});
        let e = ModelCatalogEntry::from_served("p", &item).unwrap();
        assert!(e.supports_images() && e.supports_audio());
    }

    #[test]
    fn a_cache_write_not_stated_is_charged_at_the_prompt_price() {
        let item = json!({"pricing": {
            "prompt": "0.000003", "completion": "0.000015", "input_cache_read": "0.0000003"
        }});
        let cache = served_pricing_of(&item).unwrap().cache.unwrap();
        assert_eq!(cache.write_per_mtok, 3.0);
    }

    #[test]
    fn a_price_that_is_missing_negative_or_unreadable_is_unknown() {
        for pricing in [
            json!({"prompt": "0.000003"}),
            json!({"prompt": "-1", "completion": "-1"}),
            json!({"prompt": "free", "completion": "0.1"}),
            json!({"prompt": "0.000003", "completion": "0.000015",
                   "input_cache_read": "0.00001", "input_cache_write": "0.000001"}),
        ] {
            assert_eq!(served_pricing_of(&json!({ "pricing": pricing })), None);
        }
        assert_eq!(served_pricing_of(&json!({"id": "m"})), None);
    }

    #[test]
    fn a_free_model_is_priced_at_zero_only_when_the_server_says_so() {
        let item = json!({"pricing": {"prompt": "0", "completion": "0"}});
        assert_eq!(served_pricing_of(&item).unwrap().input_per_mtok, 0.0);
    }
}
