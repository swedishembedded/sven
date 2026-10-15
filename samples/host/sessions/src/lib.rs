// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A host that serves several agent sessions on one engine, built on
//! `sven-sdk` alone.
//!
//! The host is a deploy desk: each session asks to deploy one service, and
//! the deploy tool needs a person's approval. Two ways of answering are shown,
//! one after the other:
//!
//! - **Live.** Two sessions share an engine and each has a gate handler of
//!   its own. Every approval names the run it belongs to, so the host sees
//!   that a gate reached the session that raised it, and the events of that
//!   session carry the same run id.
//! - **Parked.** The engine parks approvals instead: both sessions end
//!   `Waiting`, are suspended to files, and the process "restarts" - a fresh
//!   engine resumes them. What waits for a decision is read from the stored
//!   state alone, each approval is decided, and the sessions finish. The
//!   numbering of each session's events carries on across the restart.
//!
//! Swedish Embedded AB implements multi-session agent hosts for its clients.
//! If your team needs expertise in running many human-supervised agent
//! sessions in one service then you can procure our services by sending an
//! email to info@swedishembedded.com.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sven_sdk::config::Config;
use sven_sdk::tool::{ApprovalPolicy as ToolPolicy, Tool, ToolCall, ToolCapability, ToolOutput};
use sven_sdk::{
    AgentState, ApprovalPolicy, Decision, Engine, EngineBuilder, HumanGate, RunConclusion,
    RunEvent, RunId,
};

/// Where the deploy tool leaves a marker for each service it deployed.
const DEPLOYED: &str = "deployed";

/// The configuration `--demo` runs on: the scripted model in `demo.yaml`
/// (sven's `mock` provider, so no API key).
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

/// How to run the desk.
pub struct Options {
    /// Where states and deploy markers are kept.
    pub workspace: PathBuf,
    /// The configuration naming the model.
    pub config: Config,
}

/// One approval the live handlers saw, and where it was routed.
#[derive(Debug, Clone, PartialEq)]
pub struct Routed {
    /// The session whose handler received the gate.
    pub session: String,
    /// The service the gated call would deploy.
    pub service: String,
    /// The run the gate named.
    pub run_id: RunId,
    /// What the session's reviewer answered.
    pub approved: bool,
}

/// One approval found waiting in a stored session after the restart.
#[derive(Debug, Clone, PartialEq)]
pub struct Waiting {
    /// The session it belongs to.
    pub session: String,
    /// The service the gated call would deploy.
    pub service: String,
    /// The decision taken.
    pub decision: Decision,
    /// How the session ended once the decision was taken.
    pub concluded: RunConclusion,
}

/// What happened, act by act.
pub struct Report {
    /// The live approvals, with the run each named.
    pub routed: Vec<Routed>,
    /// The run each live session's events carried.
    pub live_runs: BTreeMap<String, RunId>,
    /// The approvals decided after the restart.
    pub decided: Vec<Waiting>,
    /// Whether each parked session numbered its events on from where it left
    /// off before the restart.
    pub numbering_continued: bool,
    /// The services that were deployed.
    pub deployed: Vec<String>,
    /// Whether the desk did what its policy says; see [`verify`].
    pub verified: Result<(), String>,
}

/// Runs both acts. See the crate documentation.
///
/// # Errors
///
/// Fails when an engine cannot be built, a run fails outright (the model
/// provider errored), or a state cannot be written or read.
pub async fn run(opts: &Options) -> anyhow::Result<Report> {
    std::fs::create_dir_all(opts.workspace.join(DEPLOYED))?;
    let (routed, live_runs) = live(opts).await?;
    let (decided, numbering_continued) = parked(opts).await?;
    let deployed = deployed(&opts.workspace)?;
    let verified = verify(&routed, &live_runs, &decided, &deployed);
    Ok(Report {
        routed,
        live_runs,
        decided,
        numbering_continued,
        deployed,
        verified,
    })
}

