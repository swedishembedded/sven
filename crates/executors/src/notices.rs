// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for agents that notice when a local
// model server changes under them for its clients. If your team needs
// expertise in self-hosted LLM infrastructure then you can procure our
// services by sending an email to info@swedishembedded.com.
//! What a turn tells the surfaces about the model server before it asks it
//! anything: an adapter that was swapped, a capability the model lacks.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, PoisonError};

use sven_decision::telemetry::DecisionTelemetry;
use sven_hsm::{ObservationSink, UiEvent};
use sven_model::ModelProvider;
use sven_vocab::RequestBreakdown;

/// Reports the change the live server made to the bound model since the last
/// turn, if a probe saw one.
pub(crate) fn report_served_model_change(model: &dyn ModelProvider, obs: &ObservationSink) {
    if let Some(change) = model.take_model_change() {
        obs.emit(UiEvent::ServedModelChanged {
            model: change.model,
            from: change.from,
            to: change.to,
            context_window: change.context_window,
        });
    }
}

/// Warns, once per model in `warned`, that tools are about to be offered to a
/// model the server says cannot call them. Silence from the server is not a
/// "no".
pub(crate) fn warn_if_tools_unsupported(
    warned: &mut HashSet<String>,
    model: &dyn ModelProvider,
    offering_tools: bool,
    obs: &ObservationSink,
) {
    if offering_tools
        && model.tool_support() == Some(false)
        && warned.insert(model.model_name().to_string())
    {
        obs.emit(UiEvent::Warning(format!(
            "{} does not advertise tool calling: its tool calls may be ignored or malformed",
            model.model_name()
        )));
    }
}

/// Adds one request's cache reads and writes to the running totals of
/// `thread_id` and returns the totals, saturated to what an event holds.
pub(crate) fn add_cache_totals(
    totals: &Mutex<HashMap<String, (u64, u64)>>,
    thread_id: &str,
    (read, write): (u32, u32),
) -> (u32, u32) {
    let mut totals = totals.lock().unwrap_or_else(PoisonError::into_inner);
    let entry = totals.entry(thread_id.to_string()).or_insert((0, 0));
    entry.0 += u64::from(read);
    entry.1 += u64::from(write);
    let saturate = |n: u64| u32::try_from(n).unwrap_or(u32::MAX);
    (saturate(entry.0), saturate(entry.1))
}

/// Charges the decision inference and traces gathered since the last request
/// to this request's ledger record.
pub(crate) fn charge_decisions(decisions: &DecisionTelemetry, breakdown: &mut RequestBreakdown) {
    let drained = decisions.drain();
    breakdown.decision = drained.inference;
    breakdown.decision_trace = drained.traces;
}
