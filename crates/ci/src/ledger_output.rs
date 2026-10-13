// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The headless token ledger: records each model request's breakdown in the
//! run's trajectory and, when enabled, prints it as a `[sven:ledger]` line.
//!
//! The `[sven:tokens]` line is a separate, pinned contract and is not
//! touched: the ledger line is additional output that exists only when the
//! caller asked for it (`--token-ledger`).

use sven_session_store::trace_session::StepAssembler;
use sven_vocab::{RequestBreakdown, SessionEvent};

use crate::output::write_stderr;

/// Prefix of the ledger line on stderr; the rest of the line is one JSON
/// object.
pub const LEDGER_LINE_PREFIX: &str = "[sven:ledger] ";

/// Per-run ledger sink shared by both headless runners.
#[derive(Debug, Default)]
pub(crate) struct LedgerOutput {
    print_lines: bool,
    requests: u32,
}

impl LedgerOutput {
    pub(crate) fn new(print_lines: bool) -> Self {
        Self {
            print_lines,
            requests: 0,
        }
    }

    /// Record one request: always into `assembler` (so the saved trajectory
    /// has it), and onto stderr when line output is on.
    pub(crate) fn record(&mut self, request: &RequestBreakdown, assembler: &mut StepAssembler) {
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
