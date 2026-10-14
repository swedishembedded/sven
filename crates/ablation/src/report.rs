// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The report: paired comparisons of variants, the attribution of savings,
//! the verdict on a decision model and the release gates, all derived from
//! the raw run records and nothing else.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::matrix::{Matrix, Role};
use crate::record::{BrainIdentity, RunRecord, RunStatus, SCRIPTED_NOTICE};
use crate::stats::{
    geometric_mean_ratio, mean, median, paired_mean, Interval, CONFIDENCE, RESAMPLES,
};
use crate::task::Suite;

/// The only report format this build writes.
pub const REPORT_SCHEMA: &str = "sven.ablation.report/1";

/// Seed of every bootstrap in a report.
pub const BOOTSTRAP_SEED: u64 = 0x5eed_ab1a;

/// Paired runs needed before an interval is reported or a gate is decided.
pub const MIN_PAIRS: usize = 3;

/// Suites a token-reduction claim is made on.
pub const TARGETED_SUITES: [Suite; 4] = [
    Suite::RelevantFile,
    Suite::NoisyOutput,
    Suite::ToolSelection,
    Suite::RestartSurvival,
];

/// What a measured quantity is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    /// Estimated input tokens of the whole task; lower is better, compared
    /// as the ratio baseline / variant.
    Tokens,
    /// 1 when the run passed its scorer.
    Pass,
    CriticalRecall,
    ShortlistRecall,
    Top1,
    Mrr,
}

impl Metric {
    #[must_use]
    pub fn value(self, run: &RunRecord) -> Option<f64> {
        let score = run.score.as_ref()?;
        match self {
            Self::Tokens => run.tokens.as_ref().map(|t| t.total as f64),
            Self::Pass => Some(f64::from(u8::from(score.passed))),
            Self::CriticalRecall => score.evidence.as_ref().map(|e| e.critical_recall),
            Self::ShortlistRecall => score.shortlist.as_ref().map(|s| s.shortlist_recall),
            Self::Top1 => score.shortlist.as_ref().and_then(|s| s.top1_rate),
            Self::Mrr => score
                .shortlist
                .as_ref()
                .and_then(|s| s.mean_reciprocal_rank),
        }
    }
}

/// The statistics a report was made with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatisticsNote {
    pub confidence: f64,
    pub resamples: usize,
    pub bootstrap_seed: u64,
    pub unit: String,
    pub minimum_pairs: usize,
}

/// What produced the runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Header {
    pub sven_commits: Vec<String>,
    pub any_dirty: bool,
    pub brain: Vec<BrainIdentity>,
    pub matrix_sha256: Vec<String>,
    pub seeds: Vec<u64>,
    pub suites: Vec<Suite>,
    pub runs_total: usize,
    pub statistics: StatisticsNote,
    /// Variants whose `context_assembly` request had no effect because the
    /// binary has no such key.
    pub context_assembly_ignored_by_binary: bool,
}

/// How many runs of a variant ended each way.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VariantSummary {
    pub id: String,
    pub name: String,
    pub role: Role,
    pub completed: usize,
    pub failed: usize,
    pub not_available: Vec<String>,
}

/// One variant in one suite.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    pub variant: String,
    pub runs: usize,
    pub tokens_total_mean: Option<f64>,
    pub tokens_total_median: Option<f64>,
    pub tokens_by_source_mean: BTreeMap<String, f64>,
    /// Baseline / variant, geometric mean over pairs.
    pub r_tokens: Option<Interval>,
    pub r_tokens_median: Option<f64>,
    /// Sum of baseline tokens over sum of variant tokens.
    pub r_tokens_of_sums: Option<f64>,
    pub passes: usize,
    pub violations: usize,
    /// Visible critical items over all critical items, over every run.
    pub critical_recall: Option<f64>,
    pub evidence_precision_mean: Option<f64>,
    pub shortlist_recall_mean: Option<f64>,
    pub top1_mean: Option<f64>,
    pub mrr_mean: Option<f64>,
    pub decision_calls_mean: Option<f64>,
    pub decision_latency_ms_mean: Option<f64>,
    pub recovery_expansions_mean: Option<f64>,
    pub duplicate_tool_calls_mean: Option<f64>,
}

/// One suite and its variants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuiteReport {
    pub suite: Suite,
    pub measures: String,
    pub cells: Vec<Cell>,
}

