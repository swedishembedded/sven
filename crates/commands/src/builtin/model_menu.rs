// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for choosing between many model
// providers without leaving the keyboard, for its clients. If your team needs
// expertise in multi-provider LLM tooling then you can procure our services by
// sending an email to info@swedishembedded.com.
//! The model menu behind `/model` completion: every model of every configured
//! provider, laid out like a web model selector.
//!
//! With nothing typed the models are grouped by provider and, within a
//! provider, by maker (alphabetically, models without a maker last). With a
//! query they are fuzzy-ranked, best first, and no longer grouped. Each row
//! shows the model's display name, its maker, its context window and its price
//! as `$in / $out` per million tokens, each only when the endpoint reported it.
//! The completion popup has no group headers, so the provider is a column of
//! every row and the groups read as runs of the same provider.
//!
//! The menu is built from the served-model cache; [`request_stale_refreshes`]
//! asks providers whose cache is missing or stale to list again in the
//! background, so the next opening is current.

use std::collections::{BTreeSet, HashSet};
use std::sync::Mutex;

use sven_model::catalog::{ModelCatalogEntry, Pricing};
use sven_model_drivers::reachable::LOCAL_DRIVERS;
use sven_model_drivers::{ModelConfig, ProviderEntry, Providers};

use crate::completion::{fuzzy_score, CompletionItem};

/// One place models are offered from, under the name `--model` takes for it.
pub(crate) struct Source {
    /// What a reference starts with: a `providers:` entry name or a driver id.
    prefix: String,
    /// The driver id whose served models are cached for this source.
    driver: String,
    /// Models the `providers:` entry names itself.
    declared: Vec<String>,
    /// The `providers:` entry, as a label for it, when this source is one.
    entry_note: Option<String>,
    /// What a background listing is started from.
    config: ModelConfig,
}

impl Source {
    /// The driver id whose served models this source offers.
    pub(crate) fn driver_id(&self) -> &str {
        &self.driver
    }

    fn driver(id: &str) -> Self {
        Self {
            prefix: id.to_string(),
            driver: id.to_string(),
            declared: Vec::new(),
            entry_note: None,
            config: ModelConfig {
                provider: id.to_string(),
                name: String::new(),
                ..ModelConfig::default()
            },
        }
    }

    fn entry(alias: &str, entry: &ProviderEntry) -> Self {
        let mut declared: Vec<String> = entry.models.keys().cloned().collect();
        declared.sort_unstable();
        Self {
            prefix: alias.to_string(),
            driver: entry.name.clone(),
            declared,
            entry_note: Some(format!(
                "custom provider from config (driver: {})",
                entry.name
            )),
            config: entry.to_model_config(""),
        }
    }
}

/// Every source the user can pick models from: each `providers:` entry, the
/// active provider, each provider with a stored key and the local servers.
/// A driver that a `providers:` entry reaches is offered under that entry's
/// name only, because the entry is what carries its endpoint and key.
pub(crate) fn sources(providers: &Providers, active: &str, stored_keys: &[String]) -> Vec<Source> {
    let mut sources: Vec<Source> = providers.iter().map(|(a, e)| Source::entry(a, e)).collect();
    let claimed: HashSet<String> = sources.iter().map(|s| s.driver.clone()).collect();
    let drivers: BTreeSet<&str> = std::iter::once(active)
        .chain(stored_keys.iter().map(String::as_str))
        .chain(LOCAL_DRIVERS)
        .filter(|d| !d.is_empty() && !claimed.contains(*d))
        .collect();
    sources.extend(drivers.into_iter().map(Source::driver));
    sources.sort_by(|a, b| a.prefix.cmp(&b.prefix));
    sources
}

/// One selectable model.
pub(crate) struct Row {
    provider: String,
    maker: Option<String>,
    name: String,
    /// What is inserted: the reference `--model` accepts.
    value: String,
    description: String,
    context_window: Option<u32>,
    pricing: Option<Pricing>,
    current: bool,
}

