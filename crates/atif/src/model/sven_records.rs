// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements trajectory formats that keep what an agent
// decided and what it remembered auditable, for its clients. If your team needs
// expertise in agent audit trails you can procure our services by sending an
// email to info@swedishembedded.com.

//! The records sven files under `extra.sven` on an agent step.
//!
//! ATIF leaves `extra` to its producer. A producer that records something
//! other consumers must be able to rely on files it under a namespace; sven's
//! is `sven`, and this module defines the two records a reader can depend on:
//!
//! - `extra.sven.gate` ([`GateRecord`]): what the permission gate decided
//!   about each tool call the step made, who decided it, and any
//!   tool-argument violations a schema pre-check found.
//! - `extra.sven.memory` ([`MemoryRecord`]): what each memory retrieval the
//!   step made returned, and which results were put into the model's context.
//!
//! A step can make several calls, so each record is an envelope holding one
//! entry per call, and carries a `schema` string naming its version. A reader
//! refuses a version it does not know instead of guessing at its shape.
//! [`crate::validate_trajectory`] enforces both.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The `extra` key sven's records nest under.
pub const SVEN_EXTRA_KEY: &str = "sven";

/// Files `record` under `extra.sven.<name>`, creating `extra` and the `sven`
/// object when absent and leaving everything else in them as it was.
fn insert_record(extra: &mut Option<Value>, name: &str, record: Value) {
    let extra = extra.get_or_insert_with(|| Value::Object(Map::new()));
    if !extra.is_object() {
        *extra = Value::Object(Map::new());
    }
    let namespace = extra
        .as_object_mut()
        .expect("just ensured object")
        .entry(SVEN_EXTRA_KEY)
        .or_insert_with(|| Value::Object(Map::new()));
    if !namespace.is_object() {
        *namespace = Value::Object(Map::new());
    }
    namespace
        .as_object_mut()
        .expect("just ensured object")
        .insert(name.to_string(), record);
}

// ── extra.sven.gate ──────────────────────────────────────────────────────────

/// What the gate decided about a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateOutcome {
    /// The call was allowed to run.
    Approved,
    /// The call was refused and did not run.
    Denied,
}

/// Who made a gate decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateAuthority {
    /// The session approves every call on its own (an approval mode, or a
    /// rule that pre-approves this call).
    Auto,
    /// A person decided.
    Human,
    /// The party holding the approval gate went away without answering, and
    /// the call was refused for it.
    HandlerDropped,
    /// The permission policy decided with no one asked.
    Policy,
}

/// One way a tool call's arguments broke the tool's parameter schema, as a
/// deterministic JSON Schema check found it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgumentViolation {
    /// JSON Pointer to the offending value inside the arguments (empty for
    /// the arguments as a whole).
    pub path: String,
    /// The JSON Schema keyword that failed, such as `required` or `type`.
    pub rule: String,
    /// The validator's description of the failure.
    pub message: String,
}

/// The gate's decision about one tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateDecision {
    /// Identifies this decision. A decision is made once per call, and the id
    /// is the one the approval request carried, so a record and the request
    /// that produced it can be matched.
    pub decision_id: String,
    /// The `tool_call_id` of the call this decision is about; it names a call
    /// of the same step.
    pub call_id: String,
    /// The capability the call needed (a `ToolCapability` variant name).
    pub capability: String,
    /// The rule that routed the call: `state_allow_set` (the capability is
    /// not permitted in the current state), `requires_approval`, or `allowed`
    /// (the policy let it through unasked).
    pub policy: String,
    /// What was decided.
    #[serde(rename = "decision")]
    pub outcome: GateOutcome,
    /// Who decided it.
    pub decided_by: GateAuthority,
    /// Why, when whoever decided said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What a schema pre-check of the call's arguments found; empty when the
    /// arguments satisfied the schema.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violations: Vec<ArgumentViolation>,
}

