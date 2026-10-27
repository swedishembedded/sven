// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The headless request record: files each model request's account (usage,
//! seed, served identity, sampled tokens) and token-ledger breakdown in the
//! run's trajectory and, when enabled, prints the ledger as a
//! `[sven:ledger]` line.
//!
//! The `[sven:tokens]` line is a separate, pinned contract and is not
//! touched: the ledger line is additional output that exists only when the
//! caller asked for it (`--token-ledger`).

use sven_session_store::trace_session::StepAssembler;
use sven_vocab::{RequestBreakdown, RequestFold, SessionEvent};

use crate::output::write_stderr;

/// Prefix of the ledger line on stderr; the rest of the line is one JSON
/// object.
pub const LEDGER_LINE_PREFIX: &str = "[sven:ledger] ";

/// Per-run ledger sink shared by both headless runners.
#[derive(Debug, Default)]
pub(crate) struct LedgerOutput {
    print_lines: bool,
    requests: u32,
    /// Each request's account (usage, seed, served identity, sampled
    /// tokens), folded from the events that carry it.
    fold: RequestFold,
}

impl LedgerOutput {
    pub(crate) fn new(print_lines: bool) -> Self {
        Self {
            print_lines,
            requests: 0,
            fold: RequestFold::default(),
        }
    }

    /// Feed one event of the run: a request's account and its ledger record
    /// are filed on the step being assembled as the request closes.
    pub(crate) fn observe(&mut self, event: &SessionEvent, assembler: &mut StepAssembler) {
        if let Some(request) = self.fold.observe(event) {
            assembler.push_request(&request);
        }
        if let Some(request) = ledger_record(event) {
            self.record(request, assembler);
        }
    }

    /// Record one request: always into `assembler` (so the saved trajectory
    /// has it), and onto stderr when line output is on.
    fn record(&mut self, request: &RequestBreakdown, assembler: &mut StepAssembler) {
        self.requests += 1;
        assembler.push_request_ledger(request);
        if self.print_lines {
            write_stderr(&ledger_line(self.requests, request));
        }
    }
}

/// The `[sven:ledger]` line for the `index`th request of a run (1-based).
///
/// The object is the request's [`RequestBreakdown`] plus its `request` index.
pub(crate) fn ledger_line(index: u32, request: &RequestBreakdown) -> String {
    let mut object = serde_json::to_value(request).expect("a breakdown always serializes");
    if let Some(map) = object.as_object_mut() {
        map.insert("request".to_string(), index.into());
    }
    format!("{LEDGER_LINE_PREFIX}{object}")
}

/// The breakdown `event` carries when it is a request's ledger record: a
/// `TokenUsage` with a breakdown and no counters. Such an event has no
/// `[sven:tokens]` line, so that line appears exactly as often as the
/// provider reported usage.
pub(crate) fn ledger_record(event: &SessionEvent) -> Option<&RequestBreakdown> {
    match event {
        SessionEvent::TokenUsage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            breakdown: Some(request),
            ..
        } => Some(request.as_ref()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_vocab::Sampled;

    fn usage(input: u32, output: u32, breakdown: Option<RequestBreakdown>) -> SessionEvent {
        SessionEvent::TokenUsage {
            input,
            output,
            cache_read: 0,
            cache_write: 0,
            cache_read_total: 0,
            cache_write_total: 0,
            max_tokens: 0,
            max_output_tokens: 0,
            cost_usd: None,
            breakdown: breakdown.map(Box::new),
        }
    }

    /// What served a request and what it sampled - the seed above all, which
    /// a replay needs - lands on the step the request produced.
    #[test]
    fn a_requests_seed_and_usage_are_filed_on_its_step() {
        let mut assembler = StepAssembler::new();
        let mut ledger = LedgerOutput::new(false);
        assembler.push_message(&sven_model::Message::user("hi"));
        for event in [
            SessionEvent::ResponseSampled(Sampled {
                seed: Some(42),
                ..Sampled::default()
            }),
            usage(10, 3, None),
            usage(0, 0, Some(RequestBreakdown::default())),
        ] {
            ledger.observe(&event, &mut assembler);
        }
        assembler.push_message(&sven_model::Message::assistant("hello"));
        let steps = assembler.finish();
        let agent = steps.last().unwrap();
        let metrics = agent.metrics.as_ref().unwrap();
        assert_eq!(metrics.extra.as_ref().unwrap()["sven"]["seed"], 42);
        assert_eq!(metrics.completion_tokens, Some(3));
    }

    #[test]
    fn the_line_is_the_breakdown_plus_its_request_index() {
        let request = RequestBreakdown {
            user: 7,
            estimated_total: 7,
            ..RequestBreakdown::default()
        };
        let line = ledger_line(3, &request);
        let json = line.strip_prefix(LEDGER_LINE_PREFIX).expect("prefixed");
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(
            (value["request"].as_u64(), value["user"].as_u64()),
            (Some(3), Some(7))
        );
    }
}
