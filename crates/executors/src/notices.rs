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

use std::collections::HashSet;

use sven_hsm::{ObservationSink, UiEvent};
use sven_model::ModelProvider;

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