/// An effect of `variant` relative to `reference`, paired by task and seed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    pub label: String,
    pub variant: String,
    pub reference: String,
    /// A suite name, or `all`, or `targeted`.
    pub scope: String,
    pub metric: Metric,
    /// `ratio` (reference / variant, above 1 is better) or `difference`
    /// (variant minus reference, above 0 is better).
    pub kind: String,
    pub n_pairs: usize,
    pub median: Option<f64>,
    pub interval: Option<Interval>,
}

impl Effect {
    /// The interval shows `variant` better than `reference`.
    #[must_use]
    pub fn better(&self) -> bool {
        self.interval.is_some_and(|i| {
            if self.kind == "ratio" {
                i.lower > 1.0
            } else {
                i.above_zero()
            }
        })
    }
}

/// Whether a decision model earned its place.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    /// Better than B, C and F with intervals that exclude zero.
    ModelContributes {
        evidence: Vec<String>,
    },
    DoesNotContribute {
        reason: String,
    },
    NotDeterminable {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelVerdict {
    pub variant: String,
    #[serde(flatten)]
    pub verdict: Verdict,
}

/// The state of a release gate. Never `Pass` for something unmeasured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GateStatus {
    #[serde(rename = "PASS")]
    Pass,
    #[serde(rename = "FAIL")]
    Fail,
    #[serde(rename = "NOT-MEASURED")]
    NotMeasured,
}

impl GateStatus {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::NotMeasured => "NOT-MEASURED",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateResult {
    pub gate: String,
    pub status: GateStatus,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateGrid {
    pub variant: String,
    pub gates: Vec<GateResult>,
}

/// The whole report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub schema: String,
    pub scripted: bool,
    pub notice: String,
    pub header: Header,
    pub variants: Vec<VariantSummary>,
    pub suites: Vec<SuiteReport>,
    pub effects: Vec<Effect>,
    pub verdicts: Vec<ModelVerdict>,
    pub gates: Vec<GateGrid>,
    /// Runs whose scripted model fetches what shortened results left out,
    /// set beside the main runs; absent when there are none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<crate::report_recovery::RecoveryReport>,
}

/// Completed, scored runs of the main experiment, by variant then task.
pub(crate) struct View<'a> {
    by_variant: BTreeMap<&'a str, BTreeMap<&'a str, &'a RunRecord>>,
}

impl<'a> View<'a> {
    pub(crate) fn new(runs: &'a [RunRecord]) -> Self {
        let mut by_variant: BTreeMap<&str, BTreeMap<&str, &RunRecord>> = BTreeMap::new();
        for run in runs.iter().filter(|r| r.probe.is_none() && r.completed()) {
            by_variant
                .entry(&run.variant.id)
                .or_default()
                .insert(&run.task_id, run);
        }
        Self { by_variant }
    }

    pub(crate) fn runs(&self, variant: &str) -> Vec<&'a RunRecord> {
        self.by_variant
            .get(variant)
            .map(|m| m.values().copied().collect())
            .unwrap_or_default()
    }

    /// `(reference value, variant value)` of `metric` for every task both
    /// completed, in `scope`.
    pub(crate) fn pairs(
        &self,
        reference: &str,
        variant: &str,
        scope: &[Suite],
        metric: Metric,
    ) -> Vec<(f64, f64)> {
        let (Some(r), Some(v)) = (self.by_variant.get(reference), self.by_variant.get(variant))
        else {
            return Vec::new();
        };
        r.iter()
            .filter(|(_, run)| scope.contains(&run.suite))
            .filter_map(|(task, run)| {
                let other = v.get(task)?;
                Some((metric.value(run)?, metric.value(other)?))
            })
            .collect()
    }
}

/// What to compare: `variant` against `reference` on `metric`, over the
/// tasks of `scope`.
pub(crate) struct Comparison<'a> {
    pub label: &'a str,
    pub reference: &'a str,
    pub variant: &'a str,
    pub scope_name: &'a str,
    pub scope: &'a [Suite],
    pub metric: Metric,
}

