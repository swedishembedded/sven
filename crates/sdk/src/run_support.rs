// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What an agent's runs are made of: how one starts, how one is stopped, and
//! how a model's answer is read as a typed value.
//!
//! Swedish Embedded AB implements embeddable agent runtimes for its clients. If
//! your team needs expertise in building services on top of agent state
//! machines then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::time::Duration;

use serde::de::DeserializeOwned;
use sven_hsm::ApprovalId;

use crate::run::Decision;

/// How long a stopped run is given to report what it had before it is left.
const WIND_DOWN: Duration = Duration::from_secs(5);

/// Stops a run: interrupts the model call in flight, tells the machine the
/// work is cancelled, and starts the wind-down clock.
pub(crate) async fn stop(
    handle: &sven_bootstrap::RuntimeHandle,
    abort_slot: &tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    wind_down: std::pin::Pin<&mut tokio::time::Sleep>,
    reason: &str,
) {
    // Before the abort is sent: the turn executor reads it when it files the
    // aborted turn into the conversation, so the trajectory says what ended it.
    if let Ok(mut store) = handle.conversation_store().lock() {
        store.set_stop_reason(reason);
    }
    if let Some(abort) = abort_slot.lock().await.take() {
        let _ = abort.send(());
    }
    let _ = handle.cancel().await;
    wind_down.reset(tokio::time::Instant::now() + WIND_DOWN);
}

/// What starts a run.
pub(crate) enum Entry {
    /// A message from the user.
    Message(String),
    /// The answer to the question the agent is waiting on.
    Answer {
        question_id: sven_hsm::QuestionId,
        /// The conversation's id for the call that asked.
        call_ref: String,
        answer: String,
    },
    /// A person's decision on the approval the agent is waiting on.
    Decision {
        approval_id: ApprovalId,
        decision: Decision,
    },
}

/// Per-turn overrides that do not belong to the agent's durable state.
#[derive(Default)]
pub(crate) struct TurnOptions {
    /// System prompt for this turn.
    pub(crate) role: Option<String>,
    /// Whether the model is denied tools outright.
    pub(crate) no_tools: bool,
    /// Domain facts the machine needs in order to run the turn.
    pub(crate) facts: serde_json::Map<String, serde_json::Value>,
}

/// Reads `answer` as `T`, describing the failure in terms a model can act on.
///
/// Tolerates a fenced code block around the JSON, because a model told to
/// answer with JSON very often answers with JSON in a fence, and rejecting that
/// spends a repair attempt on punctuation.
pub(crate) fn parse_candidate<T: DeserializeOwned>(answer: &str) -> Result<T, String> {
    let trimmed = strip_fence(answer.trim());
    if trimmed.is_empty() {
        return Err("the answer was empty".to_string());
    }
    serde_json::from_str::<T>(trimmed).map_err(|e| e.to_string())
}

/// Returns `text` without a surrounding ```/```json fence, if it has one.
fn strip_fence(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    let rest = rest.strip_prefix("json").unwrap_or(rest);
    rest.trim_start_matches(['\r', '\n'])
        .trim_end()
        .strip_suffix("```")
        .unwrap_or(rest)
        .trim()
}
