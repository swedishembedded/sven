// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a human gate names the run and the call it belongs to, goes to the
//! handler of the agent that raised it, and is denied - with a reason the
//! model reads - when its handler lets go of it.
//!
//! A host that serves several sessions routes each approval by the ids the
//! gate carries; one that loses a gate must never leave a turn waiting.
//!
//! Swedish Embedded AB implements human-in-the-loop agent runtimes for its
//! clients. If your team needs expertise in approval workflows for agents
//! then you can procure our services by sending an email to
//! info@swedishembedded.com.

mod host_common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use host_common::{builder, deploys, result_of, says};
use sven_sdk::{ApprovalId, ApprovalPolicy, HumanGate, RunConclusion, RunId};

#[tokio::test]
async fn an_approval_gate_names_its_run_and_call() {
    let ran = Arc::new(AtomicUsize::new(0));
    let engine = builder(vec![deploys("d1"), deploys("d2"), says("done")], &ran)
        .approvals(ApprovalPolicy::Manual)
        .human_gates(|_| panic!("the agent has its own handler"))
        .build()
        .expect("an engine builds");

    let seen: Arc<Mutex<Vec<(RunId, ApprovalId, String)>>> = Arc::default();
    let log = Arc::clone(&seen);
    let mut agent = engine.agent("agent").with_human_gates(move |gate| {
        if let HumanGate::Approval {
            run_id,
            approval_id,
            call,
            reply_tx,
            ..
        } = gate
        {
            let call = call.expect("a tool approval names the call it gates");
            log.lock().unwrap().push((run_id, approval_id, call.name));
            let _ = reply_tx.send(true);
        }
    });
    let mut events = agent.events();

    let outcome = agent.send("ship it").await.expect("an outcome");
    assert_eq!(outcome.conclusion, RunConclusion::Success);

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "one gate per call");
    assert_eq!(seen[0].0, seen[1].0, "both calls belong to one run");
    assert_ne!(seen[0].1, seen[1].1, "each call has its own id");
    assert!(seen.iter().all(|(_, _, name)| name == "deploy"));
    let first_event = tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
        .await
        .expect("an event")
        .expect("the stream is open");
    assert_eq!(
        first_event.run_id, seen[0].0,
        "the stream names the same run"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_dropped_gate_denies_instead_of_hanging() {
    let ran = Arc::new(AtomicUsize::new(0));
    let engine = builder(vec![deploys("d1"), says("not shipped")], &ran)
        .approvals(ApprovalPolicy::Manual)
        // A handler that loses the gate: it neither approves nor refuses.
        .human_gates(drop)
        .build()
        .expect("an engine builds");
    let mut agent = engine.agent("agent");

    let outcome = tokio::time::timeout(std::time::Duration::from_secs(20), agent.send("ship it"))
        .await
        .expect("the run ends rather than waiting for a gate nobody holds")
        .expect("an outcome");

    assert_eq!(outcome.conclusion, RunConclusion::Success);
    assert_eq!(ran.load(Ordering::SeqCst), 0, "a lost gate never approves");
    let refusal = result_of(&agent, "d1");
    assert!(refusal.contains("handler_dropped"), "{refusal}");
}

#[tokio::test]
async fn each_agent_answers_its_own_gates() {
    let ran = Arc::new(AtomicUsize::new(0));
    let engine = builder(
        vec![deploys("d1"), says("a"), deploys("d2"), says("b")],
        &ran,
    )
    .approvals(ApprovalPolicy::Manual)
    .human_gates(|_| panic!("every agent has its own handler"))
    .build()
    .expect("an engine builds");
    let denied = |answer: bool, asked: &Arc<AtomicUsize>| {
        let asked = Arc::clone(asked);
        move |gate| {
            if let HumanGate::Approval { reply_tx, .. } = gate {
                asked.fetch_add(1, Ordering::SeqCst);
                let _ = reply_tx.send(answer);
            }
        }
    };
    let (asked_a, asked_b) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let mut a = engine
        .agent("agent")
        .with_human_gates(denied(true, &asked_a));
    a.send("ship it").await.expect("an outcome");
    let mut b = engine
        .agent("agent")
        .with_human_gates(denied(false, &asked_b));
    b.send("ship it").await.expect("an outcome");

    assert_eq!(
        (
            asked_a.load(Ordering::SeqCst),
            asked_b.load(Ordering::SeqCst)
        ),
        (1, 1)
    );
    assert_eq!(
        ran.load(Ordering::SeqCst),
        1,
        "only the approving agent's call ran"
    );
}
