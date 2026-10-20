// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Moving authored templates into the run's own namespaces.
//!
//! A workflow author writes `{{ inputs.repo }}` and `{{ tasks.plan.output }}`;
//! the machine resolves `inputs`, `facts` and `steps`. Lowering rewrites each
//! hole, rejecting any that names something the author cannot reach:
//! the internal `facts` and `steps` namespaces, a task that has not run yet,
//! an undeclared input, or `failure.*` outside a repair prompt.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::error::HarnessError;
use crate::expr::{Expr, Path, PathRewrite, Template};

/// The fact a task's output is stored under: the text of its last agent turn.
pub(super) fn output_fact(task: &str) -> String {
    format!("task_{task}")
}

/// The fact a task's agent turns record how they ended under.
pub(super) fn stop_fact(task: &str) -> String {
    format!("stop_{task}")
}

/// The fact holding the tool calls a task may still make.
pub(super) fn pool_fact(task: &str) -> String {
    format!("calls_left_{task}")
}

/// The fields of `failure.*` a repair prompt may read.
pub(super) const FAILURE_FIELDS: [&str; 7] = [
    "gate",
    "attempt",
    "kind",
    "detail",
    "exit_code",
    "stdout",
    "stderr",
];

/// What a repair prompt's `failure.*` refers to.
pub(super) struct FailureView {
    /// The check that failed.
    pub check: String,
    /// The repair attempt about to run, from 1.
    pub attempt: u32,
    /// The step holding a command gate's typed result, when it was one.
    pub gate_step: Option<String>,
    /// The step holding a verifier's verdict text, when it was one.
    pub verdict_step: Option<String>,
}

/// What a template at one place in the document may read.
pub(super) struct Scope<'a> {
    /// Where in the document the template is, for error messages.
    pub place: String,
    /// The declared inputs, if any are declared.
    pub inputs: Option<&'a BTreeSet<String>>,
    /// The tasks whose output exists by now.
    pub tasks: &'a [String],
    /// The failure a repair prompt describes.
    pub failure: Option<&'a FailureView>,
    /// Whether `item`, `item_id` and `item_index` exist here (inside a
    /// `for_each` task).
    pub item: bool,
    /// Whether `iteration` exists here (inside an `until` task).
    pub iteration: bool,
    /// The tasks whose output is structured: their fields can be read.
    pub structured: &'a [String],
    /// The command gates whose latest result can be read.
    pub gates: &'a [String],
}

impl<'a> Scope<'a> {
    /// A scope over `inputs` and the outputs of `tasks`, outside any loop and
    /// any repair prompt.
    pub fn new(place: String, inputs: Option<&'a BTreeSet<String>>, tasks: &'a [String]) -> Self {
        Self {
            place,
            inputs,
            tasks,
            failure: None,
            item: false,
            iteration: false,
            structured: &[],
            gates: &[],
        }
    }

    /// The same scope, able to read the results of these command gates.
    #[must_use]
    pub fn with_gates(mut self, gates: &'a [String]) -> Self {
        self.gates = gates;
        self
    }

    /// The same scope, able to read the fields of these tasks' outputs.
    #[must_use]
    pub fn with_structured(mut self, structured: &'a [String]) -> Self {
        self.structured = structured;
        self
    }

    /// The same scope inside a loop of the given kind.
    #[must_use]
    pub fn in_loop(mut self, item: bool, iteration: bool) -> Self {
        self.item = item;
        self.iteration = iteration;
        self
    }
}

