// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that keep working inside a small
// model context window for its clients. If your team needs expertise in
// long-running agents on local models then you can procure our services by
// sending an email to info@swedishembedded.com.
//! Fork-style compaction: the conversation so far, unchanged, followed by one
//! request to write down everything needed to continue.
//!
//! The model that wrote the conversation is the one best placed to say what
//! in it still matters, so the compactor does not serialise the history into
//! text for a second model call. It sends the real message list (the same
//! system prompt, the same tool calls and results) and appends a single user
//! message, [`COMPACTION_PROMPT`]. The reply replaces the history it
//! summarises as one message ([`apply_summary`]).
//!
//! Everything here is pure; the executor owns the model call, the retries and
//! the store (`sven_executors`' `compaction`).

use sven_model::{Message, MessageContent, Role, ToolResultContent};
use sven_vocab::TodoItem;

use crate::compact::todo_block;

/// The final user message of a fork request.
pub const COMPACTION_PROMPT: &str = "\
The conversation above is about to be cut off to free context space, and you \
will continue the work from nothing but the notes you write now. Write those \
notes. Do not call tools; reply with the notes only.

Include everything needed to continue without re-reading anything:
- the goal and the assignment as it was given, and what counts as done;
- every constraint, rule and preference the requester or the project gave \
(style, gates, files not to touch, how to verify);
- decisions taken and why, including approaches that failed and why, so they \
are not retried;
- every file read, changed, created or deleted, with the key facts learned \
from it or the change made;
- commands run and what each returned (exit status, the decisive output);
- exact identifiers, names, paths, numbers, codes and strings the task still \
needs, copied verbatim - never paraphrased or rounded;
- the current state: what is done, what is half done, what is untested;
- open questions and assumptions that still need checking;
- the concrete next steps, in order.

Be dense. Prefer verbatim facts over narrative. Leave out pleasantries and \
anything that is no longer relevant.";

/// Margin, in tokens, kept free in a fork request for what the server's chat
/// template adds around the messages and for the estimate's error.
pub const FORK_MARGIN_TOKENS: usize = 256;

/// A fork request, and which part of the conversation it covers.
#[derive(Debug, Clone)]
pub struct ForkPlan {
    /// Send this: the system message, the covered messages, the prompt.
    pub request: Vec<Message>,
    /// How many of the conversation's messages after the system message the
    /// request covers (all of them when [`ForkPlan::whole`]).
    pub covered: usize,
    /// Whether the request covers the whole conversation, so the summary
    /// replaces all of it.
    pub whole: bool,
    /// Estimated tokens cut out of the request's tool results and messages
    /// because even the smallest piece of conversation did not fit. Zero in
    /// every normal case; non-zero is data loss the caller must report.
    pub clipped_tokens: usize,
}

/// Estimated tokens of `messages`, scaled by `calibration` (the server's
/// count per estimated token, at least 1.0).
#[must_use]
pub fn estimate_tokens(messages: &[Message], calibration: f32) -> usize {
    let raw = sven_model::budget::estimate_request_tokens(messages, &[], None);
    (raw as f32 * calibration.max(1.0)).ceil() as usize
}

/// Whether the conversation can be cut just before `messages[at]`: neither
/// inside a tool call's results nor between a call and its results, which
/// providers reject.
fn is_clean_boundary(messages: &[Message], at: usize) -> bool {
    if at == 0 || at >= messages.len() {
        return true;
    }
    let result_starts_here = matches!(messages[at].content, MessageContent::ToolResult { .. });
    let call_ends_before = matches!(messages[at - 1].content, MessageContent::ToolCall { .. });
    !result_starts_here && !call_ends_before
}

