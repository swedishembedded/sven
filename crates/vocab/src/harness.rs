// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Structured harness lifecycle events.
//!
//! [`HarnessEvent`] is one harness run's progress as typed payloads, carried
//! on the unified [`SessionEvent`](crate::SessionEvent) stream next to the model and tool events
//! rather than in a second log. It answers the questions a run's aftermath
//! poses - why did the harness take that branch, which step cost the extra
//! model call, did the retry change anything - from the same records the run
//! itself persists, so a live view and a replayed trace describe the run
//! identically.
//!
//! Swedish Embedded AB implements embeddable agent runtimes for its clients.
//! If your team needs expertise in building high-throughput services on top
//! of agent state machines then you can procure our services by sending an
//! email to info@swedishembedded.com.

use serde_json::Value;

/// One harness run's lifecycle, as the run itself records it.
///
/// The run-scoped fields (plan and bundle digests, the inputs) travel with
/// [`SessionEvent::Harness`](crate::SessionEvent::Harness) together with the run id that links the run's
/// events across a suspend and a resume; each variant carries what belongs
/// to that one event.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum HarnessEvent {
    /// The run started. Emitted once, before the first step.
    Started {
        /// The plan's content digest - which program is running.
        plan_digest: String,
        /// The bundle's content digest, when the harness carries helpers.
        bundle_digest: Option<String>,
        /// The inputs' digest - what the run was started with.
        input_digest: String,
    },
    /// A step began executing. For the body of a `retry`, `attempt` counts
    /// from 1; otherwise it is 1.
    StepStarted {
        /// The step that began.
        step: String,
        /// The step's primitive, in words (`predict`, `tool`, ...).
        primitive: String,
        /// The attempt this execution is, 1-based.
        attempt: u64,
    },
    /// A step's result was stored.
    StepCompleted {
        /// The step that completed.
        step: String,
        /// The operation's call id, when the step called a tool.
        call_id: Option<String>,
        /// Milliseconds between this record and the previous one being
        /// observed - approximately what the step cost. Zero when the run
        /// was observed in a burst (at a resume, or when it ended).
        duration_ms: u64,
    },
    /// A step's side effect failed and a retry frame looped back; the
    /// attempt that is about to run.
    RetryScheduled {
        /// The body step re-entered.
        step: String,
        /// The 1-based attempt about to run.
        attempt: u64,
        /// Attempts left after this one.
        remaining: u64,
    },
    /// A `branch` step chose its successor.
    BranchSelected {
        /// The branch step.
        step: String,
        /// The condition's result.
        condition: bool,
        /// The step it selected.
        selected: String,
    },
    /// A `delegate` step started a child run.
    ChildStarted {
        /// The delegate step that started it.
        step: String,
        /// The child's machine id.
        child: String,
    },
    /// A child run of a delegate or parallel step completed.
    ChildCompleted {
        /// The parent step whose child completed.
        step: String,
        /// The child's machine id.
        child: String,
        /// Whether the child reported success.
        ok: bool,
    },
    /// A `parallel` step fanned out, in declared order.
    FanOut {
        /// The parallel step.
        step: String,
        /// The branch steps, in declared order.
        branches: Vec<String>,
    },
    /// A parallel step's join settled.
    Joined {
        /// The parallel step.
        step: String,
        /// How many results the join settled.
        results: u64,
    },
    /// A `verify` step settled its verdict.
    Verified {
        /// The verifier's identity (its kind, or its name when it carries
        /// one).
        verifier: String,
        /// Whether the verdict passed.
        ok: bool,
    },
    /// An `iterate` step froze its collection.
    IterationStarted {
        /// The iterate step.
        step: String,
        /// How many items the collection holds.
        total: u64,
    },
    /// An `iterate` step started the body for an item (or a pass of an
    /// `until`, named by its number).
    ItemStarted {
        /// The iterate step.
        step: String,
        /// The item's id.
        item: String,
        /// The item's index in the frozen collection, from 0.
        index: u64,
        /// How many times this item's body has been started, from 1.
        attempt: u64,
    },
    /// An item's body finished with every check passed.
    ItemCompleted {
        /// The iterate step.
        step: String,
        /// The item's id.
        item: String,
        /// The item's index in the frozen collection, from 0.
        index: u64,
    },
    /// An agent asked to set a run variable.
    VariableSet {
        /// The agent step.
        step: String,
        /// The variable.
        name: String,
        /// Whether the value was stored.
        stored: bool,
        /// The answer the agent was given.
        answer: String,
        /// The start of the value it sent (at most 400 characters).
        preview: String,
    },
    /// An item's body failed for good in a loop with `on_item_failure`.
    ItemFailed {
        /// The iterate step.
        step: String,
        /// The item's id.
        item: String,
        /// The item's index in the frozen collection, from 0.
        index: u64,
        /// Why it failed.
        reason: String,
    },
    /// A resumed `for_each` with `reconcile: refresh` read its collection
    /// again and merged it by item id.
    ItemsReconciled {
        /// The iterate step.
        step: String,
        /// Items that stayed as they were.
        kept: u64,
        /// Completed items whose content changed and are pending again.
        invalidated: u64,
        /// Items the refreshed collection added.
        added: u64,
        /// Items the refreshed collection no longer holds.
        removed: u64,
    },
    /// A resumed loop found that the files some completed items cover had
    /// changed, and made those items pending again.
    ItemsInvalidated {
        /// The iterate step.
        step: String,
        /// The ids of the items.
        items: Vec<String>,
    },
    /// A model turn was asked again, with the reason and the attempt (1 is
    /// the first retry).
    TurnRetried {
        /// The step whose turn it was.
        step: String,
        /// Why: `empty_reply`.
        reason: String,
        /// Which retry this is.
        attempt: u64,
    },
    /// A `gate` step judged its command's report.
    GateEvaluated {
        /// The gate step.
        step: String,
        /// Whether the gate passed.
        passed: bool,
        /// `passed`, `failed` (non-zero exit), `timeout` or `exec_error`.
        kind: String,
        /// The command's exit status, when it had one.
        exit_code: Option<i64>,
    },
    /// A declared budget was exhausted and the run was suspended on it.
    BudgetExhausted {
        /// Which budget (`output_tokens`, `steps`, `model_turns`,
        /// `tool_calls`).
        budget: String,
        /// What had been consumed when the bound fired.
        consumed: u64,
        /// The bound itself.
        limit: u64,
    },
    /// The run failed. A program error (an unknown step, a template over a
    /// missing fact) and a resource failure (a budget) both land here, with
    /// `code` saying which; a model-output error arrives as the step
    /// failure that a `fail` step then reports.
    Failed {
        /// The step that was in flight.
        step: String,
        /// The failure code.
        code: String,
        /// The failure message.
        message: String,
        /// The in-flight operation's id, when the failure belongs to one.
        operation: Option<String>,
    },
    /// The run reached its terminal outcome.
    Completed {
        /// The resolved outcome value.
        outcome: Value,
    },
}

