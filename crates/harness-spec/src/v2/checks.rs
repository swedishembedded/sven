// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Lowering of a task's checks and repair attempts into steps.
//!
//! The checks of a task run in order. Each failed check either earns a repair
//! turn (and the whole sequence is run again) or, when the attempts are spent,
//! ends the run with a failure that names the check. The attempts are
//! unrolled: attempt `k` of the sequence is a copy of the check steps whose
//! failures lead to repair `k + 1`, so the plan is acyclic, its size is
//! visible and bounded, and no step can be reached twice.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use super::rewrite::{rewrite_text, FailureView, Scope};
use super::spec::Threshold;
use crate::error::HarnessError;
use crate::gate_command::GateCommand;
use crate::source_map::SourceRef;
use crate::spec::{
    AgentStepSpec, BranchStepSpec, FailStepSpec, GateStepSpec, PredictStepSpec, StepDecoding,
    StepSpec, VerifyStepSpec,
};

/// The prompt a repair turn gets when the author wrote none.
pub(super) const DEFAULT_REPAIR_PROMPT: &str = "A verification of your work failed.\n\
Check: {{ failure.gate }} ({{ failure.kind }})\n\
Detail: {{ failure.detail }}\n\
Exit code: {{ failure.exit_code }}\n\
Standard output:\n{{ failure.stdout }}\n\
Standard error:\n{{ failure.stderr }}\n\
This is repair attempt {{ failure.attempt }}. Find the cause and fix it; do not weaken, \
skip or bypass the check.";

/// The generated steps and where each came from.
#[derive(Default)]
pub(super) struct Builder {
    pub steps: BTreeMap<String, StepSpec>,
    pub source: BTreeMap<String, SourceRef>,
}

impl Builder {
    /// Adds a generated step with its authored origin.
    pub fn add(&mut self, id: &str, step: StepSpec, origin: SourceRef) {
        self.steps.insert(id.to_string(), step);
        self.source.insert(id.to_string(), origin);
    }
}

/// A command to run, with its arguments already moved into run namespaces.
#[derive(Clone)]
pub(super) struct CommandRun {
    pub steps: Vec<GateCommand>,
    pub env: BTreeMap<String, String>,
    pub timeout: u64,
    pub cwd: Option<String>,
    pub cwd_input: Option<String>,
    pub output_bytes: Option<u64>,
    pub stdin: Option<String>,
    /// The fact the typed result is kept under: the gate's latest run.
    pub store_as: String,
    /// The variable the gate's standard output is stored in when it passes.
    pub capture: Option<String>,
}

/// What one check does.
pub(super) enum Kind {
    /// Run a command; its exit status decides.
    Command(CommandRun),
    /// A file must exist (optionally with a size or a content hash).
    File {
        path: String,
        min_bytes: Option<u64>,
        sha256: Option<String>,
    },
    /// A fact the run holds must have this value.
    Fact {
        path: String,
        equals: Value,
        /// What failing it means, for the failure message.
        failure: String,
    },
    /// A number in a JSON file the command wrote, after the command ran.
    Metric {
        run: Option<CommandRun>,
        file: String,
        pointer: String,
        threshold: Threshold,
    },
}

/// A check, resolved against the declared gates.
pub(super) struct Resolved {
    /// The gate or file the check is named by in messages.
    pub label: String,
    pub kind: Kind,
    /// `false` for a check that is recorded and never repaired or failed on.
    pub required: bool,
}

impl Resolved {
    /// The label as it appears in a step id: ids never contain `.`, which
    /// separates the segments of a path that reads a step's result.
    fn slug(&self) -> String {
        let mut slug = String::new();
        for c in self.label.chars() {
            let c = if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
                c
            } else {
                '_'
            };
            // A run of separators is one: `__` separates the parts of an id.
            if c != '_' || !slug.ends_with('_') {
                slug.push(c);
            }
        }
        slug
    }
}

