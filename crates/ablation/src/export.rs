// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Training fixtures: the decisions a run provoked, with a label that can be
//! audited and a split that cannot leak.
//!
//! One JSON object per line, schema `sven.ablation.fixture/1`. An example is
//! one choice question as the decision provider was asked it: the state, the
//! options with their descriptions, the correct option and where that label
//! came from. `(state, options, label_index)` is exactly the triple a choice
//! trainer consumes. `state_ref` names the run, the task and the digest of the
//! inputs that sven recorded in its own trajectory, so any example can be
//! traced back to the repository, the request and the verdict.
//!
//! Splits are decided per task family and seed and never per example: every
//! example of one task, from every variant, lands in one split, because the
//! same question asked under several variants is the same question.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::loader::load_runs;
use crate::provider_server::ServedDecision;
use crate::record::RunRecord;
use crate::task::{read_task, Suite, Task};

/// The only fixture format this build writes.
pub const FIXTURE_SCHEMA: &str = "sven.ablation.fixture/1";

/// Where the label of an example comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelSource {
    /// The task's author knows the right answer: the generator labelled it.
    Oracle,
    /// Reserved: the label is the option that gave the better outcome when
    /// the decision was replayed with each option. Not written yet.
    CounterfactualReplay,
    /// Reserved: the label is what a scorer's evidence requires. Not written
    /// yet.
    Scorer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    Train,
    Validation,
    Test,
}

/// Where an example came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateRef {
    /// `<task id>/<variant id>`: the run directory under `runs/`.
    pub run: String,
    pub task: String,
    pub suite: Suite,
    pub seed: u64,
    pub variant: String,
    /// The digest sven recorded for these inputs in `sven_decisions`.
    pub inputs_digest: String,
    pub question_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExampleQuestion {
    pub id: String,
    pub instruction: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExampleOption {
    pub id: String,
    pub description: Option<String>,
}

/// What became of the run the decision was part of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub task_passed: bool,
    /// Baseline tokens minus this run's, over the whole task; absent when the
    /// baseline did not run this task.
    pub llm_tokens_saved: Option<i64>,
    pub recovery_calls: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Example {
    pub schema: String,
    pub decision_type: String,
    pub state_ref: StateRef,
    pub question: ExampleQuestion,
    pub options: Vec<ExampleOption>,
    pub label: String,
    pub label_index: usize,
    pub label_source: LabelSource,
    pub outcome: Outcome,
    pub split: Split,
}

/// How tasks are divided into train, validation and test.
#[derive(Debug, Clone)]
pub struct SplitPlan {
    pub seed: u64,
    pub validation_fraction: f64,
    pub test_fraction: f64,
    /// Whole task families that are test only.
    pub holdout_suites: Vec<Suite>,
}

impl Default for SplitPlan {
    fn default() -> Self {
        Self {
            seed: 0,
            validation_fraction: 0.2,
            test_fraction: 0.2,
            holdout_suites: Vec::new(),
        }
    }
}

fn rank(plan: &SplitPlan, suite: Suite, seed: u64) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(plan.seed.to_le_bytes());
    h.update(suite.name().as_bytes());
    h.update(seed.to_le_bytes());
    h.finalize().into()
}

/// The split of each `(suite, seed)` group. Within a family the seeds are
/// ordered by a hash that depends on the plan's seed and divided by the
/// fractions; a family with fewer than three seeds is all train, because
/// nothing can be held out of it without emptying it. A holdout suite is all
/// test.
#[must_use]
pub fn assign_splits(
    groups: &BTreeSet<(Suite, u64)>,
    plan: &SplitPlan,
) -> BTreeMap<(Suite, u64), Split> {
    let mut out = BTreeMap::new();
    let mut families: BTreeMap<Suite, Vec<u64>> = BTreeMap::new();
    for (suite, seed) in groups {
        families.entry(*suite).or_default().push(*seed);
    }
    for (suite, mut seeds) in families {
        if plan.holdout_suites.contains(&suite) {
            out.extend(seeds.iter().map(|s| ((suite, *s), Split::Test)));
            continue;
        }
        seeds.sort_by_key(|s| rank(plan, suite, *s));
        let n = seeds.len();
        let (validation, test) = if n < 3 {
            (0, 0)
        } else {
            let part = |f: f64| ((n as f64 * f).round() as usize).max(1);
            (part(plan.validation_fraction), part(plan.test_fraction))
        };
        for (i, seed) in seeds.iter().enumerate() {
            let split = if i < test {
                Split::Test
            } else if i < test + validation {
                Split::Validation
            } else {
                Split::Train
            };
            out.insert((suite, *seed), split);
        }
    }
    out
}

