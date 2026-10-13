// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The token ledger: where the input tokens of one model request went.
//!
//! A [`RequestBreakdown`] attributes the *estimated* size of a request to its
//! sources (system instructions, each tool schema, user turns, replayed
//! assistant turns, tool-call arguments, tool results, media) and then
//! reconciles that estimate with the prompt size the provider *reported*.
//! The provider's number stays authoritative for billing; the estimate only
//! says where the tokens came from, and the residual between the two makes
//! estimator drift visible instead of hiding it.
//!
//! Three notions of "prompt tokens" are kept apart on purpose:
//!
//! - **logical prompt** - everything the model read: [`ReportedUsage::logical_prompt`],
//!   `input + cache_read + cache_write`;
//! - **cached** - the part of it the provider served from its prompt cache
//!   ([`ReportedUsage::cache_read`]);
//! - **billed input** - what is charged at the uncached/cache-write rate
//!   (`input`, `cache_write`); the rates are the provider's business, so the
//!   ledger records the counts and never a price.
//!
//! [`LedgerSummary`] is the pure aggregator that sums breakdowns across the
//! requests of a session; it knows nothing about trajectories (the session
//! store walks those and feeds it).
//!
//! # Wire format
//!
//! A breakdown serializes as one JSON object (see the field docs). Every
//! optional or empty field is omitted, and every field deserializes with a
//! default, so records written by an older or newer sven stay readable.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

/// Key under which a step's [`RequestBreakdown`] records are stored in
/// ATIF `StepMetrics.extra` (`{"sven_ledger": [ {...}, ... ]}`): one record
/// per model request the step covers, in request order.
pub const STEP_METRICS_EXTRA_KEY: &str = "sven_ledger";

/// Key under which a step's [`DecisionTraceRecord`]s are stored in ATIF
/// `StepMetrics.extra`, beside [`STEP_METRICS_EXTRA_KEY`]: every acceptance and
/// abstention of a decision point, in the order they happened.
pub const STEP_DECISIONS_EXTRA_KEY: &str = "sven_decisions";

/// Provider-reported usage of one request, summed over every usage report the
/// provider streamed for it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedUsage {
    /// Prompt tokens processed without a cache hit.
    #[serde(default)]
    pub input: u32,
    /// Tokens generated.
    #[serde(default)]
    pub output: u32,
    /// Prompt tokens served from the provider's cache.
    #[serde(default)]
    pub cache_read: u32,
    /// Prompt tokens written into the provider's cache.
    #[serde(default)]
    pub cache_write: u32,
}

impl ReportedUsage {
    /// Everything the model read: uncached input plus cache reads and writes.
    #[must_use]
    pub fn logical_prompt(&self) -> u32 {
        self.input
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }

    /// Whether the provider reported nothing at all for this request.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Tool-execution counters visible at a request boundary.
///
/// `calls` and `duplicate_calls` are counted over the tool calls present in
/// the history sent with *this* request, so they grow monotonically through a
/// session unless compaction drops history. A duplicate is a call with the
/// same tool name and the same canonicalised arguments (object keys sorted)
/// as an earlier call in that history.
///
/// `retries` and `errors` are not observable from the history a request
/// carries (a tool result does not record failure), so they stay `None` until
/// a producer that sees tool outcomes fills them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolActivity {
    #[serde(default)]
    pub calls: u32,
    #[serde(default)]
    pub duplicate_calls: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors: Option<u32>,
}

/// Cost of the decision-model inference made on behalf of a request (for
/// example a small model that re-ranks tool search results before the main
/// model sees them): everything asked since the previous request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionInference {
    /// Decision-model inference calls made for the request.
    #[serde(default)]
    pub calls: u32,
    /// Wall time spent in those calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    /// Identifier of the decision model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// Time spent loading the model, when it was not resident.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_time_ms: Option<u64>,
    /// Number of items scored in one batch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_size: Option<u32>,
}

/// What one decision point did with a decision model's answer: accepted it,
/// or abstained and why. Recorded for every decision, so a tool that was
/// picked or a chunk that was left out can be explained from the trajectory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionTraceRecord {
    /// The decision point's policy label.
    pub policy: String,
    /// SHA-256 of the state and questions that were asked, hex. Identifies
    /// the inputs without storing them.
    pub inputs_digest: String,
    /// `accept` or `abstain`.
    pub verdict: String,
    /// What was accepted: the chosen option, for an `accept`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<String>,
    /// Why the policy abstained, for an `abstain`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abstain_reason: Option<String>,
    /// The model that answered, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// An accepted answer could only advise.
    #[serde(default)]
    pub advisory_only: bool,
    /// Whether the verdict changed behaviour (false in shadow mode).
    #[serde(default)]
    pub applied: bool,
}