/// A repair policy, ready to lower.
pub(super) struct RepairPlan<'a> {
    pub attempts: u32,
    /// The number input that says how many of the attempts a run uses.
    pub limit_input: Option<String>,
    pub prompt: String,
    pub tools: Option<Vec<String>>,
    pub fact: String,
    pub inputs: Option<&'a BTreeSet<String>>,
    pub tasks: &'a [String],
    /// Whether the repair runs inside a `for_each` / an `until` task.
    pub item: bool,
    pub iteration: bool,
    /// Decoding settings of the repair turn.
    pub decoding: StepDecoding,
    /// For a `model_only` task, the repair is a model turn without tools,
    /// answering the task's schema when it has one.
    pub model_only: Option<Option<String>>,
    /// The model role that answers the repair turn.
    pub role: Option<String>,
    /// The fact the repair turn's stop status is recorded in.
    pub stop: Option<String>,
    /// The fact holding the calls the task may still make.
    pub pool: Option<String>,
    /// Ends a repair turn that gets the same answers this many times.
    pub max_repeated_results: Option<u32>,
    /// The conversation the repair turn is in: the task's own.
    pub conversation: Option<String>,
    /// The system message of the repair turn's conversation (already moved
    /// into run namespaces).
    pub system: Option<String>,
    /// Tasks whose output is structured (for `failure` prompts that read it).
    pub structured: &'a [String],
    /// Command gates whose results the prompt can read.
    pub gates: &'a [String],
    /// What the repair turn may do with the task's variables.
    pub vars: std::collections::BTreeMap<String, crate::vars::VarAccess>,
    /// The declared variables, which the prompt can read.
    pub var_names: &'a [String],
}

/// The phase the checks belong to: a task, or the success condition.
pub(super) struct Phase<'a> {
    /// Step id prefix: the task name, or `success`.
    pub prefix: String,
    /// The task, if any, for the source map.
    pub task: Option<String>,
    /// The failure code of a check that cannot be repaired.
    pub fail_code: &'static str,
    /// How a failure message names the phase (may hold holes that resolve
    /// when the failure happens, such as the item's id).
    pub what: String,
    /// How an authoring error names the phase.
    pub place: String,
    pub repair: Option<RepairPlan<'a>>,
    /// Where an unsatisfied check leads once the repairs (if any) are spent,
    /// instead of the run's failure: back to the loop that asked.
    pub loop_back: Option<String>,
    /// Inside a loop with `on_item_failure`: the step that fails the item
    /// (and rolls it back) instead of the run, which a failure records its
    /// reason in [`ITEM_FAILED_FACT`] and continues at.
    pub item_failure: Option<String>,
}

use crate::spec::ITEM_FAILED_FACT;

fn origin(phase: &Phase<'_>, role: &str, check: &str, attempt: u32) -> SourceRef {
    SourceRef {
        task: phase.task.clone(),
        role: role.to_string(),
        check: Some(check.to_string()),
        attempt: Some(attempt),
    }
}

fn base_id(phase: &Phase<'_>, index: usize, check: &Resolved, attempt: u32) -> String {
    format!("{}__v{index}_{}_a{attempt}", phase.prefix, check.slug())
}

fn entry_id(phase: &Phase<'_>, checks: &[Resolved], index: usize, attempt: u32) -> String {
    let base = base_id(phase, index, &checks[index], attempt);
    match &checks[index].kind {
        Kind::Metric { run: Some(_), .. } => format!("{base}_run"),
        _ => base,
    }
}

pub(super) fn gate_step(run: &CommandRun, then: &str, on_fail: &str) -> StepSpec {
    StepSpec::Gate(Box::new(GateStepSpec {
        steps: run.steps.clone(),
        env: run.env.clone(),
        timeout: run.timeout,
        cwd: run.cwd.clone(),
        cwd_input: run.cwd_input.clone(),
        output_bytes: run.output_bytes,
        stdin: run.stdin.clone(),
        store_as: Some(run.store_as.clone()),
        capture: run.capture.clone(),
        then: then.to_string(),
        on_fail: on_fail.to_string(),
    }))
}

/// A threshold as a JSON number. An integral value is written without a
/// fraction (Rust prints `90.0_f64` as `90`), so that an equality against an
/// integer in the file compares equal.
fn number(value: f64) -> Value {
    serde_json::from_str::<serde_json::Number>(&format!("{value}"))
        .map_or(Value::Null, Value::Number)
}

/// Appends the verify step and the branch on its verdict.
fn verify_and_branch(
    b: &mut Builder,
    phase: &Phase<'_>,
    base: &str,
    spec: Value,
    check: (&str, u32),
    (then, on_fail): (&str, &str),
) {
    let ok = format!("{base}_ok");
    let source = |role| origin(phase, role, check.0, check.1);
    b.add(
        base,
        StepSpec::Verify(Box::new(VerifyStepSpec {
            spec,
            store_as: None,
            then: ok.clone(),
        })),
        source("check"),
    );
    b.add(
        &ok,
        StepSpec::Branch(Box::new(BranchStepSpec {
            on: json!({ "eq": format!("steps.{base}.value"), "value": "passed" }),
            when_true: then.to_string(),
            when_false: on_fail.to_string(),
        })),
        source("check"),
    );
}

