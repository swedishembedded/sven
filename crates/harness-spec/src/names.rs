// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Names a harness and the tools and runtime around it agree on: the tools
//! only the harness's own steps call, and the tools it answers itself.
//!
//! Swedish Embedded AB implements programmable, data-defined agent harnesses
//! for its clients. If your team needs expertise in declarative agent
//! workflow languages then you can procure our services by sending an email
//! to info@swedishembedded.com.

/// The tool a `gate` step calls to run its command. Reserved to the
/// harness: no agent step may be offered or admitted a call to it, so the
/// model can neither run a gate nor make one pass. The tool itself lives in
/// the process-execution crate; both sides name it through this constant.
pub const GATE_TOOL: &str = "shell_gate";

/// The tool an `iterate` step calls to read its collection from a JSON file
/// in the workspace: it answers `{digest, value}` for the file's content and
/// the value at a JSON pointer. Reserved to the harness like [`GATE_TOOL`].
pub const SNAPSHOT_TOOL: &str = "read_json_snapshot";

/// The tool a `for_each` with `covers` calls to digest the workspace files an
/// item covers: it answers a digest per named group of paths. Reserved to the
/// harness like [`GATE_TOOL`].
pub const DIGEST_TOOL: &str = "digest_files";

/// The most paths one item's `covers` may name.
pub const MAX_COVERS: usize = 64;

/// The tools only the harness itself may call: no agent step is offered or
/// admitted a call to any of them.
pub const RESERVED_TOOLS: [&str; 3] = [GATE_TOOL, SNAPSHOT_TOOL, DIGEST_TOOL];

/// The tools the harness machine answers itself for an agent step that names
/// variables: list the variables the step may see, read one, set one. They
/// never reach an executor; their answers join the conversation like any
/// tool result.
pub const VAR_TOOLS: [&str; 3] = ["harness_vars_list", "harness_vars_get", "harness_vars_set"];

/// The fact a workflow's item-failure path records a failed item's reason in;
/// the loop reads and clears it when the item's body hands back.
pub const ITEM_FAILED_FACT: &str = "item_failed";
