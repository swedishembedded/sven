// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in deferred tool
// disclosure for agents then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Spec: a call to a tool the model has not loaded is refused with an
//! instruction to load it, and is refused only after the permission gate has
//! judged it as the tool it names. Loading is never a way around the gate.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use sven_executors::ToolExecutor;
use sven_hsm::{
    Context, Effect, Event, Hsm, Machine, MachineId, PermissionPolicy, Reaction, ToolAuditOutcome,
    ToolCallId, ToolCapability,
};
use sven_kernel::Runtime;
use sven_tool_registry::{
    ApprovalPolicy, Disclosure, Tool, ToolCall, ToolOutput, ToolRegistry, DISCOVERY_TOOL,
};

/// Reports whether its body ever ran.
struct Probe(Arc<AtomicBool>);

#[async_trait]
impl Tool for Probe {
    fn name(&self) -> &str {
        "web_fetch"
    }
    fn description(&self) -> &str {
        "Fetch a page."
    }
    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::NetworkAccess
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        self.0.store(true, Ordering::SeqCst);
        ToolOutput::ok(&call.id, "fetched")
    }
}

/// A stand-in for the discovery tool: deferral needs one to be registered.
struct Discovery;

#[async_trait]
impl Tool for Discovery {
    fn name(&self) -> &str {
        DISCOVERY_TOOL
    }
    fn description(&self) -> &str {
        "discovery"
    }
    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        ToolOutput::ok(&call.id, "ok")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum St {
    Top,
    Idle,
    Done,
}

/// Calls `web_fetch` on the first user message and records how that ended.
struct Caller {
    id: MachineId,
    call: ToolCallId,
}

impl Machine for Caller {
    type State = St;
    fn id(&self) -> MachineId {
        self.id
    }
    fn top(&self) -> St {
        St::Top
    }
    fn initial(&self) -> St {
        St::Idle
    }
    fn superstate(&self, _: St) -> St {
        St::Top
    }
    fn is_terminal(&self, s: St) -> bool {
        s == St::Done
    }
    fn dispatch_state(&mut self, s: St, e: &Event, ctx: &mut Context) -> Reaction<St> {
        match (s, e) {
            (St::Idle, Event::UserMessage { .. }) => Reaction::Handled(vec![Effect::CallTool {
                call_id: self.call,
                name: "web_fetch".into(),
                capability: ToolCapability::NetworkAccess,
                args: json!({}),
            }]),
            (St::Idle, Event::ToolFailed { error, .. }) => {
                ctx.set_fact("outcome", format!("failed: {error}"));
                Reaction::Transition {
                    target: St::Done,
                    effects: vec![],
                    rationale: "the call failed".into(),
                }
            }
            (St::Idle, Event::ToolSucceeded { .. }) => {
                ctx.set_fact("outcome", "succeeded");
                Reaction::Transition {
                    target: St::Done,
                    effects: vec![],
                    rationale: "the call ran".into(),
                }
            }
            _ => Reaction::Ignored,
        }
    }
}

/// Runs one call to `web_fetch` through the kernel's gate and the tool
/// executor, with the call held back or not. Returns what the machine saw,
/// whether the tool's body ran, and the tool audit trail.
async fn call_web_fetch(
    allowed: &[ToolCapability],
    held_back: bool,
) -> (String, bool, Vec<sven_hsm::ToolAuditRecord>) {
    let ran = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Probe(Arc::clone(&ran)));
    registry.register(Discovery);
    registry.set_disclosure(Disclosure::Deferred);
    let registry = Arc::new(registry);

    let call = ToolCallId::new();
    if held_back {
        registry.hold_back(call);
    }
    let policy = PermissionPolicy::builder()
        .allow_globally(allowed.iter().copied())
        .build();
    let runtime = Runtime::spawn(
        Hsm::new(Caller {
            id: MachineId::new(),
            call,
        }),
        Context::new(),
        policy,
        ToolExecutor::unrestricted(Arc::clone(&registry)),
        16,
    );
    runtime.post(Event::user_message("go")).await;
    runtime.wait_done().await;
    let report = runtime.join().await.expect("the run ends");
    let outcome = report
        .ctx
        .fact("outcome")
        .and_then(Value::as_str)
        .unwrap_or("no outcome")
        .to_string();
    (outcome, ran.load(Ordering::SeqCst), report.ctx.tool_audit)
}

#[tokio::test]
async fn an_unloaded_tool_is_not_run_and_the_model_is_told_to_load_it() {
    let (outcome, ran, audit) = call_web_fetch(&[ToolCapability::NetworkAccess], true).await;

    assert!(!ran, "the tool body must not run");
    assert!(
        outcome.contains("not loaded")
            && outcome.contains("action=load")
            && outcome.contains("web_fetch"),
        "{outcome}"
    );
    assert!(
        audit
            .iter()
            .any(|r| r.name == "web_fetch" && r.outcome == ToolAuditOutcome::Started),
        "the gate judged the call before it was refused: {audit:?}"
    );
}

#[tokio::test]
async fn an_unloaded_call_the_policy_forbids_is_denied_by_the_gate_not_by_the_loader() {
    let (outcome, ran, audit) = call_web_fetch(&[ToolCapability::ReadFile], true).await;

    assert!(!ran);
    assert!(outcome.contains("permission denied"), "{outcome}");
    assert!(audit
        .iter()
        .any(|r| r.name == "web_fetch" && r.outcome == ToolAuditOutcome::Denied));
}

#[tokio::test]
async fn a_loaded_tool_runs_under_the_same_gate() {
    let (outcome, ran, _) = call_web_fetch(&[ToolCapability::NetworkAccess], false).await;
    assert_eq!(outcome, "succeeded");
    assert!(ran);

    let (outcome, ran, _) = call_web_fetch(&[ToolCapability::ReadFile], false).await;
    assert!(outcome.contains("permission denied"), "{outcome}");
    assert!(!ran, "loading never widens what the policy allows");
}
