// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven harness compile | explain | status` and `inspect --graph`: reading a
//! harness and a suspended run without running anything.
//!
//! Swedish Embedded AB implements reviewable, declarative agent workflows for
//! its clients. If your team needs expertise in explaining what a long-running
//! agent job does before it runs then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::path::Path;

use sven_harness_testkit::{
    compile as compile_harness, explain_plan, explain_step, graph as draw, render_prompts,
    render_step, status as read_status, CompiledHarness, GraphFormat, StatusReport, TestkitError,
};

use super::{kit, print_json, request, usage, Failure, Outcome};
use crate::cli::{GraphArg, PlanFormatArg};

/// Compiles the harness at `path`. A document that does not compile is exit
/// 1, as for `validate` and `inspect`; a path that cannot be read is 2.
fn compiled(path: &Path) -> Result<CompiledHarness, Failure> {
    compile_harness(path).map_err(|error| match error {
        TestkitError::Harness(_) | TestkitError::Manifest(_) => Failure {
            code: 1,
            message: error.to_string(),
        },
        other => kit(other),
    })
}

/// Prints the plan of the harness at `path` as a graph.
pub(super) fn graph(path: &Path, format: GraphArg) -> Outcome {
    let format = match format {
        GraphArg::Dot => GraphFormat::Dot,
        GraphArg::Mermaid => GraphFormat::Mermaid,
    };
    print!("{}", draw(&compiled(path)?.plan, format));
    Ok(0)
}

/// Prints the canonical compiled plan, to stdout or to `output`.
pub(super) fn compile(path: &Path, format: PlanFormatArg, output: Option<&Path>) -> Outcome {
    let plan = &compiled(path)?.plan;
    let mut text = match format {
        PlanFormatArg::Json => {
            serde_json::to_string_pretty(plan).map_err(|e| usage(e.to_string()))?
        }
        PlanFormatArg::Yaml => serde_yaml::to_string(plan).map_err(|e| usage(e.to_string()))?,
    };
    if !text.ends_with('\n') {
        text.push('\n');
    }
    match output {
        Some(file) => std::fs::write(file, text)
            .map_err(|e| usage(format!("cannot write {}: {e}", file.display())))?,
        None => print!("{text}"),
    }
    Ok(0)
}

/// Where `explain` takes inputs from, to show prompts as a run would send
/// them.
pub(super) struct ExplainInputs<'a> {
    pub json: Option<&'a str>,
    pub file: Option<&'a Path>,
    pub elide: usize,
}

impl ExplainInputs<'_> {
    /// The inputs the flags give, if any: a JSON object.
    fn read(&self) -> Result<Option<serde_json::Value>, Failure> {
        let value = match (self.json, self.file) {
            (Some(text), _) => request::parse_json(text, "--input")?,
            (None, Some(file)) => {
                request::parse_json(&request::read(file)?, &file.display().to_string())?
            }
            (None, None) => return Ok(None),
        };
        if value.is_object() {
            Ok(Some(value))
        } else {
            Err(usage("the inputs must be a JSON object"))
        }
    }
}

/// Explains the whole plan, or one step of it; with inputs, also what the
/// steps send.
pub(super) fn explain(path: &Path, step: Option<&str>, inputs: ExplainInputs<'_>) -> Outcome {
    let plan = &compiled(path)?.plan;
    let inputs_given = inputs.read()?;
    let mut text = match step {
        Some(id) => explain_step(plan, id).map_err(|e| usage(e.to_string()))?,
        None => explain_plan(plan),
    };
    if let Some(given) = inputs_given {
        text.push_str("\nas a run with these inputs would send it\n\n");
        match step {
            Some(id) => text.push_str(
                &render_step(plan, id, &given, inputs.elide).unwrap_or_else(|| {
                    format!("{id} sends no prompt, system message or command\n")
                }),
            ),
            None => text.push_str(&render_prompts(plan, &given, inputs.elide)),
        }
    }
    print!("{text}");
    Ok(0)
}

/// Reports where the run in `checkpoint` rests under the harness at `path`.
pub(super) fn status(path: &Path, checkpoint: &Path, json: bool) -> Outcome {
    let harness = compiled(path)?;
    let text = std::fs::read_to_string(checkpoint)
        .map_err(|e| usage(format!("cannot read {}: {e}", checkpoint.display())))?;
    let value = serde_json::from_str(&text).map_err(|e| {
        usage(format!(
            "{} is not a harness checkpoint: {e}",
            checkpoint.display()
        ))
    })?;
    let report = read_status(&harness, &value).map_err(|e| usage(e.to_string()))?;
    if json {
        print_json(&report)?;
    } else {
        print_status(&report);
    }
    Ok(i32::from(!report.resumable))
}

fn print_status(report: &StatusReport) {
    let current = &report.current;
    println!(
        "{} run {}: suspended at {} ({})",
        report.harness, report.run_id, current.step, current.primitive
    );
    if let Some(task) = &current.task {
        let mut place = format!("task {task}");
        if let Some(role) = &current.role {
            place.push_str(&format!(", {}", role.replace('_', " ")));
        }
        if let Some(check) = &current.check {
            place.push_str(&format!(" {check}"));
        }
        if let Some(attempt) = current.attempt {
            place.push_str(&format!(", attempt {attempt}"));
        }
        println!("  {place}");
    }
    if let Some(waiting) = &report.awaiting {
        println!("  waiting on: {waiting}");
    }
    let counts = &report.counts;
    println!(
        "  steps {}, model turns {}, tool calls {}, children {}",
        counts.steps, counts.model_turns, counts.tool_calls, counts.children
    );
    println!("  completed: {}", report.steps_completed.join(", "));
    for progress in &report.loops {
        let active = progress
            .active
            .as_deref()
            .map_or_else(String::new, |item| format!(", on item {item}"));
        println!(
            "  loop {}: {} of {} items completed{active}",
            progress.step, progress.completed, progress.total
        );
    }
    match &report.reason {
        None => println!("  resumable: yes"),
        Some(reason) => println!("  resumable: no - {reason}"),
    }
}
