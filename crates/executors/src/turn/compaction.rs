// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Compaction of a thread whose next request would not fit the model's
//! window: the model writes notes on the conversation (a fork of it), and
//! the notes replace the history they cover.

use sven_hsm::{CompactionStrategyUsed, ObservationSink, UiEvent};
use sven_model::Message;
use sven_turn::{apply_summary, plan_fork, stream_turn, ForkPlan};
use sven_vocab::CompactionRecord;
use tokio::sync::mpsc;

use super::{limits, TurnExecutor};
use crate::thread_store::snapshot_thread;

/// Non-system messages an emergency trim keeps: the last resort that drops
/// history without a summary.
pub(super) const EMERGENCY_KEEP: usize = 6;

/// Fork rounds one compaction may take. Each round summarises the largest
/// oldest prefix that fits and shrinks the thread, so a conversation needs
/// more than one only when it is several windows long.
const MAX_ROUNDS: usize = 8;

/// Times a compaction re-plans because the server counted a fork larger than
/// the estimate said, before it gives up on summarising.
const MAX_MISJUDGED: usize = 3;

/// What asking the model for the notes of a fork came to.
enum Notes {
    /// The notes.
    Text(String),
    /// The server rejected the request as larger than its context, and said
    /// how large it counted it: `tokens_per_estimate` times the estimate.
    TooLarge { tokens_per_estimate: f32 },
    /// No notes, from either attempt.
    None,
}

/// What a compaction works against: the window and how the server counts.
#[derive(Clone, Copy)]
pub(super) struct Fit {
    /// The model's context window, in tokens.
    pub(super) window: u32,
    /// The request size at which the thread is compacted
    /// ([`super::limits::compaction_limit`]).
    pub(super) limit: usize,
    /// Factor by which the server's counts say estimates run low.
    pub(super) calibration: f32,
}

impl TurnExecutor {
    /// The todo list the agent keeps outside the conversation, which every
    /// compaction puts back verbatim.
    pub(super) fn pinned_todos(&self) -> Vec<sven_vocab::TodoItem> {
        self.store
            .lock()
            .map(|store| store.todos().to_vec())
            .unwrap_or_default()
    }

    /// Makes the history an overflow retry was sent with (if there was one)
    /// the thread's own and records it as an emergency compaction, so the
    /// next request starts from it instead of overflowing again.
    pub(super) fn keep_overflow_trim(
        &self,
        thread_id: &str,
        trimmed: Option<Vec<Message>>,
        obs: &ObservationSink,
    ) {
        let (Some(trimmed), Ok(mut store)) = (trimmed, self.store.lock()) else {
            return;
        };
        let tokens_before: usize = store
            .snapshot(thread_id)
            .iter()
            .map(Message::approx_tokens)
            .sum();
        let tokens_after: usize = trimmed.iter().map(Message::approx_tokens).sum();
        store.replace_thread(thread_id, trimmed);
        store.record_compaction(CompactionRecord {
            thread: thread_id.to_string(),
            tokens_before,
            tokens_after,
            strategy: CompactionStrategyUsed::Emergency,
            turn: 0,
        });
        drop(store);
        self.prompt_counts.forget(thread_id);
        warn_history_dropped(obs, tokens_before.saturating_sub(tokens_after));
        obs.emit(UiEvent::ContextCompacted {
            tokens_before,
            tokens_after,
            strategy: CompactionStrategyUsed::Emergency,
            turn: 0,
        });
    }

