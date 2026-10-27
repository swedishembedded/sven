// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Builds a request's ledger record while its response streams.

use sven_model::CompletionRequest;
use sven_vocab::{ReportedUsage, SessionEvent};

use crate::ledger::estimate_request;

/// Builds the ledger record of one request while its response streams.
///
/// Created from the request just before it is sent; fed every usage report
/// the provider streams; turned into the request's closing `TokenUsage` event
/// once the response has ended.
#[derive(Debug)]
pub struct LedgerRecorder {
    breakdown: sven_vocab::RequestBreakdown,
    reported: ReportedUsage,
    started: crate::clock::Instant,
    first_token_ms: Option<u64>,
}

impl LedgerRecorder {
    /// Estimate `request` and start the request's wall clock.
    #[must_use]
    pub fn start(request: &CompletionRequest) -> Self {
        Self {
            breakdown: estimate_request(request),
            reported: ReportedUsage::default(),
            started: crate::clock::Instant::now(),
            first_token_ms: None,
        }
    }

    /// Notes that response content arrived; only the first call counts.
    pub fn note_first_token(&mut self) {
        let started = self.started;
        self.first_token_ms.get_or_insert_with(|| {
            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
        });
    }

    /// Add one usage report. Providers split a request's usage over several
    /// reports (input first, output last), each carrying only what it
    /// knows, so reports are summed.
    pub fn note_usage(&mut self, usage: ReportedUsage) {
        let r = &mut self.reported;
        r.input = r.input.saturating_add(usage.input);
        r.output = r.output.saturating_add(usage.output);
        r.cache_read = r.cache_read.saturating_add(usage.cache_read);
        r.cache_write = r.cache_write.saturating_add(usage.cache_write);
    }

    /// The request's closing event: a `TokenUsage` with every counter zero
    /// (the counters were already sent as they arrived) carrying the
    /// reconciled breakdown.
    #[must_use]
    pub fn into_event(self) -> SessionEvent {
        let mut breakdown = self.breakdown;
        breakdown.reconcile(self.reported);
        breakdown.first_token_ms = self.first_token_ms;
        breakdown.wall_ms =
            Some(u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX));
        SessionEvent::TokenUsage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_read_total: 0,
            cache_write_total: 0,
            max_tokens: 0,
            max_output_tokens: 0,
            cost_usd: None,
            breakdown: Some(Box::new(breakdown)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_model::Message;

    fn request(messages: Vec<Message>) -> CompletionRequest {
        CompletionRequest {
            messages,
            tools: vec![],
            stream: true,
            system_dynamic_suffix: None,
            cache_key: None,
            max_output_tokens_override: None,
            core_tool_count: 0,
            response_format: None,
            ..Default::default()
        }
    }

    #[test]
    fn the_closing_event_carries_the_summed_reports_and_zero_counters() {
        let mut recorder = LedgerRecorder::start(&request(vec![Message::user("a".repeat(400))]));
        recorder.note_usage(ReportedUsage {
            input: 90,
            ..ReportedUsage::default()
        });
        recorder.note_usage(ReportedUsage {
            output: 5,
            cache_read: 30,
            ..ReportedUsage::default()
        });
        let SessionEvent::TokenUsage {
            input,
            output,
            breakdown: Some(b),
            ..
        } = recorder.into_event()
        else {
            panic!("expected a TokenUsage carrying a breakdown");
        };
        assert_eq!((input, output), (0, 0));
        assert_eq!(
            b.reported.map(|r| (r.input, r.output, r.cache_read)),
            Some((90, 5, 30))
        );
        assert_eq!(b.residual, Some(20));
        assert!(b.wall_ms.is_some());
    }

    #[test]
    fn time_to_first_token_is_the_first_content_and_not_later_than_the_whole_response() {
        let mut recorder = LedgerRecorder::start(&request(vec![Message::user("hi")]));
        recorder.note_first_token();
        std::thread::sleep(std::time::Duration::from_millis(20));
        recorder.note_first_token();
        let SessionEvent::TokenUsage {
            breakdown: Some(b), ..
        } = recorder.into_event()
        else {
            panic!("expected a breakdown");
        };
        let (first, wall) = (b.first_token_ms.unwrap(), b.wall_ms.unwrap());
        assert!(first < 20 && first <= wall, "{first} {wall}");
    }

    #[test]
    fn a_response_without_content_has_no_time_to_first_token() {
        let SessionEvent::TokenUsage {
            breakdown: Some(b), ..
        } = LedgerRecorder::start(&request(vec![Message::user("hi")])).into_event()
        else {
            panic!("expected a breakdown");
        };
        assert_eq!(b.first_token_ms, None);
    }

    #[test]
    fn a_provider_that_reports_nothing_leaves_the_request_unreconciled() {
        let SessionEvent::TokenUsage {
            breakdown: Some(b), ..
        } = LedgerRecorder::start(&request(vec![Message::user("hi")])).into_event()
        else {
            panic!("expected a breakdown");
        };
        assert_eq!((b.reported, b.residual), (None, None));
    }
}
