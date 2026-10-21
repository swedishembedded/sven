// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What a command gate runs: an argv, or a script under a shell.
//!
//! A gate written `command: [argv]` is one step that runs the program
//! directly. A gate written `run:` is a script, or a list of named scripts run
//! in order, handed to `bash -eo pipefail -c` (or `sh -e -c`); lowering turns
//! each into the argv the gate tool runs, so there is one way a gate executes.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning build and test
//! scripts into trusted checks then you can procure our services by sending
//! an email to info@swedishembedded.com.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::error::HarnessError;
use crate::gate_command::GateCommand;
use crate::steps_validate::is_env_name;

/// The shell a script runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Shell {
    /// `bash -eo pipefail -c`: stop at the first failing command, and a
    /// pipeline fails when any stage does.
    #[default]
    Bash,
    /// `sh -e -c`.
    Sh,
}

impl Shell {
    /// The argv a script is appended to.
    fn prefix(self) -> &'static [&'static str] {
        match self {
            Self::Bash => &["bash", "-eo", "pipefail", "-c"],
            Self::Sh => &["sh", "-e", "-c"],
        }
    }
}

/// What `run:` holds.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Script {
    /// One script.
    Text(String),
    /// Named scripts, run in order; the first failure stops the gate.
    Steps(Vec<ScriptStep>),
}

/// One named script of a `run:` list.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptStep {
    /// What the result calls the step (`failed_step`).
    pub name: String,
    /// The script, a template.
    pub script: String,
}

/// The name a lone script, or a lone program, is reported under.
const SCRIPT_STEP: &str = "script";

impl CommandGate {
    /// The commands of the gate as written, templates not yet rewritten.
    pub(super) fn commands(&self) -> Vec<GateCommand> {
        let shell = self.shell.unwrap_or_default();
        let script = |name: &str, text: &str| GateCommand {
            name: name.to_string(),
            argv: shell
                .prefix()
                .iter()
                .map(|part| (*part).to_string())
                .chain([text.to_string()])
                .collect(),
        };
        match (&self.command, &self.run) {
            (Some(argv), _) => vec![GateCommand {
                name: argv.first().cloned().unwrap_or_default(),
                argv: argv.clone(),
            }],
            (None, Some(Script::Text(text))) => vec![script(SCRIPT_STEP, text)],
            (None, Some(Script::Steps(steps))) => steps
                .iter()
                .map(|step| script(&step.name, &step.script))
                .collect(),
            (None, None) => Vec::new(),
        }
    }
}

/// Checks what a gate runs: one of `command` and `run`, a shell only with a
/// script, scripts that say something, step names that are plain and unique,
/// and variable names the environment can hold.
pub(super) fn validate(place: &str, gate: &CommandGate) -> Result<(), HarnessError> {
    let invalid = |why: String| Err(HarnessError::Invalid(format!("{place}: {why}")));
    match (&gate.command, &gate.run) {
        (Some(_), Some(_)) | (None, None) => {
            return invalid("write exactly one of `command` (an argv) or `run` (a script)".into())
        }
        (Some(argv), None) => {
            if argv.first().is_none_or(|p| p.trim().is_empty()) {
                return invalid(
                    "`command` is an argv list whose first element names the program".into(),
                );
            }
            if gate.shell.is_some() {
                return invalid(
                    "`shell` belongs to `run`; a `command` runs without a shell".into(),
                );
            }
        }
        (None, Some(Script::Text(text))) => {
            if text.trim().is_empty() {
                return invalid("`run` is an empty script".into());
            }
        }
        (None, Some(Script::Steps(steps))) => {
            if steps.is_empty() {
                return invalid("`run` lists no steps".into());
            }
            let mut seen = BTreeSet::new();
            for step in steps {
                if !crate::workflow_validate::is_identifier(&step.name) {
                    return invalid(format!(
                        "step name `{}` is lowercase letters, digits, `-` and `_` (no `__`)",
                        step.name
                    ));
                }
                if step.script.trim().is_empty() {
                    return invalid(format!("the script of step `{}` is empty", step.name));
                }
                if !seen.insert(&step.name) {
                    return invalid(format!("step `{}` is named twice", step.name));
                }
            }
        }
    }
    if let Some(name) = gate.env.keys().find(|name| !is_env_name(name)) {
        return invalid(format!(
            "env name `{name}` is not an environment variable name ([A-Z_][A-Z0-9_]*)"
        ));
    }
    Ok(())
}

/// A command gate.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandGate {
    /// Human documentation.
    #[serde(default)]
    pub description: Option<String>,
    /// The argv; templates over inputs. Exactly one of `command` and `run`.
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// A script, or named scripts run in order; the first failure stops the
    /// gate. Templates, like an argv.
    #[serde(default)]
    pub run: Option<crate::gate_steps::Script>,
    /// The shell a script runs under (default `bash`).
    #[serde(default)]
    pub shell: Option<crate::gate_steps::Shell>,
    /// Variables set for the command or script: name to value, a template.
    /// Data should reach a script this way, not by splicing it into the text.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Seconds; the default applies when absent.
    #[serde(default)]
    pub timeout: Option<u64>,
    /// Working directory inside the workspace.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Runs in the directory a run input names instead: a string input the
    /// host sets to an absolute directory outside the workspace, such as a
    /// pinned checkout the agent cannot write. Exclusive with `cwd`.
    #[serde(default)]
    pub cwd_input: Option<String>,
    /// Text the command reads on standard input (a template over inputs and
    /// earlier outputs), for what is too large for an argument.
    #[serde(default)]
    pub stdin: Option<String>,
    /// How much of the end of each output stream the result keeps, in bytes
    /// (default 16384, at most 1048576): a check whose findings are long, a
    /// diff for a supervisor, needs more.
    #[serde(default)]
    pub output_bytes: Option<u64>,
    /// When the gate passes, its standard output becomes this variable's
    /// value (parsed as JSON for a `json` variable, a number or a boolean as
    /// such, trimmed text otherwise): a value computed by a trusted command,
    /// not by a model.
    #[serde(default)]
    pub capture: Option<String>,
    /// Numbers the command leaves in JSON files, by name.
    #[serde(default)]
    pub metrics: BTreeMap<String, MetricDecl>,
}

/// Where a number the gate's command leaves can be read.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricDecl {
    /// Workspace-relative JSON file.
    pub file: String,
    /// RFC 6901 pointer into it.
    pub pointer: String,
    /// Always `number`.
    #[serde(rename = "type")]
    pub ty: MetricType,
}

/// The only metric type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricType {
    /// A JSON number.
    Number,
}