    /// Says, once per session and again whenever it changes, when this
    /// session compacts and what room a response has, so that neither a
    /// compaction nor a cut-off response comes as a surprise.
    pub(super) fn announce_budget(
        &mut self,
        context_window: Option<u32>,
        configured_max_output: Option<u32>,
        obs: &ObservationSink,
    ) {
        let thinking_cap = self
            .limits
            .thinking
            .max_thinking_tokens
            .map_or_else(|| "none".to_string(), |cap| format!("{cap} tokens"));
        let line = match self.compaction_fit(context_window, configured_max_output) {
            Some(fit) => format!(
                "context budget: window {} tokens; compaction at ~{} tokens of request; \
                 {} tokens kept free for the response; notes of at most {} tokens; \
                 thinking cap {thinking_cap}",
                fit.window,
                fit.limit,
                self.compaction.output_reserve_tokens(fit.window),
                self.compaction.reply_cap(fit.window),
            ),
            None => format!(
                "context budget: the model's window is unknown, so the conversation is never \
                 compacted ahead of a request; thinking cap {thinking_cap}"
            ),
        };
        if self.announced_budget.as_deref() != Some(line.as_str()) {
            tracing::info!("{line}");
            // Nothing to be surprised by when no window is known: no
            // compaction will happen. Only the log carries that line.
            if context_window.is_some() {
                obs.emit(UiEvent::Warning(line.clone()));
            }
            self.announced_budget = Some(line);
        }
    }

    /// What a compaction works against, when the window is known: the request
    /// size at which it fires (see [`limits::compaction_limit`]).
    pub(super) fn compaction_fit(
        &self,
        context_window: Option<u32>,
        configured_max_output: Option<u32>,
    ) -> Option<Fit> {
        let window = context_window?;
        let budget =
            sven_model::budget::effective_input_budget(context_window, configured_max_output)?;
        let prompt_tokens =
            sven_turn::estimate_tokens(&[Message::user(sven_turn::COMPACTION_PROMPT)], 1.0);
        let limit = limits::compaction_limit(
            window,
            budget,
            self.compaction.effective_threshold(),
            self.compaction.output_reserve_tokens(window),
            prompt_tokens,
            self.compaction.reply_cap(window),
        );
        Some(Fit {
            window,
            limit,
            calibration: 1.0,
        })
    }

    /// Whether the request about to be sent, holding `sent`, must be preceded
    /// by a compaction, and against what. `plain_estimate` is the chars/4
    /// estimate of the whole request (tools and instructions included); the
    /// server's count of the thread's last request, when there is one, takes
    /// its place for the part both share. `None` when the window is unknown
    /// (nothing to compare against) or the request still fits.
    pub(super) fn compaction_due(
        &self,
        thread_id: &str,
        sent: &[Message],
        plain_estimate: usize,
        context_window: Option<u32>,
        configured_max_output: Option<u32>,
    ) -> Option<Fit> {
        let fit = self.compaction_fit(context_window, configured_max_output)?;
        let (estimate, calibration) = self.prompt_counts.estimate(thread_id, sent, plain_estimate);
        (estimate >= fit.limit).then_some(Fit { calibration, ..fit })
    }

    /// A response that spent its whole output budget thinking, with no text
    /// and no tool call, says the conversation leaves too little room to
    /// think in. Reports it with the tokens used and, once per turn, compacts
    /// the thread so the caller can ask again (the reasoning of the failed
    /// attempt is not part of the conversation, as the chat template drops
    /// reasoning of earlier turns anyway). The effect to run again, when the
    /// turn is to be retried (its instruction is already in the thread).
    pub(super) async fn recover_exhausted_output(
        &mut self,
        req: &mut sven_vocab::TurnRequest,
        turn: &sven_turn::StreamedTurn,
        output_budget: Option<u32>,
        model: &dyn sven_model::ModelProvider,
        fit: Option<Fit>,
        obs: &ObservationSink,
    ) -> Option<sven_hsm::Effect> {
        let thread_id = req.thread.as_str();
        let exhausted = turn.truncated && turn.text.trim().is_empty() && turn.tool_calls.is_empty();
        if !exhausted {
            self.exhaustion_retried.remove(thread_id);
            return None;
        }
        let spent = output_budget.map_or_else(|| "its".to_string(), |b| b.to_string());
        let retry = fit.filter(|_| !self.exhaustion_retried.contains(thread_id));
        let outcome = if retry.is_some() {
            "compacting the conversation and asking again once"
        } else {
            "not retrying"
        };
        obs.emit(UiEvent::Warning(format!(
            "the response ran out of {spent} output tokens while thinking (~{} thinking tokens, \
             no answer yet); {outcome}",
            turn.thinking_tokens
        )));
        let fit = retry?;
        self.exhaustion_retried.insert(thread_id.to_string());
        let messages = snapshot_thread(&self.store, thread_id);
        let before = messages.len();
        let compacted = self
            .compact_thread(thread_id, messages, model, fit, obs)
            .await;
        if compacted.len() >= before {
            return None;
        }
        req.instruction.clear();
        req.delay_ms = None;
        Some(sven_hsm::Effect::CallLlm {
            request: req.to_value(),
        })
    }