fn label_for(task: &Task, decision: &ServedDecision, options: &[ExampleOption]) -> Option<usize> {
    let state = decision.state.to_lowercase();
    task.oracle_labels
        .iter()
        .filter(|l| state.contains(&l.state_contains.to_lowercase()))
        .find_map(|l| options.iter().position(|o| o.id == l.correct_option))
}

fn read_decisions(path: &Path) -> Result<Vec<ServedDecision>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(|e| format!("{}: {e}", path.display())))
        .collect()
}

/// The unsplit examples of one run.
fn run_examples(
    dir: &Path,
    run: &RunRecord,
    baseline_tokens: Option<u64>,
) -> Result<Vec<Example>, String> {
    let served = dir.join("decisions.jsonl");
    if !served.exists() || !run.completed() {
        return Ok(Vec::new());
    }
    let task = read_task(dir)?;
    let outcome = Outcome {
        task_passed: run.score.as_ref().is_some_and(|s| s.passed),
        llm_tokens_saved: baseline_tokens
            .zip(run.tokens.as_ref().map(|t| t.total))
            .map(|(b, t)| b as i64 - t as i64),
        recovery_calls: run.tokens.as_ref().map_or(0, |t| t.recovery_expansions),
    };
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for decision in read_decisions(&served)? {
        for q in decision.questions.iter().filter(|q| q.kind == "choice") {
            let options: Vec<ExampleOption> = q
                .options
                .iter()
                .map(|(id, description)| ExampleOption {
                    id: id.clone(),
                    description: description.clone(),
                })
                .collect();
            let Some(label_index) = label_for(&task, &decision, &options) else {
                continue;
            };
            // The same question asked in two option orders is one example.
            let mut ids: Vec<&str> = options.iter().map(|o| o.id.as_str()).collect();
            ids.sort_unstable();
            if !seen.insert((decision.state.clone(), ids.join("\u{1}"), q.id.clone())) {
                continue;
            }
            out.push(Example {
                schema: FIXTURE_SCHEMA.to_string(),
                decision_type: "tool_routing".to_string(),
                state_ref: StateRef {
                    run: format!("{}/{}", run.task_id, run.variant.id),
                    task: run.task_id.clone(),
                    suite: run.suite,
                    seed: run.seed,
                    variant: run.variant.id.clone(),
                    inputs_digest: decision.inputs_digest.clone(),
                    question_id: q.id.clone(),
                },
                question: ExampleQuestion {
                    id: q.id.clone(),
                    instruction: q.instruction.clone(),
                    state: decision.state.clone(),
                },
                label: options[label_index].id.clone(),
                label_index,
                options,
                label_source: LabelSource::Oracle,
                outcome: outcome.clone(),
                split: Split::Train,
            });
        }
    }
    Ok(out)
}

/// The examples of every run under `dir`, split by `plan`.
///
/// # Errors
///
/// A record, task or decision log that cannot be read.
pub fn export(dir: &Path, plan: &SplitPlan) -> Result<Vec<Example>, String> {
    let runs = load_runs(dir)?;
    let baseline_tokens: BTreeMap<&str, u64> = runs
        .iter()
        .filter(|r| r.probe.is_none() && r.variant.role == crate::matrix::Role::Baseline)
        .filter_map(|r| Some((r.task_id.as_str(), r.tokens.as_ref()?.total)))
        .collect();
    let mut examples = Vec::new();
    for run in runs.iter().filter(|r| r.probe.is_none()) {
        let run_dir = dir.join("runs").join(&run.task_id).join(&run.variant.id);
        examples.extend(run_examples(
            &run_dir,
            run,
            baseline_tokens.get(run.task_id.as_str()).copied(),
        )?);
    }
    let groups: BTreeSet<(Suite, u64)> = examples
        .iter()
        .map(|e| (e.state_ref.suite, e.state_ref.seed))
        .collect();
    let splits = assign_splits(&groups, plan);
    for e in &mut examples {
        e.split = splits[&(e.state_ref.suite, e.state_ref.seed)];
    }
    examples.sort_by(|a, b| {
        (a.split, &a.state_ref.run, &a.question.state).cmp(&(
            b.split,
            &b.state_ref.run,
            &b.question.state,
        ))
    });
    Ok(examples)
}

/// The examples as JSON lines.
///
/// # Errors
///
/// An example that does not serialize.
pub fn to_jsonl(examples: &[Example]) -> Result<String, String> {
    let mut out = String::new();
    for e in examples {
        out.push_str(&serde_json::to_string(e).map_err(|e| e.to_string())?);
        out.push('\n');
    }
    Ok(out)
}
