// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the cost of agent
// workloads explicit and predictable for its clients. If your team needs
// expertise in LLM cost engineering then you can procure our services by
// sending an email to info@swedishembedded.com.
//! Spec: demoting a result changes the prompt from that result on, which
//! throws away the provider's cached prefix behind it. The assembler weighs
//! that against what the smaller prompt saves over the requests still to come,
//! and under `enforce` it leaves a discretionary demotion undone when it would
//! not pay. What was demoted in an earlier request stays demoted, so the
//! prefix the cache holds is never disturbed by the gate itself.

use std::sync::{Arc, Mutex};

use sven_chunks::{ChunkStore, FsProbe, SharedChunkStore, SourceProbe, StoreLimits};
use sven_cost::CacheAware;
use sven_executors::{CacheContext, ChunkSession, ContextAssembler};
use sven_model::{FunctionCall, Message, MessageContent, Role, ToolResultContent};
use sven_model_catalog::{CachePricing, Pricing};
use sven_turn::assembly::{ContextPolicy, PolicyConfig};
use sven_vocab::cost::{GateAction, GateReason};

fn assembler() -> (ContextAssembler, tempfile::TempDir, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let store_dir = tempfile::tempdir().unwrap();
    let store: SharedChunkStore = Arc::new(Mutex::new(
        ChunkStore::open(store_dir.path(), StoreLimits::default()).unwrap(),
    ));
    let probe: Arc<dyn SourceProbe + Send + Sync> = Arc::new(FsProbe::new(root.path()).unwrap());
    let session = Arc::new(ChunkSession::new(store, probe));
    let assembler = ContextAssembler::new(
        ContextPolicy::new(PolicyConfig {
            budget_tokens: 1000,
            keep_recent_rounds: 1,
            min_demote_tokens: 16,
        }),
        session,
    );
    (assembler, root, store_dir)
}

fn history(rounds: usize) -> Vec<Message> {
    let mut m = vec![Message::user("work")];
    for r in 0..rounds {
        let id = format!("c{r}");
        m.push(Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: id.clone(),
                function: FunctionCall {
                    name: "shell".into(),
                    arguments: format!(r#"{{"command":"step {r}"}}"#),
                },
            },
            reasoning: None,
        });
        let out = (1..=200)
            .map(|n| format!("r{r} line {n} of the output"))
            .collect::<Vec<_>>()
            .join("\n");
        m.push(Message::tool_result(&id, out));
    }
    m
}

fn result_text(m: &Message) -> &str {
    match &m.content {
        MessageContent::ToolResult {
            content: ToolResultContent::Text(t),
            ..
        } => t,
        _ => "",
    }
}

/// How many tool results are shown smaller than they are.
fn demoted(messages: &[Message]) -> usize {
    messages
        .iter()
        .filter(|m| matches!(m.content, MessageContent::ToolResult { .. }))
        .filter(|m| result_text(m).len() < 2_000)
        .count()
}

fn pricing() -> Pricing {
    Pricing {
        input_per_mtok: 3.0,
        output_per_mtok: 15.0,
        cache: Some(CachePricing {
            read_per_mtok: 0.3,
            write_per_mtok: 3.75,
            ttl_secs: Some(300),
        }),
    }
}

fn cache(mode: CacheAware, remaining: u32) -> CacheContext {
    CacheContext {
        mode,
        pricing: Some(pricing()),
        // Everything sent so far is in the provider's cache.
        warm_tokens: 1_000_000,
        requests_so_far: 3,
        declared_remaining: Some(remaining),
        input_budget: None,
    }
}

#[test]
fn a_demotion_that_would_not_pay_is_left_undone_under_enforce_and_recorded() {
    let (mut assembler, _r, _s) = assembler();
    let messages = history(5);
    let out = assembler.assemble_with(messages.clone(), &cache(CacheAware::Enforce, 1));
    assert_eq!(
        demoted(&out.messages),
        0,
        "kept whole: one request left to benefit"
    );
    assert_eq!(out.view.demoted, 0);
    let record = out.rebuilds.first().expect("the decision is recorded");
    assert_eq!(record.action, GateAction::Skipped);
    assert_eq!(record.reason, GateReason::DoesNotBreakEven);
    assert!(record.tokens_saved > 0 && record.warm_tokens > 0);
}

#[test]
fn a_demotion_that_pays_over_the_requests_to_come_goes_ahead() {
    let (mut assembler, _r, _s) = assembler();
    let out = assembler.assemble_with(history(5), &cache(CacheAware::Enforce, 400));
    assert!(demoted(&out.messages) > 0);
    let record = out.rebuilds.first().expect("recorded");
    assert_eq!(record.action, GateAction::Rebuilt);
    assert_eq!(record.reason, GateReason::BreaksEven);
}

#[test]
fn advise_records_and_off_does_not_weigh_and_neither_holds_a_demotion_back() {
    for (mode, reason, recorded) in [
        (CacheAware::Advise, GateReason::Advisory, true),
        (CacheAware::Off, GateReason::NotEvaluated, false),
    ] {
        let (mut assembler, _r, _s) = assembler();
        let out = assembler.assemble_with(history(5), &cache(mode, 1));
        assert!(demoted(&out.messages) > 0, "{mode:?}");
        assert_eq!(out.rebuilds.len(), usize::from(recorded), "{mode:?}");
        if let Some(record) = out.rebuilds.first() {
            assert_eq!(record.reason, reason);
        }
    }
}

#[test]
fn a_demotion_the_window_needs_is_never_skipped_for_cost() {
    let (mut assembler, _r, _s) = assembler();
    let mut context = cache(CacheAware::Enforce, 1);
    context.input_budget = Some(500);
    let out = assembler.assemble_with(history(5), &context);
    assert!(demoted(&out.messages) > 0);
    assert_eq!(out.rebuilds[0].reason, GateReason::RequiredToFit);
}

#[test]
fn what_an_earlier_request_demoted_stays_demoted_when_a_later_demotion_is_skipped() {
    let (mut assembler, _r, _s) = assembler();
    let first = assembler.assemble_with(history(4), &cache(CacheAware::Enforce, 400));
    let sent = demoted(&first.messages);
    assert!(sent > 0);
    assembler.mark_sent(&first.view);

    // One round later, one more result is old enough to demote; one request
    // remains, so it would not pay.
    let second = assembler.assemble_with(history(5), &cache(CacheAware::Enforce, 1));
    assert_eq!(
        demoted(&second.messages),
        sent,
        "no new demotion, no old one undone"
    );
    assert_eq!(second.rebuilds[0].action, GateAction::Skipped);
}

#[test]
fn an_unknown_price_never_holds_a_demotion_back() {
    let (mut assembler, _r, _s) = assembler();
    let mut context = cache(CacheAware::Enforce, 1);
    context.pricing = None;
    let out = assembler.assemble_with(history(5), &context);
    assert!(demoted(&out.messages) > 0);
    assert_eq!(out.rebuilds[0].reason, GateReason::PriceUnknown);
}
