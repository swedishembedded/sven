// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! One-way graphviz DOT renderer.
//!
//! Converts a validated [`Graph`] to a graphviz `digraph` string for
//! **visualization only**.  The rendering is intentionally lossy:
//!
//! * `Composite` nodes become `subgraph cluster_` blocks.
//! * `Loop` nodes get a dotted self-loop edge to visualize the in-state cycle.
//! * `Native` nodes get a grey component shape and their registered function
//!   name in the label.
//! * `Terminal` nodes use the `Msquare` double-circle shape.
//! * `initial` transitions render as dashed edges from a synthetic `__start_X`
//!   node.
//!
//! The dot output can be piped to `dot -Tsvg` for SVG rendering.

use std::collections::HashSet;
use std::fmt::Write as FmtWrite;

use crate::model::{EdgeData, EventPattern, Graph, NodeData, NodeId, NodeKind};

/// Render `graph` to a graphviz `digraph` string.
#[must_use]
pub fn render_dot(graph: &Graph) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "digraph {} {{", dot_id(&graph.name));
    let _ = writeln!(out, "  compound=true;");
    let _ = writeln!(out, "  rankdir=TB;");
    let _ = writeln!(out, "  node [fontname=\"Helvetica\"];");
    let _ = writeln!(out, "  edge [fontname=\"Helvetica\", fontsize=10];");
    let _ = writeln!(out);

    // Emit nodes/clusters.
    let mut emitted: HashSet<String> = HashSet::new();
    emit_node(graph, &graph.root.clone(), &mut out, 1, &mut emitted);

    // Emit all edges.
    let _ = writeln!(out);
    for node in graph.iter_nodes() {
        for edge in &node.edges {
            emit_edge(graph, node, edge, &mut out);
        }
        // Loop nodes: emit a synthetic self-loop to visualize the in-state cycle.
        if matches!(node.kind, NodeKind::Loop) {
            let _ = writeln!(
                out,
                "  {} -> {} [label=\"tool loop\", style=dotted, color=grey];",
                dot_node_id(&node.id),
                dot_node_id(&node.id)
            );
        }
    }

    let _ = writeln!(out, "}}");
    out
}

fn emit_node(
    graph: &Graph,
    id: &NodeId,
    out: &mut String,
    depth: usize,
    emitted: &mut HashSet<String>,
) {
    if emitted.contains(id.name()) {
        return;
    }
    emitted.insert(id.name().to_string());

    let node = graph.node(id);
    let indent = "  ".repeat(depth);

    match &node.kind {
        NodeKind::Composite => {
            let _ = writeln!(out, "{indent}subgraph cluster_{} {{", dot_id(id.name()));
            let _ = writeln!(
                out,
                "{indent}  label=\"{}\"; style=rounded; color=\"#999999\";",
                escape_label(id.name())
            );

            // Emit initial pseudo-node and dashed edge.
            if let Some(initial) = &node.initial {
                let start_id = format!("__start_{}", dot_id(id.name()));
                let _ = writeln!(
                    out,
                    "{indent}  {start_id} [shape=point, width=0.2, style=invis];"
                );
                // Children first.
                let children: Vec<NodeId> = graph
                    .iter_nodes()
                    .filter(|n| &n.parent == id && n.id != *id)
                    .map(|n| n.id)
                    .collect();
                for child in children {
                    emit_node(graph, &child, out, depth + 1, emitted);
                }
                let _ = writeln!(
                    out,
                    "{indent}  {start_id} -> {} [style=dashed, arrowhead=open];",
                    dot_node_id(initial)
                );
            }
            let _ = writeln!(out, "{indent}}}");
        }
        NodeKind::Loop => {
            let label = if let Some(spec) = &node.loop_spec {
                format!(
                    "{}\\n«loop: {}»",
                    escape_label(id.name()),
                    escape_label(&spec.thread)
                )
            } else {
                format!("{}\\n«loop»", escape_label(id.name()))
            };
            let _ = writeln!(
                out,
                "{indent}{} [label=\"{label}\", shape=box, style=\"rounded,filled\", fillcolor=lightblue];",
                dot_node_id(id)
            );
        }
        NodeKind::Native { fn_name } => {
            let label = format!(
                "{}\\n«native: {}»",
                escape_label(id.name()),
                escape_label(fn_name)
            );
            let _ = writeln!(
                out,
                "{indent}{} [label=\"{label}\", shape=component, style=filled, fillcolor=lightgrey];",
                dot_node_id(id)
            );
        }
        NodeKind::Terminal => {
            let _ = writeln!(
                out,
                "{indent}{} [label=\"{}\", shape=Msquare];",
                dot_node_id(id),
                escape_label(id.name())
            );
        }
        NodeKind::Leaf => {
            let _ = writeln!(
                out,
                "{indent}{} [label=\"{}\", shape=box];",
                dot_node_id(id),
                escape_label(id.name())
            );
        }
    }
}

