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

/// What the request that just failed or came back empty was sized with.
#[derive(Clone, Copy)]
pub(super) struct Sent {
    /// The response budget it asked for.
    pub(super) output_budget: Option<u32>,
    /// The plain estimate of the whole request.
    pub(super) plain_estimate: usize,
    /// What a compaction works against, when the window is known.
    pub(super) fit: Option<Fit>,
}

/// What compaction and the retries after it remember between requests.
#[derive(Default)]
pub(super) struct Recovery {
    /// The last "context budget" notice, so that it is repeated only when it
    /// changes.
    announced_budget: Option<sven_vocab::ContextBudget>,
    /// The tools a compaction's fork offers (with `tool_choice: none`) so it
    /// shares its prompt prefix with the requests that use them: those of the
    /// request being built, or none when the provider cannot forbid calls.
    fork_tools: Vec<sven_model::ToolSchema>,
    /// Threads whose turn was already retried after a response spent its
    /// output budget thinking.
    exhaustion_retried: std::collections::HashSet<String>,
    /// Threads whose turn was already retried after a compaction answered a
    /// request the server rejected as too long.
    overflow_retried: std::collections::HashSet<String>,
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

    /// Makes the trimmed history (if there is one) the thread's own and
    /// records it as an emergency compaction, so the next request starts from
    /// it instead of overflowing again.
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
        let budget = self
            .compaction_fit(context_window, configured_max_output)
            .map(|fit| sven_vocab::ContextBudget {
                window: fit.window,
                compaction_at: fit.limit,
                output_reserve: self.compaction.output_reserve_tokens(fit.window),
                notes_tokens: self.compaction.reply_cap(fit.window),
                thinking_cap: self.limits.thinking.max_thinking_tokens,
            });
        // Nothing to be surprised by when no window is known: no compaction
        // will happen.
        let Some(budget) = budget else {
            return;
        };
        if self.recovery.announced_budget != Some(budget) {
            tracing::info!("{}", sven_vocab::context_budget_text(&budget));
            obs.emit(UiEvent::ContextBudget(budget));
            self.recovery.announced_budget = Some(budget);
        }
    }

    /// The tools of the request being built, which a compaction's fork offers
    /// too - when the provider can forbid calling them.
    pub(super) fn offer_fork_tools(
        &mut self,
        model: &dyn sven_model::ModelProvider,
        tools: &[sven_model::ToolSchema],
    ) {
        self.recovery.fork_tools = if model.supports_tool_choice_none() {
            tools.to_vec()
        } else {
            Vec::new()
        };
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
        // The request sizes it is compared with already count the tools the
        // fork offers.
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
        if estimate >= fit.limit {
            return Some(Fit { calibration, ..fit });
        }
        // Compaction switched off (limit is unbounded): a run that must keep
        // its transcript ends at the budget gate instead.
        if fit.limit == usize::MAX {
            return None;
        }
        // A thread no server has counted is sized pessimistically for its
        // response (see `PromptCounts::sizing_estimate`), and that size can
        // pass the input budget while the estimate is still under the limit
        // (a small window, or a threshold near 1.0). The request would then
        // be refused for want of room, so compact first.
        let budget =
            sven_model::budget::effective_input_budget(context_window, configured_max_output)?;
        let sizing = self
            .prompt_counts
            .sizing_estimate(thread_id, sent, plain_estimate);
        (sizing > budget).then_some(Fit { calibration, ..fit })
    }

    /// What to do about a turn that failed because the conversation does not
    /// fit, or ended with no answer because thinking used the whole response
    /// budget: the effect to run again after a compaction, or `None`.
    pub(super) async fn recover_turn(
        &mut self,
        req: &mut sven_vocab::TurnRequest,
        result: &anyhow::Result<sven_turn::StreamedTurn>,
        sent: Sent,
        model: &dyn sven_model::ModelProvider,
        obs: &ObservationSink,
    ) -> Option<sven_hsm::Effect> {
        match result {
            Err(e) => {
                let error = format!("{e:#}");
                self.recover_prompt_overflow(req, &error, sent, model, obs)
                    .await
            }
            Ok(turn) => {
                self.recovery.overflow_retried.remove(&req.thread);
                self.recover_exhausted_output(req, turn, sent.output_budget, model, sent.fit, obs)
                    .await
            }
        }
    }

    /// A request the server rejected as too long for its context means the
    /// estimate undercounted the prompt. Compacts the thread with the fork,
    /// planned from the server's own count where it gave one, and returns the
    /// effect to ask again - once per turn. Only a window that is not known
    /// (so no fork can be planned) falls back to the model-free trim. `None`
    /// when the error is not an overflow, the turn was already retried, or
    /// compaction cannot shrink the thread.
    pub(super) async fn recover_prompt_overflow(
        &mut self,
        req: &mut sven_vocab::TurnRequest,
        error: &str,
        sent: Sent,
        model: &dyn sven_model::ModelProvider,
        obs: &ObservationSink,
    ) -> Option<sven_hsm::Effect> {
        if !sven_model::budget::is_context_overflow(error)
            || !self.recovery.overflow_retried.insert(req.thread.clone())
        {
            return None;
        }
        obs.emit(UiEvent::Warning(
            "the server rejected the request as too long for its context; compacting the \
             conversation and asking again once"
                .into(),
        ));
        let thread_id = req.thread.as_str();
        let messages = snapshot_thread(&self.store, thread_id);
        let before = messages.len();
        let compacted = if let Some(mut fit) = sent.fit {
            if let Some(needed) = super::overflow::needed_tokens(error) {
                let prompt = needed.saturating_sub(sent.output_budget.unwrap_or(0)) as f32;
                let ratio = prompt / sent.plain_estimate.max(1) as f32;
                fit.calibration = fit.calibration.max(ratio.clamp(1.0, 8.0) * 1.05);
            }
            self.compact_thread(thread_id, messages, model, fit, obs)
                .await
        } else {
            let mut trimmed = messages;
            self.emergency_trim(&mut trimmed, &self.pinned_todos(), obs);
            self.keep_overflow_trim(thread_id, Some(trimmed.clone()), obs);
            trimmed
        };
        if compacted.len() >= before {
            return None;
        }
        req.instruction.clear();
        req.delay_ms = None;
        Some(sven_hsm::Effect::CallLlm {
            request: req.to_value(),
        })
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
            self.recovery.exhaustion_retried.remove(thread_id);
            return None;
        }
        let spent = output_budget.map_or_else(|| "its".to_string(), |b| b.to_string());
        let retry = fit.filter(|_| !self.recovery.exhaustion_retried.contains(thread_id));
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
        self.recovery
            .exhaustion_retried
            .insert(thread_id.to_string());
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
        let request_budget = (fit.window as usize)
            .saturating_sub(reply_cap)
            .saturating_sub(sven_model::budget::estimate_request_tokens(
                &[],
                &self.recovery.fork_tools,
                None,
            ));

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
        let tools_tokens =
            sven_model::budget::estimate_request_tokens(&[], &self.recovery.fork_tools, None);
        let request_estimate = estimate(&plan.request, calibration) + tools_tokens;
        let room = (window as usize).saturating_sub(request_estimate + 128);
        let output = u32::try_from(reply_cap.min(room)).unwrap_or(u32::MAX);
        let started = sven_rt::Instant::now();
        for thinking in [Some(false), None] {
            // The thinking-token cap guards against a reasoning loop in the
            // agent's own turns. Notes must not be thrown away (and the
            // history with them) because the model reasoned about them.
            let mut limits = self.limits;
            limits.thinking.max_thinking_tokens = Some(u32::MAX);
            limits.enable_thinking = thinking;
            limits.tool_choice = sven_model::ToolChoice::None;

            // Throwaway channel: the notes' own text deltas must not reach
            // the observation plane as if the agent had said them.
            let (tx, mut rx) = mpsc::channel::<UiEvent>(256);
            let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
            let result = stream_turn(
                model,
                plan.request.clone(),
                self.recovery.fork_tools.clone(),
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
                    tracing::info!(
                        fork_ms = started.elapsed().as_millis() as u64,
                        request_tokens = request_estimate,
                        tools = self.recovery.fork_tools.len(),
                        "compaction: the notes were written"
                    );
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
                        let plain = (estimate(&plan.request, 1.0) + tools_tokens).max(1) as f32;
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