/// The maker of a served model: the id's part before `/`, shown as the served
/// name's part before `:` when it has one (`DeepSeek: DeepSeek V4.1 Flash`).
/// The second value is the display name without that maker prefix.
fn maker_and_title(entry: &ModelCatalogEntry) -> (Option<String>, String) {
    let Some((id_maker, _)) = entry.id.split_once('/') else {
        return (None, entry.name.clone());
    };
    match entry.name.split_once(':') {
        Some((maker, title)) if entry.name != entry.id && !maker.trim().is_empty() => {
            (Some(maker.trim().to_string()), title.trim().to_string())
        }
        _ => (Some(id_maker.to_string()), entry.name.clone()),
    }
}

/// The rows of `sources`, taken from `served` (the cache), marking the model
/// `current` (driver id, model id).
pub(crate) fn rows(
    sources: &[Source],
    served: &[ModelCatalogEntry],
    current: (&str, &str),
) -> Vec<Row> {
    let is_current = |driver: &str, id: &str| (driver, id) == current;
    let mut rows = Vec::new();
    for source in sources {
        let first = rows.len();
        for entry in served.iter().filter(|e| e.provider == source.driver) {
            let (maker, name) = maker_and_title(entry);
            rows.push(Row {
                provider: source.prefix.clone(),
                maker,
                name,
                value: format!("{}/{}", source.prefix, entry.id),
                description: entry.description.clone(),
                context_window: entry.context_window,
                pricing: entry.pricing,
                current: is_current(&source.driver, &entry.id),
            });
        }
        for id in &source.declared {
            if served
                .iter()
                .any(|e| e.provider == source.driver && &e.id == id)
            {
                continue;
            }
            rows.push(Row {
                provider: source.prefix.clone(),
                maker: None,
                name: id.clone(),
                value: format!("{}/{id}", source.prefix),
                description: source.entry_note.clone().unwrap_or_default(),
                context_window: None,
                pricing: None,
                current: is_current(&source.driver, id),
            });
        }
        if rows.len() == first {
            if let Some(note) = &source.entry_note {
                rows.push(Row {
                    provider: source.prefix.clone(),
                    maker: None,
                    name: source.prefix.clone(),
                    value: source.prefix.clone(),
                    description: note.clone(),
                    context_window: None,
                    pricing: None,
                    current: false,
                });
            }
        }
    }
    if !rows.iter().any(|r| r.current) && !current.1.is_empty() {
        rows.push(Row {
            provider: current.0.to_string(),
            maker: None,
            name: current.1.to_string(),
            value: format!("{}/{}", current.0, current.1),
            description: "currently active model".into(),
            context_window: None,
            pricing: None,
            current: true,
        });
    }
    rows
}

/// `131072` as `131k`, `1000000` as `1M`.
fn context_label(tokens: u32) -> String {
    if tokens >= 1_000_000 {
        let millions = format!("{:.1}", f64::from(tokens) / 1e6);
        format!("{}M", millions.trim_end_matches(".0"))
    } else if tokens >= 1000 {
        format!("{}k", (tokens + 500) / 1000)
    } else {
        tokens.to_string()
    }
}

/// `0.14` as `$0.14`, `0.075` as `$0.075`, `15` as `$15.00`.
fn dollars(per_mtok: f64) -> String {
    let three = format!("{per_mtok:.3}");
    format!("${}", three.strip_suffix('0').unwrap_or(&three))
}

fn price_label(p: &Pricing) -> String {
    format!(
        "{} / {}",
        dollars(p.input_per_mtok),
        dollars(p.output_per_mtok)
    )
}

impl Row {
    fn label(&self, provider_width: usize) -> String {
        let mut cells = vec![
            format!("{:<provider_width$}", self.provider),
            self.name.clone(),
        ];
        cells.extend(self.maker.clone());
        cells.extend(
            self.context_window
                .map(|n| format!("{} ctx", context_label(n))),
        );
        cells.extend(self.pricing.as_ref().map(price_label));
        format!(
            "{} {}",
            if self.current { '*' } else { ' ' },
            cells.join(" | ")
        )
    }

