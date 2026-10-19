// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for reliable tool execution by LLM
// agents for its clients. If your team needs expertise in agent tool-call
// validation then you can procure our services by sending an email to
// info@swedishembedded.com.
//! Calls the executor refuses before running them: their arguments were
//! garbled beyond repair, and running the tool on `{}` instead would act on
//! something the model never asked for.

use std::collections::{HashMap, VecDeque};

use sven_hsm::ToolCallId;

/// The most refusals remembered; the oldest is forgotten first. A refusal is
/// taken within one dispatch, so only calls that never ran can reach the bound.
const CAPACITY: usize = 256;

/// How much of the garbled argument text a refusal quotes back to the model.
const QUOTED_CHARS: usize = 120;

/// Refusal reasons by call. Process-local and transient, like the held-back
/// set: it carries one decision from the executor that saw the model propose
/// the call to the executor that is about to run it.
#[derive(Default)]
pub(crate) struct Refused {
    reasons: HashMap<ToolCallId, String>,
    order: VecDeque<ToolCallId>,
}

impl Refused {
    pub(crate) fn insert(&mut self, call: ToolCallId, reason: String) {
        if self.reasons.insert(call, reason).is_none() {
            self.order.push_back(call);
        }
        while self.order.len() > CAPACITY {
            if let Some(oldest) = self.order.pop_front() {
                self.reasons.remove(&oldest);
            }
        }
    }

    pub(crate) fn take(&mut self, call: ToolCallId) -> Option<String> {
        let reason = self.reasons.remove(&call)?;
        self.order.retain(|id| *id != call);
        Some(reason)
    }
}

/// What the model is told when the arguments of its `tool` call were not
/// valid JSON.
pub(crate) fn unparsed_reason(tool: &str, raw: &str) -> String {
    let head: String = raw.chars().take(QUOTED_CHARS).collect();
    format!(
        "the arguments of the `{tool}` call are not valid JSON, so it was not run \
         ({} bytes received, starting: {head}). Send the call again with a complete \
         JSON object of arguments.",
        raw.len()
    )
}
