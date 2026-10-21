// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Reading a compiled plan: its canonical form, its graph, and what it means
//! in the author's terms.
//!
//! A workflow lowers to many generated steps per authored task; the plan's
//! source map says where each one came from, and these views use it so a
//! person reads tasks, checks and repairs instead of step ids. A plan without
//! a source map is shown step by step.
//!
//! Swedish Embedded AB implements reviewable, declarative agent workflows for
//! its clients. If your team needs expertise in explaining what a long-running
//! agent job does before it runs then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sven_harness_spec::{HarnessPlan, PlanPrimitive, SourceRef};

use crate::dir::HarnessDir;
use crate::error::TestkitError;
use crate::source::Source;

/// A harness's main document, compiled offline.
#[derive(Debug, Clone)]
pub struct CompiledHarness {
    /// The document the plan came from.
    pub path: PathBuf,
    /// The canonical plan.
    pub plan: HarnessPlan,
    /// The bundle's content digest, when the document is a bundle.
    pub bundle_digest: Option<String>,
}

/// Compiles the main document of the harness at `path` without running it.
///
/// # Errors
///
/// [`TestkitError`] when the path cannot be read or the document does not
/// compile.
pub fn compile(path: &Path) -> Result<CompiledHarness, TestkitError> {
    let dir = HarnessDir::open(path)?;
    let source = Source::read(&dir.main)?;
    Ok(CompiledHarness {
        path: source.path,
        plan: source.plan,
        bundle_digest: source.bundle_digest,
    })
}

/// A way to draw a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphFormat {
    /// Graphviz DOT.
    Dot,
    /// A Mermaid flowchart.
    Mermaid,
}

/// A step's way out: the step it leads to and which outcome takes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    /// The successor step.
    pub to: String,
    /// The outcome that takes this edge; empty for an unconditional one.
    pub label: &'static str,
}

fn edge(to: &str, label: &'static str) -> Edge {
    Edge {
        to: to.to_string(),
        label,
    }
}

/// Every edge out of `primitive`, in the order the runtime prefers them.
pub(crate) fn edges(primitive: &PlanPrimitive) -> Vec<Edge> {
    use PlanPrimitive::{
        Agent, Assign, Branch, Delegate, Fail, Gate, Iterate, Predict, Return, Verify,
    };
    match primitive {
        Predict { then, .. }
        | Agent { then, .. }
        | Verify { then, .. }
        | Assign { then, .. }
        | Delegate { then, .. } => vec![edge(then, "")],
        Branch {
            when_true,
            when_false,
            ..
        } => vec![edge(when_true, "true"), edge(when_false, "false")],
        Gate { then, on_fail, .. } => vec![edge(then, "passes"), edge(on_fail, "fails")],
        Iterate {
            body,
            then,
            on_exhausted,
            ..
        } => [(body, "body"), (then, "done"), (on_exhausted, "exhausted")]
            .into_iter()
            .filter(|(id, _)| !id.is_empty())
            .map(|(id, label)| edge(id, label))
            .collect(),
        Return { .. } | Fail { .. } => Vec::new(),
    }
}

/// The primitive's name in the plan language (`agent`, `gate`, ...).
pub(crate) fn primitive_word(primitive: &PlanPrimitive) -> String {
    serde_json::to_value(primitive)
        .ok()
        .and_then(|v| v.get("kind").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default()
}

/// The steps reachable from the entry in breadth-first order, then any
/// unreachable ones (a plan may keep them) in id order.
pub(crate) fn walk_order(plan: &HarnessPlan) -> Vec<&str> {
    let mut seen = BTreeSet::new();
    let mut order = Vec::new();
    let mut queue = VecDeque::from([plan.entry.as_str()]);
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id) {
            continue;
        }
        let Some(step) = plan.step(id) else { continue };
        order.push(id);
        queue.extend(
            edges(&step.primitive)
                .into_iter()
                .filter_map(|e| plan.steps.get_key_value(&e.to).map(|(key, _)| key.as_str())),
        );
    }
    order.extend(
        plan.steps
            .keys()
            .map(String::as_str)
            .filter(|id| !seen.contains(id)),
    );
    order
}

/// A plan as a graph in `format`. A version 2 plan's steps are grouped by
/// authored task and labelled by role, check and attempt.
#[must_use]
pub fn graph(plan: &HarnessPlan, format: GraphFormat) -> String {
    let order = walk_order(plan);
    let mut tasks: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut loose = Vec::new();
    for id in &order {
        match plan.source_map.get(*id).and_then(|s| s.task.as_deref()) {
            Some(task) => tasks.entry(task).or_default().push(id),
            None => loose.push(*id),
        }
    }
    match format {
        GraphFormat::Dot => dot(plan, &tasks, &loose),
        GraphFormat::Mermaid => mermaid(plan, &tasks, &loose),
    }
}

