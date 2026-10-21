// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The prompts, system messages and gate commands of a plan as a run with
//! given inputs would send them.
//!
//! A workflow is edited and debugged as text, so the question "what will the
//! model be told?" has to be answerable without a run. Inputs are
//! substituted the way the runtime substitutes them (a value is data, never
//! template syntax); everything a run only learns later (a task's output, a
//! gate's result) stays visible as the hole it is.
//!
//! Swedish Embedded AB implements reviewable, declarative agent workflows for
//! its clients. If your team needs expertise in explaining what a long-running
//! agent job does before it runs then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::fmt::Write as _;

use serde_json::Value;
use sven_harness_spec::{HarnessPlan, Path, PlanPrimitive, Template};

use super::plan_view::{origin_words, primitive_word, walk_order};

/// The value of an `inputs.` path in `inputs`, if the run would have one.
fn input_value(inputs: &Value, path: &Path) -> Option<Value> {
    let (root, rest) = path.segments().split_first()?;
    if root != "inputs" {
        return None;
    }
    rest.iter()
        .try_fold(inputs, |value, key| value.get(key.as_str()))
        .cloned()
}

/// `text`, cut to `limit` characters with a note of how many were left out.
fn elided(text: &str, limit: usize) -> String {
    let total = text.chars().count();
    if total <= limit {
        return text.to_string();
    }
    let kept: String = text.chars().take(limit).collect();
    format!("{kept} ... [{} more characters]", total - limit)
}

fn shown(template: &Template, inputs: &Value, limit: usize) -> String {
    elided(&template.render(&|path| input_value(inputs, path)), limit)
}

/// What the step with `id` sends, rendered with `inputs`: its system
/// message, prompt and gate command, each cut to `limit` characters. `None`
/// for a step that sends nothing (a branch, a return).
#[must_use]
pub(super) fn render_step(
    plan: &HarnessPlan,
    id: &str,
    inputs: &Value,
    limit: usize,
) -> Option<String> {
    let step = plan.step(id)?;
    let mut out = String::new();
    match &step.primitive {
        PlanPrimitive::Agent { prompt, system, .. }
        | PlanPrimitive::Predict { prompt, system, .. } => {
            if let Some(system) = system {
                let _ = writeln!(out, "  system: {}", shown(system, inputs, limit));
            }
            let _ = writeln!(out, "  prompt: {}", shown(prompt, inputs, limit));
        }
        PlanPrimitive::Gate {
            steps, env, stdin, ..
        } => {
            for step in steps {
                let argv: Vec<String> = step.argv.iter().map(|t| shown(t, inputs, limit)).collect();
                let label = if steps.len() == 1 {
                    "command".to_string()
                } else {
                    format!("step {}", step.name)
                };
                let _ = writeln!(out, "  {label}: {}", argv.join(" "));
            }
            for (name, value) in env {
                let _ = writeln!(out, "  env {name}: {}", shown(value, inputs, limit));
            }
            if let Some(stdin) = stdin {
                let _ = writeln!(out, "  stdin: {}", shown(stdin, inputs, limit));
            }
        }
        _ => return None,
    }
    let mut heading = format!("{id} ({})", primitive_word(&step.primitive));
    if let Some(origin) = plan.source_map.get(id) {
        if let Some(task) = &origin.task {
            let _ = write!(heading, ", task {task}");
        }
        let _ = write!(heading, ", {}", origin_words(origin));
    }
    Some(format!("{heading}\n{out}"))
}

/// Every step that sends something, in the order a run reaches them.
#[must_use]
pub(super) fn render_prompts(plan: &HarnessPlan, inputs: &Value, limit: usize) -> String {
    let mut out = String::new();
    for id in walk_order(plan) {
        if let Some(text) = render_step(plan, id, inputs, limit) {
            let _ = writeln!(out, "{text}");
        }
    }
    out
}
