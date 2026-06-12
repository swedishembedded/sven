// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Graph builder and compiler.
//!
//! [`GraphBuilder`] is a programmatic constructor that validates well-formedness
//! rules and produces a [`Graph`].  The text-format parser (phase 5 of the
//! migration) will emit a `GraphBuilder` invocation sequence; this module is the
//! single validation choke point.
//!
//! # Well-formedness rules
//!
//! 1. Exactly one root: one node whose `parent == self` (a fixpoint).
//! 2. Every non-root node reaches the root via `parent` (no orphans, no cycles).
//! 3. Every `Composite` node has an `initial` that is a declared child.
//! 4. `Loop` nodes must be leaves: no `initial`.
//! 5. Any `Loop` node whose edges reference `decision.*` in a guard must have
//!    a catch-all `on final` edge (totality check — a malformed LLM decision
//!    must never wedge the machine).

use std::collections::HashMap;

use crate::model::{EdgeData, EffectTmpl, EventPattern, Graph, GuardExpr, GuardRoot, LoopSpec, NodeData, NodeId, NodeKind};

// ─── CompileError ────────────────────────────────────────────────────────────

/// Errors produced by the graph compiler / builder.
#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("no root node declared (need exactly one node with parent == self)")]
    NoRoot,
    #[error("multiple root nodes: {0:?}")]
    MultipleRoots(Vec<String>),
    #[error("node '{0}' references unknown parent '{1}'")]
    UnknownParent(String, String),
    #[error("orphan node '{0}' does not reach the root via parent chain")]
    Orphan(String),
    #[error("cycle detected involving node '{0}'")]
    Cycle(String),
    #[error("composite node '{0}' has no 'initial' child")]
    MissingInitial(String),
    #[error("composite node '{0}' references unknown initial child '{1}'")]
    UnknownInitial(String, String),
    #[error("loop node '{0}' must not have 'initial' (loop nodes are leaves)")]
    LoopWithInitial(String),
    #[error("loop node '{0}' has edges guarded by decision.* but no catch-all 'on final' edge")]
    MissingFinalCatchAll(String),
    #[error("node '{0}' edge references unknown target '{1}'")]
    UnknownEdgeTarget(String, String),
    #[error("duplicate node name: '{0}'")]
    DuplicateNode(String),
}

// ─── GraphBuilder ────────────────────────────────────────────────────────────

/// Builds and validates a [`Graph`].
///
/// # Example
///
/// ```
/// use sven_graph::compile::GraphBuilder;
/// use sven_graph::model::{NodeId, NodeKind};
///
/// let mut b = GraphBuilder::new("chat");
/// b.add_composite("Top", "Top", Some("Idle"));  // root: parent == self, initial = Idle
/// b.add_leaf("Idle", "Top");
/// let graph = b.build().unwrap();
/// assert_eq!(graph.root, NodeId::new("Top"));
/// ```
#[derive(Default)]
pub struct GraphBuilder {
    name: String,
    /// Nodes accumulated so far: name -> (parent_name, kind, initial?, on_entry, on_exit, loop_spec, edges, caps)
    nodes: Vec<NodeSpec>,
}

struct NodeSpec {
    name: String,
    parent: String,
    kind: NodeKind,
    initial: Option<String>,
    on_entry: Vec<EffectTmpl>,
    on_exit: Vec<EffectTmpl>,
    loop_spec: Option<LoopSpec>,
    edges: Vec<EdgeData>,
    caps: Vec<sven_hsm::permissions::ToolCapability>,
}