/// The effect of a comparison, paired by task and seed.
pub(crate) fn effect(view: &View<'_>, c: &Comparison<'_>) -> Effect {
    let Comparison {
        label,
        reference,
        variant,
        scope_name,
        scope,
        metric,
    } = *c;
    let pairs = view.pairs(reference, variant, scope, metric);
    let stream = format!("{label}/{scope_name}/{metric:?}/{variant}/{reference}");
    let (kind, interval, med) = if metric == Metric::Tokens {
        let ratios: Vec<f64> = pairs.iter().map(|(r, v)| r / v).collect();
        (
            "ratio",
            geometric_mean_ratio(&ratios, BOOTSTRAP_SEED, &stream),
            median(&ratios),
        )
    } else {
        let diffs: Vec<f64> = pairs.iter().map(|(r, v)| v - r).collect();
        (
            "difference",
            paired_mean(&diffs, BOOTSTRAP_SEED, &stream),
            median(&diffs),
        )
    };
    Effect {
        label: label.to_string(),
        variant: variant.to_string(),
        reference: reference.to_string(),
        scope: scope_name.to_string(),
        metric,
        kind: kind.to_string(),
        n_pairs: pairs.len(),
        median: med,
        interval,
    }
}

fn mean_of(runs: &[&RunRecord], f: impl Fn(&RunRecord) -> Option<f64>) -> Option<f64> {
    mean(&runs.iter().filter_map(|r| f(r)).collect::<Vec<_>>())
}

fn cell(view: &View<'_>, baseline: &str, variant: &str, suite: Suite) -> Cell {
    let runs: Vec<&RunRecord> = view
        .runs(variant)
        .into_iter()
        .filter(|r| r.suite == suite)
        .collect();
    let totals: Vec<f64> = runs
        .iter()
        .filter_map(|r| Metric::Tokens.value(r))
        .collect();
    let mut by_source: BTreeMap<String, f64> = BTreeMap::new();
    for run in &runs {
        if let Some(tokens) = &run.tokens {
            for (source, n) in &tokens.by_source {
                *by_source.entry(source.clone()).or_default() += *n as f64 / runs.len() as f64;
            }
        }
    }
    let pairs = view.pairs(baseline, variant, &[suite], Metric::Tokens);
    let ratios: Vec<f64> = pairs.iter().map(|(r, v)| r / v).collect();
    let (base_sum, var_sum) = pairs
        .iter()
        .fold((0.0, 0.0), |(a, b), (r, v)| (a + r, b + v));
    let (visible, total) = runs
        .iter()
        .filter_map(|r| r.score.as_ref()?.evidence.as_ref())
        .fold((0, 0), |(v, t), e| {
            (v + e.critical_visible, t + e.critical_total)
        });
    Cell {
        variant: variant.to_string(),
        runs: runs.len(),
        tokens_total_mean: mean(&totals),
        tokens_total_median: median(&totals),
        tokens_by_source_mean: by_source,
        r_tokens: geometric_mean_ratio(
            &ratios,
            BOOTSTRAP_SEED,
            &format!("cell/{}/{variant}", suite.name()),
        ),
        r_tokens_median: median(&ratios),
        r_tokens_of_sums: (var_sum > 0.0).then(|| base_sum / var_sum),
        passes: runs
            .iter()
            .filter(|r| r.score.as_ref().is_some_and(|s| s.passed))
            .count(),
        violations: runs
            .iter()
            .filter_map(|r| r.score.as_ref()?.security.as_ref())
            .map(|s| s.violations)
            .sum(),
        critical_recall: (total > 0).then(|| visible as f64 / total as f64),
        evidence_precision_mean: mean_of(&runs, |r| r.score.as_ref()?.evidence.as_ref()?.precision),
        shortlist_recall_mean: mean_of(&runs, |r| Metric::ShortlistRecall.value(r)),
        top1_mean: mean_of(&runs, |r| Metric::Top1.value(r)),
        mrr_mean: mean_of(&runs, |r| Metric::Mrr.value(r)),
        decision_calls_mean: mean_of(&runs, |r| Some(r.decisions.inference_calls as f64)),
        decision_latency_ms_mean: mean_of(&runs, |r| Some(r.decisions.latency_ms)),
        recovery_expansions_mean: mean_of(&runs, |r| {
            r.tokens.as_ref().map(|t| t.recovery_expansions as f64)
        }),
        duplicate_tool_calls_mean: mean_of(&runs, |r| {
            r.tokens.as_ref().map(|t| t.duplicate_tool_calls as f64)
        }),
    }
}

