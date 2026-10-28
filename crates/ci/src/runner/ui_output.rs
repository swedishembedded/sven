// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for headless agent runs that must
// leave a pipeable transcript and an auditable trajectory, for its clients. If
// your team needs expertise in CI-grade agent output you can procure our
// services by sending an email to info@swedishembedded.com.

//! What a headless kernel run writes: the stdout conversation document, the
//! stderr diagnostic trace and the ATIF trajectory, all folded from the
//! session's [`UiEvent`] stream.

use atif::Trajectory;
use sven_bootstrap::Config;
use sven_hsm::UiEvent;
use sven_model::{FunctionCall, Message, MessageContent, Role};
use sven_session_store::trace_session::{self, StepAssembler, SvenSessionMeta};
use sven_session_store::{apply_outcome_to_trajectory, make_title, OutcomeFold, Verdict};
use sven_tool_api::ToolCall;

use crate::ledger_output::{ledger_record, LedgerOutput};
use crate::output::{
    finalise_stdout, format_token_usage_line, tool_output_snippet, write_notice, write_progress,
    write_stderr, write_stdout,
};
use crate::runner::outcome::conclusion_for;
use crate::runner::{
    EXIT_AGENT_ERROR, EXIT_BUDGET_EXHAUSTED, EXIT_NEEDS_HUMAN, EXIT_STOPPED_WITHOUT_CONCLUSION,
    EXIT_SUCCESS,
};

/// Mutable state threaded through [`handle_ui_event`] for one CI turn.
pub(super) struct CiOutState {
    /// Verbosity level (0 = default, 1 = `-v`, 2+ = trace).
    trace: u8,
    /// The user prompt, held so `ensure_user_header` can write it lazily
    /// (see that function's doc comment for why eager doesn't work).
    prompt: String,
    /// Whether the `## User` section has been written yet this turn.
    user_header_emitted: bool,
    /// Assistant text streamed to stdout so far this turn.
    pub(super) streamed_text: String,
    /// Whether an open `## Sven` section is awaiting a close.
    sven_header_emitted: bool,
    /// Set when a fatal (non-tool) error was observed.
    pub(super) had_error: bool,
    /// Number of tool calls issued this turn.
    pub(super) tools_used: u32,
    /// Cumulative input + output tokens seen so far this run.
    run_total_tokens: u64,
    /// Optional cumulative token budget; the run stops once
    /// `run_total_tokens` reaches it. `None` means unlimited.
    token_budget: Option<u64>,
    /// Cumulative input tokens across every turn this run.
    total_input: u64,
    /// Cumulative output tokens across every turn this run.
    total_output: u64,
    /// Most recently reported cache_read_total/cache_write_total (already
    /// cumulative across turns - see `TurnExecutor`'s per-thread cache
    /// tracking - so the latest value IS the run total, nothing to sum here).
    latest_cache_read_total: u32,
    latest_cache_write_total: u32,
    /// Accumulates this run's `TraceStep`s for the ATIF trajectory written at
    /// conclusion. Fed with the same message shapes the legacy runner uses
    /// (see [`super::event::handle_event`]) so both backends produce
    /// interchangeable documents.
    pub(super) assembler: StepAssembler,
    /// Where each request's token ledger goes (trajectory, and stderr when
    /// `--token-ledger` is on).
    ledger: LedgerOutput,
    /// Outcome tally for the reward stamped on that trajectory. Reward-only:
    /// the exit code is still decided by `had_error` and the budget/timeout
    /// paths alone.
    outcome: OutcomeFold,
}

impl CiOutState {
    /// Fresh state for one turn that records into `assembler`.
    pub(super) fn new(
        trace: u8,
        prompt: String,
        token_budget: Option<u64>,
        assembler: StepAssembler,
        ledger: LedgerOutput,
    ) -> Self {
        Self {
            trace,
            prompt,
            user_header_emitted: false,
            streamed_text: String::new(),
            sven_header_emitted: false,
            had_error: false,
            tools_used: 0,
            run_total_tokens: 0,
            token_budget,
            total_input: 0,
            total_output: 0,
            latest_cache_read_total: 0,
            latest_cache_write_total: 0,
            assembler,
            ledger,
            outcome: OutcomeFold::default(),
        }
    }
}

