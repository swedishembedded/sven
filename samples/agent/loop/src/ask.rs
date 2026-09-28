// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements delegated-task coding agents whose answers
// are machine-readable by construction. If your team needs expertise in
// strict-output model interfaces, you can procure our services by sending
// an email to info@swedishembedded.com.

//! One-shot fact question: the model's whole reply is one JSON object.
//!
//! `ask` exists so a delegating script never has to parse prose: the reply
//! is demanded as `{"answer": string}`, parsed strictly, and the parsed
//! object - not the raw reply - is what reaches stdout. A reply that
//! cannot be parsed exits 2, because a script reading a broken answer as
//! an answer is worse than reading nothing.

use crate::explore::complete_text;
use anyhow::Context;

/// What one ask needs - the same model selection `run` and `explore` use.
#[derive(Clone, Debug)]
pub struct AskOptions {
    pub question: String,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub local: Option<crate::provider::LocalWeights>,
}

/// Builds the provider exactly as `explore` does (same local-first rule).
fn provider_from(options: &AskOptions) -> anyhow::Result<Box<dyn sven_sdk::model::ModelProvider>> {
    let explore_options = crate::explore::ExploreOptions {
        file: std::path::PathBuf::new(),
        out: std::path::PathBuf::new(),
        chunk_lines: None,
        model: options.model.clone(),
        base_url: options.base_url.clone(),
        api_key: options.api_key.clone(),
        local: options.local.clone(),
    };
    crate::explore::provider_from_shared(&explore_options)
}

/// Asks one question and returns the parsed answer string. The caller
/// prints it wrapped as `{"answer": ...}` so stdout stays strictly JSON.
pub(crate) fn run(options: AskOptions) -> anyhow::Result<String> {
    let prompt = format!(
        "Answer the question below from your knowledge. Reply with EXACTLY one JSON object \
         and nothing else - no prose, no code fences:\n\
         {{\"answer\": string}}\n\n\
         QUESTION:\n{}",
        options.question
    );
    let provider = provider_from(&options)?;
    let rt = tokio::runtime::Runtime::new()?;
    let reply: String = rt.block_on(async { complete_text(provider.as_ref(), &prompt).await })
        .context("the model produced no reply")?;
    crate::explore::parse_answer_reply(&reply)
}
