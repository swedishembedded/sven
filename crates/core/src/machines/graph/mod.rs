// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The generic `GraphMachine` interpreter.
//!
//! [`GraphMachine`] is a [`Machine`] implementation whose behavior is entirely
//! driven by a compiled [`Graph`] rather than hard-coded Rust `match`
//! statements. It replaces `ReactiveAgentMachine`, `SdlcMachine`, and
//! `TaskMachine` once all three are ported to built-in graph files.
//!
//! # Dispatch algorithm
//!
//! ```text
//! dispatch_state(node, event, ctx):
//!   match event:
//!     Init  → Transition(node.initial)  if Composite, else Ignored
//!     Entry → Handled(render(node.on_entry, ctx))
//!     Exit  → Handled(render(node.on_exit, ctx))
//!     _ →
//!       if node.kind == Native:
//!         return call_native(node, event, ctx)
//!       if node has loop_spec:
//!         if let Some(r) = loop_core::handle_tool_event(ctx, make_turn, event): return r
//!         if event == LlmTurnComplete:
//!           classify via on_llm_turn_complete → handle loop cases automatically;
//!           FinalAnswer → evaluate node's authored "on final" edges
//!       scan node.edges in declared order:
//!         if matches_pattern(edge, event) && eval(guard, ctx): return edge reaction
//!       return Super(node.parent)   ← implicit hierarchical bubble
//! ```
//!
//! The `Reaction::Super` bubble is implicit: if no edge matches and no special
//! case fires, the interpreter returns `Super(parent)` — mirroring the
//! hand-written `_ => Reaction::Super(parent)` arms in old machines.

pub mod policy;
pub mod render;

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use sven_graph::{
    guard::{eval as eval_guard, matches_pattern, GuardCtx},
    model::{Graph, NodeId, NodeKind},
    native::{NativeArgs, NativeOutcome, NativeRegistry},
    template::TemplateCtx,
};
use sven_hsm::{
    context::Context,
    event::{Event, InternalEvent},
    ids::MachineId,
    machine::Machine,
    status::Reaction,
};

use crate::machines::loop_core::{
    handle_tool_event, init_loop, on_llm_turn_complete, GeneratingAction, LoopState,
    LOOP_STATE_KEY,
};

use self::render::render_effects;

// ─── GraphMachine ─────────────────────────────────────────────────────────────

/// A state machine driven entirely by a compiled [`Graph`].
///
/// Register it in [`crate::mode::ModeRegistry`] with any mode name; the default
/// built-in modes will point here once the migration is complete.
pub struct GraphMachine {
    id: MachineId,
    graph: Arc<Graph>,
    native: Arc<NativeRegistry>,
    constants: HashMap<String, String>,
}

impl GraphMachine {
    /// Create a `GraphMachine` from a validated graph and a native registry.
    #[must_use]
    pub fn new(
        graph: Arc<Graph>,
        native: Arc<NativeRegistry>,
        constants: HashMap<String, String>,
    ) -> Self {
        GraphMachine {
            id: MachineId::new(),
            graph,
            native,
            constants,
        }
    }

    /// Create with an empty native registry and no constants (useful for simple
    /// graphs without native nodes).
    #[must_use]
    pub fn simple(graph: Arc<Graph>) -> Self {
        Self::new(graph, Arc::new(NativeRegistry::new()), HashMap::new())
    }

    // ── Dispatch helpers ──────────────────────────────────────────────────────

