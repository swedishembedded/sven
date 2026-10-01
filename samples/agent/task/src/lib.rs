// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! An agent that drafts and publishes release notes, built on `sven-sdk` alone.
//!
//! The task is small on purpose; the sample is about what surrounds it:
//!
//! - the agent gets exactly three tools of the application's own, plus the
//!   read-only preset (which lets it ask a question), and nothing else;
//! - every run is bounded by a deadline, an output-token budget and the
//!   configured tool-round budget, and Ctrl-C cancels it;
//! - a question the agent asks parks the run; the agent is suspended to a
//!   file and resumed by a fresh engine, which answers it;
//! - the engine runs under manual approval, so every call that changes
//!   something is put to the application first; it approves publishing only
//!   a draft that passes its own check;
//! - an independent check decides whether the work is right, never the
//!   agent's own report;
//! - the whole run is written as an ATIF trajectory.
//!
//! Swedish Embedded AB implements agent applications with bounded, auditable
//! runs for its clients. If your team needs expertise in building on agent
//! frameworks then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sven_sdk::config::Config;
use sven_sdk::tool::{ApprovalPolicy as ToolPolicy, Tool, ToolCall, ToolCapability, ToolOutput};
use sven_sdk::{
    Agent, AgentState, ApprovalPolicy, CancelToken, Engine, HumanGate, RunConclusion, RunOptions,
    RunOutcome, Toolset,
};

/// The change log the agent reads, one change per line starting with `- `.
pub const CHANGES: &str = "CHANGES";
/// The file the agent writes.
pub const NOTES: &str = "RELEASE_NOTES.md";
/// What publishing produces.
pub const PUBLISHED: &str = "PUBLISHED.md";

/// The change log a demo workspace starts with.
pub const DEMO_CHANGES: &str = "\
- Faster start-up: the index loads lazily.
- Exports keep their column order.
- Settings remember the last tab.
";

/// The configuration `--demo` runs on: the scripted model in `demo.yaml`
/// (sven's `mock` provider, so no API key), with a small tool-round budget.
#[must_use]
pub fn demo_config() -> Config {
    let mut config = Config::default();
    config.model.provider = "mock".to_string();
    config.model.name = "demo".to_string();
    config.model.mock_responses_file = Some(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("demo.yaml")
            .display()
            .to_string(),
    );
    config.agent.max_tool_rounds = 8;
    config
}

/// How to run the task.
pub struct Options {
    /// The directory the agent works in; it must contain [`CHANGES`].
    pub workspace: PathBuf,
    /// The configuration naming the model and the tool-round budget.
    pub config: Config,
    /// The answer given to the agent's question.
    pub answer: String,
    /// Cancels whichever run is in progress.
    pub cancel: CancelToken,
    /// Longest any one run may take.
    pub deadline: Duration,
}

/// What happened, step by step.
pub struct Report {
    /// How the drafting run ended (a question parks it).
    pub drafted: RunOutcome,
    /// How the run ended after the question was answered, if it was asked.
    pub answered: Option<RunOutcome>,
    /// How the publishing run ended.
    pub published: RunOutcome,
    /// Whether the notes cover every change, as checked by [`verify`].
    pub verified: Result<(), String>,
    /// Where the trajectory was written.
    pub trajectory: PathBuf,
}

/// Runs the task end to end. See the crate documentation.
///
/// # Errors
///
/// Fails when the engine cannot be built, a run fails outright (the model
/// provider errored), or the state or trajectory cannot be written. A run
/// stopped by a bound or cancelled is reported, not an error.
pub async fn run(opts: &Options) -> anyhow::Result<Report> {
    let bounds = || {
        RunOptions::new()
            .cancel(opts.cancel.clone())
            .deadline(opts.deadline)
            .max_output_tokens(20_000)
    };

    let mut agent = engine(opts)?.agent("agent");
    let drafted = agent
        .send_with("Draft the release notes for this release.", bounds())
        .await?;

    // A question parks the run. The agent is suspended to a file, exactly as
    // a service would park it until someone answers, and a fresh engine
    // picks it up.
    let mut answered = None;
    if let (RunConclusion::Waiting, Some(question)) = (drafted.conclusion, &drafted.question) {
        let saved = opts.workspace.join("agent-state.json");
        std::fs::write(&saved, serde_json::to_vec_pretty(&agent.suspend())?)?;
        let state: AgentState = serde_json::from_slice(&std::fs::read(&saved)?)?;
        agent = engine(opts)?.resume(state)?;
        answered = Some(
            agent
                .answer_with(&question.id, &opts.answer, bounds())
                .await?,
        );
    }

    let published = agent
        .send_with("Publish the release notes.", bounds())
        .await?;

    let trajectory = opts.workspace.join("trajectory.json");
    write_trajectory(&agent, &trajectory)?;
    Ok(Report {
        drafted,
        answered,
        published,
        verified: verify(&opts.workspace),
        trajectory,
    })
}

