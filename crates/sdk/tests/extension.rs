// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the framework is extensible from outside the workspace.
//!
//! A framework whose capabilities can only be added by editing it is not a
//! framework. An application must be able to give its agents tools the kernel
//! has never heard of, and to run machines that are not in the mode registry,
//! without touching any crate in this repository.
//!
//! Swedish Embedded AB implements extensible agent runtimes for its clients. If
//! your team needs expertise in embedding a deterministic agent kernel into an
//! existing system then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::sync::Arc;

use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{
    tool::{ApprovalPolicy as ToolApprovalPolicy, Tool, ToolCall, ToolOutput},
    Engine,
};

/// A tool that exists only in this test - the kernel has never heard of it.
struct StockPrice;

#[async_trait::async_trait]
impl Tool for StockPrice {
    fn name(&self) -> &str {
        "stock_price"
    }
    fn kernel_capability(&self) -> sven_sdk::tool::ToolCapability {
        sven_sdk::tool::ToolCapability::ReadFile
    }
    fn description(&self) -> &str {
        "Look up the current price of a ticker."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "ticker": { "type": "string" } },
            "required": ["ticker"],
        })
    }
    fn default_policy(&self) -> ToolApprovalPolicy {
        ToolApprovalPolicy::Auto
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let ticker = call.args["ticker"].as_str().unwrap_or("???");
        ToolOutput::ok(&call.id, format!("{ticker} is trading at 42.00"))
    }
}

fn engine_with_tool(scripts: Vec<Vec<ResponseEvent>>) -> Engine {
    Engine::builder()
        .model_provider(Arc::new(ScriptedMockProvider::new(scripts)) as Arc<_>)
        .tool(Arc::new(StockPrice))
        .approvals(sven_sdk::ApprovalPolicy::AutoApprove)
        .build()
        .expect("an engine builds")
}