    fn detail(&self) -> String {
        if self.description.is_empty() {
            self.value.clone()
        } else {
            format!("{} - {}", self.value, self.description)
        }
    }

    fn search_text(&self) -> String {
        format!(
            "{} {} {}",
            self.name,
            self.maker.as_deref().unwrap_or(""),
            self.value
        )
    }

    fn item(&self, provider_width: usize) -> CompletionItem {
        CompletionItem::with_desc(&self.value, self.label(provider_width), self.detail())
    }
}

/// The completion items for `query`: grouped by provider and maker when it is
/// empty, fuzzy-ranked and ungrouped otherwise.
pub(crate) fn items(mut rows: Vec<Row>, query: &str) -> Vec<CompletionItem> {
    let width = rows
        .iter()
        .map(|r| r.provider.chars().count())
        .max()
        .unwrap_or(0);
    if query.is_empty() {
        let key = |r: &Row| {
            (
                r.provider.clone(),
                r.maker.is_none(),
                r.maker.as_deref().unwrap_or("").to_lowercase(),
                r.name.to_lowercase(),
            )
        };
        rows.sort_by_key(key);
        return rows.iter().map(|r| r.item(width)).collect();
    }
    let mut ranked: Vec<(usize, Row)> = rows
        .into_iter()
        .filter_map(|r| fuzzy_score(query, &r.search_text()).map(|s| (s, r)))
        .collect();
    ranked.sort_by(|(a, ra), (b, rb)| b.cmp(a).then_with(|| ra.value.cmp(&rb.value)));
    ranked.iter().map(|(_, r)| r.item(width)).collect()
}

