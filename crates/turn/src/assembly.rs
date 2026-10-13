// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The context view a request carries.
//!
//! [`ContextPolicy::assemble`] is a pure function of the conversation about to
//! be sent: under a token budget it keeps the tool results of the most recent
//! rounds in full and demotes older ones to a smaller, deterministic
//! representation (see [`sven_chunks::Visibility`]), oldest first, only as
//! far as the budget requires. It returns the view, a record of every
//! demotion and the raw text of each demoted result, which the caller stores
//! so the model can expand the reference.
//!
//! # Invariants
//!
//! - A history that fits the budget is returned unchanged.
//! - A pinned result is always full; a result that reports a failure is never
//!   below `long`. The policy computes the set of forms a result may take and
//!   only ever picks from it, whoever advises.
//! - Results of the expansion tool ([`sven_chunks::EXPAND_TOOL_NAME`]) and of tools without a
//!   representation are never demoted. An expansion is what the model asked
//!   for; demoting it would undo the request.
//! - The form of a result depends only on the result, its call and its age
//!   class, so a result demoted for one request has the same text in the next.
//!   The provider's prompt cache is therefore invalidated only from the first
//!   result that changes.
//!
//! No model is called here. A [`VisibilityAdvisor`] is the seam for one: it is
//! asked for each candidate and may choose among the forms the policy allows.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use regex::Regex;
use sven_chunks::{
    allowed_visibilities, content_digest, indicates_failure, render, tokens_of, ChunkDraft,
    ChunkId, ChunkKind, Lifetime, Provenance, RenderRequest, Sensitivity, Visibility,
};
use sven_model::{Message, MessageContent, ToolResultContent};

use crate::ledger::canonical_json;

/// Tokens of conversation a request may carry before results are demoted.
pub const DEFAULT_BUDGET_TOKENS: u32 = 12_000;
/// Rounds of tool calls, counted back from the newest, whose results stay full.
pub const DEFAULT_KEEP_RECENT_ROUNDS: u32 = 2;
/// A result smaller than this is not worth a reference.
pub const DEFAULT_MIN_DEMOTE_TOKENS: u32 = 64;

/// What the policy is allowed to spend and keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyConfig {
    /// Estimated tokens of the whole conversation above which demotion starts.
    pub budget_tokens: u32,
    /// A round is the tool calls the model made in one response. Results of
    /// this many most recent rounds are never demoted.
    pub keep_recent_rounds: u32,
    /// Results with fewer estimated tokens are left alone.
    pub min_demote_tokens: u32,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            budget_tokens: DEFAULT_BUDGET_TOKENS,
            keep_recent_rounds: DEFAULT_KEEP_RECENT_ROUNDS,
            min_demote_tokens: DEFAULT_MIN_DEMOTE_TOKENS,
        }
    }
}

/// One tool result the policy may demote, as an advisor sees it.
#[derive(Debug, Clone)]
pub struct Candidate<'a> {
    pub chunk_id: &'a ChunkId,
    pub kind: ChunkKind,
    pub tool: &'a str,
    /// The result reports a failure.
    pub failure: bool,
    /// Rounds between this result's round and the newest.
    pub age_rounds: u32,
    /// Estimated tokens of the raw result.
    pub full_tokens: u32,
    /// How it is shown now.
    pub current: Visibility,
    /// The forms it may take, lowest first.
    pub allowed: &'a [Visibility],
}

/// Chooses how far to demote a candidate. It only advises: a choice outside
/// [`Candidate::allowed`], or not below [`Candidate::current`], is replaced by
/// the policy's own `default`.
pub trait VisibilityAdvisor {
    fn advise(&self, candidate: &Candidate<'_>, default: Visibility) -> Visibility;
}

/// The policy's own choice, always.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeterministicAdvisor;

impl VisibilityAdvisor for DeterministicAdvisor {
    fn advise(&self, _candidate: &Candidate<'_>, default: Visibility) -> Visibility {
        default
    }
}

/// A result that was shown smaller than it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Demotion {
    pub chunk_id: ChunkId,
    pub tool: String,
    pub from: Visibility,
    pub to: Visibility,
    pub tokens_saved: u32,
}

