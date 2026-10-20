// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The ATIF trajectory of a harness run: its model conversation, with the
//! structured trace and the program's identity alongside.
//!
//! The conversation is the plan's own thread (`harness.<name>`), every message
//! as it happened, with each model request's account (tokens, seed, prompt and
//! sampled token ids, serving identity). Children run in conversations of
//! their own; each finished child is embedded as a trajectory in
//! `subagent_trajectories`, with the same account of its own requests and
//! tool actions, and listed in `extra.harness.children`.
//!
//! Swedish Embedded AB implements deterministic, auditable agent runtimes
//! for its clients. If your team needs expertise in turning autonomous
//! workflow runs into reviewable training and audit records then you can
//! procure our services by sending an email to info@swedishembedded.com.

use serde_json::json;
use sven_bootstrap::child_records::ChildRecord;
use sven_bootstrap::RuntimeHandle;
use sven_harness_runtime::harness_thread;
use sven_harness_spec::HarnessPlan;
use sven_model::Message;

use super::run::HarnessOutcome;
use super::HarnessRun;
use crate::engine::Engine;

/// One model conversation of a run: the shared agent thread or the thread of
/// a `conversation` a model-only step opened.
struct Conversation {
    origins: Vec<(usize, String)>,
    messages: Vec<Message>,
    compactions: Vec<(usize, sven_vocab::CompactionRecord)>,
    requests: Vec<(usize, sven_vocab::RequestRecord)>,
}

/// What an executed run keeps for its trajectory: the plan thread's
/// messages as the kernel held them when the run ended, and the outcome
/// they belong to.
pub(super) struct Evidence {
    messages: Vec<Message>,
    compactions: Vec<(usize, sven_vocab::CompactionRecord)>,
    requests: Vec<(usize, sven_vocab::RequestRecord)>,
    origins: Vec<(usize, String)>,
    gate_decisions: Vec<atif::GateDecision>,
    memory_retrievals: Vec<atif::MemoryRetrieval>,
    call_timings: Vec<atif::CallTiming>,
    unparsed_arguments: std::collections::HashMap<String, String>,
    /// The conversations model-only steps ran in, by name.
    conversations: Vec<(String, Conversation)>,
    children: Vec<ChildRecord>,
    model: String,
    tools: Vec<serde_json::Value>,
    bundle_digest: Option<String>,
    outcome: HarnessOutcome,
}

