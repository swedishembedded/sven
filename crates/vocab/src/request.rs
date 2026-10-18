// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent cost accounting for its clients. If
// your team needs expertise in measuring what agent runs cost then you can
// procure our services by sending an email to info@swedishembedded.com.

//! One model request as an account: what it read and wrote, what it cost,
//! which model answered, and the tokens it sampled.
//!
//! The ledger ([`crate::ledger`]) explains where a request's input tokens
//! went; this is the plain account a run is priced, compared and reproduced
//! from.

use serde::{Deserialize, Serialize};

use crate::ledger::ReportedUsage;
use crate::served::ServedIdentity;

/// What served one request and what it sampled, as the provider reported it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sampled {
    /// The identity the provider says answered, which may differ from the
    /// model that was asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The sampling seed in effect: the one the server reports it used, else
    /// the one the request carried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// The ids of the prompt as the model read it; empty unless the request
    /// asked for them and the server returned them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompt_token_ids: Vec<i64>,
    /// What decoded the request: adapter, base checkpoint, backend, engine.
    #[serde(default, skip_serializing_if = "ServedIdentity::is_empty")]
    pub identity: ServedIdentity,
    /// The sampled completion tokens in generation order; empty unless the
    /// request asked for them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub token_ids: Vec<i64>,
    /// The log-probability of each sampled token, in the same order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub logprobs: Vec<f64>,
}

impl Sampled {
    /// Whether the provider reported nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.model.is_none()
            && self.seed.is_none()
            && self.token_ids.is_empty()
            && self.logprobs.is_empty()
            && self.prompt_token_ids.is_empty()
            && self.identity.is_empty()
    }
}

/// One model request's account.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RequestRecord {
    /// Tokens the provider reported, summed over its usage reports.
    #[serde(default)]
    pub reported: ReportedUsage,
    /// What the provider charged, when it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Who answered and what was sampled.
    #[serde(default)]
    pub sampled: Sampled,
}

impl RequestRecord {
    /// Adds one usage report of the request.
    pub fn add_usage(&mut self, report: ReportedUsage, cost_usd: Option<f64>) {
        let sum = |slot: &mut u32, n: u32| *slot = slot.saturating_add(n);
        sum(&mut self.reported.input, report.input);
        sum(&mut self.reported.output, report.output);
        sum(&mut self.reported.cache_read, report.cache_read);
        sum(&mut self.reported.cache_write, report.cache_write);
        if let Some(cost) = cost_usd {
            self.cost_usd = Some(self.cost_usd.unwrap_or(0.0) + cost);
        }
    }
}