/// Write this run's ATIF trajectory to the project auto-log, stamped with the
/// reward its outcome earned.
///
/// Only ever called once, after the run has settled, so the document is always
/// a conclusion — a consumer that sees no `final_metrics.extra.reward` on a
/// trajectory knows the session did not finish.
///
/// Writes nothing when there is no `.sven/` project directory to write into
/// (`resolve_auto_log_path` returns `None`), and nothing here is fatal: a
/// failed trace write warns and leaves the run's exit code alone. Unlike
/// `CiRunner`, this runner embeds no `subagent_trajectories` — it keeps no
/// child assemblers.
pub(super) fn write_trajectory(
    state: CiOutState,
    exit: i32,
    config: &Config,
    mode: &str,
    verdict: Option<Verdict>,
) {
    let Some(path) = sven_workspace::resolve_auto_log_path() else {
        return;
    };

    let agent = trace_session::default_agent_profile()
        .with_model(format!("{}/{}", config.model.provider, config.model.name));
    let mut trajectory = Trajectory::new(trace_session::ATIF_SCHEMA_VERSION, agent);
    trajectory.session_id = Some(trace_session::new_session_id());
    trajectory.steps = state.assembler.finish();

    let mut meta = SvenSessionMeta::new(make_title(&state.prompt));
    meta.mode = Some(mode.to_string());
    meta.apply_to_trajectory(&mut trajectory);

    // A parked run has not concluded: `Waiting` concludes as unknown, so no
    // reward (not even 0.0, which would read back as a real failure) is
    // stamped. The steps recorded so far are still written, so `--resume`
    // has the full history to continue from once a human answers.
    //
    // Only the verified-task machine ever produces a real `verdict` (via
    // [`outcome::verdict_from_context`]); every other mode passes `None` here, so
    // a claimed success is honestly unverified and `conclude` reflects that
    // as `SessionOutcome::Unknown` rather than the mechanical `1.0` this
    // runner used to stamp.
    let outcome = state.outcome.conclude(conclusion_for(exit), verdict);
    apply_outcome_to_trajectory(&mut trajectory, &outcome);

    match atif::persist::write_trajectory_atomic(&path, &trajectory, None) {
        Ok(()) => write_progress(&format!("[sven:trace] Trace written to {}", path.display())),
        Err(e) => write_stderr(&format!(
            "[sven:warn] Failed to write trace log {}: {e}",
            path.display()
        )),
    }
}

/// Write the `## User` conversation section exactly once, right before the
/// turn's first real stdout content (assistant text, a tool call, or a
/// partial answer from an abort).
///
/// Deliberately lazy, not eager: writing it unconditionally up front (as
/// this runner used to) means a turn that fails before producing anything
/// still leaves "## User\n<prompt>\n\n" on stdout, contradicting the
/// contract that a failed turn produces no stdout content at all
/// (`11_error_handling.bats`, "provider mid-stream error produces no stdout
/// content") - confirmed to reproduce on the original code with no relation
/// to any other change, i.e. a pre-existing bug independent of what's being
/// worked on here, still worth fixing under the same "always fix bugs you
/// find" instruction as everything else in this session.
fn ensure_user_header(state: &mut CiOutState) {
    if !state.user_header_emitted {
        if !state.prompt.is_empty() {
            write_stdout(&format!("## User\n{}\n\n", state.prompt));
        }
        state.user_header_emitted = true;
    }
}

/// Close an open `## Sven` streaming section, if any.
pub(super) fn close_sven_section(state: &mut CiOutState) {
    if state.sven_header_emitted {
        if state.streamed_text.ends_with('\n') {
            write_stdout("\n");
        } else {
            write_stdout("\n\n");
        }
        state.sven_header_emitted = false;
    }
}

/// Print the run-wide `[sven:tokens:total]` summary once the run has
/// settled: total input/output tokens across every turn, and the latest
/// (already cumulative - see `TurnExecutor`) cache totals.
pub(super) fn print_total_usage(state: &CiOutState) {
    write_progress(&format!(
        "[sven:tokens:total] input={} output={} cache_read={} cache_write={}",
        state.total_input,
        state.total_output,
        state.latest_cache_read_total,
        state.latest_cache_write_total
    ));
}

