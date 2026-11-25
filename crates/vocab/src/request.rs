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
use crate::sampling::SamplingControls;
use crate::served::ServedIdentity;
use crate::turn::TurnDecoding;
use crate::SessionEvent;

/// The largest sampler support a request can ask for: the bound of what a server returns per generated token.
pub const MAX_SAMPLER_SUPPORT: u32 = 256;

/// The set of tokens the sampler drew each generated token from (the survivors of its top-k / top-p / min-p cut),
/// and the log-probability of each drawn token under that truncated distribution: what a trainer needs to replay
/// the behaviour policy of a truncated sampler.
///
/// Position `i`'s support is `ids[offsets[i]..offsets[i + 1]]`, best first. A sampler that does not truncate draws
/// from the whole row and reports an empty range.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SamplerSupport {
    /// The size the request asked for: no position's support is longer.
    pub cap: u32,
    /// For each generated token, its log-probability under the distribution the sampler drew it from.
    pub log_probs: Vec<f64>,
    /// `n + 1` offsets into `ids` for `n` generated tokens: starting at 0, non-decreasing, ending at `ids.len()`.
    pub offsets: Vec<u32>,
    /// The concatenated supports.
    pub ids: Vec<i64>,
}

impl SamplerSupport {
    /// Checks the size a request asks for: 1..=[`MAX_SAMPLER_SUPPORT`], the bound of what a server returns.
    pub fn check_size(n: u32) -> Result<u32, String> {
        if (1..=MAX_SAMPLER_SUPPORT).contains(&n) {
            Ok(n)
        } else {
            Err(format!(
                "a sampler support of {n} ids is outside 1..={MAX_SAMPLER_SUPPORT}"
            ))
        }
    }

    /// The support of position `i`.
    #[must_use]
    pub fn at(&self, position: usize) -> &[i64] {
        &self.ids[self.offsets[position] as usize..self.offsets[position + 1] as usize]
    }

    /// Checks the structure for `generated` generated tokens: one log-probability per token, `generated + 1`
    /// offsets from 0 to the id count that never decrease, and no range longer than the cap.
    pub fn check(&self, generated: usize) -> Result<(), String> {
        if self.log_probs.len() != generated {
            return Err(format!(
                "{} sampler log-probabilities for {generated} generated tokens",
                self.log_probs.len()
            ));
        }
        if self.offsets.len() != generated + 1 {
            return Err(format!(
                "{} support offsets for {generated} generated tokens (expected {})",
                self.offsets.len(),
                generated + 1
            ));
        }
        if self.offsets[0] != 0 || self.offsets[generated] as usize != self.ids.len() {
            return Err(format!(
                "support offsets run from {} to {}, but there are {} ids (expected 0 to the id count)",
                self.offsets[0],
                self.offsets[generated],
                self.ids.len()
            ));
        }
        for (position, pair) in self.offsets.windows(2).enumerate() {
            if pair[1] < pair[0] {
                return Err(format!("support offsets decrease at position {position}"));
            }
            if pair[1] - pair[0] > self.cap {
                return Err(format!(
                    "the support at position {position} holds {} ids, above the cap {}",
                    pair[1] - pair[0],
                    self.cap
                ));
            }
        }
        Ok(())
    }

    /// Appends the support of a later part of the same request (or the next request of a step), moving its offsets
    /// behind the ids already held.
    pub fn append(&mut self, later: &SamplerSupport) {
        let base = self.offsets.last().copied().unwrap_or(0);
        if self.offsets.is_empty() {
            self.offsets.push(0);
        }
        self.offsets
            .extend(later.offsets.iter().skip(1).map(|o| o + base));
        self.log_probs.extend(&later.log_probs);
        self.ids.extend(&later.ids);
        self.cap = self.cap.max(later.cap);
    }
}

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
    /// With a top-K request: for each sampled token, the ids of the most probable tokens at its position, most
    /// probable first (parallel to `token_ids`); empty unless asked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_token_ids: Vec<Vec<i64>>,
    /// Their log-probabilities under the same distribution as `logprobs` (parallel to `top_token_ids`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_logprobs: Vec<Vec<f64>>,
    /// With a support request: the set the sampler drew each token from; absent unless asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampler_support: Option<SamplerSupport>,
    /// The sampling and thinking settings the request was sent with, when
    /// any were made (a harness step can set them per turn).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decoding: Option<Box<TurnDecoding>>,
    /// The sampling controls the request set that took effect: the provider
    /// declares it honours them, or the server reported using them (a seed it
    /// echoed). A consumer's "seeded" means `Seed` is here.
    #[serde(default, skip_serializing_if = "SamplingControls::is_empty")]
    pub sampling_honoured: SamplingControls,
    /// The sampling controls the request set that the provider does not
    /// declare it honours: sent only because the configuration allows best
    /// effort, so nothing says they took effect.
    #[serde(default, skip_serializing_if = "SamplingControls::is_empty")]
    pub sampling_unhonoured: SamplingControls,
    /// The response ended at the output-token ceiling, not because the model
    /// stopped: its last tokens are a cut-off, not a finished answer.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

impl Sampled {
    /// Whether the provider reported nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.model.is_none()
            && self.seed.is_none()
            && self.token_ids.is_empty()
            && self.logprobs.is_empty()
            && self.top_token_ids.is_empty()
            && self.top_logprobs.is_empty()
            && self.sampler_support.is_none()
            && self.prompt_token_ids.is_empty()
            && self.identity.is_empty()
            && self.decoding.is_none()
            && self.sampling_honoured.is_empty()
            && self.sampling_unhonoured.is_empty()
            && !self.truncated
    }
}

/// Folds a stream of observations into one [`RequestRecord`] per model
/// request.
///
/// A request's observations end with its ledger record (the `TokenUsage`
/// carrying a breakdown), by contract the last event of its response; the
/// fold hands the finished record back at that event. One definition for
/// every consumer that keeps the account of a conversation (an agent's run, a
/// child run's store), so they cannot disagree about where a request ends.
#[derive(Debug, Clone, Default)]
pub struct RequestFold {
    open: RequestRecord,
}

impl RequestFold {
    /// Folds in one observation; returns the request it completes, if any.
    pub fn observe(&mut self, event: &SessionEvent) -> Option<RequestRecord> {
        match event {
            SessionEvent::ResponseSampled(sampled) => {
                self.open.sampled = (**sampled).clone();
                None
            }
            SessionEvent::TokenUsage {
                input,
                output,
                cache_read,
                cache_write,
                cost_usd,
                breakdown,
                ..
            } => {
                self.open.add_usage(
                    ReportedUsage {
                        input: *input,
                        output: *output,
                        cache_read: *cache_read,
                        cache_write: *cache_write,
                    },
                    *cost_usd,
                );
                breakdown.is_some().then(|| std::mem::take(&mut self.open))
            }
            _ => None,
        }
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
