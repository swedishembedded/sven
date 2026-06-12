// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Core data model for the sven workflow DSL.
//!
//! A [`Graph`] is a validated hierarchical state machine: nodes are states,
//! edges are transitions, the parent chain defines the state hierarchy, and
//! `loop`-kind nodes embed the shared model↔tool loop pattern.
//!
//! # `NodeId` and permissions
//!
//! [`NodeId`] wraps the node name as a `String`. Its [`Debug`] impl prints the
//! bare name (no quotes) so that `format!("{state:?}")` produces the same string
//! the node was declared with — which is what `PermissionPolicy::state_label`
//! uses to key per-state capability grants.

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sven_hsm::permissions::ToolCapability;

// ─── NodeId ──────────────────────────────────────────────────────────────────

/// Maximum byte length of a node name in the inline storage.
pub const NODE_NAME_MAX: usize = 47;

/// A state / node identifier — an **inline, Copy-capable** string (max
/// [`NODE_NAME_MAX`] bytes).
///
/// Using an inline byte array makes `NodeId` implement `Copy`, which is
/// required by the `Machine::State: Copy` bound in `sven-hsm`. The 47-byte
/// limit covers all practical node names (longest sven names are well under
/// 32 bytes).
///
/// `Debug` prints the bare name so `format!("{:?}", node_id)` equals the name
/// string — used by `PermissionPolicy::state_label` to key per-node capability
/// grants without the permission system ever knowing about `GraphMachine`.
///
/// # Panics
///
/// [`NodeId::new`] panics if the name exceeds [`NODE_NAME_MAX`] bytes or
/// contains a null byte (names must be valid UTF-8 identifiers).
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct NodeId {
    bytes: [u8; NODE_NAME_MAX],
    len: u8,
}

impl NodeId {
    /// Create a `NodeId` from any string-like value.
    ///
    /// # Panics
    ///
    /// Panics if `name` is longer than [`NODE_NAME_MAX`] bytes.
    pub fn new(name: impl AsRef<str>) -> Self {
        let s = name.as_ref();
        let bytes_src = s.as_bytes();
        assert!(
            bytes_src.len() <= NODE_NAME_MAX,
            "node name '{}' exceeds NODE_NAME_MAX ({}) bytes",
            s,
            NODE_NAME_MAX
        );
        let mut bytes = [0u8; NODE_NAME_MAX];
        bytes[..bytes_src.len()].copy_from_slice(bytes_src);
        NodeId {
            bytes,
            len: bytes_src.len() as u8,
        }
    }

    /// The node's declared name as a `&str`.
    #[must_use]
    pub fn name(&self) -> &str {
        // SAFETY: we only ever write valid UTF-8 into `bytes` via `new()`.
        std::str::from_utf8(&self.bytes[..self.len as usize])
            .expect("NodeId bytes must be valid UTF-8")
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Bare name — no quotes — so `format!("{:?}", id)` == id.name().
        write!(f, "{}", self.name())
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

impl Serialize for NodeId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.name())
    }
}

impl<'de> Deserialize<'de> for NodeId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        if s.len() > NODE_NAME_MAX {
            return Err(serde::de::Error::custom(format!(
                "node name '{}' exceeds NODE_NAME_MAX",
                s
            )));
        }
        Ok(NodeId::new(s))
    }
}

// ─── ToolsSpec ───────────────────────────────────────────────────────────────

/// Specifies which tools a loop node exposes to the LLM.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ToolsSpec {
    /// A fixed named list.
    Named(Vec<String>),
    /// All tools whose `all_tools_mode` matches this string (e.g. `"agent"`,
    /// `"readonly"`).
    AllMode(String),
}

impl Default for ToolsSpec {
    fn default() -> Self {
        ToolsSpec::AllMode("agent".into())
    }
}

// ─── SchemaRef ───────────────────────────────────────────────────────────────

/// Reference to a JSON response schema used by a loop node.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SchemaRef {
    /// Canonical name of this schema (passed as `schema_name` to `TurnRequest`).
    pub name: String,
    /// Inline JSON schema object.
    pub schema: Value,
}

// ─── LoopSpec ────────────────────────────────────────────────────────────────

