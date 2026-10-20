// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The case manifest: one YAML file per case under a harness's `cases/`.
//!
//! A case is data, parsed strictly (unknown keys are errors, a model block
//! names exactly one kind, a tool response does exactly one thing) so a typo
//! fails loudly instead of silently weakening an expectation.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;
use sven_sdk::tool::ToolCapability;

use crate::error::TestkitError;
use crate::fixture_model::ModelFixture;
use crate::report::RunStatus;

/// One test case for a harness.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseManifest {
    /// The case's name; the file stem when the manifest does not give one.
    #[serde(default)]
    pub name: String,
    /// The inputs the run starts with.
    #[serde(default = "empty_object")]
    pub input: Value,
    /// The fixture model; absent means a model that only fails.
    #[serde(default)]
    pub model: Option<ModelFixture>,
    /// Fixture tools by name.
    #[serde(default)]
    pub tools: BTreeMap<String, ToolFixture>,
    /// Files seeded into the case's workspace (relative path to text).
    #[serde(default)]
    pub workspace: BTreeMap<String, String>,
    /// Bounds applied to the first phase.
    #[serde(default)]
    pub bounds: Bounds,
    /// Whether the run must replay offline to the same verdict and trace.
    #[serde(default = "yes")]
    pub replay: bool,
    /// What the first phase must do.
    pub expect: Expect,
    /// A second phase on the first phase's checkpoint.
    #[serde(default)]
    pub resume: Option<ResumePhase>,
}

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

fn yes() -> bool {
    true
}

impl CaseManifest {
    /// Parses a manifest from YAML text.
    ///
    /// # Errors
    ///
    /// [`TestkitError::Manifest`] for unknown keys, malformed fixtures, or a
    /// `resume` phase on a case that does not expect to be suspended.
    pub fn from_yaml(text: &str) -> Result<Self, TestkitError> {
        let case: Self = serde_yaml::from_str(text)
            .map_err(|e| TestkitError::Manifest(format!("invalid case manifest: {e}")))?;
        if case.resume.is_some() && case.expect.status != RunStatus::Suspended {
            return Err(TestkitError::Manifest(
                "a `resume` phase continues a checkpoint, so `expect.status` must be `suspended`"
                    .to_string(),
            ));
        }
        if let Some(tool) = &case.bounds.cancel_on_tool_start {
            if !case.tools.contains_key(tool) {
                return Err(TestkitError::Manifest(format!(
                    "`bounds.cancel_on_tool_start` names `{tool}`, which the case defines no fixture tool for"
                )));
            }
        }
        Ok(case)
    }

    /// Reads and parses a manifest file, naming the case after the file
    /// stem unless the manifest names itself.
    ///
    /// # Errors
    ///
    /// [`TestkitError::Io`] when the file cannot be read, otherwise as
    /// [`Self::from_yaml`], with the path prefixed.
    pub fn from_path(path: &Path) -> Result<Self, TestkitError> {
        let text = std::fs::read_to_string(path).map_err(|source| TestkitError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mut case = Self::from_yaml(&text)
            .map_err(|e| TestkitError::Manifest(format!("{}: {e}", path.display())))?;
        if case.name.is_empty() {
            case.name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
        }
        Ok(case)
    }
}

/// The second phase of a case: resume the first phase's checkpoint.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumePhase {
    /// The fixture model for the second phase.
    #[serde(default)]
    pub model: Option<ModelFixture>,
    /// The fixture tools for the second phase (fresh instances).
    #[serde(default)]
    pub tools: BTreeMap<String, ToolFixture>,
    /// What the second phase must do.
    pub expect: Expect,
}

/// Bounds on one run. All optional.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bounds {
    /// Suspends the run after this many milliseconds.
    #[serde(default)]
    pub deadline_ms: Option<u64>,
    /// Suspends the run once the model has generated this many tokens.
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    /// Cancels the run after this many milliseconds.
    #[serde(default)]
    pub cancel_after_ms: Option<u64>,
    /// Cancels the run once this fixture tool has started a call. Unlike a
    /// wall-clock bound it does not depend on how fast the machine is.
    #[serde(default)]
    pub cancel_on_tool_start: Option<String>,
}

/// What a run must do. Every key is optional; those given are all checked.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    /// How the run ends.
    pub status: RunStatus,
    /// The returned value, compared exactly.
    #[serde(default)]
    pub output: Option<Value>,
    /// A value the returned value must contain (objects recursively).
    #[serde(default)]
    pub output_includes: Option<Value>,
    /// The plan's own failure.
    #[serde(default)]
    pub failure: Option<FailureExpect>,
    /// Exact start counts of the listed top-level steps.
    #[serde(default)]
    pub steps: BTreeMap<String, u64>,
    /// An upper bound on step executions.
    #[serde(default)]
    pub max_steps: Option<u64>,
    /// The sorted multiset of fixture tools that ran (children included).
    #[serde(default)]
    pub tools_invoked: Option<Vec<String>>,
    /// Fixture tools that must not have run.
    #[serde(default)]
    pub tools_not_invoked: Vec<String>,
    /// Capabilities no executed fixture call may have exercised.
    #[serde(default)]
    pub forbidden_capabilities: Vec<ToolCapability>,
    /// An upper bound on generated tokens.
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
}

/// The plan failure a run must end with.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureExpect {
    /// The failure code.
    #[serde(default)]
    pub code: Option<String>,
    /// A substring of the failure message.
    #[serde(default)]
    pub message_contains: Option<String>,
}

/// A fixture tool: the capability it honestly exercises, and its answers.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolFixture {
    /// The tool's kernel capability. It must equal the capability the
    /// harness authorizes the tool's name under.
    pub capability: ToolCapability,
    /// One response per call, consumed in order; running out is a case
    /// error.
    #[serde(default)]
    pub responses: Vec<ToolResponse>,
}

/// What one call to a fixture tool does.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "RawToolResponse")]
pub enum ToolResponse {
    /// Answers with this value: a string verbatim, anything else as JSON
    /// text. Optionally writes a file into the workspace first.
    Ok {
        /// The answer.
        value: Value,
        /// A side effect inside the case workspace.
        write: Option<Write>,
    },
    /// Answers with a tool error.
    Error(String),
    /// Never answers; ends only by cancellation or a deadline.
    Block,
}

/// A file a fixture tool writes into the workspace.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Write {
    /// Workspace-relative path; confined to the workspace.
    pub path: String,
    /// The file's content.
    pub content: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawToolResponse {
    #[serde(default)]
    ok: Option<Value>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    block: bool,
    #[serde(default)]
    write: Option<Write>,
}

impl TryFrom<RawToolResponse> for ToolResponse {
    type Error = String;

    fn try_from(raw: RawToolResponse) -> Result<Self, String> {
        let kinds = usize::from(raw.ok.is_some())
            + usize::from(raw.error.is_some())
            + usize::from(raw.block);
        if kinds != 1 {
            return Err("a tool response has exactly one of `ok`, `error` or `block`".to_string());
        }
        if raw.write.is_some() && raw.ok.is_none() {
            return Err("`write` goes with an `ok` response".to_string());
        }
        Ok(match (raw.ok, raw.error) {
            (Some(value), _) => Self::Ok {
                value,
                write: raw.write,
            },
            (_, Some(message)) => Self::Error(message),
            _ => Self::Block,
        })
    }
}