    /// Evaluate the node's authored edges in declaration order.
    ///
    /// Returns `Some(reaction)` if an edge fires; `None` means no edge matched.
    ///
    /// We snapshot `ctx.facts`, `ctx.retry_counters`, and the loop-state fact
    /// upfront so that [`GuardCtx`] / [`TemplateCtx`] do **not** borrow `ctx`
    /// — which lets us pass `ctx` mutably to [`render_effects`] later.
    fn scan_edges(
        &self,
        state: &NodeId,
        event: &Event,
        event_json: &Value,
        decision: Option<&Value>,
        ctx: &mut Context,
    ) -> Option<Reaction<NodeId>> {
        let node = self.graph.node(state);

        // Snapshot immutable ctx data so render_effects can take &mut ctx.
        let facts = ctx.facts.clone();
        let retry = ctx.retry_counters.clone();
        let loop_state_val: Option<Value> = ctx.fact(LOOP_STATE_KEY).cloned();

        let guard_ctx = GuardCtx {
            facts: &facts,
            retry: &retry,
            decision,
            event: event_json,
            loop_state: loop_state_val.as_ref(),
        };

        for edge in &node.edges {
            // 1. Event pattern match.
            if !matches_pattern(&edge.pattern, event) {
                continue;
            }
            // 2. Guard evaluation (missing guard = unconditionally true).
            let guard_ok = edge
                .guard
                .as_ref()
                .map(|g| eval_guard(g, &guard_ctx).unwrap_or(false))
                .unwrap_or(true);
            if !guard_ok {
                continue;
            }

            // 3. Build effects via template rendering.
            //    ctx is not borrowed by guard_ctx/tmpl_ctx (snapshots), so &mut is safe.
            let tmpl_ctx = TemplateCtx {
                facts: &facts,
                decision,
                event: event_json,
                loop_state: loop_state_val.as_ref(),
                constants: &self.constants,
            };
            let effects = render_effects(&edge.effects, ctx, event, &tmpl_ctx, &self.native);

            // 4. Build the reaction.
            return Some(match &edge.target {
                None => Reaction::effects(effects),
                Some(target) => Reaction::transition(*target, effects, &*edge.rationale),
            });
        }
        None
    }

    /// Dispatch a non-lifecycle event to a loop node, running the loop runtime
    /// first and falling through to authored edges for `FinalAnswer`.
    fn dispatch_loop_event(
        &self,
        state: &NodeId,
        event: &Event,
        ctx: &mut Context,
    ) -> Reaction<NodeId> {
        let node = self.graph.node(state);
        let loop_spec = node.loop_spec.as_ref().expect("loop node must have loop_spec");

        // Build the `make_turn` closure for `handle_tool_event`.
        // thread/tools/max_rounds are already in the LoopState stored by init_loop;
        // we only need `schema` to select the right continuation-turn variant.
        let schema = loop_spec.schema.clone();

        let make_turn = move |ls: &LoopState| {
            if let Some(ref s) = schema {
                ls.continuation_turn_with_schema(s.schema.clone(), &s.name)
            } else {
                ls.continuation_turn()
            }
        };

        // Step 1: delegate tool events to loop_core.
        if let Some(reaction) = handle_tool_event(ctx, &make_turn, event) {
            return reaction;
        }

        // Step 2: classify LlmTurnComplete.
        if let Event::LlmTurnComplete { text: _, .. } = event {
            let action = on_llm_turn_complete(ctx, event);
            match action {
                GeneratingAction::CallTools { tool_effects, .. } => {
                    return Reaction::effects(tool_effects);
                }
                GeneratingAction::EmptyTurn { nudge_effect } => {
                    return Reaction::effects(vec![nudge_effect]);
                }
                GeneratingAction::MaxRoundsReached { wrapup_effect } => {
                    // If the node has an `on max_rounds` edge, evaluate it; else stay.
                    let event_json = serde_json::to_value(event).unwrap_or(Value::Null);
                    // Look for explicit MaxRoundsReached edges first.
                    if let Some(r) = self.scan_edges(state, event, &event_json, None, ctx) {
                        return r;
                    }
                    return Reaction::effects(vec![wrapup_effect]);
                }
                GeneratingAction::FinalAnswer { text: final_text, .. } => {
                    // Parse a decision if the loop spec has a schema.
                    let decision_val: Option<Value> = loop_spec.schema.as_ref().and_then(|_| {
                        crate::machines::sdlc::parse_sdlc_decision(&final_text)
                    });
                    let event_json = serde_json::to_value(event).unwrap_or(Value::Null);
                    // Evaluate authored `on final` / `on failed` edges.
                    if let Some(r) = self.scan_edges(state, event, &event_json, decision_val.as_ref(), ctx) {
                        return r;
                    }
                    // No matching edge — bubble to superstate.
                }
            }
        } else if let Event::LlmFailed { .. } = event {
            let event_json = serde_json::to_value(event).unwrap_or(Value::Null);
            if let Some(r) = self.scan_edges(state, event, &event_json, None, ctx) {
                return r;
            }
        }

        // Fallthrough: bubble (root returns Ignored to terminate the Super chain).
        let parent = self.graph.node(state).parent;
        if parent == *state { Reaction::Ignored } else { Reaction::Super(parent) }
    }