/// Plan the fork request for `messages` (a user message at the very end,
/// still waiting for its answer, is left out of it) so that the request, prompt included,
/// is estimated at no more than `budget_tokens`.
///
/// When the whole conversation fits, the request covers all of it. When it
/// does not, the request covers the largest oldest prefix that fits and ends
/// at a clean boundary; the caller applies that summary and plans again.
/// When not even the oldest tool exchange fits, its contents are clipped to
/// fit and [`ForkPlan::clipped_tokens`] says how much was lost.
#[must_use]
pub fn plan_fork(messages: &[Message], budget_tokens: usize, calibration: f32) -> ForkPlan {
    let system: Vec<Message> = messages
        .first()
        .filter(|m| m.role == Role::System)
        .cloned()
        .into_iter()
        .collect();
    let body = &messages[system.len()..];
    // A user message nobody has answered yet is not history: it stays
    // verbatim after the notes, for the model to answer.
    let pending = usize::from(body.len() > 2 && body.last().is_some_and(|m| m.role == Role::User));
    let body = &body[..body.len() - pending];
    let prompt = Message::user(COMPACTION_PROMPT);
    let fixed = estimate_tokens(&system, calibration)
        + estimate_tokens(std::slice::from_ref(&prompt), calibration);
    let room = budget_tokens.saturating_sub(fixed);

    let cost = |upto: usize| estimate_tokens(&body[..upto], calibration);
    // The task is carried over verbatim, so a request that covers only the
    // task would shrink nothing: a useful request reaches past it.
    let useful_from = if body.first().is_some_and(|m| m.role == Role::User) {
        2
    } else {
        1
    }
    .min(body.len());
    let fitting = (useful_from..=body.len())
        .rev()
        .find(|&n| is_clean_boundary(body, n) && cost(n) <= room);
    let mut clipped_tokens = 0;
    let mut slice: Vec<Message> = Vec::new();
    let covered = if let Some(n) = fitting {
        slice.extend_from_slice(&body[..n]);
        Some(n)
    } else {
        // Not even the first exchange fits: take it and clip it to the room.
        let n = (useful_from..=body.len())
            .find(|&n| is_clean_boundary(body, n))
            .unwrap_or(body.len());
        let before = cost(n);
        slice = clip_to(&body[..n], room, calibration);
        clipped_tokens = before.saturating_sub(estimate_tokens(&slice, calibration));
        Some(n)
    };
    let covered = covered.unwrap_or(0);
    let mut request = system;
    request.extend(slice);
    request.push(prompt);
    ForkPlan {
        request,
        covered,
        whole: covered == body.len(),
        clipped_tokens,
    }
}

/// `messages` with the text of each cut down (head and tail kept, the middle
/// replaced by a notice) until they are estimated at no more than `room`.
fn clip_to(messages: &[Message], room: usize, calibration: f32) -> Vec<Message> {
    let per_message_chars = (room * 4 / messages.len().max(1)).max(256);
    let per_message_chars = (per_message_chars as f32 / (calibration.max(1.0) * 1.1)) as usize;
    messages
        .iter()
        .map(|m| clip_message(m, per_message_chars))
        .collect()
}

fn clip_text(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    let half = max_chars / 2;
    let head = &text[..text.floor_char_boundary(half)];
    let tail_from = text.ceil_char_boundary(text.len() - half);
    format!(
        "{head}\n[... {} bytes cut to fit the context window ...]\n{}",
        text.len() - head.len() - (text.len() - tail_from),
        &text[tail_from..]
    )
}

fn clip_message(message: &Message, max_chars: usize) -> Message {
    let mut clipped = message.clone();
    match &mut clipped.content {
        MessageContent::Text(t) => *t = clip_text(t, max_chars),
        MessageContent::ToolResult {
            content: ToolResultContent::Text(t),
            ..
        } => *t = clip_text(t, max_chars),
        MessageContent::ToolCall { function, .. } => {
            function.arguments = clip_text(&function.arguments, max_chars);
        }
        _ => {}
    }
    clipped
}

/// The first user message of the conversation: in a run, the task itself.
fn task_message(body: &[Message]) -> Option<&Message> {
    body.iter().find(|m| m.role == Role::User)
}

