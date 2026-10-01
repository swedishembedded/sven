// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The kernel permission policy that goes with an interactive mode.

use sven_hsm::{PermissionPolicy, ToolCapability};
use sven_machines::{ReactiveAgentMachine, SdlcMachine, UiTestMachine, VerifiedTaskMachine};
use sven_vocab::{AgentMode, ApprovalMode};

/// The policy a session running `kernel_mode` enforces: the machine's own
/// (so, say, SDLC disallows writes outside Execution), under `approval`.
///
/// # Errors
///
/// Manual approval of a `ui-test` session: its machine drives a device step
/// by step and does not hold a call for a person's answer, and it runs only
/// dispatched, where nobody could give one.
pub(crate) fn session_policy(
    kernel_mode: &str,
    agent_mode: AgentMode,
    approval: ApprovalMode,
) -> anyhow::Result<PermissionPolicy> {
    anyhow::ensure!(
        !(kernel_mode == "ui-test" && approval == ApprovalMode::Manual),
        "manual approval is not available for a ui-test step: it runs dispatched, \
         with nobody to answer, and its machine does not wait for approvals"
    );
    let policy = match kernel_mode {
        "sdlc" => SdlcMachine::permission_policy(),
        "verified-task" => VerifiedTaskMachine::permission_policy(),
        "ui-test" => UiTestMachine::permission_policy(),
        _ => reactive_policy(agent_mode),
    };
    Ok(with_approval(policy, approval))
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
}
