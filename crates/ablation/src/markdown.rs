// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The human-readable summary of a report.

use std::fmt::Write;

use crate::report::{Cell, Report, Verdict};
use crate::report_recovery::RecoveryReport;
use crate::stats::Interval;

fn num(value: Option<f64>, digits: usize) -> String {
    value.map_or_else(|| "-".into(), |v| format!("{v:.digits$}"))
}

fn interval(i: Option<Interval>, digits: usize) -> String {
    i.map_or_else(
        || "-".into(),
        |i| {
            format!(
                "{:.digits$} [{:.digits$}, {:.digits$}]",
                i.estimate, i.lower, i.upper
            )
        },
    )
}

fn percent(value: Option<f64>) -> String {
    value.map_or_else(|| "-".into(), |v| format!("{:.1}%", v * 100.0))
}

/// Renders `report` as Markdown. Deterministic: the same report gives the
/// same text.
#[must_use]
pub fn render(report: &Report) -> String {
    let mut out = String::new();
    let h = &report.header;
    let _ = writeln!(out, "# Ablation report\n");
    let _ = writeln!(out, "> **{}**\n", report.notice);
    let _ = writeln!(out, "- schema: `{}`", report.schema);
    let _ = writeln!(
        out,
        "- sven commit(s): {}{}",
        h.sven_commits
            .iter()
            .map(|c| format!("`{c}`"))
            .collect::<Vec<_>>()
            .join(", "),
        if h.any_dirty {
            " (working tree had uncommitted changes)"
        } else {
            ""
        }
    );
    for b in &h.brain {
        let _ = writeln!(
            out,
            "- brain model: `{}` checkpoint `{}` commit `{}`",
            b.model_id,
            b.checkpoint.as_deref().unwrap_or("unknown"),
            b.commit.as_deref().unwrap_or("unknown")
        );
    }
    let _ = writeln!(out, "- matrix sha256: {}", h.matrix_sha256.join(", "));
    let _ = writeln!(
        out,
        "- {} runs, seeds {:?}, suites {}",
        h.runs_total,
        h.seeds,
        h.suites
            .iter()
            .map(|s| s.name())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let _ = writeln!(
        out,
        "- statistics: {:.0}% percentile bootstrap, {} resamples, seed {:#x}, unit: {}; no interval below {} pairs",
        h.statistics.confidence * 100.0,
        h.statistics.resamples,
        h.statistics.bootstrap_seed,
        h.statistics.unit,
        h.statistics.minimum_pairs
    );
    if h.context_assembly_ignored_by_binary {
        let _ = writeln!(
            out,
            "- **context assembly was requested by the variants but the sven binary has no `context.assembly.mode` key; those variants ran without it**"
        );
    }

    let _ = writeln!(out, "\n## Variants\n");
    let _ = writeln!(
        out,
        "| id | name | role | completed | failed | not available |\n|---|---|---|---:|---:|---|"
    );
    for v in &report.variants {
        let _ = writeln!(
            out,
            "| {} | {} | {:?} | {} | {} | {} |",
            v.id,
            v.name,
            v.role,
            v.completed,
            v.failed,
            if v.not_available.is_empty() {
                "-".into()
            } else {
                v.not_available.join("; ")
            }
        );
    }

    for suite in &report.suites {
        let _ = writeln!(
            out,
            "\n## Suite `{}`\n\n{}\n",
            suite.suite.name(),
            suite.measures
        );
        let _ = writeln!(
            out,
            "| variant | runs | pass | tokens mean | median | R_tokens vs baseline (geo-mean, 95% CI) | median R | critical recall | precision | shortlist recall | top-1 | MRR | decision calls | decision ms | expansions | duplicate calls | violations |"
        );
        let _ = writeln!(
            out,
            "|---|---:|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
        );
        for c in &suite.cells {
            row(&mut out, c);
        }
        let _ = writeln!(out, "\nMean tokens by source:\n");
        let _ = writeln!(out, "| variant | system | tool_schemas | user | assistant | tool_call_arguments | tool_results | media |\n|---|---:|---:|---:|---:|---:|---:|---:|");
        for c in suite.cells.iter().filter(|c| c.runs > 0) {
            let g = |k: &str| num(c.tokens_by_source_mean.get(k).copied(), 0);
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} | {} |",
                c.variant,
                g("system"),
                g("tool_schemas"),
                g("user"),
                g("assistant"),
                g("tool_call_arguments"),
                g("tool_results"),
                g("media")
            );
        }
    }

    let _ = writeln!(out, "\n## Attribution\n\nEach row compares two variants on the same tasks and seeds. `ratio` is reference / variant (above 1 is a saving); `difference` is variant minus reference (above 0 is better). An effect counts only when its interval excludes the no-effect value.\n");
    let _ = writeln!(out, "| comparison | scope | metric | pairs | median | estimate [95% CI] | excludes no-effect |\n|---|---|---|---:|---:|---|---|");
    for e in report
        .effects
        .iter()
        .filter(|e| e.scope == "all" || e.scope == "tool_selection")
    {
        let no_effect = if e.kind == "ratio" { 1.0 } else { 0.0 };
        let excludes = e
            .interval
            .is_some_and(|i| i.lower > no_effect || i.upper < no_effect);
        let _ = writeln!(
            out,
            "| {} ({} vs {}) | {} | {:?} {} | {} | {} | {} | {} |",
            e.label,
            e.variant,
            e.reference,
            e.scope,
            e.metric,
            e.kind,
            e.n_pairs,
            num(e.median, 3),
            interval(e.interval, 3),
            if e.interval.is_none() {
                "n/a"
            } else if excludes {
                "yes"
            } else {
                "no"
            }
        );
    }

    let _ = writeln!(out, "\n## Does a model contribute?\n");
    if report.verdicts.is_empty() {
        let _ = writeln!(out, "The matrix has no model variant.");
    }
    for v in &report.verdicts {
        let text = match &v.verdict {
            Verdict::ModelContributes { evidence } => {
                format!("**the model contributes** on {}", evidence.join(", "))
            }
            Verdict::DoesNotContribute { reason } => format!("**no contribution shown**: {reason}"),
            Verdict::NotDeterminable { reason } => format!("**not determinable**: {reason}"),
        };
        let _ = writeln!(out, "- variant {}: {text}", v.variant);
    }

    let _ = writeln!(out, "\n## Release gates\n");
    for grid in &report.gates {
        let _ = writeln!(
            out,
            "### Variant {}\n\n| gate | status | detail |\n|---|---|---|",
            grid.variant
        );
        for g in &grid.gates {
            let _ = writeln!(
                out,
                "| {} | **{}** | {} |",
                g.gate,
                g.status.label(),
                g.detail
            );
        }
        out.push('\n');
    }
    saved_by_reason_section(&mut out, report);
    if let Some(recovery) = &report.recovery {
        recovery_section(&mut out, recovery);
    }
    out
}