/// Configuration for a `loop`-kind node's agentic model↔tool loop.
///
/// A loop node runs the shared `loop_core` machinery: on `LlmTurnComplete` with
/// tool calls it stays put (emitting `CallTool` effects); on a tool-free turn it
/// evaluates the node's authored `on final` / `on failed` exit edges.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoopSpec {
    /// Conversation thread id (e.g. `"chat"`, `"intake"`).
    pub thread: String,
    /// Tool access specification.
    pub tools: ToolsSpec,
    /// Maximum tool-call rounds before a forced wrap-up turn.
    pub max_rounds: u32,
    /// Optional response-format schema (makes the loop a structured-output node).
    pub schema: Option<SchemaRef>,
}

// ─── NodeKind ────────────────────────────────────────────────────────────────

/// The behavior kind of a node.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum NodeKind {
    /// A composite state with an `initial` substate. Its `dispatch_state`
    /// returns `Transition(initial)` on `Init`, `Handled(on_entry)` on `Entry`,
    /// and `Handled(on_exit)` on `Exit`. Ordinary events bubble to superstate if
    /// no edge matches.
    Composite,
    /// An ordinary leaf state. No `initial` required; ordinary events match
    /// edges then bubble.
    Leaf,
    /// A leaf state that hosts the shared agentic loop (see [`LoopSpec`]).
    /// Cannot have `initial`. Tool events are handled by `loop_core` automatically;
    /// the authored edges are the loop's exit conditions.
    Loop,
    /// A leaf state whose `dispatch_state` delegates to a registered
    /// [`crate::native::NativeFn`] for logic that cannot be expressed declaratively.
    Native {
        /// The registered function name.
        fn_name: String,
    },
    /// A terminal state — `Machine::is_terminal` returns `true` here, causing
    /// the runtime to signal completion.
    Terminal,
}

// ─── EventPattern ────────────────────────────────────────────────────────────

/// Trigger pattern for an edge.
///
/// Evaluated by [`crate::guard::matches_pattern`] against the current [`Event`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EventPattern {
    /// Match any event (use sparingly — typically for catch-all fallback edges).
    Any,
    /// Match a specific [`EventKind`](sven_hsm::event::EventKind).
    Kind(sven_hsm::event::EventKind),
    /// Sugar for `LlmTurnComplete` events handled by a loop node's exit
    /// conditions. The loop runtime pre-classifies `LlmTurnComplete` into
    /// `FinalAnswer` / `CallTools` / `EmptyTurn` / `MaxRoundsReached`; this
    /// pattern fires only for `FinalAnswer` (after the loop runtime has already
    /// handled the other cases by staying put).
    Final,
    /// Sugar for `LlmFailed`.
    Failed,
    /// Sugar for `InternalEvent::SubmachineCompleted`.
    SubmachineCompleted,
    /// Match `InternalEvent::Custom { name }` with the given signal name.
    Custom(String),
}

// ─── GuardExpr ───────────────────────────────────────────────────────────────

/// A pure, non-Turing-complete guard expression.
///
/// Evaluated against a [`crate::guard::GuardCtx`] which holds read-only
/// references to context facts, the optional parsed decision, the event
/// (serialised to JSON), retry counters, and optional loop-state fields.
///
/// # Grammar (informally)
///
/// ```text
/// guard  := or
/// or     := and ("||" and)*
/// and    := cmp  ("&&" cmp)*
/// cmp    := value (op value)? | "!" cmp | "(" guard ")"
/// op     := "==" | "!=" | ">" | ">=" | "<" | "<="
/// value  := path | literal | "len(" path ")"
/// path   := root ("." key | "[" int "]")*
/// root   := "fact" | "decision" | "event" | "retry" | "loop"
/// literal:= string | number | bool | null
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum GuardExpr {
    /// A JSON literal.
    Lit(Value),
    /// A path into one of the [`GuardRoot`] namespaces.
    Path {
        root: GuardRoot,
        keys: Vec<PathKey>,
    },
    /// The length of a JSON array / object at the given path.
    Len(Box<GuardExpr>),
    /// Logical NOT.
    Not(Box<GuardExpr>),
    /// Logical AND (short-circuit).
    And(Vec<GuardExpr>),
    /// Logical OR (short-circuit).
    Or(Vec<GuardExpr>),
    /// Comparison.
    Cmp {
        op: CmpOp,
        lhs: Box<GuardExpr>,
        rhs: Box<GuardExpr>,
    },
}