fn file_spec(path: &str, min_bytes: Option<u64>, sha256: Option<&str>) -> Value {
    let mut exists = json!({ "kind": "file_exists", "path": path });
    if let Some(min) = min_bytes {
        exists["min_bytes"] = json!(min);
    }
    match sha256 {
        Some(hash) => json!({
            "kind": "all",
            "specs": [exists, { "kind": "file_hash", "path": path, "sha256": hash }],
        }),
        None => exists,
    }
}

/// Which failure a repair turn answers; decides what `failure.*` reads.
#[derive(Clone, PartialEq, Eq)]
enum Failed {
    /// A gate step's typed result, at this step.
    Command(String),
    /// A verifier's verdict, at this step.
    Verdict(String),
    /// A condition on a fact, at this step, described by this text.
    Fact(String, String),
}

/// Lowers `checks` and their repair attempts and returns the id of the
/// first step to run, or `next` when there is nothing to check.
///
/// # Errors
///
/// A repair prompt that reads something it may not.
pub(super) fn lower_checks(
    b: &mut Builder,
    phase: &Phase<'_>,
    checks: &[Resolved],
    next: &str,
) -> Result<String, HarnessError> {
    if checks.is_empty() {
        return Ok(next.to_string());
    }
    let repairs = phase.repair.as_ref().map_or(0, |r| r.attempts);
    for attempt in 0..=repairs {
        for (i, check) in checks.iter().enumerate() {
            let base = base_id(phase, i, check, attempt);
            let after = if i + 1 < checks.len() {
                entry_id(phase, checks, i + 1, attempt)
            } else {
                next.to_string()
            };
            let fail = |b: &mut Builder, failed: Failed| {
                on_failure(b, phase, (&check.label, attempt, repairs), failed, checks)
            };
            let at = origin(phase, "check", &check.label, attempt);
            match &check.kind {
                Kind::Command(run) => {
                    let to = if check.required {
                        fail(b, Failed::Command(base.clone()))?
                    } else {
                        after.clone()
                    };
                    b.add(&base, gate_step(run, &after, &to), at);
                }
                Kind::File {
                    path,
                    min_bytes,
                    sha256,
                } => {
                    let to = fail(b, Failed::Verdict(base.clone()))?;
                    let spec = file_spec(path, *min_bytes, sha256.as_deref());
                    verify_and_branch(
                        b,
                        phase,
                        &base,
                        spec,
                        (&check.label, attempt),
                        (&after, &to),
                    );
                }
                Kind::Fact {
                    path,
                    equals,
                    failure,
                } => {
                    let to = fail(b, Failed::Fact(base.clone(), failure.clone()))?;
                    b.add(
                        &base,
                        StepSpec::Branch(Box::new(BranchStepSpec {
                            on: json!({ "eq": path, "value": equals }),
                            when_true: after.clone(),
                            when_false: to,
                        })),
                        at,
                    );
                }
                Kind::Metric {
                    run,
                    file,
                    pointer,
                    threshold,
                } => {
                    let to = fail(b, Failed::Verdict(base.clone()))?;
                    if let Some(run) = run {
                        let run_id = format!("{base}_run");
                        let run_to = fail(b, Failed::Command(run_id.clone()))?;
                        b.add(&run_id, gate_step(run, &base, &run_to), at);
                    }
                    let spec = json!({
                        "kind": "json_predicate",
                        "path": file,
                        "pointer": pointer,
                        "op": threshold.cmp.as_str(),
                        "value": number(threshold.value),
                    });
                    verify_and_branch(
                        b,
                        phase,
                        &base,
                        spec,
                        (&check.label, attempt),
                        (&after, &to),
                    );
                }
            }
        }
    }
    Ok(entry_id(phase, checks, 0, 0))
}