/// What the desk's policy says happens: the ledger reviewer approves and the
/// inventory reviewer refuses; after the restart search is approved and
/// billing is refused; each gate reached its own session; and exactly the
/// approved services were deployed.
///
/// # Errors
///
/// Describes the first thing that did not hold.
pub fn verify(
    routed: &[Routed],
    live_runs: &BTreeMap<String, RunId>,
    decided: &[Waiting],
    deployed: &[String],
) -> Result<(), String> {
    for gate in routed {
        if live_runs.get(&gate.session) != Some(&gate.run_id) {
            return Err(format!(
                "the gate for {} named a run its session never ran",
                gate.service
            ));
        }
    }
    let approved_live = routed.iter().filter(|g| g.approved).count();
    let approved_parked = decided
        .iter()
        .filter(|d| d.decision == Decision::Approve)
        .count();
    if routed.len() != 2 || decided.len() != 2 {
        return Err("each act should have raised two approvals".to_string());
    }
    if deployed.len() != approved_live + approved_parked {
        return Err(format!(
            "{} services were deployed but {} were approved",
            deployed.len(),
            approved_live + approved_parked
        ));
    }
    let expected = ["ledger", "search"];
    if deployed != expected {
        return Err(format!("deployed {deployed:?}, expected {expected:?}"));
    }
    Ok(())
}

/// The engine both acts share the shape of: the configured model and the one
/// tool of the desk's own.
fn desk(opts: &Options) -> anyhow::Result<EngineBuilder> {
    let provider = sven_sdk::drivers::from_config(&opts.config.model)?;
    Ok(Engine::builder()
        .config(opts.config.clone())
        .model_provider(Arc::from(provider))
        .tool(Arc::new(Deploy(opts.workspace.join(DEPLOYED))))
        .approvals(ApprovalPolicy::Manual))
}