impl Evidence {
    pub(super) fn capture(
        engine: &Engine,
        plan: &HarnessPlan,
        handle: &RuntimeHandle,
        outcome: &HarnessOutcome,
        bundle_digest: Option<&str>,
        children: Vec<ChildRecord>,
    ) -> Self {
        let thread = harness_thread(plan);
        // The store's transcript, not its thread: a compaction rewrites the
        // thread to what the model reads next, the transcript is what
        // happened.
        let (
            messages,
            compactions,
            requests,
            origins,
            gate_decisions,
            memory_retrievals,
            call_timings,
            unparsed,
        ) = handle
            .conversation_store()
            .lock()
            .map(|store| {
                (
                    store.transcript(&thread),
                    store.transcript_compactions(&thread),
                    store.transcript_requests(&thread),
                    store.transcript_origins(&thread),
                    store.gate_decisions(),
                    store.memory_retrievals().to_vec(),
                    store.call_timings(),
                    store.unparsed_arguments().clone(),
                )
            })
            .unwrap_or_default();
        let names: std::collections::BTreeSet<&str> = plan
            .steps
            .values()
            .filter_map(|step| match &step.primitive {
                sven_harness_spec::PlanPrimitive::Predict { conversation, .. } => {
                    conversation.as_deref()
                }
                _ => None,
            })
            .collect();
        let conversations = handle
            .conversation_store()
            .lock()
            .map(|store| {
                names
                    .into_iter()
                    .map(|name| {
                        let thread = format!("{thread}.{name}");
                        (
                            name.to_string(),
                            Conversation {
                                messages: store.transcript(&thread),
                                compactions: store.transcript_compactions(&thread),
                                requests: store.transcript_requests(&thread),
                                origins: store.transcript_origins(&thread),
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let model = match engine.provider() {
            Some(provider) => provider.model_name().to_string(),
            None => engine.config().model.name.clone(),
        };
        let tools = handle
            .tool_registry()
            .schemas()
            .iter()
            .map(|s| atif::function_tool(&s.name, &s.description, &s.parameters))
            .collect();
        Self {
            messages,
            compactions,
            requests,
            origins,
            gate_decisions,
            memory_retrievals,
            call_timings,
            unparsed_arguments: unparsed,
            conversations,
            children,
            model,
            tools,
            bundle_digest: bundle_digest.map(str::to_string),
            outcome: outcome.clone(),
        }
    }
}

impl HarnessRun {
    /// What the run did, as an ATIF trajectory: the plan thread's messages,
    /// every tool call with its result, the model and the tools it was
    /// offered. The harness's name, program digests, run id and structured
    /// trace travel in `extra.harness`; the final metrics carry the token
    /// usage and the number of step executions.
    ///
    /// `None` until the run has executed. Children's own conversations are
    /// not included.
    #[must_use]
    pub fn trajectory(&self) -> Option<atif::Trajectory> {
        let evidence = self.evidence.as_ref()?;
        let mut agent = sven_session_store::default_agent_profile();
        agent.model_name = Some(evidence.model.clone());
        if !evidence.tools.is_empty() {
            agent.tool_definitions = Some(evidence.tools.clone());
        }
        let mut trajectory = atif::Trajectory::new(sven_session_store::ATIF_SCHEMA_VERSION, agent);
        trajectory.steps = sven_session_store::record_steps::conversation_steps(
            &sven_session_store::record_steps::ConversationFacts {
                messages: &evidence.messages,
                compactions: &evidence.compactions,
                requests: &evidence.requests,
                origins: &evidence.origins,
                gate_decisions: &evidence.gate_decisions,
                memory_retrievals: &evidence.memory_retrievals,
                call_timings: &evidence.call_timings,
                system_steps: true,
            },
            &|message| as_sent(evidence, message),
        );
        let conversations: Vec<atif::Trajectory> = evidence
            .conversations
            .iter()
            .map(|(name, conversation)| {
                let mut own = atif::Trajectory::new(
                    sven_session_store::ATIF_SCHEMA_VERSION,
                    trajectory.agent.clone(),
                );
                own.trajectory_id = Some(format!("{}/{name}", self.run_id));
                own.steps = sven_session_store::record_steps::conversation_steps(
                    &sven_session_store::record_steps::ConversationFacts {
                        messages: &conversation.messages,
                        compactions: &conversation.compactions,
                        requests: &conversation.requests,
                        origins: &conversation.origins,
                        gate_decisions: &[],
                        memory_retrievals: &[],
                        call_timings: &[],
                        system_steps: true,
                    },
                    &|message| as_sent(evidence, message),
                );
                own.extra = Some(json!({ "harness": { "conversation": name } }));
                own
            })
            .collect();
        let children: Vec<serde_json::Value> = evidence
            .children
            .iter()
            .flat_map(|child| {
                child.trajectories.iter().map(move |t| {
                    json!({
                        "trajectory_id": t.trajectory_id,
                        "machine": child.machine,
                        "descriptor": child.descriptor,
                        "steps": t.steps.len(),
                    })
                })
            })
            .collect();
        if !evidence.children.is_empty() || !conversations.is_empty() {
            trajectory.subagent_trajectories = Some(
                conversations
                    .into_iter()
                    .chain(
                        evidence
                            .children
                            .iter()
                            .flat_map(|child| child.trajectories.iter().cloned()),
                    )
                    .collect(),
            );
        }
        trajectory.extra = Some(json!({
            "harness": {
                "name": self.plan.name,
                "plan_digest": self.plan.digest,
                "bundle_digest": evidence.bundle_digest,
                "run_id": self.run_id,
                "trace": evidence.outcome.trace,
                "children": children,
                "conversations": evidence
                    .conversations
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect::<Vec<_>>(),
            }
        }));
        trajectory.final_metrics = Some(atif::FinalMetrics {
            total_prompt_tokens: evidence.outcome.usage.input_tokens,
            total_completion_tokens: evidence.outcome.usage.output_tokens,
            total_steps: Some(evidence.outcome.steps),
            ..atif::FinalMetrics::default()
        });
        Some(trajectory)
    }
}

/// A message as it was sent: a tool call whose arguments were not valid JSON
/// keeps the model's own text.
fn as_sent(evidence: &Evidence, message: &Message) -> Message {
    let mut message = message.clone();
    if let sven_model::MessageContent::ToolCall {
        tool_call_id,
        function,
    } = &mut message.content
    {
        if let Some(raw) = evidence.unparsed_arguments.get(tool_call_id) {
            function.arguments.clone_from(raw);
        }
    }
    message
}
