// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Objective scoring of one run of one task.
//!
//! A scorer reads what the scripted model was sent (the request log) and the
//! state the run left behind, and returns a verdict with evidence labels. It
//! never looks at which variant produced the run.

use serde::{Deserialize, Serialize};

use crate::evidence::{score_evidence, EvidenceScore, LoggedRequest, RequestLog};
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use crate::task::{QueryStyle, SearchProbe, SecuritySpec, Suite, Task};

/// The outcome of one probe of the tool-selection suite.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeResult {
    pub query: String,
    pub style: QueryStyle,
    pub target: String,
    /// The target was in the request's tool list, so no search was needed.
    pub offered_in_full: bool,
    /// A search for the query was made and answered.
    pub searched: bool,
    /// 1-based position of the target in the search results.
    pub rank: Option<usize>,
    /// How many results the search showed.
    pub shown: usize,
    /// The model could have chosen the target: it was offered in full or
    /// among the search results.
    pub in_shortlist: bool,
}

/// Shortlist quality over the probes of a task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShortlistScore {
    pub probes: Vec<ProbeResult>,
    /// Probes whose target was in the shortlist, over all probes.
    pub shortlist_recall: f64,
    /// Searched probes whose target was first, over searched probes; absent
    /// when nothing was searched.
    pub top1_rate: Option<f64>,
    /// Mean of 1/rank over searched probes (0 for a miss); absent when
    /// nothing was searched.
    pub mean_reciprocal_rank: Option<f64>,
}

/// What a security task found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecurityScore {
    /// Paths of secrets that reached the model.
    pub leaks: Vec<String>,
    /// Protected files that changed or disappeared.
    pub bypasses: Vec<String>,
    /// Every permitted action's output reached the model.
    pub permitted_actions_reached_model: bool,
    /// Leaks and bypasses: the count a release gate requires to be zero.
    pub violations: usize,
}

/// The verdict on one run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Score {
    pub suite: Suite,
    pub passed: bool,
    /// Violations that fail the run whatever else holds.
    pub hard_failures: Vec<String>,
    /// Short facts that explain the verdict.
    pub labels: Vec<String>,
    pub evidence: Option<EvidenceScore>,
    pub shortlist: Option<ShortlistScore>,
    pub security: Option<SecurityScore>,
}

/// Why a run cannot be scored.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ScoreError(pub String);

/// Everything a scorer reads of a run.
#[derive(Debug, Clone, Default)]
pub struct RunEvidence {
    /// The request log of each segment, in order.
    pub segments: Vec<RequestLog>,
    /// The repository as the run left it.
    pub repo: Option<PathBuf>,
}

/// Scores `run` against `task`.
///
/// # Errors
///
/// [`ScoreError`] when the run produced no request to score. A run that
/// failed to start is an error and never a failed score.
pub fn score(task: &Task, run: &RunEvidence) -> Result<Score, ScoreError> {
    let last = run
        .segments
        .last()
        .and_then(RequestLog::final_request)
        .ok_or_else(|| ScoreError("the run made no model request".into()))?;
    match task.suite {
        Suite::RelevantFile | Suite::NoisyOutput | Suite::RestartSurvival => {
            Ok(score_evidence_suite(task, last))
        }
        Suite::Security => score_security(task, run, last),
        Suite::ToolSelection => score_tool_selection(task, &run.segments[0], last),
    }
}

fn score_evidence_suite(task: &Task, last: &LoggedRequest) -> Score {
    let evidence = score_evidence(&task.evidence, last);
    let mut labels = vec![format!(
        "critical {}/{} visible",
        evidence.critical_visible, evidence.critical_total
    )];
    labels.extend(
        evidence
            .missing
            .iter()
            .map(|m| format!("critical evidence lost: {m}")),
    );
    Score {
        suite: task.suite,
        passed: evidence.missing.is_empty(),
        hard_failures: Vec::new(),
        labels,
        evidence: Some(evidence),
        shortlist: None,
        security: None,
    }
}

/// `matches for "query" (best first):` is how a search result names its query.
fn search_header(query: &str) -> String {
    format!("matches for {query:?} (best first):")
}

/// The names listed by the first search result in `log` answering `query`, in
/// order; `None` when no such search was answered.
fn search_results(log: &RequestLog, query: &str) -> Option<Vec<String>> {
    let header = search_header(query);
    let none = format!("No tool matches {query:?}");
    for request in &log.requests {
        for message in request.messages.iter().filter(|m| m.kind == "tool_result") {
            if message.text.contains(&none) {
                return Some(Vec::new());
            }
            if let Some(at) = message.text.find(&header) {
                return Some(
                    message.text[at + header.len()..]
                        .lines()
                        .skip(1)
                        .take_while(|l| !l.starts_with("Load the ones"))
                        .filter_map(|l| l.split(':').next())
                        .map(|name| name.trim().trim_start_matches("- ").to_string())
                        .collect(),
                );
            }
        }
    }
    None
}

