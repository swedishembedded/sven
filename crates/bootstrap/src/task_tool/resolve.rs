// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What a `task` spawn resolves to before anything starts: whether the parent
//! may delegate in the requested mode, the child's run contract, its working
//! directory, and the mode, prompt and model it runs with.
//!
//! Swedish Embedded AB implements solutions for bounding what delegated
//! agents may do for its clients. If your team needs expertise in
//! permission ceilings and delegation contracts for agent runtimes, you can
//! procure our services by sending an email to info@swedishembedded.com.
use std::path::PathBuf;
use std::time::{Duration, Instant};

use sven_hsm::ChildRunContract;
use sven_tool_api::PathScope;
use sven_vocab::{AgentMode, ApprovalMode};
use sven_workspace::AgentInfo;

/// Whether a parent in `parent` mode may start a child in ACP mode `child`.
///
/// The child mode's own policy - including what its tools do without asking
/// anyone - must stay within what the parent holds in every state (the
/// contract's ceiling, `mode_policy::session_ceiling`). An `agent` child
/// writes, so only an `agent` parent may start one; an SDLC parent holds only
/// reads in every state, so it may start no child at all. This check is what
/// makes the tool's `ReadFile` kernel capability honest: the kernel admits a
/// `task` call wherever reads are allowed, so the tool itself must not turn a
/// read into more.
pub(crate) fn child_mode_allowed(parent: AgentMode, child: &str) -> Result<(), String> {
    use crate::mode_policy::{exceeds, session_ceiling};
    let child_mode = match child {
        "research" => AgentMode::Research,
        "plan" => AgentMode::Plan,
        _ => AgentMode::Agent,
    };
    match exceeds(&session_ceiling(child_mode), &session_ceiling(parent)) {
        None => Ok(()),
        Some(capability) => Err(format!(
            "a {parent:?} session cannot delegate to a '{child}' sub-agent: the sub-agent \
             would hold {capability:?}, which this session does not"
        )),
    }
}

/// The contract a child of a session in `parent` mode under `approval` runs
/// under, started at `now`.
pub(super) fn child_contract(
    parent: AgentMode,
    approval: ApprovalMode,
    wall_clock: Option<Duration>,
    now: Instant,
) -> ChildRunContract {
    let ceiling = crate::mode_policy::session_ceiling(parent);
    let contract = ChildRunContract::new(crate::mode_policy::with_approval(ceiling, approval));
    match wall_clock {
        Some(budget) => contract.with_deadline_after(now, budget),
        None => contract,
    }
}

/// The directory the child runs in: `requested` resolved through `session`,
/// the scope it must stay inside, or the scope's root when none is named. A
/// child confined to the parent's tree cannot be pointed at another
/// repository by the model.
pub(super) fn child_workdir(
    requested: Option<&str>,
    session: &PathScope,
) -> Result<PathBuf, String> {
    let requested = requested.unwrap_or(".");
    let dir = session.resolve(requested).map_err(|e| e.to_string())?;
    if dir.is_dir() {
        Ok(dir)
    } else {
        Err(format!(
            "workdir '{requested}' is not an existing directory"
        ))
    }
}

/// What a requested `mode` resolves to: the ACP session mode to run, the
/// prompt actually sent to the sub-agent, and the model the persona asks for.
#[derive(Debug)]
pub(super) struct Resolved {
    /// Built-in ACP session mode (`research` / `plan` / `agent`).
    pub(super) acp_mode: String,
    /// Prompt with the persona's system prompt prepended, if any.
    pub(super) prompt: String,
    /// Persona's `model:` front-matter, already normalised (`inherit` -> `None`).
    pub(super) model: Option<String>,
}

/// Resolve a requested `mode` against the three built-in modes and any
/// discovered subagent persona.
///
/// A persona match prepends its `content` (system prompt) to `prompt`, carries
/// its `model` through for [`effective_model`], and maps `readonly` to an ACP
/// mode: `true` -> `research` (read-only tool profile), `false` -> `agent`
/// (full read/write tool profile). Built-in mode names always take priority
/// over a same-named persona.
pub(super) fn resolve_mode_and_prompt(
    mode: &str,
    prompt: &str,
    agents: &[AgentInfo],
) -> Result<Resolved, String> {
    if matches!(mode, "research" | "plan" | "agent") {
        return Ok(Resolved {
            acp_mode: mode.to_string(),
            prompt: prompt.to_string(),
            model: None,
        });
    }

    if let Some(persona) = agents.iter().find(|a| a.name == mode) {
        let acp_mode = if persona.readonly {
            "research"
        } else {
            "agent"
        };
        return Ok(Resolved {
            acp_mode: acp_mode.to_string(),
            prompt: format!("{}\n\n---\n\n## Task\n\n{prompt}", persona.content.trim()),
            model: persona.model.clone(),
        });
    }

    let mut valid: Vec<&str> = vec!["research", "plan", "agent"];
    valid.extend(agents.iter().map(|a| a.name.as_str()));
    Err(format!(
        "unknown mode '{mode}'. Valid options: {}",
        valid.join(", ")
    ))
}

/// Pick the model a sub-agent runs on, most specific wins: the caller's
/// explicit `model` argument, then the persona's `model:` front-matter, then
/// the session's own model.  `None` leaves the child to its own default.
pub(super) fn effective_model(
    explicit: Option<String>,
    persona: Option<String>,
    session_default: Option<String>,
) -> Option<String> {
    explicit.or(persona).or(session_default)
}