/// Ask each source whose cache `is_stale` to list its models in the
/// background (`start`), at most once per process per source, so that opening
/// the menu never waits for the network and the next opening is current.
/// `requested` remembers the sources already asked.
pub(crate) fn request_stale_refreshes(
    sources: &[Source],
    requested: &Mutex<HashSet<String>>,
    is_stale: impl Fn(&str) -> bool,
    mut start: impl FnMut(&ModelConfig),
) {
    let mut requested = requested.lock().unwrap_or_else(|e| e.into_inner());
    for source in sources {
        if is_stale(&source.driver) && requested.insert(source.prefix.clone()) {
            start(&source.config);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_model::catalog::{self, ModelCatalogEntry};
    use sven_model_drivers::ModelResolver;

    fn served(
        provider: &str,
        id: &str,
        name: &str,
        price: Option<(f64, f64)>,
    ) -> ModelCatalogEntry {
        ModelCatalogEntry {
            name: name.into(),
            context_window: Some(131_072),
            pricing: price.map(|(i, o)| Pricing {
                input_per_mtok: i,
                output_per_mtok: o,
                cache: None,
            }),
            ..ModelCatalogEntry::bare(provider, id)
        }
    }

    fn entry_of(driver: &str) -> ProviderEntry {
        ProviderEntry {
            name: driver.into(),
            ..ProviderEntry::default()
        }
    }

    fn catalog_of_two_providers() -> Vec<ModelCatalogEntry> {
        vec![
            served("hosted", "openai/gpt-x", "OpenAI: GPT X", Some((2.5, 10.0))),
            served(
                "hosted",
                "deepseek/v4-flash",
                "DeepSeek: V4 Flash",
                Some((0.14, 0.42)),
            ),
            served("hosted", "zeta", "zeta", None),
            served(
                "hosted",
                "anthropic/claude-q",
                "Anthropic: Claude Q",
                Some((3.0, 15.0)),
            ),
            ModelCatalogEntry::bare("lan", "llama3:8b"),
        ]
    }

    fn menu(query: &str) -> Vec<CompletionItem> {
        let sources = [Source::driver("lan"), Source::driver("hosted")];
        items(
            rows(&sources, &catalog_of_two_providers(), ("hosted", "zeta")),
            query,
        )
    }

    fn values(items: &[CompletionItem]) -> Vec<&str> {
        items.iter().map(|i| i.value.as_str()).collect()
    }

    #[test]
    fn without_a_query_models_group_by_provider_then_maker_with_makerless_last() {
        assert_eq!(
            values(&menu("")),
            [
                "hosted/anthropic/claude-q",
                "hosted/deepseek/v4-flash",
                "hosted/openai/gpt-x",
                "hosted/zeta",
                "lan/llama3:8b",
            ]
        );
    }

    #[test]
    fn a_query_ranks_fuzzy_matches_best_first_and_drops_the_rest() {
        let found = menu("deepseek");
        assert_eq!(values(&found).first(), Some(&"hosted/deepseek/v4-flash"));
        assert!(!values(&found).contains(&"lan/llama3:8b"));
        // Ungrouped: "flash" matches one model whatever its provider or maker.
        assert_eq!(values(&menu("flash")), ["hosted/deepseek/v4-flash"]);
    }

    #[test]
    fn labels_show_name_maker_context_and_price_only_when_known() {
        let found = menu("");
        let label = |value: &str| {
            found
                .iter()
                .find(|i| i.value == value)
                .unwrap()
                .display
                .clone()
        };
        assert_eq!(
            label("hosted/deepseek/v4-flash"),
            "  hosted | V4 Flash | DeepSeek | 131k ctx | $0.14 / $0.42"
        );
        assert!(
            label("lan/llama3:8b").ends_with("| llama3:8b"),
            "{}",
            label("lan/llama3:8b")
        );
        assert!(!label("hosted/zeta").contains('$'));
        assert!(
            label("hosted/zeta").starts_with('*'),
            "the current model is marked"
        );
    }

    #[test]
    fn prices_keep_sub_cent_digits() {
        assert_eq!(
            price_label(&Pricing {
                input_per_mtok: 0.075,
                output_per_mtok: 15.0,
                cache: None
            }),
            "$0.075 / $15.00"
        );
        assert_eq!(context_label(1_000_000), "1M");
        assert_eq!(context_label(32_768), "33k");
    }

    #[test]
    fn inserted_references_resolve_to_the_provider_and_model_listed() {
        let entry = entry_of("hosted");
        let providers = Providers::from([("mine".to_string(), entry)]);
        let sources = sources(&providers, "lan", &[]);
        let found = items(
            rows(&sources, &catalog_of_two_providers(), ("lan", "x")),
            "",
        );
        let base = ModelConfig::default();
        for (value, provider, name) in [
            ("mine/deepseek/v4-flash", "hosted", "deepseek/v4-flash"),
            ("lan/llama3:8b", "lan", "llama3:8b"),
        ] {
            assert!(values(&found).contains(&value), "{value} not offered");
            let cfg = ModelResolver::new(&base, &providers, value).resolve();
            assert_eq!((cfg.provider.as_str(), cfg.name.as_str()), (provider, name));
        }
        // The driver is offered under the entry that reaches it only.
        assert!(!values(&found).iter().any(|v| v.starts_with("hosted/")));
    }

    #[test]
    fn stale_sources_are_asked_once_and_fresh_ones_never() {
        let sources = [Source::driver("fresh"), Source::driver("stale")];
        let requested = Mutex::new(HashSet::new());
        let mut started = Vec::new();
        for _ in 0..2 {
            request_stale_refreshes(
                &sources,
                &requested,
                |d| d == "stale",
                |c| {
                    started.push(c.provider.clone());
                },
            );
        }
        assert_eq!(started, ["stale"]);
    }

    #[test]
    fn completion_reads_the_served_cache_of_configured_sources() {
        catalog::cache_seed([served("menu-cache", "a/m", "A: M", Some((1.0, 2.0)))]);
        let entry = entry_of("menu-cache");
        let providers = Providers::from([("alias".to_string(), entry)]);
        let sources = sources(&providers, "", &[]);
        let found = items(rows(&sources, &catalog::served_models(), ("", "")), "");
        assert!(values(&found).contains(&"alias/a/m"));
    }
}
