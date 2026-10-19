// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The I/O side of addressable context.
//!
//! [`ChunkSession`] is what the tool and turn executors share: the session's
//! chunk store, the probe that reads a source file's current version, and the
//! turn counter. The tool executor records each result as it arrives, so a
//! file read carries the file's version from the moment it was read.
//! [`ContextAssembler`] runs the pure [`ContextPolicy`] on the history before a
//! request is built and stores the raw text behind every result it demoted.
//!
//! A result is demoted only if its raw text is in the store: when storing it
//! fails the result stays whole, because a reference that cannot be expanded
//! would lose the evidence.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use serde_json::Value;
use sven_chunks::{
    indicates_failure, ChunkDraft, ChunkId, Lifetime, Provenance, Sensitivity, SharedChunkStore,
    SourceProbe,
};
use sven_cost::{CacheAware, CacheState, Necessity, RebuildCandidate};
use sven_model::{Message, MessageContent};
use sven_model_catalog::Pricing;
use sven_turn::assembly::{
    classify, path_argument, AssembledView, ContextPolicy, DeterministicAdvisor, Evidence,
    VisibilityAdvisor,
};
use sven_vocab::cost::{RebuildKind, RebuildRecord};
use sven_vocab::{ContextView, DemotionRecord};
use tracing::warn;

/// The label the ledger records for this assembly mode.
const DEMOTE_MODE: &str = "demote";

/// A session's chunk store and what is needed to describe where a chunk came
/// from.
pub struct ChunkSession {
    store: SharedChunkStore,
    probe: Arc<dyn SourceProbe + Send + Sync>,
    turn: AtomicU32,
}

impl ChunkSession {
    #[must_use]
    pub fn new(store: SharedChunkStore, probe: Arc<dyn SourceProbe + Send + Sync>) -> Self {
        Self {
            store,
            probe,
            turn: AtomicU32::new(0),
        }
    }

    /// The store the session records into.
    #[must_use]
    pub fn store(&self) -> &SharedChunkStore {
        &self.store
    }

    /// The probe that reads a source file's current version.
    #[must_use]
    pub fn probe(&self) -> Arc<dyn SourceProbe + Send + Sync> {
        Arc::clone(&self.probe)
    }

