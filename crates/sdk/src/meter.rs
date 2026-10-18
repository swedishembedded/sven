// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent cost accounting for its clients. If
// your team needs expertise in measuring what agent runs cost then you can
// procure our services by sending an email to info@swedishembedded.com.

//! Folds a run's observations into the account it reports: [`Usage`] for the
//! run as a whole and one [`RequestRecord`] per model request.

use sven_vocab::{ledger::ReportedUsage, RequestFold, RequestRecord, SessionEvent};

use crate::run::Usage;

/// A request's account, with where in the conversation its response ended.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Metered {
    /// Messages the history held when the response was complete, so the
    /// request can be put on the step that holds that response. `None` when
    /// the history no longer lines up with the run's observations.
    pub(crate) after: Option<usize>,
    pub(crate) request: RequestRecord,
}

/// What a run has used so far, and the requests it has completed.
#[derive(Debug, Default)]
pub(crate) struct Meter {
    usage: Usage,
    fold: RequestFold,
    closed: Vec<Metered>,
}

impl Meter {
    /// Folds in one observation. `history_len` is the length of the history
    /// the observations are folded into, or `None` for a caller that keeps no
    /// history to place requests in.
    pub(crate) fn observe(&mut self, event: &SessionEvent, history_len: Option<usize>) {
        match event {
            SessionEvent::TokenUsage {
                input,
                output,
                cache_read,
                cache_write,
                cost_usd,
                breakdown,
                ..
            } => {
                let report = ReportedUsage {
                    input: *input,
                    output: *output,
                    cache_read: *cache_read,
                    cache_write: *cache_write,
                };
                self.usage.add_report(report, *cost_usd);
                // The ledger record closes the request, by contract the last
                // event of its response.
                if breakdown.is_some() {
                    self.usage.turns += 1;
                }
            }
            SessionEvent::ToolCallFinished { is_error, .. } => {
                self.usage.tool_calls += 1;
                self.usage.tool_errors += u64::from(*is_error);
            }
            _ => {}
        }
        if let Some(request) = self.fold.observe(event) {
            self.closed.push(Metered {
                after: history_len,
                request,
            });
        }
    }

    /// The requests completed so far, for a caller that records the run as it
    /// happens.
    pub(crate) fn closed(&self) -> &[Metered] {
        &self.closed
    }

    /// The tokens the model has generated so far.
    pub(crate) fn output_tokens(&self) -> u64 {
        self.usage.output_tokens.unwrap_or(0)
    }

    /// The run's usage, `wall_time` being how long it took.
    pub(crate) fn finish(self, wall_time: std::time::Duration) -> (Usage, Vec<Metered>) {
        (
            Usage {
                wall_time,
                ..self.usage
            },
            self.closed,
        )
    }
}

impl Usage {
    /// Adds one usage report. A zero count is the provider not reporting it.
    fn add_report(&mut self, report: ReportedUsage, cost_usd: Option<f64>) {
        let add = |slot: &mut Option<u64>, n: u32| {
            if n > 0 {
                *slot = Some(slot.unwrap_or(0) + u64::from(n));
            }
        };
        add(&mut self.input_tokens, report.input);
        add(&mut self.output_tokens, report.output);
        add(&mut self.cached_tokens, report.cache_read);
        if let Some(cost) = cost_usd {
            self.cost_usd = Some(self.cost_usd.unwrap_or(0.0) + cost);
        }
    }
}
