// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The ATIF trajectory of a harness run: its model conversation, with the
//! structured trace and the program's identity alongside.
//!
//! The conversation is the plan's own thread (`harness.<name>`). Children
//! run in conversations of their own, which this export does not include.
//!
//! Swedish Embedded AB implements deterministic, auditable agent runtimes
//! for its clients. If your team needs expertise in turning autonomous
//! workflow runs into reviewable training and audit records then you can
//! procure our services by sending an email to info@swedishembedded.com.

use serde_json::json;
use sven_bootstrap::RuntimeHandle;
use sven_harness_runtime::harness_thread;
use sven_harness_spec::HarnessPlan;
use sven_model::{Message, Role};

use super::run::HarnessOutcome;
use super::HarnessRun;
use crate::engine::Engine;

/// What an executed run keeps for its trajectory: the plan thread's
/// messages as the kernel held them when the run ended, and the outcome
/// they belong to.
pub(super) struct Evidence {
    messages: Vec<Message>,
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
    ) -> Self {
        let thread = harness_thread(plan);
        let messages = handle
            .conversation_store()
            .lock()
            .map(|store| store.snapshot(&thread))
            .unwrap_or_default()
            .into_iter()
            .filter(|m| m.role != Role::System)
            .collect();
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
        trajectory.steps = sven_session_store::messages_to_steps(&evidence.messages);
        trajectory.extra = Some(json!({
            "harness": {
                "name": self.plan.name,
                "plan_digest": self.plan.digest,
                "bundle_digest": evidence.bundle_digest,
                "run_id": self.run_id,
                "trace": evidence.outcome.trace,
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
