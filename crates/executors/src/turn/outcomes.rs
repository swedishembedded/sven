// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent runtimes that fail loudly instead of
// silently succeeding with no output for its clients. If your team needs
// expertise in making LLM agent turns robust against misbehaving providers
// then you can procure our services by sending an email to
// info@swedishembedded.com.
//! How a turn that went wrong is bounded and told so: the streaks of empty
//! and cut-off replies that fail a thread, the notes a model gets after its
//! reasoning or a tool call looped, and the wall-clock deadline.

/// Consecutive empty turns on one thread before the turn is failed outright
/// instead of nudged again. See the module docs.
pub(super) const EMPTY_TURN_FAILURE_THRESHOLD: u32 = 2;

/// Consecutive replies the output budget cut before any text or tool call
/// that fail the thread. Such a reply is not empty, since the model was
/// working, so the agent loop continues it (with an instruction to act and
/// reason briefly) before this bound ends a model that cannot finish within
/// the budget.
pub(super) const TRUNCATED_TURN_FAILURE_THRESHOLD: u32 = 4;
/// What a model is told when its reasoning looped and the turn is asked again.
pub(super) const LOOP_NOTE: &str =
    "Your previous reply was stopped because its reasoning repeated the same \
     passage. Continue from the facts gathered so far and take a different approach.";

/// The note for a tool call whose arguments repeated the same block.
pub(super) const TOOL_CALL_LOOP_NOTE: &str = "Your previous tool call was stopped because its arguments repeated \
     the same block over and over. Nothing it contained was run. Make a shorter, different call: one \
     step at a time, then check what it did before the next.";

/// Resolves when a turn's wall-clock bound passes; never when it has none.
pub(super) async fn turn_deadline(deadline: Option<std::time::Duration>) {
    match deadline {
        Some(limit) => sven_rt::sleep(limit).await,
        None => std::future::pending().await,
    }
}