    /// Count a model request; returns its number.
    fn begin_turn(&self) -> u32 {
        self.turn.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Record a tool result, if its tool has a representation, with the
    /// version of the file it was read from. `content` is the text the
    /// conversation holds, so its id is the id the policy computes later.
    ///
    /// Returns the chunk id, or `None` when the result is not recordable (an
    /// unrepresented tool, or the store refused it: the result then simply
    /// stays whole in the conversation).
    pub fn record_result(&self, tool: &str, args: &Value, content: &str) -> Option<ChunkId> {
        let arguments = args.to_string();
        let kind = classify(tool, &arguments)?;
        let path = path_argument(&arguments);
        let source_version = path.as_deref().and_then(|p| match self.probe.version(p) {
            Ok(version) => version,
            Err(e) => {
                warn!(error = %e, "chunk source not inspectable; its version is not recorded");
                None
            }
        });
        let draft = ChunkDraft {
            kind,
            provenance: Provenance {
                source_tool: tool.to_string(),
                args_digest: sven_chunks::content_digest(args_canonical(&arguments).as_bytes()),
                path,
                source_version,
                turn: self.turn.load(Ordering::Relaxed),
            },
            priority: 0,
            sensitivity: Sensitivity::Internal,
            lifetime: Lifetime::Session,
            pinned: false,
            failure: indicates_failure(content),
        };
        let mut store = self.store.lock().ok()?;
        match store.put(content, draft) {
            Ok(outcome) => Some(outcome.chunk.id),
            Err(e) => {
                warn!(error = %e, tool, "tool result not recorded as a chunk");
                None
            }
        }
    }

    fn pinned(&self) -> HashSet<ChunkId> {
        self.store
            .lock()
            .map(|s| {
                s.chunks()
                    .filter(|c| c.pinned)
                    .map(|c| c.id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Store the evidence behind a demotion or a reconstructed history;
    /// whether it is now in the store.
    pub fn store_evidence(&self, evidence: &Evidence) -> bool {
        let Ok(mut store) = self.store.lock() else {
            return false;
        };
        match store.put(&evidence.raw, evidence.draft.clone()) {
            Ok(_) => true,
            Err(e) => {
                warn!(error = %e, "evidence for a demotion could not be stored; keeping the result whole");
                false
            }
        }
    }
}

fn args_canonical(arguments: &str) -> String {
    sven_turn::ledger::canonical_json(arguments)
}

/// What the cache-aware gate needs to know about the request being built.
#[derive(Debug, Clone)]
pub struct CacheContext {
    /// Whether to weigh a demotion against the cache it discards, and whether
    /// to act on the answer.
    pub mode: CacheAware,
    /// The declared price of the model; `None` is unknown, never free.
    pub pricing: Option<Pricing>,
    /// Prefix tokens the provider's cache is expected to still hold.
    pub warm_tokens: u32,
    /// Requests made in this session so far.
    pub requests_so_far: u32,
    /// Requests still expected, when the configuration declares them.
    pub declared_remaining: Option<u32>,
    /// The most input tokens the model's window admits, when known: a
    /// demotion needed to get under it is never skipped for cost.
    pub input_budget: Option<usize>,
}

/// The most requests a rebuild is weighed over when none is declared.
const MAX_ASSUMED_REMAINING: u32 = 200;

/// The context view of a request, and the cache-aware decisions made on the way.
#[derive(Debug, Clone)]
pub struct Assembled {
    pub messages: Vec<Message>,
    pub view: ContextView,
    /// The demotions that were weighed against the cache, with the numbers.
    /// Empty when the gate is off or there was nothing new to weigh.
    pub rebuilds: Vec<RebuildRecord>,
}

/// Builds the context view of each request from the thread's history.
pub struct ContextAssembler {
    policy: ContextPolicy,
    advisor: Box<dyn VisibilityAdvisor>,
    session: Arc<ChunkSession>,
    /// Chunks already named in an earlier request's record.
    reported: HashSet<ChunkId>,
}

impl ContextAssembler {
    #[must_use]
    pub fn new(policy: ContextPolicy, session: Arc<ChunkSession>) -> Self {
        Self {
            policy,
            advisor: Box::new(DeterministicAdvisor),
            session,
            reported: HashSet::new(),
        }
    }

    /// Replace the advisor that chooses among the forms the policy allows.
    #[must_use]
    pub fn with_advisor(mut self, advisor: Box<dyn VisibilityAdvisor>) -> Self {
        self.advisor = advisor;
        self
    }

    /// The messages a request carries and the record of how they were
    /// assembled. Every demoted result has its raw text in the store.
    ///
    /// The record names the results demoted for the first time; call
    /// [`mark_sent`](Self::mark_sent) once it is the record of a request that
    /// is really sent.
    pub fn assemble(&mut self, messages: Vec<Message>) -> (Vec<Message>, ContextView) {
        let (view, record) = self.assemble_parts(messages);
        (view.messages, record)
    }

    fn assemble_parts(&mut self, messages: Vec<Message>) -> (AssembledView, ContextView) {
        self.session.begin_turn();
        let original = messages.clone();
        let pinned = self.session.pinned();
        let mut view = self
            .policy
            .assemble(messages, &pinned, self.advisor.as_ref());
        self.keep_unstorable_whole(&mut view, &original);

        let mut saved_by_reason: BTreeMap<String, u32> = BTreeMap::new();
        for d in &view.demotions {
            *saved_by_reason
                .entry(d.reason.as_str().to_string())
                .or_default() += d.tokens_saved;
        }
        let record = ContextView {
            mode: DEMOTE_MODE.to_string(),
            demoted: u32::try_from(view.demotions.len()).unwrap_or(u32::MAX),
            tokens_saved: view.demotions.iter().map(|d| d.tokens_saved).sum(),
            saved_by_reason,
            new_demotions: view
                .demotions
                .iter()
                .zip(&view.spill)
                .filter(|(d, _)| !self.reported.contains(&d.chunk_id))
                .map(|(d, e)| DemotionRecord {
                    chunk: d.chunk_id.to_string(),
                    digest: sven_chunks::content_digest(e.raw.as_bytes()),
                    tool: d.tool.clone(),
                    from: d.from.as_str().to_string(),
                    to: d.to.as_str().to_string(),
                    tokens_saved: d.tokens_saved,
                    reason: d.reason.as_str().to_string(),
                })
                .collect(),
        };
        (view, record)
    }

    /// [`assemble`](Self::assemble), with the demotions that are new in this
    /// request weighed against the provider's prompt cache.
    ///
    /// A result demoted for the first time changes the prompt from that point
    /// on, so the cached prefix behind it is lost and written again. When
    /// `cache.mode` is `Enforce` and the saving over the requests still to come
    /// does not earn that back, the new demotions are left undone and the
    /// results stay whole; what earlier requests demoted stays demoted, so the
    /// gate never disturbs a prefix that is already cached. A demotion the
    /// window needs, or one whose cost is unknown, is never skipped.
    pub fn assemble_with(&mut self, messages: Vec<Message>, cache: &CacheContext) -> Assembled {
        let original = messages.clone();
        let (parts, view) = self.assemble_parts(messages);
        let assembled = parts.messages;
        let new: Vec<String> = parts
            .demotions
            .iter()
            .filter(|d| !self.reported.contains(&d.chunk_id))
            .map(|d| d.tool_call_id.clone())
            .collect();
        if cache.mode == CacheAware::Off || new.is_empty() {
            return Assembled {
                messages: assembled,
                view,
                rebuilds: Vec::new(),
            };
        }

        // The prompt as it would be sent with the new demotions undone.
        let mut kept = assembled.clone();
        for call in &new {
            restore(&mut kept, &original, call);
        }
        let tokens = |m: &[Message]| tokens_of(m);
        let (keep_tokens, rebuild_tokens) = (tokens(&kept), tokens(&assembled));
        let first_changed = kept
            .iter()
            .zip(&assembled)
            .position(|(a, b)| result_differs(a, b))
            .unwrap_or(kept.len());
        let candidate = RebuildCandidate {
            kind: RebuildKind::Demotion,
            prompt_tokens: keep_tokens,
            tokens_saved: keep_tokens.saturating_sub(rebuild_tokens),
            first_changed_token: tokens(&kept[..first_changed]),
            generation: sven_cost::Generation::default(),
        };
        let state = CacheState {
            warm_tokens: cache.warm_tokens,
        };
        let remaining = sven_cost::expected_remaining_requests(
            cache.requests_so_far,
            cache.declared_remaining,
            MAX_ASSUMED_REMAINING,
        );
        let assessment = sven_cost::assess(cache.pricing.as_ref(), state, &candidate, remaining);
        let necessity = match cache.input_budget {
            Some(budget) if keep_tokens as usize > budget => Necessity::RequiredToFit,
            _ => Necessity::Discretionary,
        };
        let decision = sven_cost::gate(cache.mode, necessity, assessment.recommendation);
        let record = sven_cost::record(&candidate, &assessment, &decision, state, remaining);

        if decision.action != sven_vocab::cost::GateAction::Skipped {
            return Assembled {
                messages: assembled,
                view,
                rebuilds: vec![record],
            };
        }
        // Skipped: the view with the new demotions undone. Only the new ones
        // leave the record; the older ones are still in the prompt.
        let mut view = view;
        let undone: usize = view.new_demotions.len();
        let saved: u32 = view.new_demotions.iter().map(|d| d.tokens_saved).sum();
        for d in &view.new_demotions {
            if let Some(reason) = view.saved_by_reason.get_mut(&d.reason) {
                *reason = reason.saturating_sub(d.tokens_saved);
            }
        }
        view.saved_by_reason.retain(|_, v| *v > 0);
        view.new_demotions.clear();
        view.demoted = view
            .demoted
            .saturating_sub(u32::try_from(undone).unwrap_or(u32::MAX));
        view.tokens_saved = view.tokens_saved.saturating_sub(saved);
        Assembled {
            messages: kept,
            view,
            rebuilds: vec![record],
        }
    }

    /// Note that `record` went out with a request: its demotions are not
    /// named again by later records.
    pub fn mark_sent(&mut self, record: &ContextView) {
        self.reported.extend(
            record
                .new_demotions
                .iter()
                .filter_map(|d| ChunkId::parse(&d.chunk).ok()),
        );
    }

    /// Put back every result whose evidence could not be stored.
    fn keep_unstorable_whole(&self, view: &mut AssembledView, original: &[Message]) {
        let mut kept = Vec::new();
        let (demotions, spill) = (
            std::mem::take(&mut view.demotions),
            std::mem::take(&mut view.spill),
        );
        for (demotion, evidence) in demotions.into_iter().zip(spill) {
            if self.session.store_evidence(&evidence) {
                kept.push((demotion, evidence));
                continue;
            }
            restore(&mut view.messages, original, &demotion.tool_call_id);
        }
        for (demotion, evidence) in kept {
            view.demotions.push(demotion);
            view.spill.push(evidence);
        }
    }
}

/// Copy the result of `tool_call_id` from `original` back into `messages`.
fn restore(messages: &mut [Message], original: &[Message], tool_call_id: &str) {
    let is_result = |m: &Message| matches!(&m.content, MessageContent::ToolResult { tool_call_id: id, .. } if id == tool_call_id);
    if let (Some(slot), Some(source)) = (
        messages.iter_mut().find(|m| is_result(m)),
        original.iter().find(|m| is_result(m)),
    ) {
        if let (
            MessageContent::ToolResult { content: slot, .. },
            MessageContent::ToolResult { content: src, .. },
        ) = (&mut slot.content, &source.content)
        {
            *slot = src.clone();
        }
    }
}

/// Estimated tokens of `messages` as a prompt.
fn tokens_of(messages: &[Message]) -> u32 {
    u32::try_from(sven_model::budget::estimate_request_tokens(
        messages,
        &[],
        None,
    ))
    .unwrap_or(u32::MAX)
}

/// Whether the tool results of `a` and `b` differ.
fn result_differs(a: &Message, b: &Message) -> bool {
    match (&a.content, &b.content) {
        (
            MessageContent::ToolResult { content: x, .. },
            MessageContent::ToolResult { content: y, .. },
        ) => x != y,
        _ => false,
    }
}