#[tokio::test]
async fn an_agent_is_offered_a_tool_the_kernel_has_never_heard_of() {
    let provider = Arc::new(ScriptedMockProvider::new(vec![vec![
        ResponseEvent::TextDelta("noted".into()),
        ResponseEvent::Done,
    ]]));
    let engine = Engine::builder()
        .model_provider(Arc::clone(&provider) as Arc<_>)
        .tool(Arc::new(StockPrice))
        .build()
        .expect("an engine builds");

    engine
        .agent("agent")
        .send("what is ACME trading at?")
        .await
        .expect("a reply");

    let seen = provider.last_request.lock().unwrap().clone().unwrap();
    assert!(
        seen.tools.iter().any(|t| t.name == "stock_price"),
        "the registered tool must be offered to the model: {:?}",
        seen.tools.iter().map(|t| &t.name).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn the_agent_can_actually_call_it_and_use_the_result() {
    let engine = engine_with_tool(vec![
        // Round 1: the model calls the tool.
        vec![
            ResponseEvent::ToolCall {
                index: 0,
                id: "tc-1".into(),
                name: "stock_price".into(),
                arguments: r#"{"ticker":"ACME"}"#.into(),
            },
            ResponseEvent::Done,
        ],
        // Round 2: it answers using what came back.
        vec![
            ResponseEvent::TextDelta("ACME is 42.00".into()),
            ResponseEvent::Done,
        ],
    ]);

    let mut agent = engine.agent("agent");
    let reply = agent
        .send("what is ACME trading at?")
        .await
        .expect("a reply")
        .reply;

    assert!(
        reply.contains("42.00"),
        "the agent must have run the tool and used its output: {reply}"
    );
    let rendered = format!("{:?}", agent.state().history());
    assert!(
        rendered.contains("42.00"),
        "and the tool result must be in the conversation: {rendered}"
    );
}

#[tokio::test]
async fn a_registered_tool_does_not_displace_the_built_in_ones() {
    let provider = Arc::new(ScriptedMockProvider::new(vec![vec![
        ResponseEvent::TextDelta("ok".into()),
        ResponseEvent::Done,
    ]]));
    let engine = Engine::builder()
        .model_provider(Arc::clone(&provider) as Arc<_>)
        .toolset(sven_sdk::Toolset::coding())
        .tool(Arc::new(StockPrice))
        .build()
        .expect("an engine builds");

    engine.agent("agent").send("hello").await.expect("a reply");

    let seen = provider.last_request.lock().unwrap().clone().unwrap();
    assert!(
        seen.tools.len() > 1,
        "adding a tool must extend the tool set, not replace it: {:?}",
        seen.tools.iter().map(|t| &t.name).collect::<Vec<_>>()
    );
}

// ── A state machine defined outside the workspace ────────────────────────────

use sven_sdk::machine::{Context, ErasedMachine, Event, Hsm, Machine, MachineId, Reaction};

/// States of [`EchoMachine`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum EchoState {
    Top,
    Idle,
    Answered,
}

/// A machine the kernel has never heard of: it answers without a model at all.
///
/// Deliberately model-free, so the test proves the kernel really is driving
/// *this* machine rather than falling back to a built-in one.
struct EchoMachine {
    id: MachineId,
}

impl Machine for EchoMachine {
    type State = EchoState;

    fn id(&self) -> MachineId {
        self.id
    }
    fn top(&self) -> EchoState {
        EchoState::Top
    }
    fn initial(&self) -> EchoState {
        EchoState::Idle
    }
    fn superstate(&self, _state: EchoState) -> EchoState {
        EchoState::Top
    }
    fn all_states(&self) -> Vec<EchoState> {
        vec![EchoState::Idle, EchoState::Answered]
    }
    fn dispatch_state(
        &mut self,
        state: EchoState,
        event: &Event,
        ctx: &mut Context,
    ) -> Reaction<EchoState> {
        match (state, event) {
            (EchoState::Idle | EchoState::Answered, Event::UserMessage { text }) => {
                ctx.set_fact("echo.said", serde_json::json!(text));
                Reaction::transition(EchoState::Answered, vec![], "echoed")
            }
            _ => Reaction::Ignored,
        }
    }
}

#[tokio::test]
async fn a_machine_defined_outside_the_workspace_can_be_registered_and_run() {
    let engine = Engine::builder()
        .model_provider(Arc::new(ScriptedMockProvider::always_text("unused")) as Arc<_>)
        .machine(
            "echo",
            Box::new(|| -> Box<dyn ErasedMachine> {
                Box::new(Hsm::new(EchoMachine {
                    id: MachineId::new(),
                }))
            }),
        )
        .build()
        .expect("an engine builds");

    assert!(
        engine.modes().iter().any(|m| m == "echo"),
        "a registered machine must show up as a runnable mode: {:?}",
        engine.modes()
    );

    let mut agent = engine.agent("echo");
    agent.send("hello there").await.expect("a step");

    let state = agent.suspend();
    let kernel = state.kernel().expect("the step was captured");
    assert_eq!(
        kernel.state, "Answered",
        "the kernel must have driven the custom machine's own transitions"
    );
    assert_eq!(
        kernel
            .context
            .facts
            .get("echo.said")
            .and_then(|v| v.as_str()),
        Some("hello there"),
        "including the facts it set"
    );
}

#[tokio::test]
async fn registering_a_machine_keeps_the_built_in_modes() {
    let engine = Engine::builder()
        .model_provider(Arc::new(ScriptedMockProvider::always_text("hi")) as Arc<_>)
        .machine(
            "echo",
            Box::new(|| -> Box<dyn ErasedMachine> {
                Box::new(Hsm::new(EchoMachine {
                    id: MachineId::new(),
                }))
            }),
        )
        .build()
        .expect("an engine builds");

    let modes = engine.modes();
    assert!(
        modes.iter().any(|m| m == "agent") && modes.iter().any(|m| m == "predict"),
        "adding a machine must extend the registry, not replace it: {modes:?}"
    );
}

#[tokio::test]
async fn a_custom_machine_agent_suspends_and_resumes() {
    let registry_engine = || {
        Engine::builder()
            .model_provider(Arc::new(ScriptedMockProvider::always_text("unused")) as Arc<_>)
            .machine(
                "echo",
                Box::new(|| -> Box<dyn ErasedMachine> {
                    Box::new(Hsm::new(EchoMachine {
                        id: MachineId::new(),
                    }))
                }),
            )
            .build()
            .expect("an engine builds")
    };

    let engine = registry_engine();
    let mut agent = engine.agent("echo");
    agent.send("first").await.expect("a step");
    let stored = serde_json::to_string(&agent.suspend()).expect("serializable");

    // A different engine, which only knows this machine because it was
    // registered there too - state files name a mode, not a machine.
    let other = registry_engine();
    let mut resumed = other
        .resume(serde_json::from_str(&stored).expect("deserializable"))
        .expect("resumable");
    resumed.send("second").await.expect("a step");

    let kernel = resumed.suspend().kernel().cloned().expect("captured");
    assert_eq!(
        kernel
            .context
            .facts
            .get("echo.said")
            .and_then(|v| v.as_str()),
        Some("second"),
        "a custom machine resumes and keeps going like any built-in one"
    );
}
