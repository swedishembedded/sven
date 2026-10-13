// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The kernel permission policy that goes with an interactive mode.

use std::sync::Arc;

use sven_hsm::submachine::ErasedMachine;
use sven_hsm::{PermissionPolicy, ToolCapability};
use sven_machines::{ModeRegistry, ReactiveAgentMachine, SdlcMachine};
use sven_vocab::{AgentMode, ApprovalMode};

/// The policy a session running `kernel_mode` enforces: the mode's
/// registered machine policy (so, say, SDLC disallows writes outside
/// Execution), narrowed by the host for the read-only `AgentMode`s, under
/// `approval`.
///
/// The mode's own policy comes from its registry registration, never from
/// this name: a mode nobody registered has no policy, and is refused here
/// rather than silently granted a default.
///
/// # Errors
///
/// An unregistered `kernel_mode`; and manual approval of a `ui-test`
/// session — its machine drives a device step by step and does not hold a
/// call for a person's answer, and it runs only dispatched, where nobody
/// could give one.
pub(crate) fn session_policy(
    registry: &ModeRegistry,
    kernel_mode: &str,
    agent_mode: AgentMode,
    approval: ApprovalMode,
) -> anyhow::Result<PermissionPolicy> {
    anyhow::ensure!(
        !(kernel_mode == "ui-test" && approval == ApprovalMode::Manual),
        "manual approval is not available for a ui-test step: it runs dispatched, \
         with nobody to answer, and its machine does not wait for approvals"
    );
    let registration = registry.get(kernel_mode).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown mode {:?}; it has no registered machine policy, so it cannot run",
            kernel_mode
        )
    })?;
    Ok(with_approval(
        host_narrowing(registration.policy.clone(), agent_mode),
        approval,
    ))
}

/// A machine and the policy its session runs it under.
pub(crate) type SessionMachine = (Box<dyn ErasedMachine>, PermissionPolicy);

/// The machine a session runs and its policy: the caller's own `supplied`
/// machine, or the one registered for `mode`.
///
/// A supplied machine arrives with the policy its host answers for (no
/// policy is guessed from the mode name), but that policy is bounded by the
/// session exactly as a registered one is: the host's narrowing for the
/// read-only modes and the approval mode apply to it too. A registered mode
/// is looked up before anything starts, and an unregistered one is refused.
///
/// # Errors
///
/// An unregistered `mode`, or any refusal of [`session_policy`].
pub(crate) fn session_machine(
    supplied: Option<SessionMachine>,
    registry: Option<Arc<ModeRegistry>>,
    mode: &str,
    agent_mode: Option<AgentMode>,
    approval: ApprovalMode,
) -> anyhow::Result<SessionMachine> {
    let agent_mode = agent_mode.unwrap_or(AgentMode::Agent);
    if let Some((machine, policy)) = supplied {
        let policy = supplied_machine_policy(policy, agent_mode, approval);
        return Ok((machine, policy));
    }
    let registry = registry.unwrap_or_else(|| Arc::new(crate::modes::mode_registry()));
    let registration = registry.get(mode).ok_or_else(|| {
        anyhow::anyhow!("unknown mode {:?}; available: {:?}", mode, registry.modes())
    })?;
    let policy = session_policy(&registry, mode, agent_mode, approval)?;
    Ok(((registration.factory)(), policy))
}

/// The policy a caller-supplied machine runs under: its own, narrowed by the
/// host for the read-only `AgentMode`s and put under `approval` - the same
/// two bounds [`session_policy`] applies to a registered machine's policy.
pub(crate) fn supplied_machine_policy(
    policy: PermissionPolicy,
    agent_mode: AgentMode,
    approval: ApprovalMode,
) -> PermissionPolicy {
    with_approval(host_narrowing(policy, agent_mode), approval)
}

/// What the host withholds from a machine's registered policy for the
/// session's interactive mode: the read-only planning modes (`Plan` /
/// `Research`) never write files, whatever machine runs beneath them. The
/// registration's policy stays untouched; this only narrows it — the first
/// intersection of the authorization model (host ∩ machine).
fn host_narrowing(policy: PermissionPolicy, agent_mode: AgentMode) -> PermissionPolicy {
    match agent_mode {
        AgentMode::Plan | AgentMode::Research => {
            policy.intersect(&ReactiveAgentMachine::plan_permission_policy())
        }
        _ => policy,
    }
}

/// The calls manual approval approves without asking: a `shell` command
/// matching `tools.auto_approve_patterns` (and no deny pattern).
pub(crate) fn preapproval(tools: &crate::config::ToolsConfig) -> sven_executors::Preapproval {
    let policy = tools.policy();
    std::sync::Arc::new(move |call: &sven_hsm::GatedCall| {
        call.name == "shell"
            && call
                .args
                .get("shell_command")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|command| {
                    policy.decide(command) == sven_tool_api::ApprovalPolicy::Auto
                })
    })
}