    /// Dispatch to a native node's registered function.
    fn dispatch_native(
        &self,
        state: &NodeId,
        fn_name: &str,
        event: &Event,
        ctx: &mut Context,
    ) -> Reaction<NodeId> {
        let args = NativeArgs::default(); // static params from the node's `params` field
        match self.native.call(fn_name, ctx, event, &args) {
            Ok(outcome) => match outcome {
                NativeOutcome::Handled(effects) => Reaction::effects(effects),
                NativeOutcome::Goto { target, effects, rationale } => {
                    Reaction::transition(NodeId::new(target), effects, rationale)
                }
                NativeOutcome::Bubble => {
                    let parent = self.graph.node(state).parent;
                    if parent == *state { Reaction::Ignored } else { Reaction::Super(parent) }
                }
                NativeOutcome::Ignore => Reaction::Ignored,
            },
            Err(e) => {
                // Log and bubble rather than panic.
                tracing::warn!("native fn '{fn_name}' not found: {e}");
                let parent = self.graph.node(state).parent;
                if parent == *state { Reaction::Ignored } else { Reaction::Super(parent) }
            }
        }
    }
}

// ─── Machine impl ─────────────────────────────────────────────────────────────

impl Machine for GraphMachine {
    type State = NodeId;

    fn id(&self) -> MachineId {
        self.id
    }

    fn top(&self) -> NodeId {
        self.graph.root
    }

    fn initial(&self) -> NodeId {
        self.graph
            .node(&self.graph.root)
            .initial
            .expect("root node must have an initial child")
    }

    fn superstate(&self, state: NodeId) -> NodeId {
        self.graph.node(&state).parent
    }

    fn is_terminal(&self, state: NodeId) -> bool {
        matches!(self.graph.node(&state).kind, NodeKind::Terminal)
    }

    fn all_states(&self) -> Vec<NodeId> {
        self.graph.all_node_ids()
    }

