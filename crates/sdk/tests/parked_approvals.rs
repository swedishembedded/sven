// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: an approval can be parked and decided later, also by another
//! process.
//!
//! A host that cannot answer on the spot - the person is away, the process is
//! restarting - parks approvals: the run ends `Waiting` with the approval, the
//! agent suspends into storage, and `decide` continues it after a resume. The
//! approval is for the one call it names.
//!
//! Swedish Embedded AB implements human-in-the-loop agent runtimes for its
//! clients. If your team needs expertise in long-running approval workflows
//! for agents then you can procure our services by sending an email to
//! info@swedishembedded.com.

mod host_common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use host_common::{builder, deploys, result_of, says};
use sven_model::ResponseEvent;
use sven_sdk::{
    Agent, AgentState, ApprovalPolicy, CallError, Decision, Engine, RunConclusion, RunOutcome,
};

fn parking_engine(scripts: Vec<Vec<ResponseEvent>>, ran: &Arc<AtomicUsize>) -> Engine {
    builder(scripts, ran)
        .approvals(ApprovalPolicy::Manual)
        .park_approvals()
        .build()
        .expect("parking needs no handler")
}

/// Runs the agent until it parks on `deploy`, then restarts it elsewhere: the
/// state goes through JSON and comes back on a fresh engine.
async fn parked_and_restarted(
    after: Vec<Vec<ResponseEvent>>,
    ran: &Arc<AtomicUsize>,
) -> (RunOutcome, Agent) {
    let first = Arc::new(AtomicUsize::new(0));
    let mut agent = parking_engine(vec![deploys("d1")], &first).agent("agent");
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(20), agent.send("ship it"))
        .await
        .expect("a parked run returns at once")
        .expect("an outcome");
    assert_eq!(first.load(Ordering::SeqCst), 0, "nothing ran while parked");
    let stored = serde_json::to_string(&agent.suspend()).expect("the state serializes");
    let state: AgentState = serde_json::from_str(&stored).expect("and reads back");
    let resumed = parking_engine(after, ran).resume(state).expect("resumes");
    (outcome, resumed)
}

#[tokio::test]
async fn a_parked_approval_survives_resume_and_is_decided() {
    let ran = Arc::new(AtomicUsize::new(0));
    let (outcome, mut agent) = parked_and_restarted(vec![says("shipped")], &ran).await;

    assert_eq!(outcome.conclusion, RunConclusion::Waiting);
    let approval = outcome.approval.expect("the run reports what it waits for");
    assert_eq!(
        approval.call.as_ref().map(|c| c.name.as_str()),
        Some("deploy")
    );
    assert!(outcome.question.is_none());
    assert_eq!(
        agent.pending_approval().map(|a| (a.id, a.run_id)),
        Some((approval.id, approval.run_id)),
        "the resumed agent knows what it waits for"
    );

    let decided = agent
        .decide(approval.id, Decision::Approve)
        .await
        .expect("the decided run continues");
    assert_eq!(decided.conclusion, RunConclusion::Success);
    assert_eq!(decided.reply, "shipped");
    assert_eq!(ran.load(Ordering::SeqCst), 1, "the approved call ran once");
    assert!(agent.pending_approval().is_none());
    assert!(result_of(&agent, "d1").contains("deployed"));
}

#[tokio::test]
async fn a_denied_parked_approval_never_runs_and_tells_the_model_why() {
    let ran = Arc::new(AtomicUsize::new(0));
    let (outcome, mut agent) = parked_and_restarted(vec![says("held back")], &ran).await;
    let approval = outcome.approval.expect("parked");

    let decided = agent
        .decide(
            approval.id,
            Decision::Deny {
                reason: "change freeze".into(),
            },
        )
        .await
        .expect("the denied run continues");

    assert_eq!(decided.conclusion, RunConclusion::Success);
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    assert!(result_of(&agent, "d1").contains("change freeze"));
}

#[tokio::test]
async fn only_the_parked_approval_can_be_decided() {
    let ran = Arc::new(AtomicUsize::new(0));
    let (outcome, mut agent) = parked_and_restarted(vec![says("shipped")], &ran).await;
    let approval = outcome.approval.expect("parked");

    let stranger = sven_sdk::ApprovalId::new();
    let refused = agent.decide(stranger, Decision::Approve).await;
    assert!(
        matches!(refused, Err(CallError::Precondition(_))),
        "{refused:?}"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 0);

    agent
        .decide(approval.id, Decision::Approve)
        .await
        .expect("decided");
    let twice = agent.decide(approval.id, Decision::Approve).await;
    assert!(
        matches!(twice, Err(CallError::Precondition(_))),
        "decided once"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 1);
}
