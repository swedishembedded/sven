// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent trajectory capture for its clients. If
// your team needs expertise in turning agent runs into training data then you
// can procure our services by sending an email to info@swedishembedded.com.
//! What a child run leaves behind for its parent's trajectory.
//!
//! A child runs in a conversation of its own, in a store the parent never
//! sees. When the child ends, its store is the authoritative record of what it
//! said and did: every message, the tool calls with their results, each model
//! request's account (tokens, sampling seed, prompt and sampled token ids,
//! serving identity), the compactions where they fired. This turns that store
//! into ATIF trajectories and keeps them where the parent's export finds them.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use sven_executors::ThreadStore;
use sven_session_store::record_steps::{conversation_steps, ConversationFacts};

/// One finished child run: how it was started and what it recorded.
#[derive(Debug, Clone)]
pub struct ChildRecord {
    /// The child's machine id, as its parent knows it.
    pub machine: String,
    /// The descriptor the parent started it with (task, harness, entry,
    /// input namespace, depth).
    pub descriptor: Value,
    /// One trajectory per conversation thread the child used; usually one.
    /// Each has a `trajectory_id`, so it can be embedded in a parent's
    /// `subagent_trajectories`.
    pub trajectories: Vec<atif::Trajectory>,
}

/// The finished children of a run, shared between the spawner that collects
/// them and the export that embeds them. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct ChildRecords {
    finished: Arc<Mutex<Vec<ChildRecord>>>,
}

impl ChildRecords {
    /// Keeps one finished child.
    pub(crate) fn push(&self, record: ChildRecord) {
        if let Ok(mut finished) = self.finished.lock() {
            finished.push(record);
        }
    }

    /// Every child that has finished so far, in the order they finished.
    #[must_use]
    pub fn finished(&self) -> Vec<ChildRecord> {
        self.finished.lock().map(|f| f.clone()).unwrap_or_default()
    }
}

/// The record of a child run, read from the store it ran in. `model` is the
/// model the child's requests went to unless the requests say otherwise.
pub(crate) fn record_child(
    store: &Mutex<ThreadStore>,
    machine: &str,
    descriptor: &Value,
    model: &str,
) -> ChildRecord {
    let Ok(store) = store.lock() else {
        return ChildRecord {
            machine: machine.to_string(),
            descriptor: descriptor.clone(),
            trajectories: Vec::new(),
        };
    };
    let threads = store.transcript_threads();
    let single = threads.len() == 1;
    let gate_decisions = store.gate_decisions();
    let trajectories = threads
        .iter()
        .map(|thread| {
            let messages = store.transcript(thread);
            let compactions = store.transcript_compactions(thread);
            let requests = store.transcript_requests(thread);
            let mut agent = sven_session_store::default_agent_profile();
            agent.model_name = Some(model.to_string());
            let mut trajectory =
                atif::Trajectory::new(sven_session_store::ATIF_SCHEMA_VERSION, agent);
            trajectory.trajectory_id = Some(if single {
                format!("child-{machine}")
            } else {
                format!("child-{machine}-{thread}")
            });
            trajectory.steps = conversation_steps(
                &ConversationFacts {
                    messages: &messages,
                    compactions: &compactions,
                    requests: &requests,
                    gate_decisions: &gate_decisions,
                    memory_retrievals: store.memory_retrievals(),
                },
                &|message| {
                    let mut message = message.clone();
                    if let sven_model::MessageContent::ToolCall {
                        tool_call_id,
                        function,
                    } = &mut message.content
                    {
                        if let Some(raw) = store.unparsed_arguments().get(tool_call_id) {
                            function.arguments.clone_from(raw);
                        }
                    }
                    message
                },
            );
            trajectory.final_metrics = sven_session_store::step_request::totals(
                requests.iter().map(|(_, request)| request),
            );
            trajectory.extra = Some(json!({
                "child": { "machine": machine, "thread": thread, "descriptor": descriptor }
            }));
            trajectory
        })
        .collect();
    ChildRecord {
        machine: machine.to_string(),
        descriptor: descriptor.clone(),
        trajectories,
    }
}