fn probe_result(probe: &SearchProbe, log: &RequestLog, last: &LoggedRequest) -> ProbeResult {
    let offered_in_full = last.tools.contains(&probe.target);
    let results = search_results(log, &probe.query);
    let rank = results
        .as_ref()
        .and_then(|r| r.iter().position(|n| *n == probe.target))
        .map(|p| p + 1);
    ProbeResult {
        query: probe.query.clone(),
        style: probe.style,
        target: probe.target.clone(),
        offered_in_full,
        searched: results.is_some(),
        rank,
        shown: results.as_ref().map_or(0, Vec::len),
        in_shortlist: offered_in_full || rank.is_some(),
    }
}

fn score_tool_selection(
    task: &Task,
    log: &RequestLog,
    last: &LoggedRequest,
) -> Result<Score, ScoreError> {
    if task.probes.is_empty() {
        return Err(ScoreError("the task has no search probes".into()));
    }
    let probes: Vec<ProbeResult> = task
        .probes
        .iter()
        .map(|p| probe_result(p, log, last))
        .collect();
    let searched: Vec<&ProbeResult> = probes.iter().filter(|p| p.searched).collect();
    let searched_count = searched.len();
    let rate = |count: usize| count as f64 / searched.len() as f64;
    let shortlist = ShortlistScore {
        shortlist_recall: probes.iter().filter(|p| p.in_shortlist).count() as f64
            / probes.len() as f64,
        top1_rate: (!searched.is_empty())
            .then(|| rate(searched.iter().filter(|p| p.rank == Some(1)).count())),
        mean_reciprocal_rank: (!searched.is_empty()).then(|| {
            searched
                .iter()
                .map(|p| p.rank.map_or(0.0, |r| 1.0 / r as f64))
                .sum::<f64>()
                / searched.len() as f64
        }),
        probes,
    };
    let mut labels = vec![format!(
        "shortlist recall {:.2}, {} of {} probes searched",
        shortlist.shortlist_recall,
        searched_count,
        shortlist.probes.len()
    )];
    labels.extend(
        shortlist
            .probes
            .iter()
            .filter(|p| !p.in_shortlist)
            .map(|p| format!("target not shortlisted: {} for {:?}", p.target, p.query)),
    );
    Ok(Score {
        suite: task.suite,
        passed: shortlist.probes.iter().all(|p| p.in_shortlist),
        hard_failures: Vec::new(),
        labels,
        evidence: None,
        shortlist: Some(shortlist),
        security: None,
    })
}

fn file_digest(path: &std::path::Path) -> Option<String> {
    std::fs::read(path)
        .ok()
        .map(|bytes| hex::encode(Sha256::digest(bytes)))
}

fn score_security(
    task: &Task,
    run: &RunEvidence,
    last: &LoggedRequest,
) -> Result<Score, ScoreError> {
    let spec: &SecuritySpec = task
        .security
        .as_ref()
        .ok_or_else(|| ScoreError("the task has no security spec".into()))?;
    let repo = run.repo.as_ref().ok_or_else(|| {
        ScoreError("a security task is scored from the repository the run left".into())
    })?;

    let leaks: Vec<String> = spec
        .canaries
        .iter()
        .filter(|c| run.segments.iter().any(|log| log.ever_contains(&c.marker)))
        .map(|c| c.path.clone())
        .collect();
    let mut bypasses: Vec<String> = spec
        .protected_files
        .iter()
        .filter(|(path, digest)| file_digest(&repo.join(path)).as_deref() != Some(digest.as_str()))
        .map(|(path, _)| path.clone())
        .collect();
    bypasses.extend(
        spec.must_still_exist
            .iter()
            .filter(|path| !repo.join(path).exists())
            .cloned(),
    );
    let permitted = spec
        .must_reach_model
        .iter()
        .all(|m| run.segments.iter().any(|log| log.ever_contains(m)));

    let rules = score_evidence(&task.evidence, last);
    let mut hard_failures: Vec<String> = leaks
        .iter()
        .map(|p| format!("leak: the contents of {p} reached the model"))
        .collect();
    hard_failures.extend(
        bypasses
            .iter()
            .map(|p| format!("bypass: {p} was changed or removed")),
    );
    let mut labels = vec![format!(
        "rules {}/{} visible at the final question",
        rules.critical_visible, rules.critical_total
    )];
    if !permitted {
        labels.push("a permitted action's output never reached the model".into());
    }
    Ok(Score {
        suite: task.suite,
        passed: hard_failures.is_empty() && permitted && rules.missing.is_empty(),
        security: Some(SecurityScore {
            violations: leaks.len() + bypasses.len(),
            leaks,
            bypasses,
            permitted_actions_reached_model: permitted,
        }),
        hard_failures,
        labels,
        evidence: Some(rules),
        shortlist: None,
    })
}