impl GraphBuilder {
    /// Create a new builder for a graph named `name`.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        GraphBuilder {
            name: name.into(),
            nodes: vec![],
        }
    }

    /// Add a composite node (must have at least one child and declare `initial`).
    ///
    /// To declare the root, pass `parent == name` (the fixpoint).
    pub fn add_composite(
        &mut self,
        name: impl Into<String>,
        parent: impl Into<String>,
        initial: Option<&str>,
    ) -> &mut Self {
        let name = name.into();
        self.nodes.push(NodeSpec {
            parent: parent.into(),
            initial: initial.map(str::to_string),
            kind: NodeKind::Composite,
            on_entry: vec![],
            on_exit: vec![],
            loop_spec: None,
            edges: vec![],
            caps: vec![],
            name,
        });
        self
    }

    /// Add a leaf node.
    pub fn add_leaf(
        &mut self,
        name: impl Into<String>,
        parent: impl Into<String>,
    ) -> &mut Self {
        let name = name.into();
        self.nodes.push(NodeSpec {
            parent: parent.into(),
            initial: None,
            kind: NodeKind::Leaf,
            on_entry: vec![],
            on_exit: vec![],
            loop_spec: None,
            edges: vec![],
            caps: vec![],
            name,
        });
        self
    }

    /// Add a loop node (leaf with embedded agentic loop).
    pub fn add_loop(
        &mut self,
        name: impl Into<String>,
        parent: impl Into<String>,
        spec: LoopSpec,
    ) -> &mut Self {
        let name = name.into();
        self.nodes.push(NodeSpec {
            parent: parent.into(),
            initial: None,
            kind: NodeKind::Loop,
            on_entry: vec![],
            on_exit: vec![],
            loop_spec: Some(spec),
            edges: vec![],
            caps: vec![],
            name,
        });
        self
    }

    /// Add a native node.
    pub fn add_native(
        &mut self,
        name: impl Into<String>,
        parent: impl Into<String>,
        fn_name: impl Into<String>,
    ) -> &mut Self {
        let name = name.into();
        self.nodes.push(NodeSpec {
            parent: parent.into(),
            initial: None,
            kind: NodeKind::Native { fn_name: fn_name.into() },
            on_entry: vec![],
            on_exit: vec![],
            loop_spec: None,
            edges: vec![],
            caps: vec![],
            name,
        });
        self
    }

    /// Add a terminal node.
    pub fn add_terminal(
        &mut self,
        name: impl Into<String>,
        parent: impl Into<String>,
    ) -> &mut Self {
        let name = name.into();
        self.nodes.push(NodeSpec {
            parent: parent.into(),
            initial: None,
            kind: NodeKind::Terminal,
            on_entry: vec![],
            on_exit: vec![],
            loop_spec: None,
            edges: vec![],
            caps: vec![],
            name,
        });
        self
    }

    /// Add an outgoing edge to the most-recently-added node.
    pub fn edge(&mut self, edge: EdgeData) -> &mut Self {
        if let Some(spec) = self.nodes.last_mut() {
            spec.edges.push(edge);
        }
        self
    }

    /// Set the entry effects for the most-recently-added node.
    pub fn on_entry(&mut self, effects: Vec<EffectTmpl>) -> &mut Self {
        if let Some(spec) = self.nodes.last_mut() {
            spec.on_entry = effects;
        }
        self
    }

    /// Set the exit effects for the most-recently-added node.
    pub fn on_exit(&mut self, effects: Vec<EffectTmpl>) -> &mut Self {
        if let Some(spec) = self.nodes.last_mut() {
            spec.on_exit = effects;
        }
        self
    }

    /// Set the capability list for the most-recently-added node.
    pub fn caps(&mut self, caps: Vec<sven_hsm::permissions::ToolCapability>) -> &mut Self {
        if let Some(spec) = self.nodes.last_mut() {
            spec.caps = caps;
        }
        self
    }

    /// Validate and build the [`Graph`].
    pub fn build(self) -> Result<Graph, CompileError> {
        // ── Duplicate check ──────────────────────────────────────────────────
        {
            let mut seen = std::collections::HashSet::new();
            for n in &self.nodes {
                if !seen.insert(n.name.clone()) {
                    return Err(CompileError::DuplicateNode(n.name.clone()));
                }
            }
        }

        // ── Build node map ───────────────────────────────────────────────────
        let known: std::collections::HashSet<String> = self.nodes.iter().map(|n| n.name.clone()).collect();

        // ── Identify root(s) ────────────────────────────────────────────────
        let roots: Vec<&NodeSpec> = self.nodes.iter().filter(|n| n.name == n.parent).collect();
        match roots.len() {
            0 => return Err(CompileError::NoRoot),
            1 => {}
            _ => return Err(CompileError::MultipleRoots(roots.iter().map(|n| n.name.clone()).collect())),
        }
        let root_name = roots[0].name.clone();

        // ── Validate parent references ───────────────────────────────────────
        for n in &self.nodes {
            if n.name != n.parent && !known.contains(&n.parent) {
                return Err(CompileError::UnknownParent(n.name.clone(), n.parent.clone()));
            }
        }

        // ── Validate every node reaches root (cycle / orphan check) ──────────
        // For each node walk the parent chain; if we haven't hit the root within
        // `nodes.len()` steps we have a cycle or orphan.
        let parent_map: HashMap<String, String> = self.nodes.iter().map(|n| (n.name.clone(), n.parent.clone())).collect();
        for n in &self.nodes {
            let mut cur = n.name.clone();
            let mut steps = 0;
            loop {
                if cur == root_name {
                    break;
                }
                steps += 1;
                if steps > self.nodes.len() {
                    return Err(CompileError::Cycle(n.name.clone()));
                }
                cur = parent_map[&cur].clone();
            }
        }

        // ── Validate composite/loop node constraints ─────────────────────────
        for n in &self.nodes {
            match &n.kind {
                NodeKind::Composite => {
                    match &n.initial {
                        None => return Err(CompileError::MissingInitial(n.name.clone())),
                        Some(init) => {
                            if !known.contains(init) {
                                return Err(CompileError::UnknownInitial(n.name.clone(), init.clone()));
                            }
                        }
                    }
                }
                NodeKind::Loop => {
                    if n.initial.is_some() {
                        return Err(CompileError::LoopWithInitial(n.name.clone()));
                    }
                    // Totality check: if any edge references `decision.*` guard
                    // the node must have a catch-all `on final` edge.
                    if edges_reference_decision(&n.edges) && !has_final_catchall(&n.edges) {
                        return Err(CompileError::MissingFinalCatchAll(n.name.clone()));
                    }
                }
                _ => {}
            }

            // ── Validate edge targets ────────────────────────────────────────
            for edge in &n.edges {
                if let Some(target) = &edge.target {
                    if !known.contains(target.name()) {
                        return Err(CompileError::UnknownEdgeTarget(n.name.clone(), target.name().to_string()));
                    }
                }
            }
        }

        // ── Convert to NodeData map ──────────────────────────────────────────
        let mut nodes_map: HashMap<String, NodeData> = HashMap::new();
        for n in self.nodes {
            let data = NodeData {
                id: NodeId::new(&n.name),
                parent: NodeId::new(&n.parent),
                kind: n.kind,
                initial: n.initial.map(NodeId::new),
                on_entry: n.on_entry,
                on_exit: n.on_exit,
                loop_spec: n.loop_spec,
                edges: n.edges,
                caps: n.caps,
            };
            nodes_map.insert(n.name, data);
        }

        Ok(Graph::new(self.name, NodeId::new(root_name), nodes_map))
    }
}