/// The step a failed check at `attempt` leads to: a repair turn while some
/// remain (created on first use), the run's failure otherwise.
fn on_failure(
    b: &mut Builder,
    phase: &Phase<'_>,
    (label, attempt, repairs): (&str, u32, u32),
    failed: Failed,
    checks: &[Resolved],
) -> Result<String, HarnessError> {
    let (gate_id, gate_step, verdict_step, detail) = match failed {
        Failed::Command(id) => {
            let detail = format!("{{{{ steps.{id}.detail }}}}");
            (id.clone(), Some(id), None, detail)
        }
        Failed::Verdict(id) => {
            let detail = format!("{{{{ steps.{id}.value }}}}");
            (id.clone(), None, Some(id), detail)
        }
        Failed::Fact(id, text) => (id, None, None, text),
    };
    if attempt < repairs {
        let id = format!("{gate_id}_repair");
        // A repair count read from an input: the attempt happens only when the
        // input allows this many; otherwise the check's failure stands.
        let limit = phase.repair.as_ref().and_then(|r| r.limit_input.clone());
        let entry = match &limit {
            Some(_) => format!("{id}_if"),
            None => id.clone(),
        };
        if b.steps.contains_key(&entry) {
            return Ok(entry);
        }
        let plan = phase.repair.as_ref().ok_or_else(|| {
            HarnessError::Invalid("internal: repair attempts without a repair plan".into())
        })?;
        let view = FailureView {
            check: label.to_string(),
            attempt: attempt + 1,
            gate_step,
            verdict_step,
        };
        let mut scope = Scope::new(
            format!("{}: repair.prompt", phase.place),
            plan.inputs,
            plan.tasks,
        )
        .in_loop(plan.item, plan.iteration)
        .with_structured(plan.structured)
        .with_gates(plan.gates)
        .with_vars(plan.var_names);
        scope.failure = Some(&view);
        let prompt = rewrite_text(&plan.prompt, &scope)?;
        let again = entry_id(phase, checks, 0, attempt + 1);
        let step = match &plan.model_only {
            Some(schema) => StepSpec::Predict(Box::new(PredictStepSpec {
                prompt,
                schema: schema.clone(),
                store_as: Some(plan.fact.clone()),
                conversation: plan.conversation.clone(),
                system: plan.system.clone(),
                origin: Some("repair".to_string()),
                role: plan.role.clone(),
                decoding: plan.decoding.clone(),
                then: again,
            })),
            None => StepSpec::Agent(Box::new(AgentStepSpec {
                prompt,
                tools: plan.tools.clone(),
                store_as: Some(plan.fact.clone()),
                conversation: plan.conversation.clone(),
                vars: plan.vars.clone(),
                system: plan.system.clone(),
                origin: Some("repair".to_string()),
                role: plan.role.clone(),
                stop_as: plan.stop.clone(),
                pool: plan.pool.clone(),
                allowance: None,
                max_repeated_results: plan.max_repeated_results,
                decoding: plan.decoding.clone(),
                then: again,
            })),
        };
        b.add(&id, step, origin(phase, "repair", label, attempt + 1));
        if let Some(name) = limit {
            let stand = failure_step(
                b,
                phase,
                (&gate_id, label, attempt),
                " after the repair attempts it allowed",
                &detail,
            );
            b.add(
                &entry,
                StepSpec::Branch(Box::new(BranchStepSpec {
                    on: super::counts::at_least(&name, u64::from(attempt) + 1),
                    when_true: id,
                    when_false: stand,
                })),
                origin(phase, "repair", label, attempt + 1),
            );
        }
        return Ok(entry);
    }
    let spent = if repairs == 0 {
        String::new()
    } else {
        format!(" after {repairs} repair attempt(s)")
    };
    Ok(failure_step(
        b,
        phase,
        (&gate_id, label, attempt),
        &spent,
        &detail,
    ))
}

/// The step that ends the run (or the loop) because check `label` failed.
fn failure_step(
    b: &mut Builder,
    phase: &Phase<'_>,
    (gate_id, label, attempt): (&str, &str, u32),
    spent: &str,
    detail: &str,
) -> String {
    if let Some(back) = &phase.loop_back {
        return back.clone();
    }
    let id = format!("{gate_id}_fail");
    if b.steps.contains_key(&id) {
        return id;
    }
    let message = format!("{}: check `{label}` failed{spent}: {detail}", phase.what);
    let step = match &phase.item_failure {
        Some(target) => fail_item(&message, target),
        None => StepSpec::Fail(Box::new(FailStepSpec {
            code: Some(phase.fail_code.to_string()),
            message: Some(message),
        })),
    };
    b.add(&id, step, origin(phase, "failure", label, attempt));
    id
}

/// The step that records why an item failed and continues at `target`, the
/// loop's item-failure path.
pub(super) fn fail_item(message: &str, target: &str) -> StepSpec {
    StepSpec::Assign(Box::new(crate::spec::AssignStepSpec {
        set: [(
            ITEM_FAILED_FACT.to_string(),
            Value::String(message.to_string()),
        )]
        .into_iter()
        .collect(),
        then: target.to_string(),
    }))
}
