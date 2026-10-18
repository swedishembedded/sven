// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Runs the `repo-repair` harness against a repository with the real coding
//! tools: the shell, `read_file` and `edit_file` act on a directory on disk,
//! confined to it, while the model is whatever [`ModelProvider`] the caller
//! hands over (a scripted one in tests, a local server in the opt-in run).
//!
//! Swedish Embedded AB implements verifiable automated code-repair workflows
//! for its clients. If your team needs expertise in agent harnesses for
//! fixing failing builds then you can procure our services by sending an
//! email to info@swedishembedded.com.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use serde::Serialize;
use serde_json::Value;
use sven_sdk::config::Config;
use sven_sdk::model::ModelProvider;
use sven_sdk::{ApprovalPolicy, Engine, RunOptions, Toolset};

/// How long one repair run may take.
const REPAIR_DEADLINE: Duration = Duration::from_secs(300);

/// What a repair run did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RepairReport {
    /// How the run ended: `completed` (the harness returned), `failed` (the
    /// harness's own failure, or a step that could not run), or the bound
    /// that stopped it.
    pub status: String,
    /// The harness's returned value, when it completed.
    pub output: Option<Value>,
    /// The harness's failure code, when it failed.
    pub failure_code: Option<String>,
    /// The failure's message.
    pub failure_message: Option<String>,
    /// How many times each step started.
    pub steps: BTreeMap<String, u64>,
    /// Generated tokens the model reported.
    pub output_tokens: u64,
}

/// Runs `harness_dir/harness.yaml` on `repo` (the project root the tools are
/// confined to) with `model`.
///
/// # Errors
///
/// The harness does not load or start, or the engine cannot be built. A run
/// that ends in the harness's own failure is a report, not an error.
pub async fn repair(
    harness_dir: &Path,
    repo: &Path,
    model: Arc<dyn ModelProvider>,
) -> anyhow::Result<RepairReport> {
    let engine = Engine::builder()
        .config(Config::default())
        .model_provider(model)
        .toolset(Toolset::coding())
        .project_root(repo)
        .approvals(ApprovalPolicy::Auto)
        .build()?;
    let source = std::fs::read_to_string(harness_dir.join("harness.yaml"))
        .with_context(|| format!("reading {}/harness.yaml", harness_dir.display()))?;
    let harness = engine.load_harness_from_str(&source)?;
    let mut run = harness.start(serde_json::json!({}))?;
    let outcome = run
        .execute(RunOptions::new().deadline(REPAIR_DEADLINE))
        .await?;
    let (output, failure) = match &outcome.result {
        Some(Ok(value)) => (Some(value.clone()), None),
        Some(Err(failure)) => (None, Some(failure)),
        None => (None, None),
    };
    let status = match (&output, &failure) {
        (Some(_), _) => "completed".to_string(),
        (_, Some(_)) => "failed".to_string(),
        _ => serde_json::to_value(outcome.conclusion)?
            .as_str()
            .unwrap_or_default()
            .to_string(),
    };
    Ok(RepairReport {
        status,
        output,
        failure_code: failure.map(|f| f.code.clone()),
        failure_message: failure.map(|f| f.message.clone()),
        steps: outcome.step_counts(),
        output_tokens: outcome.usage.output_tokens.unwrap_or(0),
    })
}
