// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Where a run's progress goes on stderr, and how it reads.
//!
//! Every progress event is one JSON line. `--progress json` prints it as
//! `[sven:harness] {...}` (the stable form scripts and logs consume);
//! `--progress text` renders the same line for a person: one indented line
//! per meaningful event under its task, with a few symbols, coloured when
//! stderr is a terminal. The text form is derived from the line alone, so it
//! says only what the event says.
//!
//! Swedish Embedded AB implements governed autonomous coding workflows for
//! its clients. If your team needs expertise in making long agent runs easy
//! to follow and audit then you can procure our services by sending an email
//! to info@swedishembedded.com.

use std::sync::{Mutex, PoisonError};

use serde_json::Value;

/// The longest value of a tool call's main argument shown in text.
const ARG_SHOWN: usize = 60;

/// Arguments that say what a tool call is about, most telling first.
const KEY_ARGS: [&str; 6] = ["path", "command", "pattern", "query", "name", "action"];

/// How text lines are decorated.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Style {
    Plain,
    Colour,
}

/// Writes progress lines to stderr in the chosen form.
pub(super) struct Progress(Form);

enum Form {
    Json,
    Text(Mutex<Renderer>),
}

impl Progress {
    pub(super) fn json() -> Self {
        Self(Form::Json)
    }

    pub(super) fn text(style: Style) -> Self {
        Self(Form::Text(Mutex::new(Renderer::new(style))))
    }

    /// Prints one progress event, given as its JSON line.
    pub(super) fn emit(&self, line: &str) {
        match &self.0 {
            Form::Json => eprintln!("[sven:harness] {line}"),
            Form::Text(renderer) => {
                let lines = renderer
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .render(line);
                for text in lines {
                    eprintln!("{text}");
                }
            }
        }
    }
}

/// Turns progress events into text lines; remembers the task being worked so
/// that a task's lines are grouped under one heading.
pub(super) struct Renderer {
    style: Style,
    task: Option<String>,
}

impl Renderer {
    pub(super) fn new(style: Style) -> Self {
        Self { style, task: None }
    }

    /// The text lines for one event line (none for events that add nothing).
    pub(super) fn render(&mut self, line: &str) -> Vec<String> {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        let source = &event["source"];
        let mut out = Vec::new();
        if let Some(task) = source["task"].as_str() {
            if self.task.as_deref() != Some(task) {
                self.task = Some(task.to_string());
                out.push(self.paint(Tone::Task, &format!("▸ {task}")));
            }
        }
        if let Some(text) = self.body(&event) {
            out.push(text);
        }
        out
    }

    fn body(&self, event: &Value) -> Option<String> {
        let source = &event["source"];
        let attempt = source["attempt"].as_u64().unwrap_or(0);
        let text = |key: &str| event[key].as_str().unwrap_or("?");
        let gate = source["gate"].as_str().unwrap_or("?");
        let after = if attempt > 0 {
            format!(" (after repair {attempt})")
        } else {
            String::new()
        };
        Some(match event["event"].as_str()? {
            "step_started" => match (text("primitive"), source["role"].as_str()) {
                ("agent", Some("repair")) => {
                    self.paint(Tone::Retry, &format!("  ↻ repair {attempt}"))
                }
                ("agent", Some(role)) => self.paint(Tone::Task, &format!("  ▸ {role}")),
                _ => return None,
            },
            "step_completed" | "variable_set" => return None,
            "tool_started" => format!(
                "    {}",
                tool_summary(text("tool"), event["args"].as_str().unwrap_or(""))
            ),
            "tool_finished" if event["error"].as_bool() == Some(true) => {
                self.paint(Tone::Fail, &format!("    ✗ {} failed", text("tool")))
            }
            "tool_finished" => return None,
            "verified" => {
                let (tone, mark) = verdict(event["ok"].as_bool() == Some(true));
                self.paint(
                    tone,
                    &format!("  {mark} verify {} {gate}{after}", text("verifier")),
                )
            }
            "gate_evaluated" => {
                let (tone, mark) = verdict(event["passed"].as_bool() == Some(true));
                let detail = match (event["passed"].as_bool(), event["exit_code"].as_i64()) {
                    (Some(false), Some(code)) => format!(" (exit {code})"),
                    (Some(false), None) => format!(" ({})", text("kind")),
                    _ => after,
                };
                self.paint(tone, &format!("  {mark} gate {gate}{detail}"))
            }
            "branch_selected" => format!("  → branch {}: {}", event["condition"], text("selected")),
            "turn_retried" => self.paint(
                Tone::Retry,
                &format!("  ↻ retry {}: {}", event["attempt"], text("reason")),
            ),
            "iteration_started" => {
                format!("  ▸ iterate {}: {} items", text("step"), event["total"])
            }
            "item_started" => {
                let attempt = event["attempt"].as_u64().unwrap_or(1);
                if attempt > 1 {
                    self.paint(
                        Tone::Retry,
                        &format!("  ↻ item {} attempt {attempt}", text("item")),
                    )
                } else {
                    format!("  ▸ item {}", text("item"))
                }
            }
            "item_completed" => self.paint(Tone::Pass, &format!("  ✓ item {}", text("item"))),
            "item_failed" => self.paint(
                Tone::Fail,
                &format!("  ✗ item {}: {}", text("item"), text("reason")),
            ),
            "child_started" => format!("  ▸ child {}", text("child")),
            "child_completed" => {
                let (tone, mark) = verdict(event["ok"].as_bool() == Some(true));
                self.paint(tone, &format!("  {mark} child {}", text("child")))
            }
            "failed" => self.paint(
                Tone::Fail,
                &format!("  ✗ failed {}: {}", text("code"), text("message")),
            ),
            "outcome" => self.paint(Tone::Task, "■ outcome produced"),
            other => format!("  · {other}"),
        })
    }

    fn paint(&self, tone: Tone, text: &str) -> String {
        match self.style {
            Style::Plain => text.to_string(),
            Style::Colour => format!("\x1b[{}m{text}\x1b[0m", tone.code()),
        }
    }
}

#[derive(Clone, Copy)]
enum Tone {
    Task,
    Pass,
    Fail,
    Retry,
}

impl Tone {
    fn code(self) -> &'static str {
        match self {
            Self::Task => "1;36",
            Self::Pass => "32",
            Self::Fail => "31",
            Self::Retry => "33",
        }
    }
}

fn verdict(passed: bool) -> (Tone, &'static str) {
    if passed {
        (Tone::Pass, "✓")
    } else {
        (Tone::Fail, "✗")
    }
}

/// `tool main-argument`, the argument cut short. The arguments arrive cut
/// off mid-JSON for long calls, so the value is read leniently.
fn tool_summary(tool: &str, args: &str) -> String {
    match KEY_ARGS.iter().find_map(|key| string_arg(args, key)) {
        Some(value) => format!("{tool} {value}"),
        None => tool.to_string(),
    }
}

/// The string value of `"key":"..."` in possibly truncated JSON text.
fn string_arg(args: &str, key: &str) -> Option<String> {
    let start = args.find(&format!("\"{key}\":\""))? + key.len() + 4;
    let mut value = String::new();
    let mut chars = args[start..].chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => value.push(chars.next().map_or(' ', |e| if e == 'n' { ' ' } else { e })),
            c => value.push(c),
        }
    }
    if value.chars().count() > ARG_SHOWN {
        value = value.chars().take(ARG_SHOWN).collect::<String>() + "...";
    }
    Some(value)
}

#[cfg(test)]
mod tests;
