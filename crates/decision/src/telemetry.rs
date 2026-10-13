// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Accounting for decisions: what they cost and what became of them.
//!
//! A session owns one [`DecisionTelemetry`]. Decision points record into it;
//! the turn executor drains it as each model request starts its ledger
//! record, so the cost of the inference a request's context was shaped by is
//! charged to that request ([`DecisionInference`]) and every verdict is
//! saved with the same step ([`DecisionTraceRecord`]).

use std::sync::Mutex;
use std::time::Duration;

use sha2::{Digest, Sha256};
use sven_vocab::{DecisionInference, DecisionTraceRecord};

use crate::policy::{AbstainReason, Verdict};
use crate::DecisionRequest;

/// One call to a decision provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallRecord {
    /// Wall time the caller waited.
    pub wall: Duration,
    /// The model that answered; `None` when the call failed.
    pub model_id: Option<String>,
    /// How many questions the call carried.
    pub batch_size: usize,
    /// Served from the cache: no inference ran, so nothing is charged.
    pub cached: bool,
}

/// What has accumulated since the last [`DecisionTelemetry::drain`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Drained {
    /// `None` when no inference ran.
    pub inference: Option<DecisionInference>,
    pub traces: Vec<DecisionTraceRecord>,
}

#[derive(Default)]
struct Pending {
    inference: DecisionInference,
    wall_ms: u64,
    traces: Vec<DecisionTraceRecord>,
}

/// The decision account of a session.
#[derive(Default)]
pub struct DecisionTelemetry {
    pending: Mutex<Pending>,
}

impl DecisionTelemetry {
    fn pending(&self) -> std::sync::MutexGuard<'_, Pending> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Charges a provider call. A cache hit runs no inference and is not charged.
    pub fn record_call(&self, call: &CallRecord) {
        if call.cached {
            return;
        }
        let mut pending = self.pending();
        pending.inference.calls = pending.inference.calls.saturating_add(1);
        pending.wall_ms = pending
            .wall_ms
            .saturating_add(u64::try_from(call.wall.as_millis()).unwrap_or(u64::MAX));
        if call.model_id.is_some() {
            pending.inference.model_id.clone_from(&call.model_id);
        }
        let batch = u32::try_from(call.batch_size).unwrap_or(u32::MAX);
        pending.inference.batch_size = Some(pending.inference.batch_size.unwrap_or(0).max(batch));
    }

    /// Saves what a decision point did with an answer.
    pub fn record_trace(&self, record: DecisionTraceRecord) {
        self.pending().traces.push(record);
    }

    /// Takes everything recorded since the last drain.
    pub fn drain(&self) -> Drained {
        let mut pending = self.pending();
        let taken = std::mem::take(&mut *pending);
        Drained {
            inference: (taken.inference.calls > 0).then_some(DecisionInference {
                latency_ms: Some(taken.wall_ms),
                ..taken.inference
            }),
            traces: taken.traces,
        }
    }
}

/// SHA-256, in hex, of what a request asked: the state and the questions
/// with their options in order. The deadline bounds the wait and is not an
/// input.
#[must_use]
pub fn inputs_digest(request: &DecisionRequest) -> String {
    let canonical = serde_json::json!({
        "state": request.state,
        "questions": request.questions,
    });
    hex::encode(Sha256::digest(canonical.to_string().as_bytes()))
}

/// The trace record of a verdict. `applied` says whether it changed
/// behaviour (it does not in shadow mode, nor on an abstention).
#[must_use]
pub fn trace_record(
    policy: &str,
    request: &DecisionRequest,
    verdict: &Verdict,
    model_id: Option<&str>,
    applied: bool,
) -> DecisionTraceRecord {
    let base = DecisionTraceRecord {
        policy: policy.to_string(),
        inputs_digest: inputs_digest(request),
        model_id: model_id.map(str::to_string),
        ..DecisionTraceRecord::default()
    };
    match verdict {
        Verdict::Accept(accepted) => DecisionTraceRecord {
            verdict: "accept".into(),
            selected: Some(accepted.selected.describe()),
            advisory_only: accepted.advisory_only,
            applied,
            ..base
        },
        Verdict::Abstain(reason) => DecisionTraceRecord {
            verdict: "abstain".into(),
            abstain_reason: Some(reason.describe()),
            ..base
        },
    }
}

impl AbstainReason {
    /// The reason as a short stable string for traces.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::LowConfidence { .. } => "low_confidence".into(),
            Self::NearTie { .. } => "near_tie".into(),
            Self::UnsupportedCandidate { id } => format!("unsupported_candidate:{id}"),
            Self::Truncated => "truncated".into(),
            Self::UnsupportedQuestion => "unsupported_question".into(),
            Self::MissingAnswer => "missing_answer".into(),
            Self::ShapeNotAllowed { .. } => "shape_not_allowed".into(),
            Self::ProviderFailed { class } => format!("provider_failed:{}", class.name()),
            Self::OrderSensitive { .. } => "order_sensitive".into(),
            Self::InputTooLarge { .. } => "input_too_large".into(),
        }
    }
}
