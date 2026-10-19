// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The real-model comparison of two variants, for example decisions disabled
//! (matrix variant B) against heuristic decisions (C).
//!
//! Scripted runs say what the harness does with a fixed model; they never say
//! whether a model succeeds, so they are not in this report. A real-model run
//! is verified by its final answer ([`crate::answer`]). The report states per
//! variant the verified success rate and the cost per verified success, and for
//! the pair the difference on the tasks both completed. It claims no
//! significance: a few seeds do not make an interval, and the counts are shown
//! so that a reader can judge.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::record::RunRecord;

/// One variant over its real-model runs that were answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RealVariant {
    pub variant: String,
    pub runs: usize,
    pub successes: usize,
    pub success_rate: f64,
    /// Estimated input tokens of all runs over verified successes.
    pub tokens_per_success: Option<f64>,
    /// Provider-reported input tokens that were not served from the cache.
    pub uncached_input_per_success: Option<f64>,
    /// Share of reported input tokens served from the cache.
    pub cache_read_share: Option<f64>,
    pub wall_s_per_success: Option<f64>,
    pub decision_calls_mean: f64,
    pub decisions_accepted: u64,
    pub decisions_abstained: u64,
    pub decisions_applied: u64,
}

/// Two variants compared on a real model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RealReport {
    pub reference: String,
    pub candidate: String,
    pub variants: Vec<RealVariant>,
    /// Tasks both variants completed and answered.
    pub paired_tasks: usize,
    /// Of those, tasks only the candidate answered correctly.
    pub success_gain: usize,
    /// Of those, tasks only the reference answered correctly.
    pub success_loss: usize,
    /// Reference tokens over candidate tokens on the paired tasks (above 1
    /// means the candidate used fewer).
    pub token_ratio: Option<f64>,
}

fn answered(run: &RunRecord) -> bool {
    !run.scripted && run.finished() && run.answer.is_some() && run.probe.is_none()
}

fn variant(id: &str, runs: &[&RunRecord]) -> RealVariant {
    let mine: Vec<&&RunRecord> = runs.iter().filter(|r| r.variant.id == id).collect();
    let successes: Vec<&&&RunRecord> = mine
        .iter()
        .filter(|r| r.answer.as_ref().is_some_and(|a| a.success))
        .collect();
    let n = successes.len() as f64;
    let sum = |f: &dyn Fn(&crate::record::TokenRecord) -> u64| -> f64 {
        mine.iter()
            .filter_map(|r| r.tokens.as_ref())
            .map(|t| f(t) as f64)
            .sum()
    };
    let per_success = |total: f64| (n > 0.0).then(|| total / n);
    let input = sum(&|t| t.input);
    RealVariant {
        variant: id.to_string(),
        runs: mine.len(),
        successes: successes.len(),
        success_rate: if mine.is_empty() {
            0.0
        } else {
            n / mine.len() as f64
        },
        tokens_per_success: per_success(sum(&|t| t.total)),
        uncached_input_per_success: per_success(input - sum(&|t| t.cache_read)),
        cache_read_share: (input > 0.0).then(|| sum(&|t| t.cache_read) / input),
        wall_s_per_success: per_success(sum(&|t| t.wall_ms) / 1000.0),
        decision_calls_mean: if mine.is_empty() {
            0.0
        } else {
            mine.iter()
                .map(|r| r.decisions.inference_calls as f64)
                .sum::<f64>()
                / mine.len() as f64
        },
        decisions_accepted: mine.iter().map(|r| r.decisions.accepted).sum(),
        decisions_abstained: mine.iter().map(|r| r.decisions.abstained).sum(),
        decisions_applied: mine.iter().map(|r| r.decisions.applied).sum(),
    }
}

/// Compares `candidate` against `reference` over the real-model runs.
#[must_use]
pub fn build_real(runs: &[RunRecord], reference: &str, candidate: &str) -> RealReport {
    let real: Vec<&RunRecord> = runs
        .iter()
        .filter(|r| answered(r) && (r.variant.id == reference || r.variant.id == candidate))
        .collect();
    let by_task = |id: &str| -> BTreeMap<&str, &RunRecord> {
        real.iter()
            .filter(|r| r.variant.id == id)
            .map(|r| (r.task_id.as_str(), *r))
            .collect()
    };
    let (ref_runs, cand_runs) = (by_task(reference), by_task(candidate));
    let (mut paired, mut gain, mut loss) = (0, 0, 0);
    let (mut ref_tokens, mut cand_tokens) = (0.0, 0.0);
    for (task, r) in &ref_runs {
        let Some(c) = cand_runs.get(task) else {
            continue;
        };
        paired += 1;
        let ok = |run: &RunRecord| run.answer.as_ref().is_some_and(|a| a.success);
        gain += usize::from(!ok(r) && ok(c));
        loss += usize::from(ok(r) && !ok(c));
        ref_tokens += r.tokens.as_ref().map_or(0.0, |t| t.total as f64);
        cand_tokens += c.tokens.as_ref().map_or(0.0, |t| t.total as f64);
    }
    RealReport {
        reference: reference.into(),
        candidate: candidate.into(),
        variants: [reference, candidate]
            .iter()
            .map(|id| variant(id, &real))
            .collect(),
        paired_tasks: paired,
        success_gain: gain,
        success_loss: loss,
        token_ratio: (cand_tokens > 0.0).then(|| ref_tokens / cand_tokens),
    }
}

fn opt(v: Option<f64>) -> String {
    v.map_or_else(|| "n/a".into(), |x| format!("{x:.1}"))
}

/// The report as Markdown.
#[must_use]
pub fn render_real(report: &RealReport) -> String {
    let mut out = format!(
        "# Real-model comparison: {} against {}\n\nVerified by the final answer: success means it cited every critical piece of evidence. Counts are small; no significance is claimed.\n\n",
        report.candidate, report.reference
    );
    out.push_str("| variant | runs | successes | success rate | tokens per verified success | uncached input per verified success | cache read share | wall s per verified success | decision calls | accepted | abstained | applied |\n|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    for v in &report.variants {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {:.2} | {} | {} | {} | {} | {:.1} | {} | {} | {} |",
            v.variant,
            v.runs,
            v.successes,
            v.success_rate,
            opt(v.tokens_per_success),
            opt(v.uncached_input_per_success),
            v.cache_read_share
                .map_or_else(|| "n/a".into(), |x| format!("{x:.2}")),
            opt(v.wall_s_per_success),
            v.decision_calls_mean,
            v.decisions_accepted,
            v.decisions_abstained,
            v.decisions_applied,
        );
    }
    let _ = write!(
        out,
        "\nOn the {} tasks both completed: {} answered only by {}, {} only by {}; reference tokens over candidate tokens {}.\n",
        report.paired_tasks,
        report.success_gain,
        report.candidate,
        report.success_loss,
        report.reference,
        report.token_ratio.map_or_else(|| "n/a".into(), |x| format!("{x:.2}")),
    );
    out
}