/// `policy` under `approval`: manual approval asks about every call that is
/// not read-only; auto asks about none.
pub(crate) fn with_approval(policy: PermissionPolicy, approval: ApprovalMode) -> PermissionPolicy {
    match approval {
        ApprovalMode::Auto => policy,
        ApprovalMode::Manual => policy.with_manual_approval(),
    }
}

/// The policy the reactive agent machine runs under in `mode`: the read-only
/// planning modes withhold `WriteFile`, so the kernel forbids file mutations
/// even if the model proposes one; every other mode gets the full policy.
pub(crate) fn reactive_policy(mode: AgentMode) -> PermissionPolicy {
    match mode {
        AgentMode::Plan | AgentMode::Research => ReactiveAgentMachine::plan_permission_policy(),
        _ => ReactiveAgentMachine::permission_policy(),
    }
}

/// What a session in `mode` may do whatever state its machine is in: the
/// authority a tool that runs inside the session can hand on, since it
/// cannot tell which state the session is in.
///
/// `chat` runs under the full reactive policy, but its tools are read-only
/// (the writing tools run only in `agent` mode), so what it can hand on is
/// the read-only policy.
// The ceilings below restate each mode's registered policy by machine, not
// by registry lookup, because they answer "what may a *kind* of session
// hand on" for every session of that kind, including hypothetical ones —
// a registry lookup would answer only for the modes this build registers.
pub(crate) fn session_ceiling(mode: AgentMode) -> PermissionPolicy {
    match mode {
        AgentMode::Sdlc => SdlcMachine::permission_policy().ceiling_in_every_state(),
        AgentMode::Chat => reactive_policy(AgentMode::Plan).ceiling_in_every_state(),
        mode => reactive_policy(mode).ceiling_in_every_state(),
    }
}

/// The most a session in `mode` can ever do, in whichever state: its
/// machine's policy across all states, as [`session_ceiling`] maps it. A
/// reactive session also starts sub-agents through its `task` tool (which
/// the kernel admits as a read), so it holds `SpawnChild` as an SDLC
/// session's Execution state does.
pub(crate) fn mode_authority(mode: AgentMode) -> PermissionPolicy {
    if mode == AgentMode::Sdlc {
        return SdlcMachine::permission_policy().ceiling_in_any_state();
    }
    let reactive = session_ceiling(mode);
    PermissionPolicy::builder()
        .allow_globally(ToolCapability::ALL.into_iter().filter(|&cap| {
            cap == ToolCapability::SpawnChild || reactive.allows_in_every_state(cap)
        }))
        .build()
}

/// The modes a session that started in `start` may switch itself to: those
/// that can do nothing, in any of their states, that `start` cannot do in
/// one of its own. Comparing only what every state may do would let a
/// read-only session switch to `sdlc` and write in its Execution state.
pub(crate) fn modes_within(start: AgentMode) -> Vec<AgentMode> {
    let authority = mode_authority(start);
    [
        AgentMode::Research,
        AgentMode::Plan,
        AgentMode::Agent,
        AgentMode::Chat,
        AgentMode::Sdlc,
    ]
    .into_iter()
    .filter(|&mode| exceeds(&mode_authority(mode), &authority).is_none())
    .collect()
}