/// `extra.sven.gate`: the gate decisions of the calls one step made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateRecord {
    /// Always [`GateRecord::SCHEMA`] for a record this crate writes.
    pub schema: String,
    /// One entry per gated call, in call order.
    pub decisions: Vec<GateDecision>,
}

impl GateRecord {
    /// The version string of the shape defined here.
    pub const SCHEMA: &'static str = "sven.gate.v1";
    /// The name this record is filed under inside `extra.sven`.
    pub const NAME: &'static str = "gate";

    /// A record of `decisions`.
    #[must_use]
    pub fn new(decisions: Vec<GateDecision>) -> Self {
        Self {
            schema: Self::SCHEMA.to_string(),
            decisions,
        }
    }

    /// Files this record at `extra.sven.gate`, replacing any earlier one.
    pub fn insert_into_extra(&self, extra: &mut Option<Value>) {
        insert_record(
            extra,
            Self::NAME,
            serde_json::to_value(self).expect("GateRecord always serializes"),
        );
    }

    /// The record at `extra.sven.gate`, if present and well-formed.
    #[must_use]
    pub fn from_extra(extra: &Value) -> Option<Self> {
        let value = extra.get(SVEN_EXTRA_KEY)?.get(Self::NAME)?;
        serde_json::from_value(value.clone()).ok()
    }
}

// ── extra.sven.memory ────────────────────────────────────────────────────────

/// Where in its source a retrieved unit sits, stable across revisions of the
/// source that keep the text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryAnchor {
    /// The headings above the unit, outermost first.
    pub heading_path: Vec<String>,
    /// The unit's position among the blocks under those headings.
    pub block_ordinal: u32,
    /// A digest of the unit's text.
    pub text_fingerprint: String,
}

/// One result of a retrieval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryResult {
    /// The retrieved unit.
    pub unit_id: String,
    /// The revision of the source the unit was cut from.
    pub revision: String,
    /// Where the unit sits in that revision.
    pub anchor: MemoryAnchor,
    /// The retrieval's score for the unit.
    pub score: f64,
    /// The unit's position in the result list, from 1.
    pub rank: u32,
    /// Whether the unit was put into the model's context; a result can be
    /// retrieved and left out.
    pub inserted: bool,
}

/// What one memory retrieval returned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryRetrieval {
    /// The `tool_call_id` of the call that retrieved; it names a call of the
    /// same step.
    pub call_id: String,
    /// The query as it was run.
    pub query: String,
    /// The spaces the retrieval searched.
    pub space_ids: Vec<String>,
    /// The index generation the query was answered from.
    pub generation_id: String,
    /// The snapshot of the memory the answer is consistent with, if the store
    /// has snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    /// The results, best first.
    pub results: Vec<MemoryResult>,
}

/// `extra.sven.memory`: the retrievals of the calls one step made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryRecord {
    /// Always [`MemoryRecord::SCHEMA`] for a record this crate writes.
    pub schema: String,
    /// One entry per retrieval, in call order.
    pub retrievals: Vec<MemoryRetrieval>,
}

impl MemoryRecord {
    /// The version string of the shape defined here.
    pub const SCHEMA: &'static str = "sven.memory.v1";
    /// The name this record is filed under inside `extra.sven`.
    pub const NAME: &'static str = "memory";

    /// A record of `retrievals`.
    #[must_use]
    pub fn new(retrievals: Vec<MemoryRetrieval>) -> Self {
        Self {
            schema: Self::SCHEMA.to_string(),
            retrievals,
        }
    }

    /// Files this record at `extra.sven.memory`, replacing any earlier one.
    pub fn insert_into_extra(&self, extra: &mut Option<Value>) {
        insert_record(
            extra,
            Self::NAME,
            serde_json::to_value(self).expect("MemoryRecord always serializes"),
        );
    }

    /// The record at `extra.sven.memory`, if present and well-formed.
    #[must_use]
    pub fn from_extra(extra: &Value) -> Option<Self> {
        let value = extra.get(SVEN_EXTRA_KEY)?.get(Self::NAME)?;
        serde_json::from_value(value.clone()).ok()
    }
}