/// The comparisons the attribution table is made of: what each step of the
/// design adds, and what a model adds on top of each cheaper alternative.
const COMPARISONS: [(&str, Role, Role); 10] = [
    ("architecture saving", Role::Baseline, Role::Architecture),
    (
        "heuristic over architecture",
        Role::Architecture,
        Role::Heuristic,
    ),
    (
        "random control over architecture",
        Role::Architecture,
        Role::NegativeControl,
    ),
    (
        "stock model over heuristic",
        Role::Heuristic,
        Role::StockModel,
    ),
    (
        "stock model over random control",
        Role::NegativeControl,
        Role::StockModel,
    ),
    (
        "stock model over architecture",
        Role::Architecture,
        Role::StockModel,
    ),
    (
        "trained model over heuristic",
        Role::Heuristic,
        Role::TrainedModel,
    ),
    (
        "trained model over random control",
        Role::NegativeControl,
        Role::TrainedModel,
    ),
    (
        "trained model over architecture",
        Role::Architecture,
        Role::TrainedModel,
    ),
    (
        "oracle headroom over heuristic",
        Role::Heuristic,
        Role::Oracle,
    ),
];

/// Metrics an effect is reported for in every scope.
const EFFECT_METRICS: [Metric; 5] = [
    Metric::Tokens,
    Metric::Pass,
    Metric::CriticalRecall,
    Metric::ShortlistRecall,
    Metric::Mrr,
];

fn effects(view: &View<'_>, matrix: &Matrix, suites: &[Suite]) -> Vec<Effect> {
    let mut out = Vec::new();
    for (label, reference, variant) in COMPARISONS {
        let (Some(r), Some(v)) = (matrix.by_role(reference), matrix.by_role(variant)) else {
            continue;
        };
        let mut scopes: Vec<(String, Vec<Suite>)> = vec![("all".into(), suites.to_vec())];
        scopes.extend(suites.iter().map(|s| (s.name().to_string(), vec![*s])));
        for (name, scope) in &scopes {
            for metric in EFFECT_METRICS {
                let e = effect(
                    view,
                    &Comparison {
                        label,
                        reference: &r.id,
                        variant: &v.id,
                        scope_name: name,
                        scope,
                        metric,
                    },
                );
                if e.n_pairs > 0 {
                    out.push(e);
                }
            }
        }
    }
    out
}

/// A model contributes when, on a primary metric, it beats the architecture
/// alone, the heuristic and the random control, each with an interval that
/// excludes zero. The primary metrics are fixed here, before any data: the
/// shortlist of tool search, critical evidence kept, and tokens.
const PRIMARY: [(&str, Metric); 4] = [
    ("tool_selection", Metric::Mrr),
    ("tool_selection", Metric::ShortlistRecall),
    ("all", Metric::CriticalRecall),
    ("all", Metric::Tokens),
];

fn verdicts(view: &View<'_>, matrix: &Matrix, suites: &[Suite]) -> Vec<ModelVerdict> {
    [Role::StockModel, Role::TrainedModel]
        .into_iter()
        .filter_map(|role| matrix.by_role(role))
        .map(|model| {
            let refs: Vec<&str> = [Role::Architecture, Role::Heuristic, Role::NegativeControl]
                .into_iter()
                .filter_map(|r| matrix.by_role(r).map(|v| v.id.as_str()))
                .collect();
            let verdict = if refs.len() < 3 {
                Verdict::NotDeterminable {
                    reason:
                        "the matrix lacks the architecture, heuristic or random-control variant"
                            .into(),
                }
            } else if view.runs(&model.id).is_empty() {
                Verdict::NotDeterminable {
                    reason: format!("variant {} has no completed runs", model.id),
                }
            } else if let Some(missing) = refs.iter().find(|r| view.runs(r).is_empty()) {
                Verdict::NotDeterminable {
                    reason: format!("variant {missing} has no completed runs to compare with"),
                }
            } else {
                decide(view, &model.id, &refs, suites)
            };
            ModelVerdict {
                variant: model.id.clone(),
                verdict,
            }
        })
        .collect()
}