/// The raw text behind a demotion, for the caller to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub raw: String,
    pub draft: ChunkDraft,
}

/// The conversation to send and what was done to it.
#[derive(Debug, Clone)]
pub struct AssembledView {
    pub messages: Vec<Message>,
    pub demotions: Vec<Demotion>,
    /// One entry per demoted result: store these before sending the view, or
    /// the references in it cannot be expanded.
    pub spill: Vec<Evidence>,
}

/// The deterministic assembly policy.
#[derive(Debug, Clone, Copy, Default)]
pub struct ContextPolicy {
    config: PolicyConfig,
}

impl ContextPolicy {
    #[must_use]
    pub fn new(config: PolicyConfig) -> Self {
        Self { config }
    }

    /// Assemble the view of `messages`. `pinned` holds the chunks that must
    /// stay full.
    #[must_use]
    pub fn assemble(
        &self,
        messages: Vec<Message>,
        pinned: &HashSet<ChunkId>,
        advisor: &dyn VisibilityAdvisor,
    ) -> AssembledView {
        let total: usize = messages.iter().map(Message::approx_tokens).sum();
        let budget = self.config.budget_tokens as usize;
        if total <= budget {
            return AssembledView {
                messages,
                demotions: Vec::new(),
                spill: Vec::new(),
            };
        }
        let mut results = self.collect(&messages, pinned);
        let mut remaining = total;
        for pass in [Visibility::Long, Visibility::Short] {
            for r in &mut results {
                if remaining <= budget {
                    break;
                }
                remaining = remaining.saturating_sub(r.demote(pass, advisor));
            }
        }
        apply(messages, results)
    }

    /// The results that may be demoted, oldest first.
    fn collect(&self, messages: &[Message], pinned: &HashSet<ChunkId>) -> Vec<Entry> {
        let calls = index_calls(messages);
        let newest = calls.values().map(|c| c.round).max().unwrap_or(0);
        let mut out = Vec::new();
        for (index, message) in messages.iter().enumerate() {
            let MessageContent::ToolResult {
                tool_call_id,
                content: ToolResultContent::Text(raw),
            } = &message.content
            else {
                continue;
            };
            let Some(call) = calls.get(tool_call_id.as_str()) else {
                continue;
            };
            let Some(kind) = classify(&call.name, &call.arguments) else {
                continue;
            };
            let age = newest - call.round;
            let full_tokens = tokens_of(raw);
            let chunk_id = ChunkId::for_content(raw.as_bytes());
            if age < self.config.keep_recent_rounds
                || full_tokens < self.config.min_demote_tokens
                || pinned.contains(&chunk_id)
            {
                continue;
            }
            out.push(Entry {
                index,
                chunk_id,
                kind,
                tool: call.name.clone(),
                arguments: call.arguments.clone(),
                round: call.round,
                age,
                failure: indicates_failure(raw),
                raw: raw.clone(),
                full_tokens,
                level: Visibility::Full,
                shown: None,
            });
        }
        out
    }
}

struct CallInfo {
    name: String,
    arguments: String,
    round: u32,
}

/// Index the calls by id and number their rounds: a round starts at a tool
/// call that does not directly follow another tool call.
fn index_calls(messages: &[Message]) -> HashMap<&str, CallInfo> {
    let mut calls = HashMap::new();
    let mut round = 0u32;
    let mut previous_was_call = false;
    for message in messages {
        if let MessageContent::ToolCall {
            tool_call_id,
            function,
        } = &message.content
        {
            if !previous_was_call {
                round += 1;
            }
            calls.insert(
                tool_call_id.as_str(),
                CallInfo {
                    name: function.name.clone(),
                    arguments: function.arguments.clone(),
                    round,
                },
            );
            previous_was_call = true;
        } else {
            previous_was_call = false;
        }
    }
    calls
}

struct Entry {
    index: usize,
    chunk_id: ChunkId,
    kind: ChunkKind,
    tool: String,
    arguments: String,
    round: u32,
    age: u32,
    failure: bool,
    raw: String,
    full_tokens: u32,
    level: Visibility,
    /// The text shown at `level`; `None` while full.
    shown: Option<String>,
}