    /// Compact `messages` for `thread_id`, write the result back to the
    /// shared store, and return it so the in-flight turn continues with the
    /// smaller history immediately.
    ///
    /// The model is asked, in a fork of the conversation (the real messages,
    /// then one instruction), for notes that let it continue; the notes
    /// replace the history. A conversation too large for one fork is
    /// summarised in pieces, the largest oldest prefix that fits first. Only
    /// when the model cannot produce notes at all does the model-free
    /// emergency trim run, and it says how much it dropped.
    ///
    /// Emits `UiEvent::ContextCompacted` once, with the strategy that ended
    /// up holding the thread.
    pub(super) async fn compact_thread(
        &self,
        thread_id: &str,
        messages: Vec<Message>,
        model: &dyn sven_model::ModelProvider,
        fit: Fit,
        obs: &ObservationSink,
    ) -> Vec<Message> {
        // The task and one more message: nothing a summary could shrink.
        if messages
            .iter()
            .filter(|m| m.role != sven_model::Role::System)
            .count()
            < 3
        {
            return messages;
        }
        let tokens_before: usize = messages.iter().map(Message::approx_tokens).sum();
        let todos = self.pinned_todos();
        let reply_cap = self.compaction.reply_cap(fit.window);
        let request_budget = (fit.window as usize).saturating_sub(reply_cap);

        let mut history = messages;
        let mut strategy = CompactionStrategyUsed::Fork;
        let mut calibration = fit.calibration;
        let (mut rounds, mut misjudged) = (0, 0);
        while rounds < MAX_ROUNDS {
            let plan = plan_fork(&history, request_budget, calibration);
            if plan.clipped_tokens > 0 {
                obs.emit(UiEvent::Warning(format!(
                    "compaction: a single exchange does not fit the context window; \
                     ~{} tokens of it were cut before it was summarised",
                    plan.clipped_tokens
                )));
            }
            let before = estimate(&history, calibration);
            let notes = match self
                .fork_notes(model, &plan, reply_cap, fit.window, calibration)
                .await
            {
                Notes::Text(notes) => notes,
                Notes::TooLarge {
                    tokens_per_estimate,
                } if misjudged < MAX_MISJUDGED => {
                    // The server counted the request larger than estimated
                    // (dense text, a burst of tool results): plan again with
                    // its own ratio, so the piece that fits is smaller.
                    misjudged += 1;
                    calibration = calibration.max(tokens_per_estimate * 1.05);
                    tracing::info!(
                        calibration,
                        "compaction: the fork was too large; planning again"
                    );
                    continue;
                }
                Notes::TooLarge { .. } | Notes::None => {
                    strategy = CompactionStrategyUsed::Emergency;
                    self.emergency_trim(&mut history, &todos, obs);
                    break;
                }
            };
            rounds += 1;
            history = apply_summary(&history, &plan, &notes, &todos);
            let after = estimate(&history, calibration);
            tracing::info!(
                thread = %thread_id, rounds, whole = plan.whole, before, after,
                notes_tokens = notes.len() / 4, "compaction: summarised"
            );
            if plan.whole || after < fit.limit {
                break;
            }
            if after >= before {
                // The notes were no smaller than what they replaced: another
                // round would repeat this one.
                strategy = CompactionStrategyUsed::Emergency;
                self.emergency_trim(&mut history, &todos, obs);
                break;
            }
        }

        let tokens_after: usize = history.iter().map(Message::approx_tokens).sum();
        if let Ok(mut store) = self.store.lock() {
            store.replace_thread(thread_id, history.clone());
            store.record_compaction(CompactionRecord {
                thread: thread_id.to_string(),
                tokens_before,
                tokens_after,
                strategy: strategy.clone(),
                turn: 0,
            });
        }
        // The count the server gave for the old conversation is of no use
        // for the new one: the next request is counted from nothing.
        self.prompt_counts.forget(thread_id);
        obs.emit(UiEvent::ContextCompacted {
            tokens_before,
            tokens_after,
            strategy,
            turn: 0,
        });
        history
    }