/// Root namespace for guard path expressions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuardRoot {
    /// `ctx.facts[key]`.
    Fact,
    /// The parsed LLM decision object (only present in loop nodes).
    Decision,
    /// The triggering event (serialised to JSON).
    Event,
    /// `ctx.retry_counters[key]` — as a number.
    Retry,
    /// `LoopState` fields (from `ctx.facts["lc_state"]`).
    Loop,
}

/// A single path step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PathKey {
    /// An object field.
    Field(String),
    /// An array index.
    Index(usize),
}

/// Comparison operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CmpOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

// ─── EffectTmpl ──────────────────────────────────────────────────────────────

/// A declarative template for a kernel [`Effect`](sven_hsm::effect::Effect).
///
/// Templates are rendered to concrete `Effect`s (and optional context mutations)
/// by the `GraphMachine` in `sven-core`, which has access to `loop_core`.
/// Fields marked with `_tmpl` suffix contain `{{ path }}` interpolation strings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EffectTmpl {
    /// Emit a `CallLlm { kind:"turn" }` effect.
    ///
    /// The `prompt_tmpl` is rendered via template interpolation; the result
    /// becomes the `instruction` field of the `TurnRequest`.  When `tools` is
    /// `None` the loop node's own `LoopSpec` tools are used.
    CallLlm {
        thread: String,
        prompt_tmpl: String,
        tools: Option<ToolsSpec>,
        max_rounds: u32,
        schema: Option<SchemaRef>,
        /// Optional model override (e.g. `"fast"`, a specific model id).
        model: Option<String>,
    },
    /// Emit a `CallTool` effect.
    CallTool {
        name: String,
        capability: ToolCapability,
        /// JSON template for the tool arguments.
        args_tmpl: String,
    },
    /// Emit an `AskUser` effect.
    AskUser {
        prompt_tmpl: String,
    },
    /// Emit a `RequestHumanApproval` effect.
    Approve {
        capability: ToolCapability,
        description_tmpl: String,
    },
    /// Emit a `CreateCheckpoint` effect.
    Checkpoint {
        label: String,
    },
    /// Emit a `RollbackToCheckpoint` effect.
    Rollback {
        label: String,
    },
    /// Emit an `EmitInternal { name, payload }` effect.
    Emit {
        name: String,
        payload_tmpl: String,
    },
    /// Emit `InstantiateSubmachine` effects for each element in a JSON array.
    ///
    /// `path` is a guard-path string resolved to a JSON array in the current
    /// context/event; `graph_name` names the child graph; `descriptor_tmpl` is
    /// rendered once per element (with `$item` bound to the element).
    SpawnEach {
        /// Guard path expression that resolves to the JSON array to iterate.
        path: GuardExpr,
        /// Name of the child graph (looked up in the graph registry).
        graph_name: String,
        /// JSON template for the per-child descriptor.
        descriptor_tmpl: String,
    },
    /// Emit a single `InstantiateSubmachine` effect.
    Spawn {
        graph_name: String,
        descriptor_tmpl: String,
    },
    /// Schedule a timeout.
    Timer {
        timer_id_tmpl: String,
        duration: Duration,
    },
    /// Cancel a timer.
    CancelTimer {
        timer_id_tmpl: String,
    },
    /// Initialise loop bookkeeping in context (calls `loop_core::init_loop`).
    /// Produces no kernel `Effect` but mutates context; rendered by `GraphMachine`.
    StartLoop {
        thread: String,
        tools: ToolsSpec,
        max_rounds: u32,
        schema: Option<SchemaRef>,
    },
    /// Store a value in `ctx.facts`.
    /// Produces no kernel `Effect` but mutates context; rendered by `GraphMachine`.
    SetFact {
        key: String,
        value_tmpl: String,
    },
    /// Delegate to a registered [`crate::native::NativeFn`] for imperative logic.
    /// The named function may produce a `Vec<Effect>` but must not do I/O.
    Native {
        fn_name: String,
        params: Value,
    },
}

// ─── EdgeData ────────────────────────────────────────────────────────────────

