// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Compaction of a thread whose request would not fit the model's window: a
//! summary from the model itself, or the model-free fallback.

use sven_hsm::{CompactionStrategyUsed, ObservationSink, UiEvent};
use sven_model::Message;
use sven_turn::stream_turn;
use sven_vocab::CompactionRecord;
use tokio::sync::mpsc;

use super::TurnExecutor;

impl TurnExecutor {
    /// The todo list the agent keeps outside the conversation, which every
    /// compaction puts back verbatim.
    pub(super) fn pinned_todos(&self) -> Vec<sven_vocab::TodoItem> {
        self.store
            .lock()
            .map(|store| store.todos().to_vec())
            .unwrap_or_default()
    }

    /// Compact `messages` for `thread_id`, write the result back to the
    /// shared store, and return it so the in-flight turn continues with the
    /// smaller history immediately (no need to wait for the next turn).
    ///
    /// Tries LLM-based summarization first (`sven_turn::prepare_compaction` /
    /// `finish_compaction`), asking the model itself for a summary of
    /// everything except the most recent `compaction.keep_recent` messages.
    /// Falls back to the deterministic, model-free `emergency_compact` (drop
    /// older history, keep a shrinking tail, no model call) whenever the
    /// LLM path can't be used or fails:
    /// - Nothing to summarize (the whole history already fits in the kept
    ///   tail) - `prepare_compaction` returns `None`.
    /// - The summarization request itself wouldn't fit the budget - asking
    ///   for a summary would just repeat the same failure one level down.
    /// - The summarization call errors, or the model returns empty text.
    ///
    /// Emits `UiEvent::ContextCompacted` on success (whichever path was
    /// used); does not emit anything and returns `messages` unchanged if
    /// there was truly nothing to compact.
    pub(super) async fn compact_thread(
        &self,
        thread_id: &str,
        messages: Vec<Message>,
        model: &dyn sven_model::ModelProvider,
        context_window: Option<u32>,
        configured_max_output: Option<u32>,
        obs: &ObservationSink,
    ) -> Vec<Message> {
        let Some(plan) = sven_turn::prepare_compaction(
            &messages,
            &self.compaction.strategy,
            self.compaction.keep_recent,
        ) else {
            return messages;
        };
        let tokens_before = plan.tokens_before;
        // The goal (the first user message) and the todo list are state the
        // agent keeps outside the conversation: they go back in verbatim.
        let todos = self.pinned_todos();
        let plan = plan.with_todos(&todos);

        let budget =
            sven_model::budget::effective_input_budget(context_window, configured_max_output);
        let request_estimate =
            sven_model::budget::estimate_request_tokens(&plan.summarize_request, &[], None);
        let request_fits = budget.is_none_or(|b| request_estimate <= b);

        let (final_messages, strategy_used) = if request_fits {
            match self
                .run_compaction_turn(&plan, model, context_window, configured_max_output)
                .await
            {
                Some(summary_text) if !summary_text.trim().is_empty() => {
                    let strategy_used = match self.compaction.strategy {
                        sven_turn::CompactionStrategy::Structured => {
                            CompactionStrategyUsed::Structured
                        }
                        sven_turn::CompactionStrategy::Narrative => {
                            CompactionStrategyUsed::Narrative
                        }
                    };
                    (
                        sven_turn::finish_compaction(plan, &summary_text),
                        strategy_used,
                    )
                }
                _ => {
                    tracing::warn!(
                        thread = %thread_id,
                        "compaction: summarization call failed or returned empty text; \
                         falling back to emergency_compact"
                    );
                    let mut fallback = messages.clone();
                    sven_turn::emergency_compact(
                        &mut fallback,
                        plan.system_msg,
                        self.compaction.keep_recent,
                        &todos,
                    );
                    (fallback, CompactionStrategyUsed::Emergency)
                }
            }
        } else {
            // Even the summarization request alone wouldn't fit - asking the
            // model for a summary would just fail the same way one level
            // down. Skip straight to the deterministic fallback.
            let mut fallback = messages.clone();
            sven_turn::emergency_compact(
                &mut fallback,
                plan.system_msg,
                self.compaction.keep_recent,
                &todos,
            );
            (fallback, CompactionStrategyUsed::Emergency)
        };

        let tokens_after: usize = final_messages.iter().map(Message::approx_tokens).sum();
        if let Ok(mut store) = self.store.lock() {
            store.replace_thread(thread_id, final_messages.clone());
            store.record_compaction(CompactionRecord {
                thread: thread_id.to_string(),
                tokens_before,
                tokens_after,
                strategy: strategy_used.clone(),
                turn: 0,
            });
        }
        obs.emit(UiEvent::ContextCompacted {
            tokens_before,
            tokens_after,
            strategy: strategy_used,
            turn: 0,
        });
        final_messages
    }

    /// Run the summarization call for a compaction plan and return the
    /// summary text, or `None` on any streaming/completion error.
    ///
    /// Uses a throwaway event channel: the compaction call's own text
    /// deltas must not reach the observation plane as if the agent had said
    /// them in response to the user's actual message.
    async fn run_compaction_turn(
        &self,
        plan: &sven_turn::CompactionPlan,
        model: &dyn sven_model::ModelProvider,
        context_window: Option<u32>,
        configured_max_output: Option<u32>,
    ) -> Option<String> {
        let request_estimate =
            sven_model::budget::estimate_request_tokens(&plan.summarize_request, &[], None);
        let max_output_tokens_override =
            self.limits
                .output_budget(context_window, configured_max_output, request_estimate);

        // The thinking-token cap guards against a reasoning loop in the agent's
        // own turns. A summary must not be thrown away (and the history with
        // it) because the model reasoned about what to write.
        let mut limits = self.limits;
        limits.thinking.max_thinking_tokens = Some(u32::MAX);

        let (tx, mut rx) = mpsc::channel::<UiEvent>(256);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });

        let result = stream_turn(
            model,
            plan.summarize_request.clone(),
            vec![],
            None,
            None,
            None,
            max_output_tokens_override,
            limits,
            &tx,
        )
        .await;
        drop(tx);
        let _ = drain.await;

        match result {
            Ok(turn) => Some(turn.text),
            Err(e) => {
                tracing::warn!(error = %e, "compaction: summarization call failed");
                None
            }
        }
    }
}