/// Where the input tokens of one model request went.
///
/// All `u32` figures other than [`reported`](Self::reported) are *estimates*
/// made by [`estimator`](Self::estimator); they sum to
/// [`estimated_total`](Self::estimated_total).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestBreakdown {
    /// Name of the estimator that produced the figures.
    #[serde(default)]
    pub estimator: String,
    /// System instructions that do not change within a session.
    #[serde(default)]
    pub system_stable: u32,
    /// System-prompt sections assembled from the runtime context (skills,
    /// subagents, knowledge, project/git/CI context, the dynamic suffix),
    /// by section name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub system_runtime: BTreeMap<String, u32>,
    /// Each tool's schema (name, description, parameters), by tool name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_schemas: BTreeMap<String, u32>,
    /// User-authored text.
    #[serde(default)]
    pub user: u32,
    /// Assistant text replayed from the history.
    #[serde(default)]
    pub assistant: u32,
    /// Tool-call names and arguments replayed from the history, by tool name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_call_arguments: BTreeMap<String, u32>,
    /// Tool-result text replayed from the history, by the name of the tool
    /// that produced it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_results: BTreeMap<String, u32>,
    /// Images and audio, wherever they are attached.
    #[serde(default)]
    pub media: u32,
    /// Sum of every figure above.
    #[serde(default)]
    pub estimated_total: u32,
    /// What the provider reported for this request; `None` when it reported
    /// nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported: Option<ReportedUsage>,
    /// `reported.logical_prompt() - estimated_total`: positive when the
    /// estimator undercounts. `None` exactly when `reported` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub residual: Option<i64>,
    /// Tool-execution counters at this request.
    #[serde(default)]
    pub tool_activity: ToolActivity,
    /// Decision-model inference cost, see [`DecisionInference`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<DecisionInference>,
    /// What the decision points did since the previous request. Carried by
    /// the event; a trajectory stores them under [`STEP_DECISIONS_EXTRA_KEY`]
    /// rather than inside the ledger record.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decision_trace: Vec<DecisionTraceRecord>,
    /// Wall time from sending the request to the end of the response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_ms: Option<u64>,
}

impl RequestBreakdown {
    /// Sum of the attributed parts, which [`estimated_total`](Self::estimated_total)
    /// must equal.
    #[must_use]
    pub fn attributed_total(&self) -> u32 {
        let sum = |m: &BTreeMap<String, u32>| m.values().sum::<u32>();
        self.system_stable
            + sum(&self.system_runtime)
            + sum(&self.tool_schemas)
            + self.user
            + self.assistant
            + sum(&self.tool_call_arguments)
            + sum(&self.tool_results)
            + self.media
    }

    /// Record what the provider reported and the residual against the
    /// estimate. An empty report counts as "nothing reported".
    pub fn reconcile(&mut self, reported: ReportedUsage) {
        if reported.is_empty() {
            self.reported = None;
            self.residual = None;
            return;
        }
        self.residual =
            Some(i64::from(reported.logical_prompt()) - i64::from(self.estimated_total));
        self.reported = Some(reported);
    }
}