    fn dispatch_state(
        &mut self,
        state: Self::State,
        event: &Event,
        ctx: &mut Context,
    ) -> Reaction<Self::State> {
        let node = self.graph.node(&state);

        // ── Lifecycle signals ─────────────────────────────────────────────────
        match event {
            Event::Internal(InternalEvent::Init) => {
                return match &node.initial {
                    Some(initial) => {
                        Reaction::transition(*initial, vec![], "init composite")
                    }
                    None => Reaction::Ignored,
                };
            }
            Event::Internal(InternalEvent::Entry) => {
                // Initialise loop state on entry if this is a loop node.
                if let (NodeKind::Loop, Some(spec)) = (&node.kind, &node.loop_spec) {
                    let tools = match &spec.tools {
                        sven_graph::model::ToolsSpec::Named(v) => v.clone(),
                        sven_graph::model::ToolsSpec::AllMode(_) => vec![],
                    };
                    let mode = match &spec.tools {
                        sven_graph::model::ToolsSpec::Named(_) => String::new(),
                        sven_graph::model::ToolsSpec::AllMode(m) => m.clone(),
                    };
                    init_loop(ctx, &spec.thread, &tools, &mode, spec.max_rounds);
                }
                let event_json = serde_json::to_value(event).unwrap_or(Value::Null);
                // Snapshot after init_loop so the loop state is included.
                let facts = ctx.facts.clone();
                let loop_state_val: Option<Value> = ctx.fact(LOOP_STATE_KEY).cloned();
                let tmpl_ctx = TemplateCtx {
                    facts: &facts,
                    decision: None,
                    event: &event_json,
                    loop_state: loop_state_val.as_ref(),
                    constants: &self.constants,
                };
                let effects = render_effects(&node.on_entry, ctx, event, &tmpl_ctx, &self.native);
                return Reaction::effects(effects);
            }
            Event::Internal(InternalEvent::Exit) => {
                let event_json = serde_json::to_value(event).unwrap_or(Value::Null);
                let facts = ctx.facts.clone();
                let loop_state_val: Option<Value> = ctx.fact(LOOP_STATE_KEY).cloned();
                let tmpl_ctx = TemplateCtx {
                    facts: &facts,
                    decision: None,
                    event: &event_json,
                    loop_state: loop_state_val.as_ref(),
                    constants: &self.constants,
                };
                let effects = render_effects(&node.on_exit, ctx, event, &tmpl_ctx, &self.native);
                return Reaction::effects(effects);
            }
            _ => {}
        }

        // ── Ordinary events ───────────────────────────────────────────────────
        match &node.kind {
            NodeKind::Native { fn_name } => {
                let fn_name = fn_name.clone();
                self.dispatch_native(&state, &fn_name, event, ctx)
            }
            NodeKind::Loop => self.dispatch_loop_event(&state, event, ctx),
            NodeKind::Composite | NodeKind::Leaf | NodeKind::Terminal => {
                let event_json = serde_json::to_value(event).unwrap_or(Value::Null);
                match self.scan_edges(&state, event, &event_json, None, ctx) {
                    Some(r) => r,
                    // Root node's parent is itself — return Ignored to terminate the Super chain.
                    None if node.parent == state => Reaction::Ignored,
                    None => Reaction::Super(node.parent),
                }
            }
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sven_graph::compile::GraphBuilder;
    use sven_graph::model::{EdgeData, EventPattern, NodeId};
    use sven_hsm::{context::Context, dispatch::Hsm, event::Event};

    use super::GraphMachine;

    /// Build the simplest 3-node graph: Top → Session → Idle.
    fn three_node_graph() -> sven_graph::model::Graph {
        let mut b = GraphBuilder::new("test");
        b.add_composite("Top", "Top", Some("Session"));
        b.add_composite("Session", "Top", Some("Idle"));
        b.add_leaf("Idle", "Session");
        // Idle: on UserMessage -> internal transition (stay)
        b.edge(EdgeData {
            pattern: EventPattern::Kind(sven_hsm::event::EventKind::UserMessage),
            guard: None,
            target: None, // internal transition
            effects: vec![],
            rationale: "got message".into(),
        });
        b.build().unwrap()
    }

    #[test]
    fn graph_machine_implements_machine_trait() {
        use sven_hsm::machine::Machine;
        let g = Arc::new(three_node_graph());
        let m = GraphMachine::simple(g);
        assert_eq!(m.top(), NodeId::new("Top"));
        assert_eq!(m.initial(), NodeId::new("Session"));
        assert_eq!(m.superstate(NodeId::new("Idle")), NodeId::new("Session"));
        assert_eq!(m.superstate(NodeId::new("Session")), NodeId::new("Top"));
        assert_eq!(m.superstate(NodeId::new("Top")), NodeId::new("Top")); // root fixpoint
    }

    #[test]
    fn hsm_drives_graph_machine_to_initial_state() {
        let g = Arc::new(three_node_graph());
        let m = GraphMachine::simple(g);
        let mut hsm = Hsm::new(m);
        let mut ctx = Context::new();
        hsm.init(&mut ctx);
        // After init, should be in Idle (the leaf of the hierarchy).
        assert_eq!(hsm.state(), NodeId::new("Idle"));
    }

    #[test]
    fn user_message_handled_in_idle() {
        let g = Arc::new(three_node_graph());
        let m = GraphMachine::simple(g);
        let mut hsm = Hsm::new(m);
        let mut ctx = Context::new();
        hsm.init(&mut ctx);
        let outcome = hsm.dispatch(&Event::user_message("hello"), &mut ctx);
        assert!(outcome.handled);
        // Internal transition: stays in Idle.
        // DispatchOutcome.to is the Debug label string of the target state.
        assert_eq!(outcome.to, "Idle");
    }

    #[test]
    fn unhandled_event_is_ignored_at_root() {
        let g = Arc::new(three_node_graph());
        let m = GraphMachine::simple(g);
        let mut hsm = Hsm::new(m);
        let mut ctx = Context::new();
        hsm.init(&mut ctx);
        let outcome = hsm.dispatch(&Event::UserCancelled, &mut ctx);
        // Not handled by any node — bubbles to root and is ignored.
        assert!(!outcome.handled);
    }
}
