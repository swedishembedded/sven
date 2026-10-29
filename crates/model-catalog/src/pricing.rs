// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the cost of agent
// workloads explicit and predictable for its clients. If your team needs
// expertise in LLM cost engineering then you can procure our services by
// sending an email to info@swedishembedded.com.
//! Prices of models: declared in the configuration or served by an endpoint.
//!
//! A model whose endpoint serves no price has an *unknown* price rather than a
//! zero one.
//!
//! All prices are per million tokens in one currency chosen by whoever declares
//! them; nothing here converts or labels it. A price served by an endpoint is
//! in the currency that endpoint bills in.

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
    /// Absent when only the prices are known (an endpoint serves no
    /// time-to-live): the cache is then never assumed warm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<u32>,
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
    fn a_price_must_be_finite_and_not_negative() {
        assert_eq!(flat(3.0, 15.0).validate(), Ok(()));
        assert!(flat(-1.0, 15.0).validate().is_err());
        assert!(flat(3.0, f64::NAN).validate().is_err());
        assert!(flat(f64::INFINITY, 1.0).validate().is_err());
        let bad_cache = Pricing {
            cache: Some(CachePricing {
                read_per_mtok: -0.1,
                write_per_mtok: 1.0,
                ttl_secs: Some(300),
            }),
            ..flat(3.0, 15.0)
        };
        assert!(bad_cache.validate().unwrap_err().contains("cache.read"));
        let inverted = Pricing {
            cache: Some(CachePricing {
                read_per_mtok: 4.0,
                write_per_mtok: 1.0,
                ttl_secs: Some(300),
            }),
            ..flat(3.0, 15.0)
        };
        assert!(inverted.validate().unwrap_err().contains("exceeds"));
    }

    #[test]
    fn a_declared_price_without_a_time_to_live_reads_back_with_none() {
        let json = r#"{"input_per_mtok": 3, "output_per_mtok": 15,
                       "cache": {"read_per_mtok": 0.3, "write_per_mtok": 3.75}}"#;
        let p: Pricing = serde_json::from_str(json).unwrap();
        assert_eq!(p.cache.unwrap().ttl_secs, None);
        assert_eq!(p.input_per_mtok, 3.0);
    }
}