    /// The last resort: drop older history without a summary. Says how much.
    fn emergency_trim(
        &self,
        history: &mut Vec<Message>,
        todos: &[sven_vocab::TodoItem],
        obs: &ObservationSink,
    ) {
        let before: usize = history.iter().map(Message::approx_tokens).sum();
        let system = history
            .first()
            .filter(|m| m.role == sven_model::Role::System)
            .cloned();
        sven_turn::emergency_compact(history, system, EMERGENCY_KEEP, todos);
        let after: usize = history.iter().map(Message::approx_tokens).sum();
        warn_history_dropped(obs, before.saturating_sub(after));
    }

    /// Ask the model for the notes of a fork `plan`: with reasoning switched
    /// off, so that the reply budget cannot be spent thinking; then, if that
    /// fails or comes back empty, once more with the provider's default.
    /// [`Notes::None`] when neither attempt produced text.
    async fn fork_notes(
        &self,
        model: &dyn sven_model::ModelProvider,
        plan: &ForkPlan,
        reply_cap: usize,
        window: u32,
        calibration: f32,
    ) -> Notes {
        let request_estimate = estimate(&plan.request, calibration);
        let room = (window as usize).saturating_sub(request_estimate + 128);
        let output = u32::try_from(reply_cap.min(room)).unwrap_or(u32::MAX);
        for thinking in [Some(false), None] {
            // The thinking-token cap guards against a reasoning loop in the
            // agent's own turns. Notes must not be thrown away (and the
            // history with them) because the model reasoned about them.
            let mut limits = self.limits;
            limits.thinking.max_thinking_tokens = Some(u32::MAX);
            limits.enable_thinking = thinking;

            // Throwaway channel: the notes' own text deltas must not reach
            // the observation plane as if the agent had said them.
            let (tx, mut rx) = mpsc::channel::<UiEvent>(256);
            let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
            let result = stream_turn(
                model,
                plan.request.clone(),
                vec![],
                None,
                None,
                None,
                Some(output),
                limits,
                &tx,
            )
            .await;
            drop(tx);
            let _ = drain.await;

            match result {
                Ok(turn) if !turn.text.trim().is_empty() => {
                    if turn.truncated {
                        tracing::warn!(
                            "compaction: the notes hit the {output}-token reply cap and may be cut off"
                        );
                    }
                    return Notes::Text(turn.text);
                }
                Ok(_) => tracing::warn!(
                    ?thinking,
                    "compaction: the model returned no notes; trying again"
                ),
                Err(e) => {
                    let error = format!("{e:#}");
                    tracing::warn!(?thinking, %error, request_tokens = request_estimate, output, "compaction: the notes request failed");
                    // Where the server says how many tokens it counted, the
                    // fork was too large and asking again as it is cannot help.
                    if let Some(needed) = super::overflow::needed_tokens(&error) {
                        let prompt = needed.saturating_sub(output) as f32;
                        let plain = estimate(&plan.request, 1.0).max(1) as f32;
                        return Notes::TooLarge {
                            tokens_per_estimate: (prompt / plain).clamp(1.0, 8.0),
                        };
                    }
                }
            }
        }
        Notes::None
    }
}

fn estimate(messages: &[Message], calibration: f32) -> usize {
    sven_turn::estimate_tokens(messages, calibration)
}

/// Tells the person that history went without a summary.
pub(super) fn warn_history_dropped(obs: &ObservationSink, tokens: usize) {
    let text = format!(
        "context emergency-trimmed: ~{tokens} tokens of earlier conversation were dropped \
         WITHOUT a summary (the model could not summarise it); the task and the todo list \
         were kept"
    );
    tracing::warn!("{text}");
    obs.emit(UiEvent::Warning(text));
}