/// The node's caption: its role in the workflow, or its id and primitive.
fn caption(plan: &HarnessPlan, id: &str) -> String {
    let primitive = plan
        .step(id)
        .map(|s| primitive_word(&s.primitive))
        .unwrap_or_default();
    match plan.source_map.get(id) {
        Some(origin) => format!("{} ({primitive})\n{id}", origin_words(origin)),
        None => format!("{id} ({primitive})"),
    }
}

fn dot(plan: &HarnessPlan, tasks: &BTreeMap<&str, Vec<&str>>, loose: &[&str]) -> String {
    let quote = |text: &str| {
        format!(
            "\"{}\"",
            text.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
        )
    };
    let node = |id: &str| {
        let primitive = plan
            .step(id)
            .map(|s| primitive_word(&s.primitive))
            .unwrap_or_default();
        let shape = match primitive.as_str() {
            "branch" => "diamond",
            "gate" | "verify" => "hexagon",
            "return" => "doublecircle",
            "fail" => "octagon",
            _ => "box",
        };
        format!(
            "{} [shape={shape}, label={}];",
            quote(id),
            quote(&caption(plan, id))
        )
    };
    let mut out = format!("digraph {} {{\n  rankdir=TB;\n", quote(&plan.name));
    out.push_str("  node [fontname=\"Helvetica\"];\n");
    out.push_str("  \"__start\" [shape=point];\n");
    let _ = writeln!(out, "  \"__start\" -> {};", quote(&plan.entry));
    for (task, ids) in tasks {
        let _ = writeln!(out, "  subgraph {} {{", quote(&format!("cluster_{task}")));
        let _ = writeln!(out, "    label={};", quote(&format!("task {task}")));
        for id in ids {
            let _ = writeln!(out, "    {}", node(id));
        }
        out.push_str("  }\n");
    }
    for id in loose {
        let _ = writeln!(out, "  {}", node(id));
    }
    for (id, step) in &plan.steps {
        for e in edges(&step.primitive) {
            if e.label.is_empty() {
                let _ = writeln!(out, "  {} -> {};", quote(id), quote(&e.to));
            } else {
                let _ = writeln!(
                    out,
                    "  {} -> {} [label={}];",
                    quote(id),
                    quote(&e.to),
                    quote(e.label)
                );
            }
        }
    }
    out.push_str("}\n");
    out
}

fn mermaid(plan: &HarnessPlan, tasks: &BTreeMap<&str, Vec<&str>>, loose: &[&str]) -> String {
    // Mermaid ids are plain words; the prefix keeps a step called `end` from
    // being read as the keyword.
    let ident = |id: &str| {
        let word: String = id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        format!("n_{word}")
    };
    let node = |id: &str| {
        let primitive = plan
            .step(id)
            .map(|s| primitive_word(&s.primitive))
            .unwrap_or_default();
        let label = caption(plan, id)
            .replace('"', "#quot;")
            .replace('\n', "<br/>");
        let (open, close) = match primitive.as_str() {
            "branch" => ("{", "}"),
            "gate" | "verify" => ("{{", "}}"),
            "return" => ("((", "))"),
            "fail" => (">", "]"),
            _ => ("[", "]"),
        };
        format!("{}{open}\"{label}\"{close}", ident(id))
    };
    let mut out = String::from("flowchart TD\n");
    let _ = writeln!(out, "  start_(( )) --> {}", ident(&plan.entry));
    for (task, ids) in tasks {
        let word: String = task
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let _ = writeln!(out, "  subgraph task_{word}[\"task {task}\"]");
        for id in ids {
            let _ = writeln!(out, "    {}", node(id));
        }
        out.push_str("  end\n");
    }
    for id in loose {
        let _ = writeln!(out, "  {}", node(id));
    }
    for (id, step) in &plan.steps {
        for e in edges(&step.primitive) {
            if e.label.is_empty() {
                let _ = writeln!(out, "  {} --> {}", ident(id), ident(&e.to));
            } else {
                let _ = writeln!(out, "  {} -->|{}| {}", ident(id), e.label, ident(&e.to));
            }
        }
    }
    out
}

/// What a step does for its task, in words.
fn role_words(role: &str) -> &'static str {
    match role {
        "when" => "decides whether the task runs",
        "work" => "the task's agent turn",
        "follow_up" => "a follow-up user turn",
        "check" => "a check",
        "repair" => "a repair turn",
        "failure" => "ends the run as failed",
        "success" => "the workflow's success condition",
        "complete" => "completes the run",
        _ => "a generated step",
    }
}