/// Sums of [`RequestBreakdown`]s over a session.
///
/// Figures are totals over requests, so they double-count history that is
/// replayed on every request: that is the point, since replay is what each
/// request pays for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerSummary {
    /// Model requests summed.
    #[serde(default)]
    pub requests: u64,
    /// Trajectories summed: the session itself plus every subagent.
    #[serde(default)]
    pub trajectories: u64,
    #[serde(default)]
    pub estimated_total: u64,
    #[serde(default)]
    pub system_stable: u64,
    #[serde(default)]
    pub system_runtime: BTreeMap<String, u64>,
    #[serde(default)]
    pub tool_schemas: BTreeMap<String, u64>,
    #[serde(default)]
    pub user: u64,
    #[serde(default)]
    pub assistant: u64,
    #[serde(default)]
    pub tool_call_arguments: BTreeMap<String, u64>,
    #[serde(default)]
    pub tool_results: BTreeMap<String, u64>,
    #[serde(default)]
    pub media: u64,
    /// Requests for which the provider reported usage.
    #[serde(default)]
    pub reconciled_requests: u64,
    /// Estimated total over the reconciled requests only, so that it can be
    /// compared with [`reported_logical_prompt`](Self::reported_logical_prompt).
    #[serde(default)]
    pub estimated_reconciled: u64,
    /// Logical prompt tokens the provider reported over the same requests.
    #[serde(default)]
    pub reported_logical_prompt: u64,
    /// `reported_logical_prompt - estimated_reconciled`.
    #[serde(default)]
    pub residual: i64,
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_write: u64,
    /// Tool calls made, summed over trajectories.
    #[serde(default)]
    pub tool_calls: u64,
    /// Duplicate tool calls, summed over trajectories.
    #[serde(default)]
    pub duplicate_tool_calls: u64,
    /// Wall time of the requests that recorded one.
    #[serde(default)]
    pub wall_ms: u64,
}

fn add_named(into: &mut BTreeMap<String, u64>, from: &BTreeMap<String, u32>) {
    for (name, tokens) in from {
        *into.entry(name.clone()).or_default() += u64::from(*tokens);
    }
}

impl LedgerSummary {
    /// Fold one request into the summary.
    pub fn add_request(&mut self, request: &RequestBreakdown) {
        self.requests += 1;
        self.estimated_total += u64::from(request.estimated_total);
        self.system_stable += u64::from(request.system_stable);
        add_named(&mut self.system_runtime, &request.system_runtime);
        add_named(&mut self.tool_schemas, &request.tool_schemas);
        self.user += u64::from(request.user);
        self.assistant += u64::from(request.assistant);
        add_named(&mut self.tool_call_arguments, &request.tool_call_arguments);
        add_named(&mut self.tool_results, &request.tool_results);
        self.media += u64::from(request.media);
        self.wall_ms += request.wall_ms.unwrap_or(0);
        if let Some(reported) = request.reported {
            self.reconciled_requests += 1;
            self.estimated_reconciled += u64::from(request.estimated_total);
            self.reported_logical_prompt += u64::from(reported.logical_prompt());
            self.residual += request.residual.unwrap_or(0);
            self.input += u64::from(reported.input);
            self.output += u64::from(reported.output);
            self.cache_read += u64::from(reported.cache_read);
            self.cache_write += u64::from(reported.cache_write);
        }
    }

    /// Fold the tool counters of one trajectory into the summary.
    pub fn add_tool_activity(&mut self, activity: ToolActivity) {
        self.tool_calls += u64::from(activity.calls);
        self.duplicate_tool_calls += u64::from(activity.duplicate_calls);
    }

    /// Fold another summary into this one.
    pub fn merge(&mut self, other: &Self) {
        let named = |into: &mut BTreeMap<String, u64>, from: &BTreeMap<String, u64>| {
            for (name, tokens) in from {
                *into.entry(name.clone()).or_default() += tokens;
            }
        };
        self.requests += other.requests;
        self.trajectories += other.trajectories;
        self.estimated_total += other.estimated_total;
        self.system_stable += other.system_stable;
        named(&mut self.system_runtime, &other.system_runtime);
        named(&mut self.tool_schemas, &other.tool_schemas);
        self.user += other.user;
        self.assistant += other.assistant;
        named(&mut self.tool_call_arguments, &other.tool_call_arguments);
        named(&mut self.tool_results, &other.tool_results);
        self.media += other.media;
        self.reconciled_requests += other.reconciled_requests;
        self.estimated_reconciled += other.estimated_reconciled;
        self.reported_logical_prompt += other.reported_logical_prompt;
        self.residual += other.residual;
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        self.tool_calls += other.tool_calls;
        self.duplicate_tool_calls += other.duplicate_tool_calls;
        self.wall_ms += other.wall_ms;
    }

