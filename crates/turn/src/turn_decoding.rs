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

use crate::TurnLimits;

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
        }
    }
}