impl Entry {
    /// Move one step toward `default`, or wherever the advisor and the
    /// allowed set permit. Returns the estimated tokens saved.
    fn demote(&mut self, default: Visibility, advisor: &dyn VisibilityAdvisor) -> usize {
        let allowed = allowed_visibilities(self.failure, false);
        let floor = allowed.first().copied().unwrap_or(Visibility::Full);
        let default = default.max(floor);
        let advised = advisor.advise(
            &Candidate {
                chunk_id: &self.chunk_id,
                kind: self.kind,
                tool: &self.tool,
                failure: self.failure,
                age_rounds: self.age,
                full_tokens: self.full_tokens,
                current: self.level,
                allowed: &allowed,
            },
            default,
        );
        let target = if allowed.contains(&advised) && advised < self.level {
            advised
        } else {
            default
        };
        if target >= self.level {
            return 0;
        }
        let rendered = render(
            &RenderRequest {
                id: self.chunk_id.clone(),
                kind: self.kind,
                source_tool: &self.tool,
                path: path_argument(&self.arguments).as_deref(),
                failure: self.failure,
                pinned: false,
                raw: &self.raw,
            },
            target,
        );
        let before = self.shown.as_deref().map_or(self.full_tokens, tokens_of);
        let after = tokens_of(&rendered.text);
        if after >= before {
            return 0;
        }
        self.level = rendered.visibility;
        self.shown = Some(rendered.text);
        (before - after) as usize
    }
}

fn apply(mut messages: Vec<Message>, results: Vec<Entry>) -> AssembledView {
    let mut demotions = Vec::new();
    let mut spill = Vec::new();
    for entry in results {
        let Some(shown) = entry.shown else { continue };
        if let MessageContent::ToolResult { content, .. } = &mut messages[entry.index].content {
            *content = ToolResultContent::Text(shown.clone());
        }
        demotions.push(Demotion {
            chunk_id: entry.chunk_id,
            tool: entry.tool.clone(),
            from: Visibility::Full,
            to: entry.level,
            tokens_saved: entry.full_tokens.saturating_sub(tokens_of(&shown)),
        });
        spill.push(Evidence {
            draft: ChunkDraft {
                kind: entry.kind,
                provenance: Provenance {
                    source_tool: entry.tool,
                    args_digest: content_digest(canonical_json(&entry.arguments).as_bytes()),
                    path: path_argument(&entry.arguments),
                    source_version: None,
                    turn: entry.round,
                },
                priority: 0,
                sensitivity: Sensitivity::Internal,
                lifetime: Lifetime::Session,
                pinned: false,
                failure: entry.failure,
            },
            raw: entry.raw,
        });
    }
    AssembledView {
        messages,
        demotions,
        spill,
    }
}

/// What kind of evidence a tool's result is, or `None` when the result has no
/// representation and is always sent as it is.
#[must_use]
pub fn classify(tool: &str, arguments: &str) -> Option<ChunkKind> {
    // `EXPAND_TOOL_NAME` is deliberately absent: an expansion is never demoted.
    match tool {
        "read_file" => Some(ChunkKind::File),
        "shell" => Some(if is_test_command(arguments) {
            ChunkKind::TestResult
        } else {
            ChunkKind::CommandOutput
        }),
        "grep" | "find_file" | "web_fetch" | "web_search" | "read_lints" => {
            Some(ChunkKind::CommandOutput)
        }
        "task" => Some(ChunkKind::SubagentFinding),
        _ => None,
    }
}

fn is_test_command(arguments: &str) -> bool {
    static TEST: OnceLock<Regex> = OnceLock::new();
    let re = TEST.get_or_init(|| {
        Regex::new(r"\b(cargo (test|nextest)|pytest|go test|npm (run )?test|bats|ctest|make (test|check))\b")
            .expect("test command pattern is valid")
    });
    argument(arguments, "command").is_some_and(|c| re.is_match(&c))
}

fn path_argument(arguments: &str) -> Option<String> {
    argument(arguments, "path").or_else(|| argument(arguments, "file_path"))
}

fn argument(arguments: &str, key: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(arguments).ok()?;
    value.get(key)?.as_str().map(str::to_string)
}
