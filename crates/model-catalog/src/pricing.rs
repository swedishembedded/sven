// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the cost of agent
// workloads explicit and predictable for its clients. If your team needs
// expertise in LLM cost engineering then you can procure our services by
// sending an email to info@swedishembedded.com.
//! Declared prices of models.
//!
//! Prices live beside the catalog but not inside [`crate::ModelCatalogEntry`]:
//! the live overlay replaces a provider's entries wholesale with what the
//! provider's own listing reports, and a listing carries no prices. A price is
//! therefore looked up by provider and model id from its own table
//! (`prices.yaml`), and a model without an entry has an *unknown* price rather
//! than a zero one.
//!
//! All prices are per million tokens in one currency chosen by whoever declares
//! them; nothing here converts or labels it.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// What a model's provider charges, per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    /// Prompt tokens that are neither read from nor written to the cache.
    pub input_per_mtok: f64,
    /// Generated tokens.
    pub output_per_mtok: f64,
    /// The provider's prompt cache. Absent: the provider has none, and every
    /// prompt token is charged at `input_per_mtok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CachePricing>,
}

/// Prompt-cache terms of a provider.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CachePricing {
    /// Prompt tokens served from the cache.
    pub read_per_mtok: f64,
    /// Prompt tokens written to the cache. Equal to the input price for
    /// providers that cache automatically and charge nothing extra.
    pub write_per_mtok: f64,
    /// How long a cached prefix stays warm after its last use, in seconds.
    pub ttl_secs: u32,
}

impl Pricing {
    /// Check that every figure is a finite, non-negative number and that a
    /// cache read does not cost more than a cache write.
    ///
    /// # Errors
    /// Names the first figure that is not.
    pub fn validate(&self) -> Result<(), String> {
        let figure = |name: &str, v: f64| {
            if v.is_finite() && v >= 0.0 {
                Ok(())
            } else {
                Err(format!(
                    "{name} must be a finite number of at least 0, got {v}"
                ))
            }
        };
        figure("input_per_mtok", self.input_per_mtok)?;
        figure("output_per_mtok", self.output_per_mtok)?;
        if let Some(cache) = &self.cache {
            figure("cache.read_per_mtok", cache.read_per_mtok)?;
            figure("cache.write_per_mtok", cache.write_per_mtok)?;
            if cache.read_per_mtok > cache.write_per_mtok {
                return Err(format!(
                    "cache.read_per_mtok ({}) exceeds cache.write_per_mtok ({}): \
                     a cache hit priced above a miss is not a cache",
                    cache.read_per_mtok, cache.write_per_mtok
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct PriceEntry {
    provider: String,
    id: String,
    #[serde(flatten)]
    pricing: Pricing,
}

#[derive(Debug, Deserialize)]
struct PriceFile {
    prices: Vec<PriceEntry>,
}

fn bundled() -> &'static [PriceEntry] {
    static PRICES: OnceLock<Vec<PriceEntry>> = OnceLock::new();
    PRICES.get_or_init(|| {
        let file: PriceFile = serde_yaml::from_str(include_str!("../prices.yaml"))
            .expect("bundled prices.yaml must be valid");
        file.prices
    })
}

/// The declared price of `model_id` at `provider`; `None` when none is
/// declared, which means unknown and never free.
#[must_use]
pub fn lookup_pricing(provider: &str, model_id: &str) -> Option<Pricing> {
    bundled()
        .iter()
        .find(|e| e.provider == provider && e.id == model_id)
        .map(|e| e.pricing)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(input: f64, output: f64) -> Pricing {
        Pricing {
            input_per_mtok: input,
            output_per_mtok: output,
            cache: None,
        }
    }

    #[test]
    fn an_undeclared_model_has_no_price_rather_than_a_zero_one() {
        assert!(lookup_pricing("openai", "no-such-model").is_none());
    }

    #[test]
    fn the_bundled_table_parses_and_every_entry_is_usable() {
        for e in bundled() {
            assert_eq!(e.pricing.validate(), Ok(()), "{}/{}", e.provider, e.id);
        }
    }

    #[test]
    fn a_price_must_be_finite_and_not_negative() {
        assert_eq!(flat(3.0, 15.0).validate(), Ok(()));
        assert!(flat(-1.0, 15.0).validate().is_err());
        assert!(flat(3.0, f64::NAN).validate().is_err());
        assert!(flat(f64::INFINITY, 1.0).validate().is_err());
        let bad_cache = Pricing {
            cache: Some(CachePricing {
                read_per_mtok: -0.1,
                write_per_mtok: 1.0,
                ttl_secs: 300,
            }),
            ..flat(3.0, 15.0)
        };
        assert!(bad_cache.validate().unwrap_err().contains("cache.read"));
        let inverted = Pricing {
            cache: Some(CachePricing {
                read_per_mtok: 4.0,
                write_per_mtok: 1.0,
                ttl_secs: 300,
            }),
            ..flat(3.0, 15.0)
        };
        assert!(inverted.validate().unwrap_err().contains("exceeds"));
    }

    #[test]
    fn a_declared_entry_reads_back_with_its_cache_terms() {
        let yaml = "prices:\n  - provider: p\n    id: m\n    input_per_mtok: 3\n    output_per_mtok: 15\n    cache: {read_per_mtok: 0.3, write_per_mtok: 3.75, ttl_secs: 300}\n";
        let file: PriceFile = serde_yaml::from_str(yaml).unwrap();
        let p = file.prices[0].pricing;
        assert_eq!(p.cache.unwrap().ttl_secs, 300);
        assert_eq!(p.input_per_mtok, 3.0);
    }
}