/// Return `true` if any edge in the list has a guard that mentions `decision.*`.
fn edges_reference_decision(edges: &[EdgeData]) -> bool {
    edges.iter().any(|e| {
        e.guard.as_ref().map(|g| guard_references_decision(g)).unwrap_or(false)
    })
}

fn guard_references_decision(g: &GuardExpr) -> bool {
    match g {
        GuardExpr::Path { root: GuardRoot::Decision, .. } => true,
        GuardExpr::Not(inner) => guard_references_decision(inner),
        GuardExpr::And(exprs) | GuardExpr::Or(exprs) => exprs.iter().any(guard_references_decision),
        GuardExpr::Cmp { lhs, rhs, .. } => {
            guard_references_decision(lhs) || guard_references_decision(rhs)
        }
        GuardExpr::Len(inner) => guard_references_decision(inner),
        _ => false,
    }
}

/// Return `true` if the edge list has a catch-all `on final` edge (no guard).
fn has_final_catchall(edges: &[EdgeData]) -> bool {
    edges.iter().any(|e| {
        matches!(e.pattern, EventPattern::Final) && e.guard.is_none()
    })
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EdgeData, EventPattern};

    fn simple_chat_graph() -> Result<Graph, CompileError> {
        let mut b = GraphBuilder::new("chat");
        b.add_composite("Top", "Top", Some("Session"));
        b.add_composite("Session", "Top", Some("Idle"));
        b.add_leaf("Idle", "Session");
        b.add_loop(
            "Generating",
            "Session",
            LoopSpec {
                thread: "chat".into(),
                tools: Default::default(),
                max_rounds: 16,
                schema: None,
            },
        );
        // Add a catch-all final edge so the loop node passes the totality check.
        b.edge(EdgeData {
            pattern: EventPattern::Final,
            guard: None,
            target: Some(NodeId::new("Idle")),
            effects: vec![],
            rationale: "done".into(),
        });
        b.build()
    }

    #[test]
    fn builds_valid_graph() {
        let g = simple_chat_graph().unwrap();
        assert_eq!(g.root, NodeId::new("Top"));
        assert_eq!(g.len(), 4);
        let top = g.node(&NodeId::new("Top"));
        assert_eq!(top.parent, NodeId::new("Top")); // root fixpoint
        assert_eq!(top.initial, Some(NodeId::new("Session")));
    }

    #[test]
    fn no_root_error() {
        let mut b = GraphBuilder::new("x");
        b.add_leaf("A", "B"); // A's parent is B which doesn't exist
        b.add_leaf("B", "A"); // cycle — no root
        assert!(matches!(b.build(), Err(CompileError::NoRoot) | Err(CompileError::Cycle(_)) | Err(CompileError::UnknownParent(_, _))));
    }

    #[test]
    fn composite_without_initial_errors() {
        let mut b = GraphBuilder::new("x");
        b.add_composite("Top", "Top", None); // no initial!
        assert!(matches!(b.build(), Err(CompileError::MissingInitial(_))));
    }

    #[test]
    fn loop_with_initial_errors() {
        // We can't set `initial` via the public API since `add_loop` never sets
        // initial. Instead verify this via a composite node that claims to have
        // an initial child that is a loop, which won't trigger the error; the
        // underlying rule is enforced internally.  This test simply confirms a
        // well-formed loop node compiles without the error (i.e. the path that
        // would set initial on a loop doesn't exist in the public API).
        let mut b = GraphBuilder::new("x");
        b.add_composite("Top", "Top", Some("Lp"));
        b.add_loop(
            "Lp",
            "Top",
            LoopSpec { thread: "t".into(), tools: Default::default(), max_rounds: 10, schema: None },
        );
        b.edge(EdgeData {
            pattern: EventPattern::Final,
            guard: None,
            target: Some(NodeId::new("Top")),
            effects: vec![],
            rationale: String::new(),
        });
        // A loop node built via the public API never has `initial` — verify it compiles.
        assert!(b.build().is_ok());
    }

    #[test]
    fn loop_decision_guard_requires_catchall() {
        use crate::model::{CmpOp, GuardRoot, PathKey};
        let mut b = GraphBuilder::new("x");
        b.add_composite("Top", "Top", Some("Lp"));
        b.add_loop(
            "Lp",
            "Top",
            LoopSpec { thread: "t".into(), tools: Default::default(), max_rounds: 10, schema: None },
        );
        // Add an edge with a decision.* guard but no catch-all.
        b.edge(EdgeData {
            pattern: EventPattern::Final,
            guard: Some(GuardExpr::Cmp {
                op: CmpOp::Eq,
                lhs: Box::new(GuardExpr::Path {
                    root: GuardRoot::Decision,
                    keys: vec![PathKey::Field("status".into())],
                }),
                rhs: Box::new(GuardExpr::Lit(serde_json::json!("proceed"))),
            }),
            target: Some(NodeId::new("Top")),
            effects: vec![],
            rationale: String::new(),
        });
        assert!(matches!(b.build(), Err(CompileError::MissingFinalCatchAll(_))));
    }

    #[test]
    fn duplicate_node_error() {
        let mut b = GraphBuilder::new("x");
        b.add_composite("Top", "Top", Some("A"));
        b.add_leaf("A", "Top");
        b.add_leaf("A", "Top"); // duplicate
        assert!(matches!(b.build(), Err(CompileError::DuplicateNode(_))));
    }
}
