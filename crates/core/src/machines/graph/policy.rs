// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Derive a [`PermissionPolicy`] from a [`Graph`]'s per-node capability
//! annotations.
//!
//! Each node can declare `caps: [ToolCapability, ...]` in its data.  Those
//! capabilities are allowed **in that specific state** (i.e. when the machine's
//! active leaf is that node).  Because `PermissionPolicy` keys on
//! `format!("{state:?}")` and [`NodeId`]'s `Debug` impl prints the bare name,
//! the state label is exactly the node name.

use sven_graph::model::Graph;
use sven_hsm::permissions::PermissionPolicy;

/// Build a [`PermissionPolicy`] from the capability annotations in `graph`.
///
/// For each node that declares at least one capability, a per-state allow entry
/// is added.  Nodes with no `caps` annotations are not added (they receive no
/// explicit per-state grants, though global grants still apply).
#[must_use]
pub fn policy_from_graph(graph: &Graph) -> PermissionPolicy {
    let mut builder = PermissionPolicy::builder();
    for node in graph.iter_nodes() {
        if !node.caps.is_empty() {
            builder = builder.allow_in(node.id, node.caps.iter().copied());
        }
    }
    builder.build()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use sven_graph::{compile::GraphBuilder, model::NodeId};
    use sven_hsm::{
        context::Context,
        effect::Effect,
        ids::ToolCallId,
        permissions::{validate_effects_are_allowed, ToolCapability},
    };

    use super::*;

    fn read_file_effect() -> Effect {
        Effect::CallTool {
            call_id: ToolCallId::new(),
            name: "read".into(),
            capability: ToolCapability::ReadFile,
            args: Value::Null,
        }
    }

    fn write_file_effect() -> Effect {
        Effect::CallTool {
            call_id: ToolCallId::new(),
            name: "write".into(),
            capability: ToolCapability::WriteFile,
            args: Value::Null,
        }
    }

    #[test]
    fn derives_policy_from_caps() {
        let mut b = GraphBuilder::new("test");
        b.add_composite("Top", "Top", Some("Active"));
        b.add_leaf("Active", "Top");
        b.caps(vec![ToolCapability::ReadFile, ToolCapability::WriteFile]);
        let g = b.build().unwrap();
        let policy = policy_from_graph(&g);

        let ctx = Context::new();
        // Active state allows ReadFile and WriteFile.
        let active = NodeId::new("Active");
        assert!(validate_effects_are_allowed(&policy, &active, &[read_file_effect()], &ctx).is_ok());
        assert!(validate_effects_are_allowed(&policy, &active, &[write_file_effect()], &ctx).is_ok());
        // Top state does not allow ReadFile.
        let top = NodeId::new("Top");
        assert!(validate_effects_are_allowed(&policy, &top, &[read_file_effect()], &ctx).is_err());
    }

    #[test]
    fn node_without_caps_forbids_tools() {
        let mut b = GraphBuilder::new("test");
        b.add_composite("Top", "Top", Some("Idle"));
        b.add_leaf("Idle", "Top");
        let g = b.build().unwrap();
        let policy = policy_from_graph(&g);
        let ctx = Context::new();
        let idle = NodeId::new("Idle");
        assert!(validate_effects_are_allowed(&policy, &idle, &[read_file_effect()], &ctx).is_err());
    }
}
