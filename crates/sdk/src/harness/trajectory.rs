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

/// One model conversation of a run: the shared thread, or a conversation a
/// task opened (`harness.<name>.<conversation>`).
struct Conversation {
    origins: Vec<(usize, String)>,
    reasonings: Vec<(usize, String)>,
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
    reasonings: Vec<(usize, String)>,
    gate_decisions: Vec<atif::GateDecision>,
    memory_retrievals: Vec<atif::MemoryRetrieval>,
    call_timings: Vec<atif::CallTiming>,
    unparsed_arguments: std::collections::HashMap<String, String>,
    /// The conversations the run's tasks ran in, by name, in the order each
    /// began.
    conversations: Vec<(String, Conversation)>,
    children: Vec<ChildRecord>,
    model: String,
    tools: Vec<serde_json::Value>,
    bundle_digest: Option<String>,
    /// What each command gate step last stored (`passed`, `detail`,
    /// `stdout`, ...), by step id: why a gate refused what it refused.
    gate_results: std::collections::BTreeMap<String, serde_json::Value>,
    outcome: HarnessOutcome,
}

impl Evidence {
    pub(super) fn capture(
        engine: &Engine,
        plan: &HarnessPlan,
        handle: &RuntimeHandle,
        outcome: &HarnessOutcome,
        (bundle_digest, steps): (
            Option<&str>,
            &std::collections::BTreeMap<String, sven_harness_runtime::StepRecord>,
        ),
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
            reasonings,
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
                    store.transcript_reasonings(&thread),
                    store.gate_decisions(),
                    store.memory_retrievals().to_vec(),
                    store.call_timings(),
                    store.unparsed_arguments().clone(),
                )
            })
            .unwrap_or_default();
        // Every conversation the run opened (a task's own, one per item of a
        // loop), in the order each began: the names are resolved at run time,
        // so they come from the store, not from the plan.
        let prefix = format!("{thread}.");
        let conversations = handle
            .conversation_store()
            .lock()
            .map(|store| {
                store
                    .transcript_threads()
                    .into_iter()
                    .filter_map(|id| Some((id.strip_prefix(&prefix)?.to_string(), id.clone())))
                    .map(|(name, thread)| {
                        (
                            name,
                            Conversation {
                                messages: store.transcript(&thread),
                                compactions: store.transcript_compactions(&thread),
                                requests: store.transcript_requests(&thread),
                                origins: store.transcript_origins(&thread),
                                reasonings: store.transcript_reasonings(&thread),
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
        let gate_results = plan
            .steps
            .iter()
            .filter(|(_, step)| {
                matches!(
                    step.primitive,
                    sven_harness_spec::PlanPrimitive::Gate { .. }
                )
            })
            .filter_map(|(id, _)| Some((id.clone(), steps.get(id)?.value.clone())))
            .collect();
        Self {
            messages,
            compactions,
            requests,
            origins,
            reasonings,
            gate_decisions,
            memory_retrievals,
            call_timings,
            unparsed_arguments: unparsed,
            conversations,
            children,
            model,
            tools,
            bundle_digest: bundle_digest.map(str::to_string),
            gate_results,
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
        Some(build_trajectory(evidence, &self.plan, &self.run_id))
    }

    /// Each conversation of the run as a trajectory of its own, with its name
    /// (a task's, `<task>#<item index>` inside a `for_each`), in the order
    /// each began: one agent's work, on its own, as training material wants
    /// it. `None` until the run has executed.
    #[must_use]
    pub fn conversations(&self) -> Option<Vec<(String, atif::Trajectory)>> {
        let trajectory = self.trajectory()?;
        Some(
            trajectory
                .subagent_trajectories
                .unwrap_or_default()
                .into_iter()
                .filter_map(|t| {
                    let name = t.extra.as_ref()?["harness"]["conversation"]
                        .as_str()?
                        .to_string();
                    Some((name, t))
                })
                .collect(),
        )
    }
}

/// The ATIF trajectory `evidence` makes for the run `run_id` of `plan`.
pub(super) fn build_trajectory(
    evidence: &Evidence,
    plan: &HarnessPlan,
    run_id: &str,
) -> atif::Trajectory {
    {
        let mut trajectory =
            atif::Trajectory::new(sven_session_store::ATIF_SCHEMA_VERSION, run_agent(evidence));
        trajectory.steps = sven_session_store::record_steps::conversation_steps(
            &sven_session_store::record_steps::ConversationFacts {
                messages: &evidence.messages,
                compactions: &evidence.compactions,
                requests: &evidence.requests,
                origins: &evidence.origins,
                reasonings: &evidence.reasonings,
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
                conversation_trajectory(evidence, &trajectory.agent, run_id, name, conversation)
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
                "name": plan.name,
                "plan_digest": plan.digest,
                "bundle_digest": evidence.bundle_digest,
                "run_id": run_id,
                "trace": evidence.outcome.trace,
                "children": children,
                "conversations": evidence
                    .conversations
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect::<Vec<_>>(),
            }
        }));
        trajectory.final_metrics = Some(step_totals(&trajectory, evidence.outcome.steps));
        trajectory
    }
}

/// The run's totals, summed over the steps of the shared thread and of every
/// embedded conversation, so they mean what ATIF says they mean: prompt
/// tokens include the cached part (the run's `Usage::input_tokens` does
/// not), and the cached tokens and the cost are reported too. A total no
/// step reports is `None`, never zero.
fn step_totals(trajectory: &atif::Trajectory, steps: u64) -> atif::FinalMetrics {
    let metrics: Vec<&atif::StepMetrics> = trajectory
        .steps
        .iter()
        .chain(
            trajectory
                .subagent_trajectories
                .iter()
                .flatten()
                .flat_map(|t| t.steps.iter()),
        )
        .filter_map(|s| s.metrics.as_ref())
        .collect();
    fn total<T: std::iter::Sum<T>>(values: impl Iterator<Item = Option<T>>) -> Option<T> {
        let known: Vec<T> = values.flatten().collect();
        (!known.is_empty()).then(|| known.into_iter().sum())
    }
    atif::FinalMetrics {
        total_prompt_tokens: total(metrics.iter().map(|m| m.prompt_tokens)),
        total_completion_tokens: total(metrics.iter().map(|m| m.completion_tokens)),
        total_cached_tokens: total(metrics.iter().map(|m| m.cached_tokens)),
        total_cost_usd: total(metrics.iter().map(|m| m.cost_usd)),
        total_steps: Some(steps),
        ..atif::FinalMetrics::default()
    }
}

/// The agent profile every trajectory of the run carries.
fn run_agent(evidence: &Evidence) -> atif::AgentProfile {
    let mut agent = sven_session_store::default_agent_profile();
    agent.model_name = Some(evidence.model.clone());
    if !evidence.tools.is_empty() {
        agent.tool_definitions = Some(evidence.tools.clone());
    }
    agent
}

/// One conversation of the run as a trajectory of its own.
fn conversation_trajectory(
    evidence: &Evidence,
    agent: &atif::AgentProfile,
    run_id: &str,
    name: &str,
    conversation: &Conversation,
) -> atif::Trajectory {
    let mut own = atif::Trajectory::new(sven_session_store::ATIF_SCHEMA_VERSION, agent.clone());
    own.trajectory_id = Some(format!("{run_id}/{name}"));
    own.steps = sven_session_store::record_steps::conversation_steps(
        &sven_session_store::record_steps::ConversationFacts {
            messages: &conversation.messages,
            compactions: &conversation.compactions,
            requests: &conversation.requests,
            origins: &conversation.origins,
            reasonings: &conversation.reasonings,
            gate_decisions: &[],
            memory_retrievals: &[],
            call_timings: &[],
            system_steps: true,
        },
        &|message| as_sent(evidence, message),
    );
    own.extra = Some(json!({ "harness": { "conversation": name } }));
    own
}

impl Evidence {
    /// The conversations that gained messages or requests since `delivered`
    /// last saw them, each with its position in the order the conversations
    /// began; `delivered` is updated to what is returned.
    pub(super) fn changed_conversations(
        &self,
        run_id: &str,
        delivered: &mut std::collections::HashMap<String, (usize, usize)>,
    ) -> Vec<(usize, String, atif::Trajectory)> {
        let agent = run_agent(self);
        let mut changed = Vec::new();
        for (index, (name, conversation)) in self.conversations.iter().enumerate() {
            let version = (conversation.messages.len(), conversation.requests.len());
            if delivered.insert(name.clone(), version) != Some(version) {
                changed.push((
                    index,
                    name.clone(),
                    conversation_trajectory(self, &agent, run_id, name, conversation),
                ));
            }
        }
        changed
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

/// A message the supervisor of a `corrections` block sent the agent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Correction {
    /// The message, as the agent received it.
    pub message: String,
    /// Tool calls the agent had made in all before this message.
    pub tool_calls_before: usize,
}

/// A message the supervisor wrote that its accept gate refused.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RefusedMessage {
    /// The supervisor's conversation (its task name).
    pub author: String,
    /// What the supervisor wrote.
    pub message: String,
    /// Why it was refused: the text the supervisor was sent to rewrite it.
    pub reason: String,
}

impl HarnessRun {
    /// The corrections the run's supervisors sent the agent, in order, each
    /// with the number of tool calls the agent had made before it. `None`
    /// until the run has executed.
    #[must_use]
    pub fn corrections(&self) -> Option<Vec<Correction>> {
        let evidence = self.evidence.as_ref()?;
        // A correction is a user turn in the corrected task's conversation;
        // the shared thread is searched too, for a task that names it.
        let shared = (&evidence.messages, &evidence.origins);
        let conversations = std::iter::once(shared).chain(
            evidence
                .conversations
                .iter()
                .map(|(_, c)| (&c.messages, &c.origins)),
        );
        let mut found = Vec::new();
        for (messages, origins) in conversations {
            let calls = |upto: usize| {
                messages[..upto]
                    .iter()
                    .filter(|m| matches!(m.content, sven_model::MessageContent::ToolCall { .. }))
                    .count()
            };
            found.extend(
                origins
                    .iter()
                    .filter(|(_, origin)| origin == "correction")
                    .filter_map(|(at, _)| {
                        let message = messages.get(*at)?;
                        Some(Correction {
                            message: message.as_text()?.to_string(),
                            tool_calls_before: calls(*at),
                        })
                    }),
            );
        }
        Some(found)
    }

    /// Every supervisor message an accept gate refused, in order. A refusal
    /// that was sent back for a rewrite carries the text the supervisor was
    /// sent; the last refusal, when the block's `retries` were spent, was not
    /// sent back and carries the gate's own account (its output, else its
    /// exit status). `None` until the run has executed.
    #[must_use]
    pub fn refused_messages(&self) -> Option<Vec<RefusedMessage>> {
        let evidence = self.evidence.as_ref()?;
        // An author's conversation is named by a literal: rendering it with
        // nothing to resolve gives the name.
        let authors: std::collections::BTreeSet<String> = self
            .plan
            .steps
            .iter()
            .filter(|(id, _)| {
                self.plan
                    .source_map
                    .get(*id)
                    .is_some_and(|origin| origin.role == "correction")
            })
            .filter_map(|(_, step)| match &step.primitive {
                sven_harness_spec::PlanPrimitive::Predict { conversation, .. } => {
                    conversation.as_ref().map(|t| t.render(&|_| None))
                }
                _ => None,
            })
            .collect();
        let mut refused = Vec::new();
        for (name, conversation) in &evidence.conversations {
            if !authors.contains(name) {
                continue;
            }
            for (at, origin) in &conversation.origins {
                if origin != "repair" || *at == 0 {
                    continue;
                }
                let (Some(sent), Some(reason)) = (
                    conversation.messages.get(*at - 1).and_then(|m| m.as_text()),
                    conversation.messages.get(*at).and_then(|m| m.as_text()),
                ) else {
                    continue;
                };
                refused.push(RefusedMessage {
                    author: name.clone(),
                    message: sent.to_string(),
                    reason: reason.to_string(),
                });
            }
        }
        refused.extend(self.unanswered_refusal(evidence, &authors));
        Some(refused)
    }

    /// The refusal that ended a `corrections` block: the author's last draft
    /// when no rewrite was asked for and the agent never received it.
    fn unanswered_refusal(
        &self,
        evidence: &Evidence,
        authors: &std::collections::BTreeSet<String>,
    ) -> Option<RefusedMessage> {
        let delivered = self.corrections()?;
        let (name, conversation) = evidence
            .conversations
            .iter()
            .rfind(|(name, _)| authors.contains(name))?;
        let last = conversation.messages.last()?;
        let draft = last.as_text()?;
        if last.role != sven_model::Role::Assistant
            || delivered.last().is_some_and(|c| c.message == draft)
        {
            return None;
        }
        // The accept gate's latest failing attempt names why.
        let (_, result) = self
            .plan
            .source_map
            .iter()
            .filter(|(_, origin)| origin.role == "check")
            .filter(|(id, _)| id.contains("__accept__"))
            .filter_map(|(id, origin)| {
                let result = evidence.gate_results.get(id)?;
                (result["passed"] == false).then_some((origin.attempt?, result))
            })
            .max_by_key(|(attempt, _)| *attempt)?;
        let text = |key: &str| result[key].as_str().map(str::trim).unwrap_or_default();
        let reason = [text("stdout"), text("stderr"), text("detail")]
            .into_iter()
            .find(|t| !t.is_empty())?;
        Some(RefusedMessage {
            author: name.clone(),
            message: draft.to_string(),
            reason: reason.to_string(),
        })
    }
}