impl Scope<'_> {
    fn invalid(&self, path: &Path, why: &str) -> HarnessError {
        HarnessError::Invalid(format!("{}: `{path}` {why}", self.place))
    }

    fn map(&self, path: &Path) -> Result<PathRewrite, HarnessError> {
        let segments = path.segments();
        match path.root() {
            "inputs" => {
                let declared = segments
                    .get(1)
                    .ok_or_else(|| self.invalid(path, "needs an input name: inputs.<name>"))?;
                match self.inputs {
                    Some(names) if names.contains(declared) => Ok(PathRewrite::Path(path.clone())),
                    Some(names) => Err(self.invalid(
                        path,
                        &format!(
                            "reads input `{declared}`, which is not declared (declared: {})",
                            names.iter().cloned().collect::<Vec<_>>().join(", ")
                        ),
                    )),
                    None => {
                        Err(self
                            .invalid(path, "reads an input but the workflow declares no `inputs`"))
                    }
                }
            }
            "tasks" => {
                let [_, task, field, fields @ ..] = segments else {
                    return Err(self.invalid(path, "is not of the form tasks.<name>.output"));
                };
                if !["output", "stop", "calls_left"].contains(&field.as_str()) {
                    return Err(self.invalid(
                        path,
                        "can only read a task's `output`, `stop` or `calls_left`",
                    ));
                }
                if field != "output" && !fields.is_empty() {
                    return Err(self.invalid(path, &format!("`{field}` has no fields")));
                }
                if !fields.is_empty() && !self.structured.contains(task) {
                    return Err(self.invalid(
                        path,
                        &format!("reads a field of the output of `{task}`, which has no `schema`"),
                    ));
                }
                if !self.tasks.contains(task) {
                    return Err(self.invalid(
                        path,
                        &format!(
                            "reads the output of task `{task}`, which has not run by then \
                             (earlier tasks: {})",
                            if self.tasks.is_empty() {
                                "none".to_string()
                            } else {
                                self.tasks.join(", ")
                            }
                        ),
                    ));
                }
                let rest: String = fields.iter().map(|f| format!(".{f}")).collect();
                let fact = match field.as_str() {
                    "stop" => stop_fact(task),
                    "calls_left" => pool_fact(task),
                    _ => output_fact(task),
                };
                Path::parse(&format!("facts.{fact}{rest}")).map(PathRewrite::Path)
            }
            "failure" => self.map_failure(path),
            "gates" => {
                let [_, gate, rest @ ..] = segments else {
                    return Err(self.invalid(path, "is not of the form gates.<name>.<field>"));
                };
                if !self.gates.contains(gate) {
                    return Err(self.invalid(
                        path,
                        &format!(
                            "reads the result of gate `{gate}`, which is not a declared command \
                             gate (declared: {})",
                            self.gates.join(", ")
                        ),
                    ));
                }
                let rest: String = rest.iter().map(|f| format!(".{f}")).collect();
                Path::parse(&format!("facts.gate_{gate}{rest}")).map(PathRewrite::Path)
            }
            root @ ("item" | "item_id" | "item_index") => {
                if !self.item {
                    return Err(self.invalid(path, "is only available inside a `for_each` task"));
                }
                if root != "item" && segments.len() > 1 {
                    return Err(self.invalid(path, &format!("`{root}` has no fields")));
                }
                Path::parse(&format!("facts.{}", segments.join("."))).map(PathRewrite::Path)
            }
            "iteration" => {
                if !self.iteration {
                    return Err(self.invalid(path, "is only available inside an `until` task"));
                }
                if segments.len() > 1 {
                    return Err(self.invalid(path, "`iteration` is the number of the pass, from 1"));
                }
                Path::parse("facts.iteration").map(PathRewrite::Path)
            }
            other => Err(self.invalid(
                path,
                &format!(
                    "starts with `{other}`; a workflow reads `inputs.<name>`, \
                     `tasks.<name>.output` and, inside loops, `item`, `item_id`, \
                     `item_index` and `iteration` (`facts` and `steps` are internal)"
                ),
            )),
        }
    }

    fn map_failure(&self, path: &Path) -> Result<PathRewrite, HarnessError> {
        let Some(view) = self.failure else {
            return Err(self.invalid(path, "is only available in a `repair.prompt`"));
        };
        let field = match path.segments() {
            [_, field] if FAILURE_FIELDS.contains(&field.as_str()) => field.as_str(),
            _ => {
                return Err(self.invalid(
                    path,
                    &format!("is not one of failure.{{{}}}", FAILURE_FIELDS.join(", ")),
                ))
            }
        };
        let result = |step: &str, name: &str| {
            Path::parse(&format!("steps.{step}.{name}")).map(PathRewrite::Path)
        };
        match (field, &view.gate_step, &view.verdict_step) {
            ("gate", _, _) => Ok(PathRewrite::Text(view.check.clone())),
            ("attempt", _, _) => Ok(PathRewrite::Text(view.attempt.to_string())),
            ("kind", Some(step), _) => result(step, "kind"),
            ("detail", Some(step), _) => result(step, "detail"),
            ("exit_code", Some(step), _) => result(step, "exit_code"),
            ("stdout", Some(step), _) => result(step, "stdout"),
            ("stderr", Some(step), _) => result(step, "stderr"),
            ("detail", None, Some(step)) => result(step, "value"),
            ("kind", None, _) => Ok(PathRewrite::Text("check".to_string())),
            // A file or metric check has no exit code or output.
            _ => Ok(PathRewrite::Text(String::new())),
        }
    }
}

/// Rewrites the holes of `text` into the run's namespaces.
pub(super) fn rewrite_text(text: &str, scope: &Scope<'_>) -> Result<String, HarnessError> {
    let template = Template::parse(text)
        .map_err(|e| HarnessError::Invalid(format!("{}: {e}", scope.place)))?;
    Ok(String::from(template.rewrite(&mut |path| scope.map(path))?))
}

/// Rewrites every string inside a JSON value.
pub(super) fn rewrite_value(value: &Value, scope: &Scope<'_>) -> Result<Value, HarnessError> {
    Ok(match value {
        Value::String(text) => Value::String(rewrite_text(text, scope)?),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| rewrite_value(v, scope))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| Ok((k.clone(), rewrite_value(v, scope)?)))
                .collect::<Result<_, HarnessError>>()?,
        ),
        other => other.clone(),
    })
}

/// Parses an authored condition and moves its paths into the run's
/// namespaces, returning it in the form a `branch` step takes.
pub(super) fn rewrite_condition(
    value: &Value,
    scope: &Scope<'_>,
    current_task: &str,
) -> Result<Value, HarnessError> {
    // `{last_turn: <stop>}` is `{eq: tasks.<this task>.stop, value: <stop>}`.
    let sugared;
    let value = match value {
        Value::Object(map) if map.len() == 1 && map.contains_key("last_turn") => {
            sugared = json!({
                "eq": format!("tasks.{current_task}.stop"),
                "value": map["last_turn"],
            });
            &sugared
        }
        other => other,
    };
    let expr = Expr::parse_value(value)
        .map_err(|e| HarnessError::Invalid(format!("{}: bad condition: {e}", scope.place)))?;
    let lowered = expr.rewrite_paths(&mut |path| match scope.map(path)? {
        PathRewrite::Path(path) => Ok(path),
        PathRewrite::Text(_) => Err(scope.invalid(path, "cannot be used in a condition")),
    })?;
    Ok(Value::from(lowered))
}
