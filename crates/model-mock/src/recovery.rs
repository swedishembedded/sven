// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! A scripted model that follows a reference back to the raw text.
//!
//! A rule may carry a [`RecoverRule`]. Before it answers, the mock reads the
//! results already in the request: every result that holds a reference (a
//! match of `reference`, whose first group names what to fetch) and that
//! mentions one of the `focus` terms is fetched with `call`, once. The rule is
//! fixed and knows nothing of which lines matter; it is what a model does that
//! was told what to look at and sees a shortened result that mentions it.
//!
//! The reference text is data. It is matched, never interpreted, and the call
//! is made only if the request offers the tool, as a model cannot call a tool
//! it was not given.

use std::collections::BTreeSet;

use regex::Regex;
use serde::Deserialize;
use sven_model::{CompletionRequest, MessageContent};

use crate::yaml_mock::ToolCallDef;

/// The call that fetches what a reference names. `{1}` in a string argument
/// stands for the first group of the reference.
#[derive(Debug, Deserialize)]
pub struct RecoverCall {
    pub tool: String,
    pub args: serde_json::Value,
}

/// How the scripted model recovers what a result left out.
#[derive(Debug, Deserialize)]
pub struct RecoverRule {
    /// A regular expression; its first group is the reference's target.
    pub reference: String,
    pub call: RecoverCall,
    /// Terms the question names. A result is followed only if its text
    /// contains one of them, ignoring case.
    pub focus: Vec<String>,
}

impl RecoverRule {
    /// Checks the rule can be applied.
    ///
    /// # Errors
    ///
    /// The reference is not a regular expression with a group, or the rule
    /// names no focus (a rule that follows every reference is not a choice).
    pub fn validate(&self) -> anyhow::Result<()> {
        let re = Regex::new(&self.reference)
            .map_err(|e| anyhow::anyhow!("recover.reference is not a pattern: {e}"))?;
        anyhow::ensure!(
            re.captures_len() >= 2,
            "recover.reference needs a group that names the target"
        );
        anyhow::ensure!(!self.focus.is_empty(), "recover.focus names no term");
        Ok(())
    }

    /// The calls that fetch the referenced results mentioning the focus, in
    /// the order the results appear, each target once and none that an
    /// earlier call of the conversation already fetched.
    pub(crate) fn calls(&self, req: &CompletionRequest) -> Vec<ToolCallDef> {
        if !req.tools.iter().any(|t| t.name == self.call.tool) {
            return Vec::new();
        }
        let Ok(re) = Regex::new(&self.reference) else {
            return Vec::new();
        };
        let focus: Vec<String> = self.focus.iter().map(|f| f.to_lowercase()).collect();
        let mut done: BTreeSet<String> = req
            .messages
            .iter()
            .filter_map(|m| match &m.content {
                MessageContent::ToolCall { function, .. } if function.name == self.call.tool => {
                    serde_json::from_str::<serde_json::Value>(&function.arguments)
                        .ok()
                        .map(|v| v.to_string())
                }
                _ => None,
            })
            .collect();
        let mut calls = Vec::new();
        for message in &req.messages {
            let MessageContent::ToolResult { content, .. } = &message.content else {
                continue;
            };
            let Some(text) = content.as_text() else {
                continue;
            };
            let lower = text.to_lowercase();
            if !focus.iter().any(|f| lower.contains(f)) {
                continue;
            }
            for captures in re.captures_iter(text) {
                let args = substitute(&self.call.args, &captures[1]);
                if done.insert(args.to_string()) {
                    calls.push(ToolCallDef {
                        id: format!("mock-recover-{}", calls.len()),
                        tool: self.call.tool.clone(),
                        args,
                    });
                }
            }
        }
        calls
    }
}

fn substitute(template: &serde_json::Value, target: &str) -> serde_json::Value {
    match template {
        serde_json::Value::String(s) => serde_json::Value::String(s.replace("{1}", target)),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(|v| substitute(v, target)).collect())
        }
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), substitute(v, target)))
                .collect(),
        ),
        other => other.clone(),
    }
}
