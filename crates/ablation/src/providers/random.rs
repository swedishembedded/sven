// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The negative control: valid decisions that carry no information.

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use sven_decision::{Choice, DecisionError, DecisionProvider, DecisionRequest, DecisionResult};

use super::answer_request;

/// Spread of the random weights: large enough that the winner sometimes
/// clears a policy's confidence and margin, which is what makes the control
/// able to do harm.
const SPREAD: f64 = 4.0;

/// Answers every choice with a random distribution that depends only on the
/// seed, the state, the question and each option's id, never on the order of
/// the options: asking again in the opposite order cannot sample twice.
#[derive(Debug, Clone, Copy)]
pub struct RandomProvider {
    seed: u64,
}

impl RandomProvider {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self { seed }
    }

    fn weigh(&self, request: &DecisionRequest, choice: &Choice) -> Vec<f64> {
        let mut base = Sha256::new();
        base.update(self.seed.to_le_bytes());
        base.update(request.state.as_bytes());
        base.update(choice.id.as_bytes());
        base.update(choice.instruction.as_bytes());
        choice
            .options
            .iter()
            .map(|option| {
                let mut h = base.clone();
                h.update(option.id.as_bytes());
                let digest = h.finalize();
                let mut eight = [0_u8; 8];
                eight.copy_from_slice(&digest[..8]);
                let unit = (u64::from_le_bytes(eight) >> 11) as f64 / (1_u64 << 53) as f64;
                (SPREAD * unit).exp()
            })
            .collect()
    }
}

#[async_trait]
impl DecisionProvider for RandomProvider {
    async fn decide(&self, request: &DecisionRequest) -> Result<DecisionResult, DecisionError> {
        answer_request("ablation/random", request, |r, c| self.weigh(r, c))
    }
}