impl std::fmt::Display for HarnessEvent {
    /// The event in one human-readable line — the form the CLI runner
    /// traces on stderr and any log line takes. Identifiers are
    /// abbreviated to their first eight characters.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn short(id: &str) -> &str {
            id.get(..8).unwrap_or(id)
        }
        match self {
            Self::Started {
                plan_digest,
                bundle_digest,
                input_digest,
            } => match bundle_digest {
                Some(bundle) => write!(
                    f,
                    "started (plan {}, bundle {}, inputs {})",
                    short(plan_digest),
                    short(bundle),
                    short(input_digest)
                ),
                None => write!(
                    f,
                    "started (plan {}, inputs {})",
                    short(plan_digest),
                    short(input_digest)
                ),
            },
            Self::StepStarted {
                step,
                primitive,
                attempt,
            } => write!(f, "step `{step}` ({primitive}) started, attempt {attempt}"),
            Self::StepCompleted {
                step,
                call_id,
                duration_ms,
            } => match call_id {
                Some(call_id) => write!(
                    f,
                    "step `{step}` completed (call {}, {duration_ms} ms)",
                    short(call_id)
                ),
                None => write!(f, "step `{step}` completed ({duration_ms} ms)"),
            },
            Self::RetryScheduled {
                step,
                attempt,
                remaining,
            } => write!(
                f,
                "step `{step}` retrying: attempt {attempt}, {remaining} left after it"
            ),
            Self::BranchSelected {
                step,
                condition,
                selected,
            } => write!(
                f,
                "step `{step}` chose `{selected}` (condition {condition})"
            ),
            Self::ChildStarted { step, child } => {
                write!(f, "child {} started for step `{step}`", short(child))
            }
            Self::ChildCompleted { step, child, ok } => write!(
                f,
                "child {} completed for step `{step}` (ok: {ok})",
                short(child)
            ),
            Self::FanOut { step, branches } => {
                write!(f, "step `{step}` fanned out: {}", branches.join(", "))
            }
            Self::Joined { step, results } => {
                write!(f, "step `{step}` joined {results} results")
            }
            Self::Verified { verifier, ok } => {
                write!(
                    f,
                    "verifier {verifier} {}",
                    if *ok { "passed" } else { "did not pass" }
                )
            }
            Self::IterationStarted { step, total } => {
                write!(f, "loop `{step}` froze {total} item(s)")
            }
            Self::ItemStarted {
                step,
                item,
                index,
                attempt,
            } => write!(
                f,
                "loop `{step}` started item `{item}` (#{index}, start {attempt})"
            ),
            Self::ItemCompleted { step, item, index } => {
                write!(f, "loop `{step}` completed item `{item}` (#{index})")
            }
            Self::ItemFailed {
                step,
                item,
                index,
                reason,
            } => write!(f, "loop `{step}` failed item `{item}` (#{index}): {reason}"),
            Self::VariableSet { step, answer, .. } => write!(f, "step `{step}`: {answer}"),
            Self::TurnRetried {
                step,
                reason,
                attempt,
            } => write!(f, "step `{step}` asked its model turn again ({reason}, retry {attempt})"),
            Self::ItemsInvalidated { step, items } => write!(
                f,
                "loop `{step}` found changed files and made {} item(s) pending again: {}",
                items.len(),
                items.join(", ")
            ),
            Self::ItemsReconciled {
                step,
                kept,
                invalidated,
                added,
                removed,
            } => write!(
                f,
                "loop `{step}` reconciled its collection: {kept} kept, {invalidated} invalidated, {added} added, {removed} removed"
            ),
            Self::GateEvaluated {
                step,
                passed,
                kind,
                exit_code,
            } => match exit_code {
                Some(code) => write!(
                    f,
                    "gate `{step}` {} ({kind}, exit {code})",
                    if *passed { "passed" } else { "failed" }
                ),
                None => write!(
                    f,
                    "gate `{step}` {} ({kind})",
                    if *passed { "passed" } else { "failed" }
                ),
            },
            Self::BudgetExhausted {
                budget,
                consumed,
                limit,
            } => write!(f, "budget {budget} exhausted: {consumed}/{limit}"),
            Self::Failed {
                step,
                code,
                message,
                operation,
            } => match operation {
                Some(operation) => write!(
                    f,
                    "failed at step `{step}` ({code}, operation {}): {message}",
                    short(operation)
                ),
                None => write!(f, "failed at step `{step}` ({code}): {message}"),
            },
            Self::Completed { outcome } => write!(f, "completed: {outcome}"),
        }
    }
}