/// Which demotions the saving of each architecture variant came from.
fn saved_by_reason_section(out: &mut String, report: &Report) {
    let rows: Vec<(&str, &str, &Cell)> = report
        .suites
        .iter()
        .flat_map(|s| {
            s.cells
                .iter()
                .map(move |c| (s.suite.name(), c.variant.as_str(), c))
        })
        .filter(|(_, _, c)| !c.context_saved_by_reason_mean.is_empty())
        .collect();
    if rows.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "## Tokens saved by reason\n\nMean estimated tokens per run that context assembly saved, summed over the requests of the task, by why the result was demoted: `age` (the only copy of its evidence, oldest first), `superseded` and `duplicate` (a later message replaced it), `passing_test`.\n"
    );
    let _ = writeln!(
        out,
        "| suite | variant | age | superseded | duplicate | passing_test |\n|---|---|---:|---:|---:|---:|"
    );
    for (suite, variant, c) in rows {
        let m = &c.context_saved_by_reason_mean;
        let n = |k: &str| num(m.get(k).copied(), 0);
        let _ = writeln!(
            out,
            "| {suite} | {variant} | {} | {} | {} | {} |",
            n("age"),
            n("superseded"),
            n("duplicate"),
            n("passing_test")
        );
    }
    out.push('\n');
}

/// The recovery-aware tables: the same tasks with a scripted model that
/// fetches what a shortened result left out.
fn recovery_section(out: &mut String, recovery: &RecoveryReport) {
    let _ = writeln!(
        out,
        "## With recovery\n\nThe tasks whose final question names what to look at, run a second time with a scripted model that follows a reference back to the raw text when the shortened result mentions a term of the question (a fixed rule that does not know which lines matter). The main tables above are the model that never asks. `recall before` is the critical evidence in the request that first carried the final question, which is all the harness delivers on its own; `recall after` is the request the model answered from. Tokens include every fetch. Not measured: whether a real model would ask.\n"
    );
    for suite in &recovery.suites {
        let _ = writeln!(
            out,
            "### Suite `{}`\n\n| variant | runs | pass | recall before | recall after | tokens mean | R_tokens vs baseline (geo-mean, 95% CI) | median R | expansions | tokens fetched |\n|---|---:|---:|---:|---:|---:|---|---:|---:|---:|",
            suite.suite.name()
        );
        for c in suite.cells.iter().filter(|c| c.runs > 0) {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                c.variant,
                c.runs,
                c.passes,
                percent(c.recall_without_recovery),
                percent(c.recall_after_recovery),
                num(c.tokens_total_mean, 0),
                interval(c.r_tokens, 2),
                num(c.r_tokens_median, 2),
                num(c.expansions_mean, 1),
                num(c.expanded_tokens_mean, 0)
            );
        }
        out.push('\n');
    }
    let _ = writeln!(
        out,
        "### Release gates on the recovery-aware measurement\n\nThe main runs, each replaced by its recovery run where there is one.\n"
    );
    for grid in &recovery.gates {
        let _ = writeln!(
            out,
            "#### Variant {}\n\n| gate | status | detail |\n|---|---|---|",
            grid.variant
        );
        for g in &grid.gates {
            let _ = writeln!(
                out,
                "| {} | **{}** | {} |",
                g.gate,
                g.status.label(),
                g.detail
            );
        }
        out.push('\n');
    }
}

fn row(out: &mut String, c: &Cell) {
    let _ = writeln!(
        out,
        "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
        c.variant,
        c.runs,
        c.passes,
        num(c.tokens_total_mean, 0),
        num(c.tokens_total_median, 0),
        interval(c.r_tokens, 2),
        num(c.r_tokens_median, 2),
        percent(c.critical_recall),
        percent(c.evidence_precision_mean),
        percent(c.shortlist_recall_mean),
        percent(c.top1_mean),
        num(c.mrr_mean, 3),
        num(c.decision_calls_mean, 1),
        num(c.decision_latency_ms_mean, 1),
        num(c.recovery_expansions_mean, 1),
        num(c.duplicate_tool_calls_mean, 1),
        c.violations
    );
}
