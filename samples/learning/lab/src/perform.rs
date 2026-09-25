// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

//! Perform a demonstration's actions with the real tools, and keep what they
//! said.
//!
//! A demonstration is only worth training on if everything except the
//! decisions is real. The decisions are ours; the observations must not be.
//! Inventing a tool's output would teach the model to expect a format it will
//! never be shown, and the format is not guessable - `read_file` prefixes
//! every line with `L<n>:`, `shell` frames stdout and stderr its own way.
//!
//! So the actions run through `sven tool call`, which is the same executor the
//! agent uses and the same rendering the agent is shown. What comes back is
//! the observation, not a reconstruction of one.
//!
//! # Why not drive the agent
//!
//! Because it does not need to be driven to produce these. Leading the agent
//! through a scripted episode exercises the whole loop, which is valuable and
//! is what the measured arms do - but for building a demonstration it adds a
//! dependency on the loop behaving, and the loop currently stalls after two
//! scripted rounds (see the sample's README). The tools are the part a
//! demonstration needs, and they are reachable on their own.
//!
//! What this costs, stated plainly: the demonstration is assembled rather than
//! observed end to end. The actions, their observations and the final verdict
//! are all real; the ordering between them is ours rather than a loop's. That
//! is why these records carry [`Provenance::Scripted`] and can never count as
//! the model improving on its own.
//!
//! [`Provenance::Scripted`]: crate::Provenance

use std::path::Path;
use std::process::Command;

/// One action to perform, named the way the tool's own schema names it.
#[derive(Clone, Debug)]
pub struct Action {
    pub tool: String,
    /// The arguments as a JSON object; rendered to `key=value` pairs for the
    /// CLI and kept verbatim for the training record.
    pub arguments: serde_json::Value,
}

impl Action {
    pub fn new(tool: &str, arguments: serde_json::Value) -> Action {
        Action {
            tool: tool.to_string(),
            arguments,
        }
    }
}

/// What an action did.
#[derive(Clone, Debug)]
pub struct Performed {
    pub action: Action,
    /// Exactly what the tool printed - the observation the model would see.
    pub observation: String,
    pub failed: bool,
}

/// Why a demonstration could not be performed.
#[derive(Debug)]
pub enum PerformError {
    Spawn {
        tool: String,
        source: std::io::Error,
    },
    /// A tool reported an error. A demonstration whose own actions fail is not
    /// a demonstration, and continuing would build a record of doing the wrong
    /// thing and calling it correct.
    Refused {
        tool: String,
        output: String,
    },
    Arguments(String),
}

impl std::fmt::Display for PerformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PerformError::Spawn { tool, source } => write!(f, "could not run {tool}: {source}"),
            PerformError::Refused { tool, output } => {
                write!(f, "{tool} refused the demonstrated action: {output}")
            }
            PerformError::Arguments(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for PerformError {}

/// Run one action through the real tool executor, in `workspace`.
///
/// `sven` is the binary to use; the caller names it so a sample is not tied to
/// one install.
pub fn perform(
    sven: &Path,
    workspace: &Path,
    env: &[(String, String)],
    action: &Action,
) -> Result<Performed, PerformError> {
    let object = action.arguments.as_object().ok_or_else(|| {
        PerformError::Arguments(format!("{}: arguments must be an object", action.tool))
    })?;

    let mut command = Command::new(sven);
    command
        .arg("tool")
        .arg("call")
        .arg(&action.tool)
        .current_dir(workspace);
    for (key, value) in object {
        // Strings go through unquoted; everything else as its JSON text, which
        // is how the CLI's `key=value` parser expects numbers and booleans.
        let rendered = match value {
            serde_json::Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        command.arg(format!("{key}={rendered}"));
    }
    for (key, value) in env {
        command.env(key, value);
    }
    // The built-in read resolves a missing absolute path by dropping
    // components, which can reach a different file than the one asked for.
    command.env("SVEN_NO_PATH_ASCENT", "1");

    let output = command.output().map_err(|source| PerformError::Spawn {
        tool: action.tool.clone(),
        source,
    })?;

    let observation = String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string();
    let stderr = String::from_utf8_lossy(&output.stderr)
        .trim_end()
        .to_string();

    if !output.status.success() {
        return Err(PerformError::Refused {
            tool: action.tool.clone(),
            output: if stderr.is_empty() {
                observation
            } else {
                stderr
            },
        });
    }

    Ok(Performed {
        action: action.clone(),
        observation,
        failed: false,
    })
}

/// Perform every action in order, stopping at the first refusal.
pub fn perform_all(
    sven: &Path,
    workspace: &Path,
    env: &[(String, String)],
    actions: &[Action],
) -> Result<Vec<Performed>, PerformError> {
    let mut done = Vec::with_capacity(actions.len());
    for action in actions {
        done.push(perform(sven, workspace, env, action)?);
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_must_be_an_object() {
        let action = Action::new("shell", serde_json::json!("not an object"));
        let err = perform(Path::new("/nonexistent"), Path::new("/tmp"), &[], &action)
            .expect_err("a non-object must be refused before anything is run");
        assert!(matches!(err, PerformError::Arguments(_)), "{err}");
    }

    #[test]
    fn a_missing_binary_is_reported_against_the_tool_that_needed_it() {
        let action = Action::new("shell", serde_json::json!({ "shell_command": "true" }));
        let err = perform(
            Path::new("/nonexistent/sven"),
            Path::new("/tmp"),
            &[],
            &action,
        )
        .expect_err("a missing binary cannot run anything");
        assert!(err.to_string().contains("shell"), "{err}");
    }
}
