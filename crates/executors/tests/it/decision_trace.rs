// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for traceable decision-model use in
// agents for its clients. If your team needs expertise in auditing what a
// decision model did to an agent's context then you can procure our services
// by sending an email to info@swedishembedded.com.
//! What decision points did since the last model request is charged to the
//! next request's ledger record and traced with it.

use std::sync::Arc;
use std::time::Duration;

use sven_decision::telemetry::{CallRecord, DecisionTelemetry};
use sven_executors::{CompositeExecutorBuilder, ThreadStore, TurnExecutor};
use sven_hsm::{
    dispatch::Hsm, submachine::ErasedMachine, Context, Event, PermissionPolicy, UiEvent,
};
use sven_kernel::ErasedRuntime;
use sven_machines::ReactiveAgentMachine;
use sven_vocab::DecisionTraceRecord;

struct Server;

#[async_trait::async_trait]
impl sven_model::ModelProvider for Server {
    fn name(&self) -> &str {
        "local-server"
    }
    fn model_name(&self) -> &str {
        "served"
    }
    async fn complete(
        &self,
        _req: sven_model::CompletionRequest,
    ) -> anyhow::Result<sven_model::ResponseStream> {
        let events: Vec<anyhow::Result<sven_model::ResponseEvent>> = vec![
            Ok(sven_model::ResponseEvent::TextDelta("pong".into())),
            Ok(sven_model::ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

/// The ledger record of every request of `turns` user turns.
async fn ledgers(
    telemetry: Option<Arc<DecisionTelemetry>>,
    turns: usize,
) -> Vec<sven_vocab::RequestBreakdown> {
    let mut turn = TurnExecutor::new(
        Arc::new(Server),
        None,
        Arc::new(sven_tool_registry::ToolRegistry::new()),
        Arc::new(std::sync::Mutex::new(ThreadStore::new())),
        Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        Arc::new(tokio::sync::Mutex::new(None)),
    );
    if let Some(telemetry) = telemetry {
        turn = turn.with_decision_telemetry(telemetry);
    }
    let rt = ErasedRuntime::spawn(
        Box::new(Hsm::new(ReactiveAgentMachine::new())) as Box<dyn ErasedMachine>,
        Context::new(),
        PermissionPolicy::builder().build(),
        CompositeExecutorBuilder::default().with_turn(turn).build(),
        64,
    );
    let mut rx = rt.subscribe_observations();
    let mut records = Vec::new();
    for n in 0..turns {
        rt.post(Event::user_message(format!("ping {n}"))).await;
        let bound = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            match tokio::time::timeout_at(bound, rx.recv()).await {
                Ok(Ok(UiEvent::TokenUsage {
                    breakdown: Some(b), ..
                })) => records.push(*b),
                Ok(Ok(UiEvent::TurnComplete)) => break,
                Ok(Ok(_)) => {}
                _ => panic!("turn {n} did not complete"),
            }
        }
    }
    rt.abort();
    records
}

#[tokio::test]
async fn decisions_made_before_a_request_are_charged_and_traced_on_it() {
    let telemetry = Arc::new(DecisionTelemetry::default());
    telemetry.record_call(&CallRecord {
        wall: Duration::from_millis(42),
        model_id: Some("laya".into()),
        batch_size: 4,
        cached: false,
    });
    telemetry.record_trace(DecisionTraceRecord {
        policy: "tool_routing".into(),
        inputs_digest: "ab".into(),
        verdict: "abstain".into(),
        abstain_reason: Some("near_tie".into()),
        ..DecisionTraceRecord::default()
    });

    let records = ledgers(Some(telemetry), 2).await;
    assert_eq!(records.len(), 2);
    let inference = records[0]
        .decision
        .as_ref()
        .expect("charged to the first request");
    assert_eq!(
        (
            inference.calls,
            inference.latency_ms,
            inference.model_id.as_deref(),
            inference.batch_size
        ),
        (1, Some(42), Some("laya"), Some(4))
    );
    assert_eq!(records[0].decision_trace.len(), 1);
    assert!(records[1].decision.is_none() && records[1].decision_trace.is_empty());
}

#[tokio::test]
async fn without_decisions_the_ledger_is_what_it_was() {
    let records = ledgers(None, 1).await;
    assert!(records[0].decision.is_none() && records[0].decision_trace.is_empty());
}