fn emit_edge(_graph: &Graph, node: &NodeData, edge: &EdgeData, out: &mut String) {
    // Don't emit edges for nodes inside clusters separately (already handled by
    // their cluster blocks). Only emit edges whose source is a non-composite leaf.
    if matches!(node.kind, NodeKind::Composite) {
        return;
    }

    let src = dot_node_id(&node.id);
    let dst = edge
        .target
        .as_ref()
        .map(dot_node_id)
        .unwrap_or_else(|| dot_node_id(&node.id)); // self-loop for internal transitions

    let label = format_edge_label(edge);
    let style = if edge.target.is_none() {
        ", style=dashed".to_string()
    } else {
        String::new()
    };

    let _ = writeln!(out, "  {src} -> {dst} [label=\"{label}\"{style}];");
}

fn format_edge_label(edge: &EdgeData) -> String {
    let mut label = format_pattern(&edge.pattern);
    if edge.guard.is_some() {
        label.push_str(" [guard]");
    }
    if !edge.rationale.is_empty() {
        label.push_str(&format!("\\n{}", escape_label(&edge.rationale)));
    }
    label
}

fn format_pattern(p: &EventPattern) -> String {
    match p {
        EventPattern::Any => "*".into(),
        EventPattern::Kind(k) => format!("{k:?}"),
        EventPattern::Final => "final".into(),
        EventPattern::Failed => "failed".into(),
        EventPattern::SubmachineCompleted => "SubmachineCompleted".into(),
        EventPattern::Custom(name) => format!("custom:{name}"),
    }
}

/// Produce a valid dot identifier from an arbitrary string.
fn dot_id(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

/// Produce a dot node id by prefixing with `n_` to avoid keyword clashes.
fn dot_node_id(id: &NodeId) -> String {
    format!("n_{}", dot_id(id.name()))
}

/// Escape a label string for use in dot `"..."` labels.
fn escape_label(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::GraphBuilder;
    use crate::model::{EdgeData, EventPattern, LoopSpec, NodeId};

    #[test]
    fn renders_simple_graph() {
        let mut b = GraphBuilder::new("test");
        b.add_composite("Top", "Top", Some("Idle"));
        b.add_leaf("Idle", "Top");
        let g = b.build().unwrap();
        let dot = render_dot(&g);
        assert!(dot.contains("digraph test"));
        assert!(dot.contains("cluster_Top"));
        assert!(dot.contains("n_Idle"));
    }

    #[test]
    fn loop_node_gets_self_loop() {
        let mut b = GraphBuilder::new("test");
        b.add_composite("Top", "Top", Some("Lp"));
        b.add_loop(
            "Lp",
            "Top",
            LoopSpec {
                thread: "chat".into(),
                tools: Default::default(),
                max_rounds: 16,
                schema: None,
            },
        );
        b.edge(EdgeData {
            pattern: EventPattern::Final,
            guard: None,
            target: Some(NodeId::new("Top")),
            effects: vec![],
            rationale: String::new(),
        });
        let g = b.build().unwrap();
        let dot = render_dot(&g);
        assert!(dot.contains("tool loop"));
        assert!(dot.contains("style=dotted"));
    }
}