/// The engine every run uses: the configured model, the read-only preset
/// and this application's three tools, under manual approval answered by
/// checking the draft before it is published, and with questions parked.
fn engine(opts: &Options) -> anyhow::Result<Engine> {
    let provider = sven_sdk::drivers::from_config(&opts.config.model)?;
    let workspace = opts.workspace.clone();
    Ok(Engine::builder()
        .config(opts.config.clone())
        .model_provider(Arc::from(provider))
        .toolset(Toolset::research())
        .tool(Arc::new(ReadChanges(opts.workspace.clone())))
        .tool(Arc::new(WriteNotes(opts.workspace.clone())))
        .tool(Arc::new(Publish(opts.workspace.clone())))
        .approvals(ApprovalPolicy::Manual)
        .park_questions()
        .human_gates(move |gate| match gate {
            // Publishing is approved only for a draft that passes the same
            // check the application applies at the end; every other change
            // is the agent's to make.
            HumanGate::Approval { call, reply_tx, .. } => {
                let publishing = call.as_ref().is_some_and(|c| c.name == "publish");
                let _ = reply_tx.send(!publishing || verify(&workspace).is_ok());
            }
            HumanGate::Question { reply_tx, .. } => {
                let _ = reply_tx.send(sven_sdk::tool::NO_USER_ANSWER.to_string());
            }
        })
        .build()?)
}

/// Checks the work independently of what the agent said about it: the notes
/// exist and mention every change in the change log.
///
/// # Errors
///
/// Describes what is missing.
pub fn verify(workspace: &Path) -> Result<(), String> {
    let changes = std::fs::read_to_string(workspace.join(CHANGES))
        .map_err(|e| format!("cannot read {CHANGES}: {e}"))?;
    let notes = std::fs::read_to_string(workspace.join(NOTES))
        .map_err(|e| format!("cannot read {NOTES}: {e}"))?
        .to_lowercase();
    let missing: Vec<&str> = changes
        .lines()
        .filter_map(|line| line.strip_prefix("- "))
        .filter(|change| !notes.contains(&keyword(change)))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("the notes do not cover: {}", missing.join("; ")))
    }
}

/// The first word of a change, which the notes must mention.
fn keyword(change: &str) -> String {
    change
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

fn write_trajectory(agent: &Agent, path: &Path) -> anyhow::Result<()> {
    let trajectory = agent.trajectory();
    sven_sdk::atif::validate_trajectory(&trajectory)
        .map_err(|e| anyhow::anyhow!("the trajectory is not valid ATIF: {e:?}"))?;
    sven_sdk::atif::persist::write_trajectory_atomic(path, &trajectory, None)?;
    Ok(())
}

/// Reads the change log.
struct ReadChanges(PathBuf);

#[async_trait::async_trait]
impl Tool for ReadChanges {
    fn name(&self) -> &str {
        "read_changes"
    }
    fn description(&self) -> &str {
        "Read the change log of this release, one change per line."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn default_policy(&self) -> ToolPolicy {
        ToolPolicy::Auto
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        match std::fs::read_to_string(self.0.join(CHANGES)) {
            Ok(text) => ToolOutput::ok(&call.id, text),
            Err(e) => ToolOutput::err(&call.id, format!("cannot read {CHANGES}: {e}")),
        }
    }
}

/// Writes the release notes; the only file the agent can write.
struct WriteNotes(PathBuf);

#[async_trait::async_trait]
impl Tool for WriteNotes {
    fn name(&self) -> &str {
        "write_notes"
    }
    fn description(&self) -> &str {
        "Write the release notes (Markdown), replacing any earlier draft."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {"content": {"type": "string", "description": "The notes, in Markdown."}},
            "required": ["content"]
        })
    }
    fn default_policy(&self) -> ToolPolicy {
        ToolPolicy::Auto
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::WriteFile
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let Some(content) = call.args.get("content").and_then(|v| v.as_str()) else {
            return ToolOutput::err(&call.id, "missing 'content'");
        };
        match std::fs::write(self.0.join(NOTES), content) {
            Ok(()) => ToolOutput::ok(&call.id, format!("wrote {NOTES}")),
            Err(e) => ToolOutput::err(&call.id, format!("cannot write {NOTES}: {e}")),
        }
    }
}

/// Publishes the notes by running the release command. Running a command is
/// what makes the kernel ask a human first.
struct Publish(PathBuf);

#[async_trait::async_trait]
impl Tool for Publish {
    fn name(&self) -> &str {
        "publish"
    }
    fn description(&self) -> &str {
        "Publish the release notes. Needs a human's approval."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn default_policy(&self) -> ToolPolicy {
        ToolPolicy::Ask
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ExecuteShell
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let status = tokio::process::Command::new("cp")
            .arg(NOTES)
            .arg(PUBLISHED)
            .current_dir(&self.0)
            .status()
            .await;
        match status {
            Ok(s) if s.success() => ToolOutput::ok(&call.id, format!("published {PUBLISHED}")),
            Ok(s) => ToolOutput::err(&call.id, format!("the release command failed: {s}")),
            Err(e) => ToolOutput::err(&call.id, format!("cannot run the release command: {e}")),
        }
    }
}
