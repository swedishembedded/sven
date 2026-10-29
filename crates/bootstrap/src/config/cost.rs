// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the cost of agent
// workloads explicit and predictable for its clients. If your team needs
// expertise in LLM cost engineering then you can procure our services by
// sending an email to info@swedishembedded.com.
//! The `cost` section: whether a rewrite of the prompt is weighed against the
//! provider's prompt cache, and the prices the weighing uses. See
//! `docs/internals/cost-model.md`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sven_config::Schema;
use sven_cost::CacheAware;
use sven_executors::CostPolicy;
use sven_model_catalog::Pricing;

/// `cost`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CostSettings {
    /// `off` weighs nothing; `advise` records, per request, whether a demotion
    /// pays for the cached prefix it discards and demotes regardless;
    /// `enforce` leaves a demotion undone that does not pay, unless the model's
    /// window needs it or a price is undeclared.
    pub cache_aware: CacheAware,
    /// Requests still expected in a session. Unset: about as many as it has made
    /// so far.
    pub remaining_requests: Option<u32>,
    /// Prices per million tokens by `provider/model`, over the ones
    /// the model's endpoint serves. A model with neither has an unknown price, which
    /// never justifies skipping a demotion.
    pub prices: BTreeMap<String, Pricing>,
}

impl CostSettings {
    /// The keys of the `cost` section.
    #[must_use]
    pub fn schema() -> Schema {
        Schema::keys(&["cache_aware", "remaining_requests", "prices"])
    }

    /// What the turn executor is given.
    ///
    /// # Errors
    /// Names the first declared price that is not a usable one.
    pub fn policy(&self) -> Result<CostPolicy, String> {
        for (model, price) in &self.prices {
            price
                .validate()
                .map_err(|why| format!("cost.prices.{model}: {why}"))?;
        }
        Ok(CostPolicy {
            mode: self.cache_aware,
            declared_remaining: self.remaining_requests,
            prices: self.prices.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use sven_config::ConfigDocument;

    use super::*;
    use crate::config::Config;

    fn load(yaml: &str) -> Config {
        Config::from_document(&ConfigDocument::from_yaml(yaml).unwrap())
    }

    #[test]
    fn the_gate_is_off_unless_the_file_turns_it_on() {
        assert_eq!(load("agent: {}").cost.cache_aware, CacheAware::Off);
        let config = load(
            r"
cost:
  cache_aware: enforce
  remaining_requests: 12
  prices:
    openai/gpt:
      input_per_mtok: 3.0
      output_per_mtok: 15.0
      cache: {read_per_mtok: 0.3, write_per_mtok: 3.75, ttl_secs: 300}
",
        );
        let policy = config.cost.policy().unwrap();
        assert_eq!(policy.mode, CacheAware::Enforce);
        assert_eq!(policy.declared_remaining, Some(12));
        assert!(policy.prices.contains_key("openai/gpt"));
    }

    #[test]
    fn a_price_that_is_not_usable_is_refused_naming_the_model() {
        let mut settings = CostSettings::default();
        settings.prices.insert(
            "p/m".into(),
            Pricing {
                input_per_mtok: -1.0,
                output_per_mtok: 1.0,
                cache: None,
            },
        );
        assert!(settings.policy().unwrap_err().contains("cost.prices.p/m"));
    }
}