/// The first capability `child` grants beyond `ceiling`: one `ceiling` does
/// not allow, or one it allows only with an approval that `child` waives.
/// `None` when `child` stays within `ceiling`.
pub(crate) fn exceeds(
    child: &PermissionPolicy,
    ceiling: &PermissionPolicy,
) -> Option<ToolCapability> {
    ToolCapability::ALL.into_iter().find(|&cap| {
        child.allows_in_every_state(cap)
            && (!ceiling.allows_in_every_state(cap)
                || (ceiling.requires_approval(cap) && !child.requires_approval(cap)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_machines::{ModeRegistry, ReactiveAgentMachine, SdlcMachine};

    /// A session may switch only to a mode that can do nothing, in any of
    /// its states, that the starting mode cannot do in one of its own.
    #[test]
    fn a_mode_switch_never_gains_what_any_state_of_the_start_lacks() {
        for start in [AgentMode::Research, AgentMode::Plan, AgentMode::Chat] {
            assert!(
                !modes_within(start).contains(&AgentMode::Sdlc),
                "{start:?} -> sdlc would gain Execution's writes"
            );
        }
        assert!(modes_within(AgentMode::Agent).contains(&AgentMode::Sdlc));
        for target in modes_within(AgentMode::Sdlc) {
            assert!(
                exceeds(&mode_authority(target), &mode_authority(AgentMode::Sdlc)).is_none(),
                "sdlc -> {target:?}"
            );
        }
        assert!(
            !modes_within(AgentMode::Sdlc).contains(&AgentMode::Agent),
            "agent reaches the network, which no sdlc state does"
        );
        assert!(modes_within(AgentMode::Sdlc).contains(&AgentMode::Sdlc));
    }

    /// A mode nobody registered has no policy: building the session is
    /// refused, and nothing falls back to the reactive policy — a silent
    /// default grant is how an unknown mode would get away with everything
    /// the reactive agent may do.
    #[test]
    fn an_unregistered_mode_has_no_policy_and_cannot_run() {
        let registry = ModeRegistry::default_registry();
        let err = session_policy(&registry, "teleport", AgentMode::Agent, ApprovalMode::Auto)
            .expect_err("an unregistered mode must be refused");
        assert!(
            err.to_string().contains("teleport"),
            "the error names the mode: {err}"
        );
    }

    /// The policy a session runs is the mode's *registered* policy under the
    /// session's approval mode — per-state grants intact (sdlc writes in
    /// Execution, not in Planning).
    #[test]
    fn a_session_runs_its_registered_policy_under_its_approval_mode() {
        let registry = ModeRegistry::default_registry();
        let policy = session_policy(&registry, "sdlc", AgentMode::Sdlc, ApprovalMode::Auto)
            .expect("sdlc is registered");
        assert_eq!(policy, SdlcMachine::permission_policy());
        use sven_machines::machines::sdlc::SdlcState;
        assert!(
            policy.allows(&SdlcState::Execution, ToolCapability::WriteFile),
            "the Execution state writes"
        );
        assert!(
            !policy.allows(&SdlcState::Planning, ToolCapability::WriteFile),
            "Planning does not"
        );
    }

    /// The host narrows, never widens: a `plan` session on the reactive
    /// machine cannot write, and the same session on a machine whose
    /// registered policy allows nothing (predict) stays at nothing.
    #[test]
    fn host_narrowing_never_widens_a_registered_policy() {
        let registry = ModeRegistry::default_registry();
        let plan = session_policy(&registry, "agent", AgentMode::Plan, ApprovalMode::Auto)
            .expect("agent is registered");
        assert!(
            !plan.allows_in_every_state(ToolCapability::WriteFile),
            "a plan session never writes files"
        );
        assert!(
            plan.allows_in_every_state(ToolCapability::ReadFile),
            "reading stays"
        );

        let predict = session_policy(&registry, "predict", AgentMode::Agent, ApprovalMode::Auto)
            .expect("predict is registered");
        for cap in ToolCapability::ALL {
            assert!(
                !predict.allows_in_every_state(cap),
                "predict's registered policy allows nothing; {cap:?} leaked in"
            );
        }
    }

    /// Manual approval only adds approval requirements; auto leaves the
    /// registered policy exactly as registered.
    #[test]
    fn approval_narrows_but_never_widens() {
        let registry = ModeRegistry::default_registry();
        let auto = session_policy(&registry, "agent", AgentMode::Agent, ApprovalMode::Auto)
            .expect("agent is registered");
        let manual = session_policy(&registry, "agent", AgentMode::Agent, ApprovalMode::Manual)
            .expect("agent is registered");
        assert!(!auto.requires_approval(ToolCapability::ExecuteShell));
        assert!(manual.requires_approval(ToolCapability::ExecuteShell));
        for cap in ToolCapability::ALL {
            assert!(
                !(auto.allows_in_every_state(cap) && !manual.allows_in_every_state(cap)),
                "manual approval must not withdraw {cap:?}"
            );
        }
    }

    /// The registered policy of a mode is its machine's own — pinned for the
    /// reactive pair here, per-mode in `sven-machines`.
    #[test]
    fn the_registry_policy_comes_from_the_registration_not_the_name() {
        let mut registry = ModeRegistry::default_registry();
        // A mode named "chat" with a different machine is that machine's
        // policy, not what "chat" used to grant: the name carries nothing.
        registry.register(
            "chat",
            Box::new(|| -> Box<dyn sven_hsm::submachine::ErasedMachine> {
                Box::new(sven_hsm::dispatch::Hsm::new(SdlcMachine::new()))
            }),
            SdlcMachine::permission_policy(),
        );
        let policy = session_policy(&registry, "chat", AgentMode::Agent, ApprovalMode::Auto)
            .expect("chat is registered");
        assert_eq!(policy, SdlcMachine::permission_policy());
        assert_ne!(
            policy,
            ReactiveAgentMachine::permission_policy(),
            "what a mode named \"chat\" grants is its machine's, not what the \
             reactive machine used to grant every unmatched name"
        );
    }
}
