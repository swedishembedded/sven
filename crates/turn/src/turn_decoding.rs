// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! One turn's own decoding settings over a run's limits.
//!
//! Swedish Embedded AB implements reproducible agent runs on locally served
//! models for its clients. If your team needs expertise in recording exactly
//! how an agent's model was sampled then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::time::Duration;

use sven_model::{ModelProvider, SamplingControl, SamplingControls, SamplingRefused};
use sven_vocab::Sampled;

use crate::TurnLimits;

/// What one turn's sampling controls come to with the provider that answers
/// it: the controls that take effect, and those only sent on best effort.
pub(crate) struct SamplingAccount {
    asked_seed: Option<u64>,
    pub(crate) honoured: SamplingControls,
    pub(crate) unhonoured: SamplingControls,
}

impl SamplingAccount {
    /// The seed the driver says is in effect when the server reports none:
    /// the one asked for, which is in effect only where seeds are honoured.
    pub(crate) fn seed_in_effect(&self, seed: Option<u64>) -> Option<u64> {
        seed.filter(|_| !self.unhonoured.contains(SamplingControl::Seed))
    }

    /// A server that reports using the seed asked for honoured it, declared
    /// or not.
    pub(crate) fn note_reported_seed(&self, sampled: &mut Sampled, reported: Option<u64>) {
        if reported.is_some() && reported == self.asked_seed {
            let seed = SamplingControls::of(&[SamplingControl::Seed]);
            sampled.sampling_honoured = sampled.sampling_honoured.union(seed);
            sampled.sampling_unhonoured = sampled.sampling_unhonoured.without(seed);
        }
    }
}

impl TurnLimits {
    /// Lays one turn's decoding settings over these limits: each setting the
    /// turn makes replaces the run's, the others stay.
    #[must_use]
    pub fn with_turn_decoding(mut self, decoding: &sven_vocab::TurnDecoding) -> Self {
        let s = &mut self.sampling;
        s.temperature = decoding.temperature.or(s.temperature);
        s.top_p = decoding.top_p.or(s.top_p);
        s.top_k = decoding.top_k.or(s.top_k);
        s.seed = decoding.seed.or(s.seed);
        self.enable_thinking = decoding.thinking.or(self.enable_thinking);
        self.thinking.max_thinking_tokens = decoding
            .thinking_budget
            .or(self.thinking.max_thinking_tokens);
        self.max_output_tokens = decoding.max_output_tokens.or(self.max_output_tokens);
        self.deadline = decoding
            .deadline
            .filter(|&s| s > 0)
            .map(Duration::from_secs)
            .or(self.deadline);
        if let Some(secs) = decoding.first_response_timeout.filter(|&s| s > 0) {
            let wait = Duration::from_secs(secs);
            self.stall.first_byte = Some(wait);
            self.stall.first_token = Some(wait);
        }
        self
    }

    /// The settings these limits send, as the request's account records them.
    #[must_use]
    pub fn recorded_decoding(&self) -> sven_vocab::TurnDecoding {
        sven_vocab::TurnDecoding {
            temperature: self.sampling.temperature,
            top_p: self.sampling.top_p,
            top_k: self.sampling.top_k,
            seed: self.sampling.seed,
            thinking: self.enable_thinking,
            thinking_budget: self.thinking.max_thinking_tokens,
            max_output_tokens: self.max_output_tokens,
            first_response_timeout: self.stall.first_token.map(|d| d.as_secs()),
            deadline: self.deadline.map(|d| d.as_secs()),
        }
    }

    /// Accounts for these limits' sampling controls with `model`.
    ///
    /// # Errors
    ///
    /// [`SamplingRefused`] when a control is one `model` does not declare it
    /// honours and its configuration does not allow sending it on best
    /// effort: the request is refused before anything is sent.
    pub(crate) fn sampling_account(
        &self,
        model: &dyn ModelProvider,
    ) -> Result<SamplingAccount, SamplingRefused> {
        let unhonoured = self.sampling.unhonoured_by(model);
        if !unhonoured.is_empty() && !model.sampling_best_effort() {
            return Err(SamplingRefused {
                model: model.model_name().to_string(),
                controls: unhonoured,
            });
        }
        Ok(SamplingAccount {
            asked_seed: self.sampling.seed,
            honoured: self.sampling.controls().without(unhonoured),
            unhonoured,
        })
    }
}
