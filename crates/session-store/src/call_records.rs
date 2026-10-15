// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent audit trails that keep every permission
// decision and every memory read reproducible, for its clients. If your team
// needs expertise in auditable agent runtimes you can procure our services by
// sending an email to info@swedishembedded.com.

//! What is known about the calls a step made beyond their arguments and
//! results: the gate's decisions and the memory they read.
//!
//! The facts arrive on their own schedule. A decision can be made before the
//! model's call is complete, or after the step that holds the call has closed,
//! so they are kept by call id and written onto whichever step holds the call
//! as `extra.sven.gate` and `extra.sven.memory`, whenever either appears.

use std::collections::BTreeMap;

use atif::{GateDecision, GateRecord, MemoryRecord, MemoryRetrieval, TraceStep};

/// Gate decisions and memory retrievals, by the id of the call they concern.
#[derive(Debug, Default, Clone)]
pub(crate) struct CallRecords {
    decisions: BTreeMap<String, GateDecision>,
    retrievals: BTreeMap<String, Vec<MemoryRetrieval>>,
}

impl CallRecords {
    /// Keeps `decision` for its call, replacing an earlier decision about it.
    pub(crate) fn add_decision(&mut self, decision: GateDecision) {
        self.decisions.insert(decision.call_id.clone(), decision);
    }

    /// Keeps `retrieval` for its call, after the call's earlier retrievals.
    pub(crate) fn add_retrieval(&mut self, retrieval: MemoryRetrieval) {
        self.retrievals
            .entry(retrieval.call_id.clone())
            .or_default()
            .push(retrieval);
    }

    /// Writes the records of the calls `step` made into its `extra`, in the
    /// order the step made them. A step that made no call known here is left
    /// untouched.
    pub(crate) fn annotate(&self, step: &mut TraceStep) {
        let Some(calls) = &step.tool_calls else {
            return;
        };
        let ids: Vec<&str> = calls.iter().map(|c| c.tool_call_id.as_str()).collect();
        let decisions: Vec<GateDecision> = ids
            .iter()
            .filter_map(|id| self.decisions.get(*id).cloned())
            .collect();
        let retrievals: Vec<MemoryRetrieval> = ids
            .iter()
            .filter_map(|id| self.retrievals.get(*id))
            .flatten()
            .cloned()
            .collect();
        if !decisions.is_empty() {
            GateRecord::new(decisions).insert_into_extra(&mut step.extra);
        }
        if !retrievals.is_empty() {
            MemoryRecord::new(retrievals).insert_into_extra(&mut step.extra);
        }
    }
}

/// Whether `step` made the call `call_id`.
pub(crate) fn makes_call(step: &TraceStep, call_id: &str) -> bool {
    step.tool_calls
        .iter()
        .flatten()
        .any(|call| call.tool_call_id == call_id)
}
