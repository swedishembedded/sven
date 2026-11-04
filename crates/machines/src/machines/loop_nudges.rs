// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent runtimes that keep a model on task
// for its clients. If your team needs expertise in agent loop design then
// you can procure our services by sending an email to info@swedishembedded.com.
//! The instructions the shared tool loop ([`super::loop_core`]) gives the
//! model when a turn went wrong: it came back empty, the output budget cut
//! it, or the round budget ran out.

/// Nudge instruction appended when the model emits an empty turn.
pub(super) const EMPTY_TURN_NUDGE: &str =
    "Your last response was empty. Please provide your analysis, answer, or \
     use one of the available tools to make progress.";

/// Continuation after a reply the output budget cut before it called a tool
/// or finished its answer - typically reasoning that ran to the limit.
pub(super) const TRUNCATED_TURN_NUDGE: &str =
    "Your last reply reached the output limit before it finished, so nothing \
     was run and no answer was given. Do not repeat that reasoning: act now - \
     make the tool call you were working towards (write the file you were \
     drafting, run the next command) - and keep further reasoning short.";

/// Wrap-up instruction fired when the model has answered the empty-turn
/// nudge with more empty turns past [`super::loop_core::EMPTY_TURN_NUDGE_BUDGET`]. Same
/// faithful-report demands as [`MAX_ROUNDS_NUDGE`] - a pressured model is
/// most likely to narrate unmade work exactly here.
pub(super) const EMPTY_TURN_WRAPUP_NUDGE: &str =
    "Your last several responses were all empty, so this turn is being \
     ended rather than nudged again. Respond now with whatever you have - \
     even a partial answer. Report faithfully what you actually did and \
     know; do not describe changes you have not made.";

/// Instruction appended when `max_tool_rounds` is exceeded.
///
/// This path is always schema-less (see the wrap-up construction), and it is
/// the moment a pressured model is most likely to describe work it never did
/// as if it existed. The wording demands a faithful report.
pub(super) const MAX_ROUNDS_NUDGE: &str =
    "You have reached the maximum tool-call budget. Do not call any more tools. \
     Respond now with your final reply. Report faithfully what you actually \
     changed and what the checks showed; if the task is incomplete, say \
     precisely what remains - do not describe changes you have not made.";

/// Budget-progress guidance for the continuation turn at `round` of `max`
/// rounds, if one is due.
///
/// A model that spends its whole round budget on discovery never learns to
/// switch to making changes until the terminal wrap-up fires, which is too
/// late to act on. The loop tells it where it stands twice: at half the
/// budget, and shortly before the end. Stateless by construction - the
/// round counter crosses each threshold exactly once, so no extra persisted
/// stage is needed. The loop gives it only when no stall redirect is armed:
/// that redirect is the more urgent instruction.
pub(super) fn progress_instruction(round: u32, max: u32) -> Option<String> {
    let half = max.div_ceil(2);
    let late = max.saturating_sub(max / 5);
    if round == half && round < late {
        Some(format!(
            "You have used {round} of your {max} tool-call rounds - half the budget. \
             Move from reading and exploring to making the required changes now; \
             keep further tool calls economical."
        ))
    } else if round == late {
        Some(format!(
            "You have {} of {max} tool-call rounds left. Finish the required \
             changes within them; do not start new exploration.",
            max.saturating_sub(round)
        ))
    } else {
        None
    }
}
