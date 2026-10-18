// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Runs a harness directory's cases through the public SDK alone.
//!
//! The harness samples are data directories judged by the test kit
//! (`sven harness test`). This crate is the other half of that claim: what
//! the kit does for a case that needs no tools, an application can do with
//! `sven-sdk` and a few lines of its own - load the document on an engine,
//! start a run, execute it, compare the verdict. It deliberately carries
//! none of the kit: a case that names tools, rule-driven or live models,
//! workspaces, bounds or a resume phase is refused with a pointer to the kit
//! (a directory run reports a live-model case as skipped instead).
//!
//! Swedish Embedded AB implements embeddable agent runtimes for its clients.
//! If your team needs expertise in running agent workflows from your own
//! application then you can procure our services by sending an email to
//! info@swedishembedded.com.

mod model;
mod real_tools;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context as _};
use serde::Deserialize;
use serde_json::Value;
use sven_sdk::config::Config;
use sven_sdk::{Engine, RunOptions, Toolset};

pub use model::{Answer, ScriptedCall, SequencedModel};
pub use real_tools::{repair, RepairReport};

/// How long a case may run before the runner gives up on it.
const CASE_DEADLINE: Duration = Duration::from_secs(60);

/// A case's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Every expectation held.
    Passed,
    /// These expectations did not hold.
    Failed(Vec<String>),
    /// The case names a live model, which a directory run never reaches;
    /// the reason says how to run it.
    Skipped(String),
}

/// One judged case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseResult {
    /// The case's name (its file stem).
    pub case: String,
    /// What happened.
    pub outcome: Outcome,
}

/// The part of the case manifest this runner understands.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    #[serde(default = "no_input")]
    input: Value,
    #[serde(default)]
    model: Option<ModelBlock>,
    expect: Expect,
}

