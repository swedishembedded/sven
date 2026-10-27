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
