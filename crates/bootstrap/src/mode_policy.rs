// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The kernel permission policy that goes with an interactive mode.

use sven_config::AgentMode;
use sven_hsm::{PermissionPolicy, ToolCapability};
use sven_machines::{ReactiveAgentMachine, SdlcMachine};

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