/// A single outgoing transition from a node.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EdgeData {
    /// Event pattern that activates this edge.
    pub pattern: EventPattern,
    /// Optional boolean guard.  If absent the edge fires on pattern match alone.
    pub guard: Option<GuardExpr>,
    /// Destination node.  `None` = internal transition (stay in current state).
    pub target: Option<NodeId>,
    /// Effects emitted by the transition action.
    pub effects: Vec<EffectTmpl>,
    /// Human-readable rationale recorded in the audit trail.
    pub rationale: String,
}

// ─── NodeData ────────────────────────────────────────────────────────────────

/// Data associated with a single node in the graph.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeData {
    /// The node's unique identifier / state name.
    pub id: NodeId,
    /// The parent node.  For the root node, `parent == id` (fixpoint).
    pub parent: NodeId,
    /// Behavioral kind.
    pub kind: NodeKind,
    /// For `Composite` nodes: the default sub-state entered on `Init`.
    pub initial: Option<NodeId>,
    /// Effects emitted on state entry (no transitions allowed).
    pub on_entry: Vec<EffectTmpl>,
    /// Effects emitted on state exit (no transitions allowed).
    pub on_exit: Vec<EffectTmpl>,
    /// Loop configuration (only meaningful when `kind == NodeKind::Loop`).
    pub loop_spec: Option<LoopSpec>,
    /// Outgoing transitions, evaluated in declaration order.
    pub edges: Vec<EdgeData>,
    /// Capabilities this state permits (used to derive `PermissionPolicy`).
    pub caps: Vec<ToolCapability>,
}

// ─── Graph ───────────────────────────────────────────────────────────────────

/// A validated workflow graph.
///
/// Built via [`crate::compile::GraphBuilder`] which enforces:
/// - Exactly one root (fixpoint).
/// - Every node reaches root via `parent`.
/// - Every `Composite` has an `initial` descendant.
/// - `Loop` nodes are leaves (no `initial`).
/// - Any node with `decision.*` guards has a catch-all `on final` edge.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Graph {
    /// Graph name (used as mode key in `ModeRegistry`).
    pub name: String,
    /// The root node id (fixpoint: `node(root).parent == root`).
    pub root: NodeId,
    /// Node storage, keyed by node name.
    nodes: HashMap<String, NodeData>,
}

impl Graph {
    /// Build a graph from a pre-validated node map.
    /// Prefer [`crate::compile::GraphBuilder`] for programmatic construction.
    #[must_use]
    pub fn new(name: impl Into<String>, root: NodeId, nodes: HashMap<String, NodeData>) -> Self {
        Graph {
            name: name.into(),
            root,
            nodes,
        }
    }

    /// Look up node data by id.
    ///
    /// # Panics
    ///
    /// Panics if `id` does not correspond to any node in this graph.  This
    /// should be impossible for ids produced by this graph.
    #[must_use]
    pub fn node(&self, id: &NodeId) -> &NodeData {
        self.nodes
            .get(id.name())
            .unwrap_or_else(|| panic!("unknown node id: {id}"))
    }

    /// All node ids in the graph.
    #[must_use]
    pub fn all_node_ids(&self) -> Vec<NodeId> {
        self.nodes.values().map(|n| n.id).collect()
    }

    /// Number of nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// `true` if there are no nodes (only possible for an invalid graph, before
    /// the builder validates).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Iterate over all nodes.
    pub fn iter_nodes(&self) -> impl Iterator<Item = &NodeData> {
        self.nodes.values()
    }
}

// ─── GraphError ──────────────────────────────────────────────────────────────

/// Errors that can occur when operating on a [`Graph`].
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("unknown node id: {0}")]
    UnknownNode(NodeId),
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_debug_prints_bare_name() {
        let id = NodeId::new("Idle");
        assert_eq!(format!("{id:?}"), "Idle");
        assert_eq!(format!("{id}"), "Idle");
    }

    #[test]
    fn graph_lookup() {
        let root = NodeId::new("Top");
        let data = NodeData {
            id: root.clone(),
            parent: root.clone(),
            kind: NodeKind::Composite,
            initial: Some(NodeId::new("Idle")),
            on_entry: vec![],
            on_exit: vec![],
            loop_spec: None,
            edges: vec![],
            caps: vec![],
        };
        let mut nodes = HashMap::new();
        nodes.insert("Top".to_string(), data);
        let g = Graph::new("test", root.clone(), nodes);
        assert_eq!(g.node(&root).id, root);
        assert_eq!(g.len(), 1);
    }
}