fn decide(view: &View<'_>, model: &str, refs: &[&str], suites: &[Suite]) -> Verdict {
    let mut evidence = Vec::new();
    for (scope, metric) in PRIMARY {
        let suites_in: Vec<Suite> = if scope == "all" {
            suites.to_vec()
        } else {
            suites
                .iter()
                .copied()
                .filter(|s| s.name() == scope)
                .collect()
        };
        let wins = refs.iter().all(|r| {
            effect(
                view,
                &Comparison {
                    label: "verdict",
                    reference: r,
                    variant: model,
                    scope_name: scope,
                    scope: &suites_in,
                    metric,
                },
            )
            .better()
        });
        if wins {
            evidence.push(format!("{scope}/{metric:?}"));
        }
    }
    if evidence.is_empty() {
        Verdict::DoesNotContribute {
            reason: "on no primary metric does it beat the architecture, the heuristic and the random control with an interval that excludes zero".into(),
        }
    } else {
        Verdict::ModelContributes { evidence }
    }
}

/// Builds the report for `runs` of `matrix`.
#[must_use]
pub fn build(matrix: &Matrix, runs: &[RunRecord]) -> Report {
    let view = View::new(runs);
    let baseline = matrix.baseline().id.clone();
    let mut suites: Vec<Suite> = runs.iter().map(|r| r.suite).collect();
    suites.sort();
    suites.dedup();
    let mut seeds: Vec<u64> = runs.iter().map(|r| r.seed).collect();
    seeds.sort_unstable();
    seeds.dedup();

    let distinct = |f: &dyn Fn(&RunRecord) -> String| {
        let mut v: Vec<String> = runs.iter().map(f).collect();
        v.sort();
        v.dedup();
        v
    };
    let mut brain: Vec<BrainIdentity> = runs
        .iter()
        .filter_map(|r| r.provenance.brain.clone())
        .collect();
    brain.sort_by(|a, b| {
        a.model_id
            .cmp(&b.model_id)
            .then(a.checkpoint.cmp(&b.checkpoint))
    });
    brain.dedup();

    let header = Header {
        sven_commits: distinct(&|r| r.provenance.sven_commit.clone()),
        any_dirty: runs.iter().any(|r| r.provenance.sven_dirty),
        brain,
        matrix_sha256: distinct(&|r| r.provenance.matrix_sha256.clone()),
        seeds,
        suites: suites.clone(),
        runs_total: runs.len(),
        statistics: StatisticsNote {
            confidence: CONFIDENCE,
            resamples: RESAMPLES,
            bootstrap_seed: BOOTSTRAP_SEED,
            unit: "one task at one seed run under two variants (a pair); turns and requests are never resampled".into(),
            minimum_pairs: MIN_PAIRS,
        },
        context_assembly_ignored_by_binary: runs.iter().any(|r| {
            r.realization.as_ref().is_some_and(|x| {
                !x.context_assembly_supported && x.context_assembly_requested != "off"
            })
        }),
    };

    let variants = matrix
        .variants
        .iter()
        .map(|v| {
            let mine: Vec<&RunRecord> = runs
                .iter()
                .filter(|r| r.variant.id == v.id && r.probe.is_none())
                .collect();
            let mut not_available: Vec<String> = mine
                .iter()
                .filter_map(|r| match &r.status {
                    RunStatus::NotAvailable { reason } => Some(reason.clone()),
                    _ => None,
                })
                .collect();
            not_available.sort();
            not_available.dedup();
            VariantSummary {
                id: v.id.clone(),
                name: v.name.clone(),
                role: v.role,
                completed: mine.iter().filter(|r| r.completed()).count(),
                failed: mine
                    .iter()
                    .filter(|r| matches!(r.status, RunStatus::Failed { .. }))
                    .count(),
                not_available,
            }
        })
        .collect();

    let suite_reports = suites
        .iter()
        .map(|suite| SuiteReport {
            suite: *suite,
            measures: runs
                .iter()
                .find(|r| r.suite == *suite)
                .map(|_| crate::suites::measures(*suite))
                .unwrap_or_default(),
            cells: matrix
                .variants
                .iter()
                .map(|v| cell(&view, &baseline, &v.id, *suite))
                .collect(),
        })
        .collect();

    Report {
        schema: REPORT_SCHEMA.to_string(),
        scripted: runs.iter().all(|r| r.scripted),
        notice: SCRIPTED_NOTICE.to_string(),
        header,
        variants,
        suites: suite_reports,
        effects: effects(&view, matrix, &suites),
        verdicts: verdicts(&view, matrix, &suites),
        gates: crate::gates::grid(matrix, runs),
        recovery: crate::report_recovery::build(matrix, runs, &view),
    }
}
