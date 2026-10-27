// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the cost of agent
// workloads explicit and predictable for its clients. If your team needs
// expertise in LLM cost engineering then you can procure our services by
// sending an email to info@swedishembedded.com.
//! Spec: in a running session `cost.cache_aware` decides whether the first
//! demotion of old tool results goes out and the comparison is in the request's
//! ledger: `off` records nothing, `advise` and `enforce` record what was weighed.
//! (That a demotion that does not pay is left undone is pinned where the
//! assembler is, in the executors' `cache_gate` tests; a session's first
//! request has no warm cache to lose.)

use std::sync::Arc;
use std::time::Duration;

use sven_bootstrap::{AssemblyMode, Config, RuntimeBuilder, RuntimeContext};
use sven_cost::CacheAware;
use sven_hsm::UiEvent;
use sven_model::{CompletionRequest, FunctionCall, Message, MessageContent, Role};
use sven_model_catalog::{CachePricing, Pricing};
use sven_model_mock::ScriptedMockProvider;
use sven_turn::ledger::estimate_request;
use sven_vocab::cost::{GateAction, GateReason, RebuildRecord};

fn call(id: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: MessageContent::ToolCall {
            tool_call_id: id.into(),
            function: FunctionCall {
                name: "shell".into(),
                arguments: format!(r#"{{"command":"step {id}"}}"#),
            },
        },
        reasoning: None,
    }
}

fn old_session() -> Vec<Message> {
    let mut m = vec![Message::user("Work through the steps.")];
    for r in 0..6 {
        let id = format!("c{r}");
        m.push(call(&id));
        let out = (1..=300)
            .map(|n| format!("{id} line {n} of the recorded output"))
            .collect::<Vec<_>>()
            .join("\n");
        m.push(Message::tool_result(&id, out));
    }
    m.push(Message::assistant("Six steps are done."));
    m
}

struct Sent {
    request: CompletionRequest,
    rebuilds: Vec<RebuildRecord>,
}

async fn send(mode: CacheAware, root: &std::path::Path) -> Sent {
    let mut config = Config::default();
    config.model.provider = "mock".into();
    config.model.name = "mock-model".into();
    config.context.assembly.mode = AssemblyMode::Demote;
    config.context.assembly.budget_tokens = 1_000;
    config.context.assembly.keep_recent_rounds = 1;
    config.cost.cache_aware = mode;
    config.cost.remaining_requests = Some(1);
    config.cost.prices.insert(
        "scripted-mock/scripted-mock-model".into(),
        Pricing {
            input_per_mtok: 3.0,
            output_per_mtok: 15.0,
            cache: Some(CachePricing {
                read_per_mtok: 0.3,
                write_per_mtok: 3.75,
                ttl_secs: 300,
            }),
        },
    );
    let provider = ScriptedMockProvider::always_text("done");
    let last_request = provider.last_request.clone();
    let ctx = RuntimeContext {
        project_root: Some(root.to_path_buf()),
        ..RuntimeContext::default()
    };
    let (runtime, handle, _channels, _mcp, _mcp_events) =
        RuntimeBuilder::new(Arc::new(config), "chat")
            .with_runtime_context(ctx)
            .with_model_provider(Box::new(provider))
            .with_initial_history(old_session())
            .build()
            .await
            .expect("the session builds");

    let mut events = handle.subscribe_observations();
    handle.send_user_message("Continue.".into()).await;
    let mut rebuilds = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Ok(UiEvent::TokenUsage {
                breakdown: Some(breakdown),
                ..
            })) => {
                rebuilds = breakdown.cost.map(|c| c.rebuilds).unwrap_or_default();
            }
            Ok(Ok(UiEvent::TurnComplete)) => break,
            Ok(Ok(_)) => {}
            other => panic!("the turn did not complete: {other:?}"),
        }
    }
    runtime.abort();
    let request = last_request.lock().unwrap().clone().expect("a request");
    Sent { request, rebuilds }
}

fn tokens(sent: &Sent) -> u32 {
    estimate_request(&sent.request).estimated_total
}

#[tokio::test]
async fn the_gate_is_wired_into_a_session_and_its_decision_is_in_the_requests_ledger() {
    let root = tempfile::tempdir().unwrap();
    let off = send(CacheAware::Off, root.path()).await;
    let enforced = send(CacheAware::Enforce, root.path()).await;
    let advised = send(CacheAware::Advise, root.path()).await;

    assert!(off.rebuilds.is_empty(), "off weighs nothing");
    assert!(tokens(&off) < 12_000, "off demotes: {}", tokens(&off));

    // Nothing was sent before, so the provider's cache holds nothing and a
    // demotion discards nothing: even on one remaining request it pays, and
    // the ledger says what was weighed.
    let record = enforced
        .rebuilds
        .first()
        .expect("the decision is in the ledger");
    assert_eq!(record.remaining_requests, 1);
    assert_eq!(record.warm_tokens, 0);
    assert!(record.tokens_saved > 0);
    assert_eq!(record.action, GateAction::Rebuilt);
    assert_eq!(record.reason, GateReason::BreaksEven);
    assert!(
        tokens(&enforced) < 12_000,
        "so enforce demoted: {}",
        tokens(&enforced)
    );

    assert_eq!(advised.rebuilds.len(), 1);
    assert_eq!(advised.rebuilds[0].reason, GateReason::Advisory);
}