/// A step's origin as one phrase: `check test, attempt 1`.
pub(crate) fn origin_words(origin: &SourceRef) -> String {
    let mut words = origin.role.replace('_', " ");
    if let Some(check) = &origin.check {
        let _ = write!(words, " {check}");
    }
    words
}

fn attempt_words(attempt: u32) -> String {
    if attempt == 0 {
        "after the first work turn".to_string()
    } else {
        format!("after repair {attempt}")
    }
}

/// A one-line account of what the step does, from the plan.
fn describe(plan: &HarnessPlan, id: &str) -> String {
    let Some(step) = plan.step(id) else {
        return String::new();
    };
    let Ok(value) = serde_json::to_value(&step.primitive) else {
        return String::new();
    };
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(first_line);
    match primitive_word(&step.primitive).as_str() {
        "gate" => {
            let commands: Vec<String> = value
                .get("steps")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|step| {
                    let argv: Vec<&str> = step
                        .get("argv")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect();
                    first_line(&argv.join(" "))
                })
                .collect();
            let timeout = value
                .get("timeout_secs")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            format!(
                "runs `{}` (at most {timeout} s); only exit status 0 passes",
                commands.join("`, then `")
            )
        }
        "agent" => {
            let tools = value.get("tools").and_then(Value::as_array).map(|t| {
                t.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            });
            format!(
                "an agent turn{}, {}",
                tools.map_or_else(String::new, |t| format!(" with tools [{t}]")),
                prompt_words(&value)
            )
        }
        "predict" => format!("a model turn, {}", prompt_words(&value)),
        "tool" => format!("calls the tool `{}`", text("name").unwrap_or_default()),
        "verify" => format!(
            "verifies {}",
            value.get("spec").map(compact).unwrap_or_default()
        ),
        "branch" => format!(
            "branches on {}",
            value.get("on").map(compact).unwrap_or_default()
        ),
        "fail" => format!("fails with `{}`", text("code").unwrap_or_default()),
        "return" => "returns the run's outputs".to_string(),
        other => other.to_string(),
    }
}

/// How the step's prompt template starts and how long it is.
fn prompt_words(step: &Value) -> String {
    let prompt = step.get("prompt").and_then(Value::as_str).unwrap_or("");
    format!(
        "a {}-character prompt starting \"{}\"",
        prompt.chars().count(),
        first_line(prompt)
    )
}

fn compact(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() > 100 {
        format!("{}...", text.chars().take(100).collect::<String>())
    } else {
        text
    }
}

fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.chars().count() > 80 {
        format!("{}...", line.chars().take(80).collect::<String>())
    } else {
        line.to_string()
    }
}

fn header(plan: &HarnessPlan) -> String {
    let mut out = format!(
        "{} (workflow, api_version {})\n  digest: {}\n",
        plan.name, plan.api_version, plan.digest
    );
    if let Some(profile) = &plan.profile {
        let _ = writeln!(out, "  tool profile: {profile}");
    }
    let _ = writeln!(out, "  capabilities: {}", plan.capabilities.join(", "));
    let _ = writeln!(out, "  entry: {}", plan.entry);
    out
}

/// The plan in the author's terms: a workflow task by task, a plan without a
/// source map step by step.
#[must_use]
pub fn explain_plan(plan: &HarnessPlan) -> String {
    let mut out = header(plan);
    if plan.source_map.is_empty() {
        out.push_str("  steps, in the order a run reaches them:\n");
        for id in walk_order(plan) {
            let _ = writeln!(out, "    {id}: {}", describe(plan, id));
        }
        return out;
    }
    let order = walk_order(plan);
    let mut tasks: Vec<&str> = Vec::new();
    for id in &order {
        if let Some(task) = plan.source_map.get(*id).and_then(|s| s.task.as_deref()) {
            if !tasks.contains(&task) {
                tasks.push(task);
            }
        }
    }
    for task in tasks {
        explain_task(plan, task, &order, &mut out);
    }
    explain_success(plan, &order, &mut out);
    out
}

fn steps_of<'a>(
    plan: &'a HarnessPlan,
    order: &[&'a str],
    task: Option<&str>,
    role: &str,
) -> Vec<(&'a str, &'a SourceRef)> {
    order
        .iter()
        .filter_map(|id| plan.source_map.get(*id).map(|origin| (*id, origin)))
        .filter(|(_, origin)| origin.task.as_deref() == task && origin.role == role)
        .collect()
}