/// Bridge a single [`UiEvent`] to CI output.
///
/// stderr carries the structured `[sven:*]` diagnostic trace; stdout carries
/// the pipeable conversation document (`## User` / `## Sven` / `## Tool` /
/// `## Tool Result`). This mirrors the contract emitted by the legacy runner's
/// [`handle_event`](super::event::handle_event) so headless output is stable
/// across the two backends.
///
/// Returns `Some(exit_code)` when the turn has settled (the caller should stop
/// driving the loop), or `None` to keep going.
pub(super) fn handle_ui_event(ev: UiEvent, state: &mut CiOutState) -> Option<i32> {
    // Fold into the reward tally before dispatching, so no arm can forget it.
    state.outcome.observe(&ev);
    state.ledger.observe(&ev, &mut state.assembler);
    if ledger_record(&ev).is_some() {
        return None;
    }
    match ev {
        UiEvent::TextDelta(d) => {
            if !state.sven_header_emitted {
                ensure_user_header(state);
                write_stdout("## Sven\n");
                state.sven_header_emitted = true;
            }
            state.streamed_text.push_str(&d);
            write_stdout(&d);
        }
        // Deltas were already streamed; only emit the complete text if nothing
        // streamed (some providers send a single TextComplete with no deltas).
        UiEvent::TextComplete(t) => {
            if !t.is_empty() {
                state.assembler.push_message(&Message::assistant(&t));
            }
            if state.streamed_text.is_empty() && !t.is_empty() {
                ensure_user_header(state);
                write_stdout("## Sven\n");
                state.streamed_text.push_str(&t);
                write_stdout(&t);
                state.sven_header_emitted = true;
            }
            close_sven_section(state);
        }
        UiEvent::ThinkingDelta(_) => {}
        // Thinking is high-value CI signal: emit the full content at every
        // verbosity level (only stderr, never stdout).
        UiEvent::ThinkingComplete(c) => {
            if !c.is_empty() {
                write_progress(&format!("[sven:thinking] {c}"));
                state.assembler.push_thinking(&c);
            }
        }
        UiEvent::ToolCallStarted(ToolCall {
            id: call_id,
            name,
            args,
        }) => {
            // A tool call ends any open assistant text section.
            close_sven_section(state);
            ensure_user_header(state);
            state.tools_used += 1;
            let args_str = serde_json::to_string(&args).unwrap_or_default();
            write_progress(&format!(
                "[sven:tool:call] id=\"{call_id}\" name=\"{name}\" args={args_str}"
            ));
            let envelope = serde_json::json!({
                "tool_call_id": call_id,
                "name": name,
                "args": args,
            });
            let pretty = serde_json::to_string_pretty(&envelope).unwrap_or_default();
            write_stdout(&format!("## Tool\n```json\n{pretty}\n```\n\n"));
            state.assembler.push_message(&Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: call_id,
                    function: FunctionCall {
                        name,
                        arguments: args_str,
                    },
                },
                reasoning: None,
            });
        }
        UiEvent::ModelWaiting {
            elapsed_secs,
            phase,
        } => {
            write_progress(&crate::output::format_model_waiting_line(
                elapsed_secs,
                phase,
            ));
        }
        UiEvent::ServedModelChanged { .. } | UiEvent::Warning(_) | UiEvent::ContextBudget(_) => {
            write_notice(&ev);
        }
        UiEvent::ToolProgress { message, .. } => {
            if state.trace >= 1 {
                write_progress(&format!("[sven:progress] {message}"));
            }
        }
        // A harness run's lifecycle, one line per event, behind the same
        // verbosity gate as the other progress traces.
        UiEvent::Harness { event, .. } => {
            if state.trace >= 1 {
                write_progress(&format!("[sven:harness] {event}"));
            }
        }
        UiEvent::ToolCallFinished {
            call_id,
            tool_name: name,
            is_error,
            output,
        } => {
            // The `output=` snippet is verbose-only signal for both success and
            // error outcomes: it appears solely at `-v` (trace >= 1) and is
            // truncated, so a large tool payload never floods stderr at default
            // verbosity. Tool errors are non-fatal — the model receives the
            // error and may recover on the next round, so the run still
            // succeeds; only the `success=<bool>` field distinguishes them.
            let success = !is_error;
            let output_snippet = tool_output_snippet(state.trace, &output);
            write_progress(&format!(
                "[sven:tool:result] id=\"{call_id}\" name=\"{name}\" success={success} size={}{}",
                output.len(),
                output_snippet
            ));
            write_stdout(&format!("## Tool Result\n```\n{output}\n```\n\n"));
            state
                .assembler
                .push_message(&Message::tool_result(&call_id, &output));
        }
        UiEvent::TokenUsage {
            input,
            output,
            cache_read,
            cache_write,
            cache_read_total,
            cache_write_total,
            max_tokens,
            max_output_tokens,
            ..
        } => {
            // Usage often arrives before TextComplete (many providers send
            // the usage chunk as the last SSE frame ahead of [DONE]). Close
            // any open `## Sven` streaming section first so this stderr
            // write doesn't land glued onto the end of the still-open
            // stdout text with no newline between them.
            close_sven_section(state);
            write_progress(&format_token_usage_line(
                input,
                output,
                cache_read,
                cache_write,
                cache_read_total,
                cache_write_total,
                max_tokens,
                max_output_tokens,
            ));
            // Enforce the cumulative token budget (mirrors the legacy runner's
            // `--max-tokens`). Stop the run when the running total reaches it.
            state.run_total_tokens += u64::from(input) + u64::from(output);
            state.total_input += u64::from(input);
            state.total_output += u64::from(output);
            state.latest_cache_read_total = cache_read_total;
            state.latest_cache_write_total = cache_write_total;
            if let Some(budget) = state.token_budget {
                if budget > 0 && state.run_total_tokens >= budget {
                    write_stderr(&format!(
                        "[sven:error] Token budget exhausted: {} tokens used (budget: {budget}). Stopping.",
                        state.run_total_tokens
                    ));
                    close_sven_section(state);
                    finalise_stdout(&state.streamed_text);
                    print_total_usage(state);
                    return Some(EXIT_BUDGET_EXHAUSTED);
                }
            }
        }
        UiEvent::ContextCompacted {
            tokens_before,
            tokens_after,
            strategy,
            turn,
            ..
        } => {
            write_progress(&format!(
                "[sven:compact] strategy={strategy} turn={turn} {tokens_before}->{tokens_after}"
            ));
            state.assembler.push_context_compacted(
                tokens_before,
                tokens_after,
                Some(&strategy.to_string()),
                Some(turn),
            );
        }
        UiEvent::TodoUpdate(_) => {}
        UiEvent::ModeChanged(m) => write_progress(&format!("[sven:mode] {m}")),
        UiEvent::ModelChanged(m) => write_progress(&format!("[sven:model] {m}")),
        UiEvent::Transition { from, to, event } => {
            if state.trace >= 2 {
                write_progress(&format!("[sven:transition] {from} -> {to} on {event}"));
            }
        }
        UiEvent::Error(e) => {
            state.had_error = true;
            write_stderr(&format!("[sven:error] {e}"));
        }
        UiEvent::TurnComplete => {
            close_sven_section(state);
            finalise_stdout(&state.streamed_text);
            print_total_usage(state);
            return Some(if state.had_error {
                EXIT_AGENT_ERROR
            } else if state.outcome.stopped_without_conclusion() {
                write_stderr(
                    "[sven:error] the model's last turn announced more work and made no call; \
                     the run stopped without concluding",
                );
                EXIT_STOPPED_WITHOUT_CONCLUSION
            } else {
                EXIT_SUCCESS
            });
        }
        UiEvent::Aborted { partial_text } => {
            // Record whatever the model managed to say before the abort, on
            // the same condition stdout uses: only when nothing was streamed,
            // since a streamed turn ends in `TextComplete`, which already
            // pushed it. The run scores zero either way.
            if state.streamed_text.is_empty() && !partial_text.is_empty() {
                state
                    .assembler
                    .push_message(&Message::assistant(&partial_text));
            }
            if state.streamed_text.is_empty() && !partial_text.is_empty() {
                ensure_user_header(state);
                write_stdout("## Sven\n");
                write_stdout(&partial_text);
                state.streamed_text.push_str(&partial_text);
                state.sven_header_emitted = true;
            }
            close_sven_section(state);
            finalise_stdout(&state.streamed_text);
            print_total_usage(state);
            // A user cancel is not a CI failure; an abort the runtime
            // explained with an error (the thinking watchdog) is.
            return Some(if state.had_error {
                EXIT_AGENT_ERROR
            } else {
                EXIT_SUCCESS
            });
        }
        // Subagent / delegate / team observations are otherwise rendered as
        // rich child-session views by the interactive frontends; this runner
        // has no such surface. It writes an ATIF trajectory (the project
        // auto-log; `--output-trace` still routes to `CiRunner`, see
        // `run/ci.rs`'s runner-routing comment) but keeps no child
        // assemblers, so that document carries no `subagent_trajectories` —
        // unlike `CiRunner`'s. Still emit the same `[sven:subagent:...]` stderr tokens
        // `CiRunner` does (see `crates/ci/src/runner/event.rs`) so a plain
        // `sven --headless` run (no `--output-trace`) has the same
        // subagent-lifecycle visibility on stderr instead of silently
        // dropping these — previously the case for every field here.
        UiEvent::SubagentStarted {
            call_id,
            handle_id,
            description,
            ..
        } => {
            write_stderr(&format!(
                "[sven:subagent:started] call_id=\"{call_id}\" handle_id=\"{handle_id}\" description={description:?}"
            ));
        }
        UiEvent::SubagentEvent {
            handle_id, update, ..
        } => match update {
            sven_tool_api::events::SubagentUpdate::Finished { .. } => {
                write_stderr(&format!(
                    "[sven:subagent:finished] handle_id=\"{handle_id}\""
                ));
            }
            sven_tool_api::events::SubagentUpdate::Failed { reason } => {
                write_stderr(&format!(
                    "[sven:subagent:failed] handle_id=\"{handle_id}\" reason={reason:?}"
                ));
            }
            _ => {}
        },
        UiEvent::DelegateSummary {
            to_name,
            task_title,
            duration_ms,
            status,
            result_preview,
        } => {
            write_stderr(&format!(
                "[sven:subagent:delegate_summary] to=\"{to_name}\" task={task_title:?} status=\"{status}\" duration_ms={duration_ms} result_preview={result_preview:?}"
            ));
        }
        // A tool call parked awaiting a human answer (see
        // `ToolExecutor`'s parking branch). Not a failure and not a
        // success - the run stops here, unscored, so it can be resumed
        // once a human answers (see `sven questions` / `EXIT_NEEDS_HUMAN`).
        // `id`/`questions` are already in `[sven:questions]`-shaped text on
        // stderr for a human watching the run live; the ledger record (with
        // the matching `question_id` to answer by) is written separately by
        // whatever drains `UserExecutor`'s parked-question channel.
        UiEvent::Question { id, questions } => {
            write_stderr(&format!(
                "[sven:questions:parked] id={id} {}",
                questions.join(" | ")
            ));
            close_sven_section(state);
            finalise_stdout(&state.streamed_text);
            print_total_usage(state);
            return Some(EXIT_NEEDS_HUMAN);
        }
        // QuestionAnswer never fires in headless mode (nothing here ever
        // posts an answer back through this event); TitleGenerated has no
        // headless stdout/stderr representation.
        UiEvent::CollabEvent(_)
        | UiEvent::PeerList(_)
        | UiEvent::QuestionAnswer { .. }
        | UiEvent::TitleGenerated(_)
        | UiEvent::EventsDropped { .. } => {}
        // `SessionEvent` is non-exhaustive: an event added later has no
        // headless representation until it is given one here.
        _ => {}
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(trace: u8) -> CiOutState {
        CiOutState {
            trace,
            prompt: "test prompt".to_string(),
            user_header_emitted: false,
            streamed_text: String::new(),
            sven_header_emitted: false,
            had_error: false,
            tools_used: 0,
            run_total_tokens: 0,
            token_budget: None,
            total_input: 0,
            total_output: 0,
            latest_cache_read_total: 0,
            latest_cache_write_total: 0,
            assembler: StepAssembler::new(),
            ledger: LedgerOutput::default(),
            outcome: OutcomeFold::default(),
        }
    }

    #[test]
    fn a_request_ledger_record_lands_in_the_trajectory_without_touching_the_token_totals() {
        let mut st = state(0);
        st.assembler.push_message(&Message::user("hi"));
        st.assembler.push_message(&Message::assistant("hello"));
        let record = UiEvent::TokenUsage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_read_total: 0,
            cache_write_total: 0,
            max_tokens: 0,
            max_output_tokens: 0,
            cost_usd: None,
            breakdown: Some(Box::new(sven_vocab::RequestBreakdown {
                user: 2,
                estimated_total: 2,
                ..Default::default()
            })),
        };
        assert_eq!(handle_ui_event(record, &mut st), None);
        assert_eq!(
            (st.total_input, st.total_output, st.run_total_tokens),
            (0, 0, 0)
        );
        let steps = st.assembler.finish();
        let agent = steps.last().expect("an agent step");
        assert_eq!(
            sven_session_store::step_ledger::requests_in_step(agent)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn text_delta_streams_to_buffer() {
        let mut st = state(0);
        let r = handle_ui_event(UiEvent::TextDelta("pong".into()), &mut st);
        assert!(r.is_none());
        assert_eq!(st.streamed_text, "pong");
        assert!(!st.had_error);
        assert!(st.sven_header_emitted);
    }

    #[test]
    fn turn_complete_returns_success() {
        let mut st = state(0);
        st.streamed_text.push_str("pong");
        let r = handle_ui_event(UiEvent::TurnComplete, &mut st);
        assert_eq!(r, Some(EXIT_SUCCESS));
    }

    #[test]
    fn a_turn_that_ends_announcing_a_call_it_did_not_make_is_not_success() {
        let mut st = state(0);
        handle_ui_event(
            UiEvent::TextComplete("Now the test file, let me find the tests:".into()),
            &mut st,
        );
        let r = handle_ui_event(UiEvent::TurnComplete, &mut st);
        assert_eq!(r, Some(EXIT_STOPPED_WITHOUT_CONCLUSION));
        let outcome = st.outcome.conclude(conclusion_for(r.unwrap()), None);
        let sven_session_store::SessionOutcome::Scored(reward) = outcome else {
            panic!("a stop without conclusion scores confidently, got {outcome:?}");
        };
        assert_eq!(reward.outcome, "stopped_without_conclusion");
    }

    #[test]
    fn error_then_turn_complete_returns_agent_error() {
        let mut st = state(0);
        handle_ui_event(UiEvent::Error("boom".into()), &mut st);
        let r = handle_ui_event(UiEvent::TurnComplete, &mut st);
        assert_eq!(r, Some(EXIT_AGENT_ERROR));
    }

    /// Regression test for the eager-`## User`-header bug: a turn that
    /// fails before producing any content must never write "## User" to
    /// stdout, matching `11_error_handling.bats`'s "produces no stdout
    /// content" contract. `ensure_user_header` is only called from the
    /// content-writing arms (`TextDelta`, `TextComplete`'s fallback,
    /// `ToolStarted`, `Aborted`-with-partial-text) - an error-only turn
    /// never reaches any of them, so `user_header_emitted` must stay false.
    #[test]
    fn error_only_turn_never_emits_the_user_header() {
        let mut st = state(0);
        handle_ui_event(UiEvent::Error("boom".into()), &mut st);
        handle_ui_event(UiEvent::TurnComplete, &mut st);
        assert!(
            !st.user_header_emitted,
            "an error-only turn must produce no stdout content at all"
        );
    }

    #[test]
    fn text_delta_emits_the_user_header_exactly_once() {
        let mut st = state(0);
        handle_ui_event(UiEvent::TextDelta("a".into()), &mut st);
        assert!(st.user_header_emitted);
        // A second delta must not re-emit it.
        handle_ui_event(UiEvent::TextDelta("b".into()), &mut st);
        assert!(st.user_header_emitted);
    }

    #[test]
    fn tool_started_emits_the_user_header_when_no_text_preceded_it() {
        let mut st = state(0);
        handle_ui_event(
            UiEvent::ToolCallStarted(ToolCall {
                id: "tc-1".into(),
                name: "write_file".into(),
                args: serde_json::json!({}),
            }),
            &mut st,
        );
        assert!(st.user_header_emitted);
    }

    #[test]
    fn aborted_with_partial_text_emits_the_user_header() {
        let mut st = state(0);
        handle_ui_event(
            UiEvent::Aborted {
                partial_text: "partial".into(),
            },
            &mut st,
        );
        assert!(st.user_header_emitted);
    }

    #[test]
    fn aborted_with_no_partial_text_never_emits_the_user_header() {
        let mut st = state(0);
        handle_ui_event(
            UiEvent::Aborted {
                partial_text: String::new(),
            },
            &mut st,
        );
        assert!(!st.user_header_emitted);
    }

    #[test]
    fn tool_started_counts_and_traces() {
        let mut st = state(0);
        let r = handle_ui_event(
            UiEvent::ToolCallStarted(ToolCall {
                id: "tc-1".into(),
                name: "write_file".into(),
                args: serde_json::json!({"path": "src/x.rs"}),
            }),
            &mut st,
        );
        assert!(r.is_none());
        assert_eq!(st.tools_used, 1);
    }

    fn token_usage(input: u32, output: u32) -> UiEvent {
        UiEvent::TokenUsage {
            input,
            output,
            cache_read: 0,
            cache_write: 0,
            cache_read_total: 0,
            cache_write_total: 0,
            max_tokens: 0,
            max_output_tokens: 0,
            cost_usd: None,
            breakdown: None,
        }
    }

    #[test]
    fn token_budget_exhaustion_stops_run() {
        let mut st = state(0);
        st.token_budget = Some(100);
        // First update stays under budget.
        assert_eq!(handle_ui_event(token_usage(40, 10), &mut st), None);
        assert_eq!(st.run_total_tokens, 50);
        // Second update crosses the budget → run stops with the budget code.
        let r = handle_ui_event(token_usage(40, 20), &mut st);
        assert_eq!(r, Some(EXIT_BUDGET_EXHAUSTED));
    }

    #[test]
    fn token_usage_without_budget_never_stops() {
        let mut st = state(0);
        assert_eq!(handle_ui_event(token_usage(1_000, 1_000), &mut st), None);
        assert_eq!(st.run_total_tokens, 2_000);
    }

    fn token_usage_with_cache(
        input: u32,
        output: u32,
        cache_read_total: u32,
        cache_write_total: u32,
    ) -> UiEvent {
        UiEvent::TokenUsage {
            input,
            output,
            cache_read: 0,
            cache_write: 0,
            cache_read_total,
            cache_write_total,
            max_tokens: 0,
            max_output_tokens: 0,
            cost_usd: None,
            breakdown: None,
        }
    }

    #[test]
    fn total_input_and_output_accumulate_across_turns() {
        let mut st = state(0);
        handle_ui_event(token_usage(40, 10), &mut st);
        handle_ui_event(token_usage(15, 7), &mut st);
        assert_eq!(st.total_input, 55);
        assert_eq!(st.total_output, 17);
    }

    #[test]
    fn latest_cache_totals_track_the_most_recent_event_not_a_sum() {
        // cache_read_total/cache_write_total on the event are ALREADY
        // cumulative (TurnExecutor's job) - CiOutState must track the latest
        // value, not sum successive already-cumulative numbers.
        let mut st = state(0);
        handle_ui_event(token_usage_with_cache(10, 2, 5, 3), &mut st);
        handle_ui_event(token_usage_with_cache(10, 2, 10, 6), &mut st);
        assert_eq!(st.latest_cache_read_total, 10);
        assert_eq!(st.latest_cache_write_total, 6);
    }

    #[test]
    fn tool_error_is_non_fatal() {
        let mut st = state(0);
        handle_ui_event(
            UiEvent::ToolCallFinished {
                call_id: "tc-1".into(),
                tool_name: "read_file".into(),
                output: "no such file".into(),
                is_error: true,
            },
            &mut st,
        );
        // A tool error must not mark the whole turn as failed.
        assert!(!st.had_error);
        let r = handle_ui_event(UiEvent::TurnComplete, &mut st);
        assert_eq!(r, Some(EXIT_SUCCESS));
        // ... but it does grade a verified success's reward down.
        let outcome = st.outcome.conclude(
            conclusion_for(EXIT_SUCCESS),
            Some(sven_session_store::Verdict::Passed),
        );
        let sven_session_store::SessionOutcome::Scored(reward) = outcome else {
            panic!("expected Scored, got {outcome:?}");
        };
        assert!(
            reward.reward > 0.0 && reward.reward < 1.0,
            "graded reward expected, got {}",
            reward.reward
        );
        assert_eq!(reward.outcome, "verified_success");
    }

    #[test]
    fn a_parked_question_stops_the_run_unscored_not_failed() {
        let mut st = state(0);
        let r = handle_ui_event(
            UiEvent::Question {
                id: "call-1".into(),
                questions: vec!["Which framework?".into()],
            },
            &mut st,
        );
        assert_eq!(
            r,
            Some(EXIT_NEEDS_HUMAN),
            "a parked question must stop the run distinctly from success or failure"
        );
    }

    #[test]
    fn an_abort_that_follows_an_error_is_a_failed_run() {
        // A watchdog abort says why through an `Error` before `Aborted`; the
        // run must not then end as success.
        let mut st = state(0);
        handle_ui_event(
            UiEvent::Error("thinking-token limit exceeded".into()),
            &mut st,
        );
        let r = handle_ui_event(
            UiEvent::Aborted {
                partial_text: String::new(),
            },
            &mut st,
        );
        assert_eq!(r, Some(EXIT_AGENT_ERROR));
    }

    #[test]
    fn an_aborted_run_exits_zero_but_scores_zero() {
        // `Aborted` returns EXIT_SUCCESS (a cancelled run is not a CI failure),
        // so the event stream is the only thing that knows not to train on it.
        let mut st = state(0);
        let r = handle_ui_event(
            UiEvent::Aborted {
                partial_text: "half a thought".into(),
            },
            &mut st,
        );
        assert_eq!(r, Some(EXIT_SUCCESS));
        let outcome = st.outcome.conclude(conclusion_for(EXIT_SUCCESS), None);
        let sven_session_store::SessionOutcome::Scored(reward) = outcome else {
            panic!("an observed abort must score confidently, got {outcome:?}");
        };
        assert_eq!(reward.reward, 0.0);
        assert_eq!(reward.outcome, "cancelled");
    }

    #[test]
    fn the_conversation_is_recorded_into_the_trajectory() {
        let mut st = state(0);
        st.assembler.push_message(&Message::user("do a thing"));
        handle_ui_event(
            UiEvent::ToolCallStarted(ToolCall {
                id: "tc-1".into(),
                name: "read_file".into(),
                args: serde_json::json!({"path": "a.txt"}),
            }),
            &mut st,
        );
        handle_ui_event(
            UiEvent::ToolCallFinished {
                call_id: "tc-1".into(),
                tool_name: "read_file".into(),
                output: "contents".into(),
                is_error: false,
            },
            &mut st,
        );
        handle_ui_event(UiEvent::TextComplete("all done".into()), &mut st);

        let steps = st.assembler.finish();
        assert!(!steps.is_empty(), "the run must produce trace steps");
        let rendered = serde_json::to_string(&steps).expect("serialize steps");
        assert!(rendered.contains("do a thing"), "user prompt missing");
        assert!(rendered.contains("read_file"), "tool call missing");
        assert!(rendered.contains("contents"), "tool result missing");
        assert!(rendered.contains("all done"), "assistant reply missing");
    }
}
