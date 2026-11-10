// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for splitting software work into
// independent, verifiable pieces that agents build in parallel for its
// clients. If your team needs expertise in agent task decomposition then you
// can procure our services by sending an email to info@swedishembedded.com.
//! The task graph a model submits to `plan_tasks`, and the deterministic
//! checks that decide whether it may run.
//!
//! Validation never repairs: a graph that breaks a rule is refused with every
//! violation named, so the model can fix its plan and submit it again.

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use sven_workspace::glob_match;

/// The bounds a graph is held to and run under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Most tasks running at once.
    pub max_parallel: usize,
    /// Widest layer of tasks that could run side by side.
    pub max_fanout: usize,
    /// Most tasks in one graph.
    pub max_tasks: usize,
    /// Most graphs one session may run.
    pub max_graphs: u32,
}

/// One unit of work for one writing child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSpec {
    pub id: String,
    pub title: String,
    /// The child's whole brief: it sees nothing else of the conversation.
    pub prompt: String,
    /// Files, directories or globs this task alone writes.
    pub owns: Vec<String>,
    pub acceptance: String,
    pub depends_on: Vec<String>,
}

/// A submitted graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub tasks: Vec<TaskSpec>,
    /// Runs after the graph; its failure output goes back to the model.
    pub verify_command: Option<String>,
}

fn string_field(task: &Value, key: &str) -> String {
    task.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn string_list(task: &Value, key: &str, errors: &mut Vec<String>, label: &str) -> Vec<String> {
    match task.get(key) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item.as_str() {
                Some(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
                _ => {
                    errors.push(format!("{label}: `{key}` holds only non-empty strings"));
                    None
                }
            })
            .collect(),
        Some(_) => {
            errors.push(format!("{label}: `{key}` is a list of strings"));
            Vec::new()
        }
    }
}

impl Plan {
    /// The graph in `args`, or every way its shape is wrong.
    ///
    /// # Errors
    /// Messages for each malformed field.
    pub fn parse(args: &Value) -> Result<Self, Vec<String>> {
        let Some(tasks) = args.get("tasks").and_then(Value::as_array) else {
            return Err(vec!["`tasks` is required: a list of task objects".into()]);
        };
        let mut errors = Vec::new();
        let mut parsed = Vec::with_capacity(tasks.len());
        for (index, task) in tasks.iter().enumerate() {
            let id = string_field(task, "id");
            let label = if id.is_empty() {
                format!("tasks[{index}]")
            } else {
                format!("task `{id}`")
            };
            let title = string_field(task, "title");
            parsed.push(TaskSpec {
                title: if title.is_empty() { id.clone() } else { title },
                prompt: string_field(task, "prompt"),
                owns: string_list(task, "owns", &mut errors, &label),
                acceptance: string_field(task, "acceptance"),
                depends_on: string_list(task, "depends_on", &mut errors, &label),
                id,
            });
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        let verify_command = args
            .get("verify_command")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_string);
        Ok(Self {
            tasks: parsed,
            verify_command,
        })
    }

    /// Checks the rules a graph must meet before anything runs.
    ///
    /// # Errors
    /// One message per violation, each saying what to change.
    pub fn validate(&self, limits: &Limits) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        if self.tasks.is_empty() {
            return Err(vec!["`tasks` is empty: submit at least one task".into()]);
        }
        let too_many = (self.tasks.len() > limits.max_tasks).then(|| {
            format!(
                "{} tasks submitted; a graph holds at most {}. Merge related tasks or do the small ones yourself",
                self.tasks.len(),
                limits.max_tasks
            )
        });
        self.check_fields(&mut errors);
        let index = self.index_of(&mut errors);
        self.check_dependencies(&index, &mut errors);
        if errors.is_empty() {
            match self.layers(&index) {
                Err(cycle) => errors.push(format!(
                    "dependency cycle among: {}. Remove a depends_on so the tasks can be ordered",
                    cycle.join(", ")
                )),
                Ok(layers) => {
                    let widest = layers.iter().map(Vec::len).max().unwrap_or(0);
                    if widest > limits.max_fanout {
                        errors.push(format!(
                            "{widest} tasks could run side by side; at most {} may. Order some of them with depends_on",
                            limits.max_fanout
                        ));
                    }
                    self.check_overlaps(&index, &mut errors);
                }
            }
        }
        errors.splice(0..0, too_many);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    fn check_fields(&self, errors: &mut Vec<String>) {
        for (i, task) in self.tasks.iter().enumerate() {
            let label = if task.id.is_empty() {
                format!("tasks[{i}]")
            } else {
                format!("task `{}`", task.id)
            };
            let id_ok = !task.id.is_empty()
                && task
                    .id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
            if !id_ok {
                errors.push(format!(
                    "{label}: `id` is required and holds only letters, digits, `_`, `-` and `.`"
                ));
            }
            if task.prompt.is_empty() {
                errors.push(format!(
                    "{label}: `prompt` is empty; the child sees only its brief, so write it out in full"
                ));
            }
            if task.owns.is_empty() {
                errors.push(format!(
                    "{label}: `owns` is empty; name the files or directories only this task writes"
                ));
            }
            for path in &task.owns {
                if path.starts_with('/') || path.split('/').any(|s| s == "..") {
                    errors.push(format!(
                        "{label}: `owns` entry `{path}` must be relative and stay inside the project"
                    ));
                }
            }
        }
    }

