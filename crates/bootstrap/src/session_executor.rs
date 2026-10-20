// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The two assembly steps that turn a session's shared resources into what
//! the kernel runs against: the child spawner that services
//! `InstantiateSubmachine`, and the composite effect executor that services
//! everything else.
//!
//! Both are pure wiring over values [`crate::runtime_builder::RuntimeBuilder`]
//! has already built; they live here so the builder stays a sequence of
//! decisions and the executor composition has one place to change.
//!
//! Swedish Embedded AB implements solutions for assembling kernel-driven
//! agent runtimes for its clients. If your team needs expertise in
//! effect-executor wiring and agent session architecture then you can
//! procure our services by sending an email to info@swedishembedded.com.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sven_executors::user::{ApprovalRequest, UserQuestion};
use sven_executors::{
    CompositeExecutorBuilder, GateJournal, ThreadStore, ToolExecutor, TurnExecutor, UserExecutor,
};
use sven_hsm::{AuditTrailHandle, ToolCallId};
use sven_kernel::{ChildSpawner, EffectExecutor, SystemClock};
use sven_vocab::ApprovalMode;
use tokio::sync::{mpsc, oneshot};

use crate::child_spawner::SdlcChildSpawner;
use crate::runtime_builder::ToolExecutorFactory;
use crate::Config;

/// The channels and policy through which a session reaches its person: the
/// session's own user executor and every child it spawns share them.
pub(crate) struct UserGates {
    pub questions: mpsc::Sender<UserQuestion>,
    pub approvals: mpsc::Sender<ApprovalRequest>,
    pub approval_mode: ApprovalMode,
    /// The session's own approvals wait to be decided from outside the run
    /// instead of being put to the approval channel. Children still ask.
    pub park_approvals: bool,
}

/// The root `Effect::Verify` is jailed to: the session's project root, or
/// the working directory when it has none.
fn verify_root(project_root: Option<&Path>) -> PathBuf {
    project_root.map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// The cost policy the configuration declares. A price that is not usable
/// is reported and the gate stays off rather than weighing with it.
fn cost_policy(config: &Config) -> sven_executors::CostPolicy {
    config.cost.policy().unwrap_or_else(|why| {
        tracing::warn!(%why, "cost.cache_aware stays off");
        sven_executors::CostPolicy::default()
    })
}

/// What a session hands every child run it may start, beyond the terms the
/// kernel gives it: the root their verifier is jailed to, how they decode, and
/// where finished children leave their records.
#[derive(Default)]
pub(crate) struct ChildSetup<'a> {
    pub project_root: Option<&'a Path>,
    pub decoding: sven_turn::Decoding,
    pub records: Option<crate::child_records::ChildRecords>,
}

/// Parallel-execution fan-out: in SDLC mode, and in a session with
/// registered harness documents, the kernel can service
/// `Effect::InstantiateSubmachine` by running each child as an isolated
/// concurrent kernel. A harness's `delegate` and `parallel` steps emit the
/// same effect, and their children resolve through the session's registry.
pub(crate) fn child_spawner(
    mode: &str,
    harnesses: &Arc<sven_harness_runtime::HarnessRegistry>,
    model: &Arc<dyn sven_model::ModelProvider>,
    config: &Arc<Config>,
    tool_registry: &Arc<sven_tool_registry::ToolRegistry>,
    gates: &UserGates,
    setup: ChildSetup<'_>,
) -> Option<Arc<dyn ChildSpawner>> {
    if mode != "sdlc" && harnesses.is_empty() {
        return None;
    }
    let spawner = SdlcChildSpawner::new(
        Arc::clone(model),
        Arc::clone(config),
        Arc::clone(tool_registry),
    );
    let ChildSetup {
        project_root,
        decoding,
        records,
    } = setup;
    let spawner = match records {
        Some(records) => spawner.with_records(records),
        None => spawner,
    };
    Some(Arc::new(
        spawner
            .with_harnesses(Arc::clone(harnesses))
            .with_decoding(decoding)
            .with_verify_root(verify_root(project_root))
            .with_gates(
                gates.questions.clone(),
                gates.approvals.clone(),
                gates.approval_mode,
            ),
    ))
}