/// The service a gated `deploy` call names.
fn service_of(call: Option<&sven_sdk::machine::GatedCall>) -> String {
    call.and_then(|c| c.args.get("service"))
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Act one: two sessions on one engine, each with a handler of its own.
async fn live(opts: &Options) -> anyhow::Result<(Vec<Routed>, BTreeMap<String, RunId>)> {
    // An agent without a handler of its own gets this one, which refuses.
    let engine = desk(opts)?
        .human_gates(|gate| {
            if let HumanGate::Approval { reply_tx, .. } = gate {
                let _ = reply_tx.send(false);
            }
        })
        .build()?;
    let routed: Arc<Mutex<Vec<Routed>>> = Arc::default();
    let mut live_runs = BTreeMap::new();

    for (session, service, approves) in [
        ("ledger", "ledger", true),
        ("inventory", "inventory", false),
    ] {
        let log = Arc::clone(&routed);
        let name = session.to_string();
        let mut agent = engine.agent("agent").with_human_gates(move |gate| {
            if let HumanGate::Approval {
                run_id,
                call,
                reply_tx,
                ..
            } = gate
            {
                log.lock().unwrap().push(Routed {
                    session: name.clone(),
                    service: service_of(call.as_ref()),
                    run_id,
                    approved: approves,
                });
                let _ = reply_tx.send(approves);
            }
        });
        let mut events = agent.events();
        agent
            .send(&format!("Deploy the {service} service."))
            .await?;
        if let Some(first) = first_event(&mut events).await {
            live_runs.insert(session.to_string(), first.run_id);
        }
    }
    let routed = routed.lock().unwrap().clone();
    Ok((routed, live_runs))
}

/// The first event of the run that has just ended.
async fn first_event(events: &mut sven_sdk::EventStream) -> Option<RunEvent> {
    tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
        .await
        .ok()
        .flatten()
}

/// Act two: park, restart, decide.
async fn parked(opts: &Options) -> anyhow::Result<(Vec<Waiting>, bool)> {
    let sessions = [("search", "search"), ("billing", "billing")];
    let mut last_seq = BTreeMap::new();

    // Before the restart: each session runs until it needs a decision.
    {
        let engine = desk(opts)?.park_approvals().build()?;
        for (session, service) in sessions {
            let mut agent = engine.agent("agent");
            let mut events = agent.events();
            let outcome = agent
                .send(&format!("Deploy the {service} service."))
                .await?;
            anyhow::ensure!(
                outcome.conclusion == RunConclusion::Waiting && outcome.approval.is_some(),
                "{session} should be waiting for a decision, not {:?}",
                outcome.conclusion
            );
            last_seq.insert(session, last_seq_of(&mut events).await);
            save(&opts.workspace, session, &agent.suspend())?;
        }
    }

    // After the restart: nothing of the first engine or its agents is left.
    let engine = desk(opts)?.park_approvals().build()?;
    let mut decided = Vec::new();
    let mut numbering_continued = true;
    for (session, _) in sessions {
        let state = load(&opts.workspace, session)?;
        // What waits for a decision is in the stored state alone.
        let approval = state
            .pending_approval()
            .ok_or_else(|| anyhow::anyhow!("{session} has nothing waiting"))?;
        let service = service_of(approval.call.as_ref());
        let decision = if service == "billing" {
            Decision::Deny {
                reason: "billing is frozen until the audit ends".to_string(),
            }
        } else {
            Decision::Approve
        };

        let mut agent = engine.resume(state)?;
        let mut events = agent.events();
        let outcome = agent.decide(approval.id, decision.clone()).await?;
        if let Some(first) = first_event(&mut events).await {
            numbering_continued &= first.run_id == approval.run_id
                && Some(first.seq) > last_seq.get(session).copied().flatten();
        }
        decided.push(Waiting {
            session: session.to_string(),
            service,
            decision,
            concluded: outcome.conclusion,
        });
    }
    Ok((decided, numbering_continued))
}

/// The highest sequence number the stream holds.
async fn last_seq_of(events: &mut sven_sdk::EventStream) -> Option<u64> {
    let mut last = None;
    while let Ok(Some(event)) =
        tokio::time::timeout(std::time::Duration::from_millis(200), events.recv()).await
    {
        last = Some(event.seq);
    }
    last
}

fn state_path(workspace: &Path, session: &str) -> PathBuf {
    workspace.join(format!("{session}.json"))
}

fn save(workspace: &Path, session: &str, state: &AgentState) -> anyhow::Result<()> {
    std::fs::write(
        state_path(workspace, session),
        serde_json::to_vec_pretty(state)?,
    )?;
    Ok(())
}

fn load(workspace: &Path, session: &str) -> anyhow::Result<AgentState> {
    Ok(serde_json::from_slice(&std::fs::read(state_path(
        workspace, session,
    ))?)?)
}

/// The services the deploy tool left a marker for, sorted.
fn deployed(workspace: &Path) -> anyhow::Result<Vec<String>> {
    let mut services = Vec::new();
    for entry in std::fs::read_dir(workspace.join(DEPLOYED))? {
        services.push(entry?.file_name().to_string_lossy().into_owned());
    }
    services.sort();
    Ok(services)
}

/// Deploys a service by leaving a marker for it. Running a command is what
/// makes the kernel ask a person first.
struct Deploy(PathBuf);

#[async_trait::async_trait]
impl Tool for Deploy {
    fn name(&self) -> &str {
        "deploy"
    }
    fn description(&self) -> &str {
        "Deploy a service to production. Needs a person's approval."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {"service": {"type": "string", "description": "The service to deploy."}},
            "required": ["service"]
        })
    }
    fn default_policy(&self) -> ToolPolicy {
        ToolPolicy::Ask
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ExecuteShell
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let Some(service) = call.args.get("service").and_then(|v| v.as_str()) else {
            return ToolOutput::err(&call.id, "missing 'service'");
        };
        // A service name is a file name here; refuse anything that is a path.
        if service.is_empty() || service.contains(['/', '\\']) || service.starts_with('.') {
            return ToolOutput::err(&call.id, format!("{service:?} is not a service name"));
        }
        match std::fs::write(self.0.join(service), "deployed\n") {
            Ok(()) => ToolOutput::ok(&call.id, format!("deployed {service}")),
            Err(e) => ToolOutput::err(&call.id, format!("cannot deploy {service}: {e}")),
        }
    }
}