    /// Position of each id; a repeated id is an error.
    fn index_of(&self, errors: &mut Vec<String>) -> HashMap<&str, usize> {
        let mut index = HashMap::new();
        for (i, task) in self.tasks.iter().enumerate() {
            if !task.id.is_empty() && index.insert(task.id.as_str(), i).is_some() {
                errors.push(format!("duplicate task id `{}`", task.id));
            }
        }
        index
    }

    fn check_dependencies(&self, index: &HashMap<&str, usize>, errors: &mut Vec<String>) {
        for task in &self.tasks {
            for dep in &task.depends_on {
                if dep == &task.id {
                    errors.push(format!("task `{}` depends on itself", task.id));
                } else if !index.contains_key(dep.as_str()) {
                    errors.push(format!(
                        "task `{}` depends on unknown task `{dep}`; known ids: {}",
                        task.id,
                        self.tasks
                            .iter()
                            .map(|t| t.id.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
        }
    }

    /// Tasks grouped by longest dependency path, or the tasks on a cycle.
    fn layers(&self, index: &HashMap<&str, usize>) -> Result<Vec<Vec<usize>>, Vec<String>> {
        let mut depth: Vec<Option<usize>> = vec![None; self.tasks.len()];
        let mut layers: Vec<Vec<usize>> = Vec::new();
        let mut placed = 0;
        while placed < self.tasks.len() {
            let ready: Vec<usize> = (0..self.tasks.len())
                .filter(|&i| depth[i].is_none())
                .filter(|&i| {
                    self.tasks[i]
                        .depends_on
                        .iter()
                        .all(|d| depth[index[d.as_str()]].is_some())
                })
                .collect();
            if ready.is_empty() {
                return Err((0..self.tasks.len())
                    .filter(|&i| depth[i].is_none())
                    .map(|i| self.tasks[i].id.clone())
                    .collect());
            }
            for &i in &ready {
                depth[i] = Some(layers.len());
            }
            placed += ready.len();
            layers.push(ready);
        }
        Ok(layers)
    }

    /// Whether `a` must finish before `b` starts, through any chain.
    fn precedes(&self, index: &HashMap<&str, usize>, a: usize, b: usize) -> bool {
        let mut stack = vec![b];
        let mut seen = HashSet::new();
        while let Some(at) = stack.pop() {
            for dep in &self.tasks[at].depends_on {
                let d = index[dep.as_str()];
                if d == a {
                    return true;
                }
                if seen.insert(d) {
                    stack.push(d);
                }
            }
        }
        false
    }

    fn check_overlaps(&self, index: &HashMap<&str, usize>, errors: &mut Vec<String>) {
        for a in 0..self.tasks.len() {
            for b in a + 1..self.tasks.len() {
                if self.precedes(index, a, b) || self.precedes(index, b, a) {
                    continue;
                }
                for (pa, pb) in self.tasks[a]
                    .owns
                    .iter()
                    .flat_map(|pa| self.tasks[b].owns.iter().map(move |pb| (pa, pb)))
                {
                    if owns_overlap(pa, pb) {
                        errors.push(format!(
                            "tasks `{}` and `{}` could run at the same time but both own `{pa}` / `{pb}`; \
                             make one depend on the other, or give each its own files",
                            self.tasks[a].id, self.tasks[b].id
                        ));
                    }
                }
            }
        }
    }
}

fn segments(path: &str) -> Vec<&str> {
    path.trim_start_matches("./")
        .trim_end_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect()
}

fn has_glob(path: &str) -> bool {
    path.contains(['*', '?'])
}

/// The directory segments before the first glob segment.
fn fixed_prefix(path: &str) -> Vec<&str> {
    segments(path)
        .into_iter()
        .take_while(|s| !has_glob(s))
        .collect()
}

fn is_prefix(short: &[&str], long: &[&str]) -> bool {
    long.starts_with(short)
}

/// Whether two ownership entries could name a common file. A plain path owns
/// everything beneath it. Two globs are compared by where they are rooted, so
/// the answer errs toward "overlap": a false alarm costs one `depends_on`, a
/// miss costs a merge conflict.
#[must_use]
pub fn owns_overlap(a: &str, b: &str) -> bool {
    let (pa, pb) = (fixed_prefix(a), fixed_prefix(b));
    if !(is_prefix(&pa, &pb) || is_prefix(&pb, &pa)) {
        return false;
    }
    match (has_glob(a), has_glob(b)) {
        (false, false) => true,
        (true, true) => true,
        (true, false) => glob_reaches(a, b),
        (false, true) => glob_reaches(b, a),
    }
}

/// Whether glob `g` matches the plain path `p` or anything under it.
fn glob_reaches(g: &str, p: &str) -> bool {
    let (pg, pp) = (fixed_prefix(g), fixed_prefix(p));
    if is_prefix(&pp, &pg) {
        // The glob lives inside the directory `p` owns.
        return true;
    }
    let p = segments(p).join("/");
    glob_match(g, &p) || glob_match(g, &format!("{p}/x")) || glob_match(g, &format!("{p}/x/x"))
}

#[cfg(test)]
#[path = "graph_tests.rs"]
mod tests;
