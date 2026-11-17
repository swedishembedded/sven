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

/// What a session was already warned about.
#[derive(Debug, Default)]
pub(crate) struct Warned {
    /// Models said to lack tool calling.
    without_tools: HashSet<String>,
    /// Models sent sampling controls they do not declare they honour.
    unhonoured_sampling: HashSet<String>,
}

/// Says what about to be sent may not be acted on, once per model and
/// session: tools offered to a model the server says cannot call them
/// (silence from the server is not a "no"), and sampling controls sent on
/// best effort to a provider that does not declare it honours them.
pub(crate) fn warn_before_request(
    warned: &mut Warned,
    model: &dyn ModelProvider,
    offering_tools: bool,
    limits: &sven_turn::TurnLimits,
    obs: &ObservationSink,
) {
    let name = model.model_name();
    if offering_tools
        && model.tool_support() == Some(false)
        && warned.without_tools.insert(name.to_string())
    {
        obs.emit(UiEvent::Warning(format!(
            "{name} does not advertise tool calling: its tool calls may be ignored or malformed"
        )));
    }
    let unhonoured = limits.sampling.unhonoured_by(model);
    if !unhonoured.is_empty()
        && model.sampling_best_effort()
        && warned.unhonoured_sampling.insert(name.to_string())
    {
        obs.emit(UiEvent::Warning(format!(
            "{name} does not declare it honours the sampling control(s) {unhonoured}: they are \
             sent on best effort (model.sampling_best_effort) and may be ignored; the trajectory \
             records them as unhonoured"
        )));
    }
}

/// Files a request refused for its sampling controls in the conversation
/// store, so a caller that reads the store can report it as what it is
/// rather than as a failed turn.
pub(crate) fn file_sampling_refusal(store: &Mutex<crate::ThreadStore>, error: &anyhow::Error) {
    if let Some(refused) = error.downcast_ref::<sven_model::SamplingRefused>() {
        let mut store = store.lock().unwrap_or_else(PoisonError::into_inner);
        store.set_sampling_refusal(refused.clone());
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