fn explain_task(plan: &HarnessPlan, task: &str, order: &[&str], out: &mut String) {
    let _ = writeln!(out, "\ntask {task}");
    if !steps_of(plan, order, Some(task), "when").is_empty() {
        let _ = writeln!(out, "  runs only when its condition holds");
    }
    for (id, _) in steps_of(plan, order, Some(task), "work") {
        let _ = writeln!(out, "  work: {}", describe(plan, id));
    }
    let follow_ups = steps_of(plan, order, Some(task), "follow_up").len();
    if follow_ups > 0 {
        let _ = writeln!(out, "  follow-ups: {follow_ups}");
    }
    let mut checks: Vec<&str> = Vec::new();
    for (_, origin) in steps_of(plan, order, Some(task), "check") {
        if let Some(check) = origin.check.as_deref() {
            if !checks.contains(&check) {
                checks.push(check);
            }
        }
    }
    if !checks.is_empty() {
        let _ = writeln!(out, "  checks: {}", checks.join(", "));
    }
    let repairs: BTreeSet<u32> = steps_of(plan, order, Some(task), "repair")
        .iter()
        .filter_map(|(_, origin)| origin.attempt)
        .collect();
    if !repairs.is_empty() {
        let n = repairs.len();
        let _ = writeln!(
            out,
            "  repair: {n} attempt{}, each shown the failed check's output",
            if n == 1 { "" } else { "s" }
        );
        let _ = writeln!(
            out,
            "  when the attempts are spent the run fails with `gate_failed`"
        );
    }
}

fn explain_success(plan: &HarnessPlan, order: &[&str], out: &mut String) {
    let _ = writeln!(out, "\nsuccess");
    // The workflow's own conditions are the checks that belong to no task.
    let mut checks: Vec<&str> = Vec::new();
    for (_, origin) in steps_of(plan, order, None, "check") {
        if let Some(check) = origin.check.as_deref() {
            if !checks.contains(&check) {
                checks.push(check);
            }
        }
    }
    if checks.is_empty() {
        let _ = writeln!(out, "  no condition beyond the tasks' own checks");
    }
    for check in checks {
        // The step that runs the command is the one worth describing.
        let runs = steps_of(plan, order, None, "check")
            .into_iter()
            .filter(|(_, origin)| origin.check.as_deref() == Some(check))
            .map(|(id, _)| id)
            .find(|id| {
                plan.step(id)
                    .is_some_and(|s| primitive_word(&s.primitive) == "gate")
            });
        let _ = writeln!(
            out,
            "  {check}: {}",
            runs.map_or_else(String::new, |id| describe(plan, id))
        );
    }
}

/// One generated step: what it is, where it came from, how it is reached
/// and where it goes.
///
/// # Errors
///
/// [`TestkitError::Harness`] naming the plan's steps when `id` is not one.
pub fn explain_step(plan: &HarnessPlan, id: &str) -> Result<String, TestkitError> {
    let Some(step) = plan.step(id) else {
        let known: Vec<&str> = plan.steps.keys().map(String::as_str).collect();
        return Err(TestkitError::Harness(format!(
            "no step `{id}` in {}; its steps are: {}",
            plan.name,
            known.join(", ")
        )));
    };
    let mut out = format!("{id} ({})\n", primitive_word(&step.primitive));
    if let Some(origin) = plan.source_map.get(id) {
        let mut line = String::new();
        if let Some(task) = &origin.task {
            let _ = write!(line, "task {task}, ");
        }
        match (&origin.role[..], &origin.check) {
            ("check", Some(check)) => {
                let _ = write!(line, "check {check}");
            }
            ("repair", _) => {
                let _ = write!(line, "{}", role_words("repair"));
            }
            (role, check) => {
                let _ = write!(line, "{}", role_words(role));
                if let Some(check) = check {
                    let _ = write!(line, ", for check {check}");
                }
            }
        }
        match (origin.attempt, &origin.role[..]) {
            (Some(attempt), "repair") => {
                let _ = write!(line, " {attempt}");
            }
            (Some(attempt), _) if origin.task.is_some() => {
                let _ = write!(line, ", {}", attempt_words(attempt));
            }
            _ => {}
        }
        let _ = writeln!(out, "  {line}");
    }
    let _ = writeln!(out, "  {}", describe(plan, id));
    for e in edges(&step.primitive) {
        let label = if e.label.is_empty() { "then" } else { e.label };
        let _ = writeln!(out, "  {label} -> {}", e.to);
    }
    let entered_from: Vec<&str> = plan
        .steps
        .iter()
        .filter(|(_, s)| edges(&s.primitive).iter().any(|e| e.to == id))
        .map(|(from, _)| from.as_str())
        .collect();
    if id == plan.entry {
        let _ = writeln!(out, "  the entry step");
    }
    if !entered_from.is_empty() {
        let _ = writeln!(out, "  reached from {}", entered_from.join(", "));
    }
    Ok(out)
}