    /// A short human-readable table: tokens per source, largest first within
    /// each group, followed by the reconciliation with the provider's report.
    #[must_use]
    pub fn render_table(&self) -> String {
        let total = self.estimated_total.max(1);
        let mut out = String::new();
        let _ = writeln!(
            out,
            "requests: {}   trajectories: {}   estimated input tokens: {}",
            self.requests, self.trajectories, self.estimated_total
        );
        let mut row = |label: &str, tokens: u64| {
            if tokens > 0 {
                let _ = writeln!(
                    out,
                    "  {label:<34} {tokens:>12} {:>5.1}%",
                    tokens as f64 * 100.0 / total as f64
                );
            }
        };
        row("system (stable)", self.system_stable);
        for (name, tokens) in by_size(&self.system_runtime) {
            row(&format!("system: {name}"), tokens);
        }
        let schemas: u64 = self.tool_schemas.values().sum();
        row("tool schemas", schemas);
        for (name, tokens) in by_size(&self.tool_schemas) {
            row(&format!("  schema: {name}"), tokens);
        }
        row("user", self.user);
        row("assistant (replayed)", self.assistant);
        let args: u64 = self.tool_call_arguments.values().sum();
        row("tool-call arguments", args);
        for (name, tokens) in by_size(&self.tool_call_arguments) {
            row(&format!("  args: {name}"), tokens);
        }
        let results: u64 = self.tool_results.values().sum();
        row("tool results", results);
        for (name, tokens) in by_size(&self.tool_results) {
            row(&format!("  result: {name}"), tokens);
        }
        row("media", self.media);
        let _ = writeln!(
            out,
            "reported by provider ({} of {} requests): logical prompt {}, input {}, cache_read {}, cache_write {}, output {}",
            self.reconciled_requests,
            self.requests,
            self.reported_logical_prompt,
            self.input,
            self.cache_read,
            self.cache_write,
            self.output
        );
        let _ = writeln!(
            out,
            "estimator residual (reported - estimated, same requests): {:+}",
            self.residual
        );
        let _ = writeln!(
            out,
            "tool calls: {}   duplicate: {}",
            self.tool_calls, self.duplicate_tool_calls
        );
        out
    }
}

/// Entries of `map` by descending size, ties by name.
fn by_size(map: &BTreeMap<String, u64>) -> Vec<(&String, u64)> {
    let mut entries: Vec<_> = map.iter().map(|(k, v)| (k, *v)).collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(user: u32, read_file: u32, reported_input: u32) -> RequestBreakdown {
        let mut b = RequestBreakdown {
            estimator: "test".into(),
            user,
            tool_results: BTreeMap::from([("read_file".to_string(), read_file)]),
            estimated_total: user + read_file,
            ..RequestBreakdown::default()
        };
        if reported_input > 0 {
            b.reconcile(ReportedUsage {
                input: reported_input,
                output: 7,
                ..ReportedUsage::default()
            });
        }
        b
    }

    #[test]
    fn reconcile_records_residual_against_the_logical_prompt() {
        let mut b = request(10, 30, 0);
        assert_eq!((b.reported, b.residual), (None, None));
        b.reconcile(ReportedUsage {
            input: 20,
            cache_read: 25,
            cache_write: 5,
            output: 1,
        });
        // logical prompt 50 vs estimated 40.
        assert_eq!(b.residual, Some(10));
        assert_eq!(b.reported.unwrap().logical_prompt(), 50);
    }

    #[test]
    fn a_record_written_without_newer_fields_still_reads() {
        let old = r#"{"estimator":"x","user":3,"estimated_total":3}"#;
        let b: RequestBreakdown = serde_json::from_str(old).unwrap();
        assert_eq!((b.user, b.estimated_total, b.reported), (3, 3, None));
        assert_eq!(b.tool_activity, ToolActivity::default());
    }

    #[test]
    fn summary_sums_requests_and_reconciles_only_reported_ones() {
        let mut s = LedgerSummary::default();
        s.add_request(&request(10, 30, 45));
        s.add_request(&request(5, 5, 0));
        assert_eq!(s.requests, 2);
        assert_eq!(s.tool_results["read_file"], 35);
        assert_eq!(s.estimated_total, 50);
        assert_eq!(s.reconciled_requests, 1);
        assert_eq!(
            (s.estimated_reconciled, s.reported_logical_prompt),
            (40, 45)
        );
        assert_eq!(s.residual, 5);
        let mut both = LedgerSummary::default();
        both.merge(&s);
        both.merge(&s);
        assert_eq!((both.requests, both.tool_results["read_file"]), (4, 70));
    }

    #[test]
    fn table_names_each_source_and_the_residual() {
        let mut s = LedgerSummary::default();
        s.add_request(&request(10, 30, 45));
        let table = s.render_table();
        assert!(table.contains("result: read_file"), "{table}");
        assert!(table.contains("residual"), "{table}");
    }
}