/// The conversation after the fork's `summary` replaced the first
/// `plan.covered` messages after the system message: the system message, the
/// task verbatim, the summary as one message that tells the model to continue
/// from it, the todo list verbatim, then every message the plan did not
/// cover.
///
/// The summary is a user message, so the model's next turn answers it - in a
/// tool loop that is the instruction to carry on with the next step.
#[must_use]
pub fn apply_summary(
    messages: &[Message],
    plan: &ForkPlan,
    summary: &str,
    todos: &[TodoItem],
) -> Vec<Message> {
    let system = messages.first().filter(|m| m.role == Role::System);
    let body = &messages[usize::from(system.is_some())..];
    let (covered, rest) = body.split_at(plan.covered.min(body.len()));
    let task = task_message(covered);

    let mut note = format!(
        "[The conversation so far was compacted to free context space. These are \
         the notes you wrote from it; continue the work from them, do not redo \
         finished steps.]\n\n{}",
        summary.trim()
    );
    if let Some(block) = todo_block(todos) {
        note.push_str("\n\n");
        note.push_str(&block);
    }

    let mut out = Vec::with_capacity(rest.len() + 3);
    out.extend(system.cloned());
    out.extend(task.cloned());
    out.push(Message::user(note));
    out.extend_from_slice(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_model::FunctionCall;
    use sven_vocab::TodoStatus;

    fn same(a: &Message, b: &Message) -> bool {
        serde_json::to_string(a).unwrap() == serde_json::to_string(b).unwrap()
    }

    fn call(id: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: id.into(),
                function: FunctionCall {
                    name: "shell".into(),
                    arguments: format!("{{\"cmd\":\"cat {id}\"}}"),
                },
            },
        }
    }

    /// system, task, then `n` tool exchanges of `chars` characters each.
    fn run(n: usize, chars: usize) -> Vec<Message> {
        let mut m = vec![Message::system("sys"), Message::user("the task")];
        for i in 0..n {
            m.push(call(&format!("c{i}")));
            m.push(Message::tool_result(
                format!("c{i}"),
                format!("CODEWORD {i} {}", "x".repeat(chars)),
            ));
        }
        m
    }

    #[test]
    fn the_fork_sends_the_real_conversation_then_the_prompt_as_the_last_user_message() {
        let msgs = run(2, 100);
        let plan = plan_fork(&msgs, 100_000, 1.0);
        assert!(plan.whole);
        assert_eq!(plan.clipped_tokens, 0);
        assert_eq!(plan.request.len(), msgs.len() + 1);
        for (sent, original) in plan.request.iter().zip(&msgs) {
            assert!(same(sent, original), "history is sent as is");
        }
        let last = plan.request.last().unwrap();
        assert_eq!(last.role, Role::User);
        assert_eq!(last.as_text(), Some(COMPACTION_PROMPT));
    }

    #[test]
    fn the_prompt_asks_for_everything_a_continuation_needs() {
        for need in [
            "goal",
            "constraint",
            "decisions",
            "files",
            "commands",
            "verbatim",
            "current state",
            "open questions",
            "next steps",
        ] {
            assert!(COMPACTION_PROMPT.contains(need), "missing {need}");
        }
    }

    #[test]
    fn a_conversation_that_does_not_fit_is_summarised_in_its_largest_oldest_prefix() {
        let msgs = run(10, 4000); // ~1000 tokens per exchange
        let budget = 5_000;
        let plan = plan_fork(&msgs, budget, 1.0);
        assert!(!plan.whole);
        assert!(plan.covered > 2 && plan.covered < msgs.len() - 1);
        assert!(estimate_tokens(&plan.request, 1.0) <= budget);
        // One more exchange would not have fit.
        let more = plan_fork(&msgs[..msgs.len()], budget, 1.0);
        assert_eq!(more.covered, plan.covered);
        let bigger = {
            let mut p = msgs[..=plan.covered + 2].to_vec();
            p.push(Message::user(COMPACTION_PROMPT));
            estimate_tokens(&p, 1.0)
        };
        assert!(bigger > budget, "the prefix is the largest that fits");
        assert_eq!(plan.clipped_tokens, 0);
    }

    #[test]
    fn a_cut_never_separates_a_tool_call_from_its_result() {
        let msgs = run(10, 4000);
        for budget in (1_000..12_000).step_by(250) {
            let plan = plan_fork(&msgs, budget, 1.0);
            let sent = &plan.request[1..plan.request.len() - 1];
            if let Some(last) = sent.last() {
                assert!(
                    !matches!(last.content, MessageContent::ToolCall { .. }),
                    "budget {budget}: request ends inside a tool exchange"
                );
            }
            assert!(
                !matches!(
                    sent.first().map(|m| &m.content),
                    Some(MessageContent::ToolResult { .. })
                ),
                "budget {budget}: request starts with an orphan result"
            );
            // The remainder starts at a clean boundary too.
            let rest = &msgs[1 + plan.covered..];
            assert!(
                !matches!(
                    rest.first().map(|m| &m.content),
                    Some(MessageContent::ToolResult { .. })
                ),
                "budget {budget}: the remainder starts with an orphan result"
            );
        }
    }

    #[test]
    fn a_question_still_waiting_for_its_answer_stays_out_of_the_fork_and_after_the_notes() {
        let mut msgs = run(3, 50);
        msgs.push(Message::user("what is CODEWORD 2?"));
        let plan = plan_fork(&msgs, 100_000, 1.0);
        assert!(plan.whole);
        let sent: Vec<_> = plan.request.iter().filter_map(Message::as_text).collect();
        assert!(!sent.contains(&"what is CODEWORD 2?"));
        let out = apply_summary(&msgs, &plan, "notes", &[]);
        assert_eq!(out.last().unwrap().as_text(), Some("what is CODEWORD 2?"));
        assert_eq!(out.len(), 4, "system, task, notes, question: {out:?}");
    }

    #[test]
    fn calibration_makes_the_plan_cover_less() {
        let msgs = run(10, 4000);
        let plain = plan_fork(&msgs, 6_000, 1.0);
        let scaled = plan_fork(&msgs, 6_000, 2.0);
        assert!(scaled.covered < plain.covered);
    }

    #[test]
    fn a_single_exchange_larger_than_the_budget_is_clipped_and_the_loss_reported() {
        let msgs = run(1, 100_000); // ~25k tokens
        let plan = plan_fork(&msgs, 3_000, 1.0);
        assert!(plan.whole);
        assert!(plan.clipped_tokens > 20_000, "{}", plan.clipped_tokens);
        assert!(estimate_tokens(&plan.request, 1.0) <= 3_000);
        let result = plan
            .request
            .iter()
            .find_map(|m| match &m.content {
                MessageContent::ToolResult {
                    content: ToolResultContent::Text(t),
                    ..
                } => Some(t.clone()),
                _ => None,
            })
            .unwrap();
        assert!(
            result.starts_with("CODEWORD 0"),
            "the head survives the clip"
        );
        assert!(result.contains("cut to fit the context window"));
    }

    fn todos() -> Vec<TodoItem> {
        vec![TodoItem {
            id: "2".into(),
            content: "Replace the sleep".into(),
            status: TodoStatus::InProgress,
        }]
    }

    #[test]
    fn the_summary_replaces_the_history_and_keeps_the_task_and_todos_verbatim() {
        let msgs = run(3, 50);
        let plan = plan_fork(&msgs, 100_000, 1.0);
        let out = apply_summary(&msgs, &plan, "NOTES: CODEWORD 0 is A-1", &todos());
        assert_eq!(out.len(), 3, "system, task, summary: {out:?}");
        assert_eq!(out[0].role, Role::System);
        assert_eq!(out[1].as_text(), Some("the task"));
        assert_eq!(out[2].role, Role::User);
        let note = out[2].as_text().unwrap();
        assert!(note.contains("NOTES: CODEWORD 0 is A-1"));
        assert!(note.contains("[in_progress] 2: Replace the sleep"));
    }

    #[test]
    fn a_partial_summary_keeps_the_uncovered_messages_in_order() {
        let msgs = run(10, 4000);
        let plan = plan_fork(&msgs, 5_000, 1.0);
        let out = apply_summary(&msgs, &plan, "notes", &[]);
        assert_eq!(out.len(), 3 + msgs.len() - 1 - plan.covered);
        assert!(same(out.last().unwrap(), msgs.last().unwrap()));
        assert!(same(&out[3], &msgs[1 + plan.covered]));
    }

    #[test]
    fn a_second_compaction_folds_the_first_notes_in_and_keeps_one_task() {
        let msgs = run(2, 50);
        let first = apply_summary(&msgs, &plan_fork(&msgs, 100_000, 1.0), "first", &[]);
        let mut grown = first.clone();
        grown.extend(run(2, 50).into_iter().skip(2));
        let plan = plan_fork(&grown, 100_000, 1.0);
        let second = apply_summary(&grown, &plan, "second", &[]);
        assert_eq!(second.len(), 3);
        assert_eq!(
            second
                .iter()
                .filter(|m| m.as_text() == Some("the task"))
                .count(),
            1
        );
        assert!(second[2].as_text().unwrap().contains("second"));
    }
}
