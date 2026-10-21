// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The trace as the unified event stream carries it.
//!
//! A run records its progress once, in its trace; the session-event
//! payloads are that same record in its outward form, so a live view, a
//! replayed trace and a checkpoint describe a run identically rather than
//! through two loosely synchronized representations. Only the step
//! duration is not part of the record: it is a wall-clock measurement the
//! publisher adds when it observes the run, not something a replay
//! reconstructs.
//!
//! Swedish Embedded AB implements embeddable agent runtimes for its
//! clients. If your team needs expertise in building high-throughput
//! services on top of agent state machines then you can procure our
//! services by sending an email to info@swedishembedded.com.

use sven_vocab::HarnessEvent;

use crate::run::TraceRecord;

/// The event one trace record describes. `step_duration_ms` is the
/// publisher's measurement for a [`TraceRecord::StepCompleted`]; zero
/// where no measurement is attached (every other record, and a completed
/// step observed in a burst).
#[must_use]
pub fn harness_event(record: &TraceRecord, step_duration_ms: u64) -> HarnessEvent {
    match record {
        TraceRecord::StepStarted {
            step,
            primitive,
            attempt,
        } => HarnessEvent::StepStarted {
            step: step.clone(),
            primitive: primitive.clone(),
            attempt: *attempt,
        },
        TraceRecord::StepCompleted { step, call_id } => HarnessEvent::StepCompleted {
            step: step.clone(),
            call_id: call_id.as_ref().map(ToString::to_string),
            duration_ms: step_duration_ms,
        },
        TraceRecord::RetryAttempt {
            step,
            attempt,
            remaining,
        } => HarnessEvent::RetryScheduled {
            step: step.clone(),
            attempt: *attempt,
            remaining: *remaining,
        },
        TraceRecord::BranchSelected {
            step,
            condition,
            selected,
        } => HarnessEvent::BranchSelected {
            step: step.clone(),
            condition: *condition,
            selected: selected.clone(),
        },
        TraceRecord::IterationStarted { step, total } => HarnessEvent::IterationStarted {
            step: step.clone(),
            total: *total,
        },
        TraceRecord::ItemStarted {
            step,
            item,
            index,
            attempt,
        } => HarnessEvent::ItemStarted {
            step: step.clone(),
            item: item.clone(),
            index: *index,
            attempt: *attempt,
        },
        TraceRecord::TurnRetried {
            step,
            reason,
            attempt,
        } => HarnessEvent::TurnRetried {
            step: step.clone(),
            reason: reason.clone(),
            attempt: *attempt,
        },
        TraceRecord::ItemsInvalidated { step, items } => HarnessEvent::ItemsInvalidated {
            step: step.clone(),
            items: items.clone(),
        },
        TraceRecord::ItemsReconciled {
            step,
            kept,
            invalidated,
            added,
            removed,
        } => HarnessEvent::ItemsReconciled {
            step: step.clone(),
            kept: *kept,
            invalidated: *invalidated,
            added: *added,
            removed: *removed,
        },
        TraceRecord::ItemCompleted { step, item, index } => HarnessEvent::ItemCompleted {
            step: step.clone(),
            item: item.clone(),
            index: *index,
        },
        TraceRecord::ItemFailed {
            step,
            item,
            index,
            reason,
        } => HarnessEvent::ItemFailed {
            step: step.clone(),
            item: item.clone(),
            index: *index,
            reason: reason.clone(),
        },
        TraceRecord::GateEvaluated {
            step,
            passed,
            kind,
            exit_code,
        } => HarnessEvent::GateEvaluated {
            step: step.clone(),
            passed: *passed,
            kind: kind.clone(),
            exit_code: *exit_code,
        },
        TraceRecord::Verified { verifier, ok } => HarnessEvent::Verified {
            verifier: verifier.clone(),
            ok: *ok,
        },
        TraceRecord::ChildStarted { step, child } => HarnessEvent::ChildStarted {
            step: step.clone(),
            child: child.clone(),
        },
        TraceRecord::ChildCompleted { step, child, ok } => HarnessEvent::ChildCompleted {
            step: step.clone(),
            child: child.clone(),
            ok: *ok,
        },
        TraceRecord::FanOut { step, branches } => HarnessEvent::FanOut {
            step: step.clone(),
            branches: branches.clone(),
        },
        TraceRecord::Joined { step, results } => HarnessEvent::Joined {
            step: step.clone(),
            results: *results as u64,
        },
        TraceRecord::Failed {
            step,
            code,
            message,
            operation,
        } => HarnessEvent::Failed {
            step: step.clone(),
            code: code.clone(),
            message: message.clone(),
            operation: operation.clone(),
        },
        TraceRecord::Outcome { outcome } => HarnessEvent::Completed {
            outcome: outcome.clone(),
        },
    }
}