fn no_input() -> Value {
    Value::Object(serde_json::Map::new())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelBlock {
    sequence: Vec<Item>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    #[serde(default)]
    text: String,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Usage {
    #[serde(default)]
    input: u32,
    #[serde(default)]
    output: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Expect {
    status: Status,
    #[serde(default)]
    output: Option<Value>,
    #[serde(default)]
    failure: Option<FailureExpect>,
    #[serde(default)]
    steps: BTreeMap<String, u64>,
    #[serde(default)]
    max_steps: Option<u64>,
    #[serde(default)]
    max_output_tokens: Option<u64>,
}

#[derive(Deserialize, PartialEq, Eq, Debug, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Status {
    Completed,
    Failed,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FailureExpect {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message_contains: Option<String>,
}

/// Runs every `cases/*.yaml` of the harness directory `dir`, in name order.
/// A case that names a live model is reported as skipped, not run.
///
/// # Errors
///
/// As [`run_case`], for the first case that cannot be run at all.
pub async fn run_dir(dir: &Path) -> anyhow::Result<Vec<CaseResult>> {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(dir.join("cases"))
        .with_context(|| format!("reading {}/cases", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "yaml"))
        .collect();
    cases.sort();
    let mut results = Vec::new();
    for case in cases {
        if names_a_live_model(&case)? {
            results.push(CaseResult {
                case: case_name(&case),
                outcome: Outcome::Skipped(
                    "names a live model; run it with `sven harness test <dir> --model-endpoint <url>`"
                        .to_string(),
                ),
            });
            continue;
        }
        results.push(run_case(dir, &case).await?);
    }
    Ok(results)
}

/// Runs one case against the harness directory `dir` and judges it.
///
/// # Errors
///
/// A case this runner will not run (see the crate documentation), a harness
/// that does not load, or inputs the harness does not accept. A run that
/// ends in the plan's own failure is a verdict, not an error.
pub async fn run_case(dir: &Path, case_path: &Path) -> anyhow::Result<CaseResult> {
    let name = case_name(case_path);
    let text = std::fs::read_to_string(case_path)
        .with_context(|| format!("reading {}", case_path.display()))?;
    let case = parse_case(&name, &text)?;

    let answers = case
        .model
        .map(|model| model.sequence)
        .unwrap_or_default()
        .into_iter()
        .map(|item| Answer {
            text: item.text,
            usage: item.usage.map(|u| (u.input, u.output)),
            tool_calls: Vec::new(),
        });
    // An explicit default configuration and an injected model: the runner
    // never reads the user's configuration, so it cannot reach a model the
    // case did not give it.
    let engine = Engine::builder()
        .config(Config::default())
        .model_provider(Arc::new(SequencedModel::new(answers)))
        .toolset(Toolset::none())
        .build()?;
    let source = std::fs::read_to_string(dir.join("harness.yaml"))
        .with_context(|| format!("reading {}/harness.yaml", dir.display()))?;
    let harness = engine.load_harness_from_str(&source)?;
    let mut run = harness.start(case.input)?;
    let outcome = run
        .execute(RunOptions::new().deadline(CASE_DEADLINE))
        .await?;

    let mut reasons = Vec::new();
    let (verdict, value, failure) = match &outcome.result {
        Some(Ok(value)) => (Some(Status::Completed), Some(value), None),
        Some(Err(failure)) => (Some(Status::Failed), None, Some(failure)),
        None => (None, None, None),
    };
    if verdict != Some(case.expect.status) {
        reasons.push(format!(
            "status: expected {:?}, the run ended {:?} ({:?})",
            case.expect.status, verdict, outcome.conclusion
        ));
    }
    if let (Some(expected), Some(actual)) = (&case.expect.output, value) {
        if expected != actual {
            reasons.push(format!("output: expected {expected}, got {actual}"));
        }
    }
    if let Some(expected) = &case.expect.failure {
        match failure {
            None => reasons.push("failure: expected one, the run did not fail".to_string()),
            Some(actual) => {
                if expected
                    .code
                    .as_ref()
                    .is_some_and(|code| *code != actual.code)
                {
                    reasons.push(format!(
                        "failure code: expected {:?}, got {:?}",
                        expected.code, actual.code
                    ));
                }
                if let Some(fragment) = &expected.message_contains {
                    if !actual.message.contains(fragment.as_str()) {
                        reasons.push(format!(
                            "failure message: expected to contain {fragment:?}, got {:?}",
                            actual.message
                        ));
                    }
                }
            }
        }
    }
    let counts = outcome.step_counts();
    for (step, expected) in &case.expect.steps {
        let actual = counts.get(step).copied().unwrap_or(0);
        if actual != *expected {
            reasons.push(format!("steps.{step}: expected {expected}, got {actual}"));
        }
    }
    if case.expect.max_steps.is_some_and(|max| outcome.steps > max) {
        reasons.push(format!(
            "max_steps: {} steps ran, at most {:?} allowed",
            outcome.steps, case.expect.max_steps
        ));
    }

    let spent = outcome.usage.output_tokens.unwrap_or(0);
    if case.expect.max_output_tokens.is_some_and(|max| spent > max) {
        reasons.push(format!(
            "max_output_tokens: {spent} generated, at most {:?} allowed",
            case.expect.max_output_tokens
        ));
    }

    Ok(CaseResult {
        case: name,
        outcome: if reasons.is_empty() {
            Outcome::Passed
        } else {
            Outcome::Failed(reasons)
        },
    })
}

/// Parses a case, refusing by name anything that needs the test kit.
/// A case's name: its file stem.
fn case_name(case_path: &Path) -> String {
    case_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Whether the case file asks for a live model.
fn names_a_live_model(case_path: &Path) -> anyhow::Result<bool> {
    let text = std::fs::read_to_string(case_path)
        .with_context(|| format!("reading {}", case_path.display()))?;
    let raw: serde_yaml::Value = serde_yaml::from_str(&text)
        .with_context(|| format!("{} is not valid YAML", case_path.display()))?;
    Ok(raw
        .get("model")
        .and_then(|model| model.get("live"))
        .is_some_and(|live| !live.is_null()))
}

fn parse_case(name: &str, text: &str) -> anyhow::Result<Case> {
    let raw: serde_yaml::Value = serde_yaml::from_str(text).context("not valid YAML")?;
    if let Some(feature) = needs_the_kit(&raw) {
        bail!(
            "case `{name}` uses `{feature}`, which this runner (public SDK only) does not \
             carry; run it with `sven harness test <dir>`"
        );
    }
    serde_yaml::from_value(raw).with_context(|| format!("case `{name}`"))
}

/// The first manifest feature that only the test kit provides, if any.
fn needs_the_kit(case: &serde_yaml::Value) -> Option<String> {
    let present =
        |value: &serde_yaml::Value, key: &str| value.get(key).is_some_and(|v| !v.is_null());
    for key in ["tools", "workspace", "bounds", "resume"] {
        // An empty `tools: {}` asks for nothing.
        let empty = case
            .get(key)
            .is_some_and(|v| v.as_mapping().is_some_and(serde_yaml::Mapping::is_empty));
        if present(case, key) && !empty {
            return Some(key.to_string());
        }
    }
    if let Some(model) = case.get("model") {
        for key in ["rules", "file", "fail", "live"] {
            if present(model, key) {
                return Some(key.to_string());
            }
        }
        let calls_tools = model
            .get("sequence")
            .and_then(serde_yaml::Value::as_sequence)
            .is_some_and(|items| items.iter().any(|item| present(item, "tool_calls")));
        if calls_tools {
            return Some("tool_calls".to_string());
        }
    }
    let expect = case.get("expect")?;
    [
        "output_includes",
        "tools_invoked",
        "tools_not_invoked",
        "forbidden_capabilities",
    ]
    .into_iter()
    .find(|key| present(expect, key))
    .map(|key| format!("expect.{key}"))
}