/// What the default composite executor is built from.
pub(crate) struct ExecutorParts {
    pub gates: UserGates,
    pub model: Arc<dyn sven_model::ModelProvider>,
    pub config: Arc<Config>,
    pub tool_registry: Arc<sven_tool_registry::ToolRegistry>,
    /// Shared by the turn and tool executors: tool results must be appended
    /// under the exact thread and call id the model assigned.
    pub conv_store: Arc<Mutex<ThreadStore>>,
    pub call_id_to_thread: Arc<Mutex<HashMap<ToolCallId, (String, String)>>>,
    pub cancel_handle: Arc<tokio::sync::Mutex<Option<oneshot::Sender<()>>>>,
    pub no_tools: bool,
    /// Sampling and sampled-token capture for every request of the session.
    pub decoding: sven_turn::Decoding,
    /// Where the turn executor accounts for the session's decision calls.
    pub decision_telemetry: Arc<sven_decision::telemetry::DecisionTelemetry>,
    /// The session's addressable context, when it is on.
    pub context: Option<crate::context_wiring::ContextWiring>,
    /// The project root every effect shares (also where verification runs)
    /// and the activation of conditional instructions.
    pub instructions: crate::instruction_wiring::InstructionWiring,
    pub audit_trail: AuditTrailHandle,
    pub parked_tx: mpsc::Sender<sven_executors::user::ParkedQuestion>,
    /// Replaces only the tool slot, keeping every other default executor.
    pub tool_executor_override: Option<ToolExecutorFactory>,
    /// Hold every tool call to the capability its registered tool really has
    /// (see [`ToolExecutor::with_capability_reconciliation`]). For sessions
    /// whose machine derives call capabilities from tool names. Not applied
    /// to a tool slot substituted by `tool_executor_override`, whose
    /// executor the override owns.
    pub reconcile_capabilities: bool,
}

/// A caller-supplied executor replaces the default composite wholesale;
/// otherwise the default composite is wired, substituting only the tool slot
/// when a tool-executor override is set.
pub(crate) fn compose(
    custom: Option<Box<dyn EffectExecutor>>,
    parts: ExecutorParts,
) -> Box<dyn EffectExecutor> {
    if let Some(custom) = custom {
        return custom;
    }
    let audit_log_path = parts
        .instructions
        .project_root
        .as_ref()
        .map(|r| r.join(".sven").join("audit.jsonl"))
        .unwrap_or_else(|| PathBuf::from(".sven/audit.jsonl"));
    // Only the verified-task machine's `Verifying` state ever emits
    // `Effect::Verify` (see its `permission_policy`), so wiring it here
    // unconditionally is harmless for every other mode.
    let verify_root = verify_root(parts.instructions.project_root.as_deref());
    let (chunk_session, assembler) = match parts.context {
        Some(c) => (Some(c.session), Some(c.assembler)),
        None => (None, None),
    };
    let turn_executor = TurnExecutor::new(
        Arc::clone(&parts.model),
        None,
        Arc::clone(&parts.tool_registry),
        Arc::clone(&parts.conv_store),
        Arc::clone(&parts.call_id_to_thread),
        parts.cancel_handle,
    )
    .with_no_tools(parts.no_tools)
    .with_compaction_config(parts.config.agent.compaction())
    .with_turn_limits(
        parts
            .config
            .agent
            .turn_limits()
            .with_decoding(parts.decoding),
    )
    .with_cost_policy(cost_policy(&parts.config))
    .with_decision_telemetry(parts.decision_telemetry)
    .with_context_assembler(assembler)
    .with_instructions(Some(parts.instructions.activator()));
    let user_executor = UserExecutor::new(parts.gates.questions, parts.gates.approvals)
        .with_gate_journal(GateJournal::new(
            Arc::clone(&parts.conv_store),
            Arc::clone(&parts.call_id_to_thread),
        ))
        .with_approval_mode(parts.gates.approval_mode)
        .with_preapproval(crate::mode_policy::preapproval(&parts.config.tools));
    let user_executor = if parts.gates.park_approvals {
        user_executor.with_parked_approvals()
    } else {
        user_executor
    };
    let user_executor = user_executor.with_parked_questions(parts.parked_tx);
    let base = CompositeExecutorBuilder::default()
        .with_user_slot(Box::new(user_executor))
        .with_timers(Arc::new(SystemClock::new()))
        .with_audit_trail(audit_log_path, parts.audit_trail)
        .with_turn(turn_executor)
        .with_verify(verify_root);
    let composed = match parts.tool_executor_override {
        Some(factory) => base.with_tool_slot(factory(parts.conv_store, parts.call_id_to_thread)),
        None => {
            let tools = ToolExecutor::with_shared_store(
                parts.tool_registry,
                Default::default(),
                parts.call_id_to_thread,
                parts.conv_store,
            )
            .with_no_tools(parts.no_tools)
            .with_tool_result_token_cap(parts.config.agent.tool_result_token_cap)
            .with_repeat_notes(parts.config.agent.note_repeated_results)
            .with_chunk_session(chunk_session)
            .with_instructions(Some(parts.instructions.activator()));
            base.with_tool_executor(if parts.reconcile_capabilities {
                tools.with_capability_reconciliation()
            } else {
                tools
            })
        }
    };
    Box::new(composed.build())
}
