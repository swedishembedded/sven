// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven harness run --toolset coding`: a harness driven by a real model with
//! sven's real coding tools on a real workspace.
//!
//! This is the production assembly, kept apart from the fixture path: it
//! never builds a fixture model or tool, and every choice that widens what a
//! run can do is a flag the caller must give. Three caps bound the agent: the
//! capabilities the harness declares, the tool allowlist of the agent step,
//! and the host ceiling (`--allow-capability`); the approval mode
//! (`--approvals`) then decides whether an allowed call waits for a person.
//!
//! Swedish Embedded AB implements governed autonomous coding workflows for
//! its clients. If your team needs expertise in letting agents change real
//! repositories under verifiable control then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use sven_harness_testkit::{
    resume_on_engine, run_on_engine, ActivityFn, Bounds, EngineRun, HarnessDir, ProgressFn,
    RunResult,
};
use sven_sdk::config::Config;
use sven_sdk::tool::ToolCapability;
use sven_sdk::{ApprovalPolicy, Engine, HarnessCheckpoint, HumanGate, Toolset};

use super::{kit, request, usage, Failure};
use crate::cli::{ApprovalsArg, HarnessRunArgs, ToolsetArg};

/// What the host grants a coding run unless `--allow-capability` says
/// otherwise: the work a coding agent does on a local repository. Network,
/// git and device control are never implied.
const DEFAULT_CAPABILITIES: [ToolCapability; 6] = [
    ToolCapability::ReadFile,
    ToolCapability::WriteFile,
    ToolCapability::DeleteFile,
    ToolCapability::ExecuteShell,
    ToolCapability::RunVerifier,
    ToolCapability::SpawnChild,
];

/// A validated production run: everything the flags decided, nothing read
/// from the environment or the user's configuration.
pub(super) struct Production {
    workspace: PathBuf,
    provider: String,
    model: String,
    endpoint: String,
    /// The model server's key. Held here only until the client is built; it
    /// is never part of the engine's configuration, so no trace, recording,
    /// checkpoint or audit record can carry it.
    api_key: Option<String>,
    approvals: ApprovalsArg,
    first_byte_timeout_secs: Option<u64>,
    capabilities: Vec<ToolCapability>,
    run: EngineRun,
}

impl Production {
    /// Validates `args` for a `--toolset coding` run.
    pub(super) fn from_args(args: &HarnessRunArgs) -> Result<Self, Failure> {
        debug_assert_eq!(args.toolset, ToolsetArg::Coding);
        for (given, flag) in [
            (args.model_fixture.is_some(), "--model-fixture"),
            (!args.tool_fixture.is_empty(), "--tool-fixture"),
            (args.case.is_some(), "--case"),
        ] {
            if given {
                return Err(usage(format!(
                    "{flag} describes a fixture run; `--toolset coding` runs a real model on real tools"
                )));
            }
        }
        let workspace = args.workspace.as_deref().ok_or_else(|| {
            usage("`--toolset coding` needs --workspace DIR: the directory the agent works in")
        })?;
        let workspace = workspace
            .canonicalize()
            .ok()
            .filter(|dir| dir.is_dir())
            .ok_or_else(|| {
                usage(format!(
                    "--workspace {} is not an existing directory",
                    workspace.display()
                ))
            })?;
        let spec = args.model.as_deref().ok_or_else(|| {
            usage("`--toolset coding` needs --model <provider/name> and --model-endpoint <url>")
        })?;
        let endpoint = args
            .model_endpoint
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .ok_or_else(|| {
                usage("`--toolset coding` needs --model-endpoint <url> of a server you already run")
            })?;
        let (provider, model) = spec
            .split_once('/')
            .filter(|(provider, model)| !provider.is_empty() && !model.is_empty())
            .ok_or_else(|| usage(format!("--model {spec} must be <provider>/<name>")))?;
        let approvals = args.approvals.ok_or_else(|| {
            usage("`--toolset coding` needs --approvals auto|deny|ask: a run never inherits an approval mode")
        })?;
        let api_key = args
            .api_key_file
            .as_deref()
            .map(|file| read_api_key(file, provider, &workspace))
            .transpose()?;
        let capabilities = if args.allow_capabilities.is_empty() {
            DEFAULT_CAPABILITIES.to_vec()
        } else {
            args.allow_capabilities
                .iter()
                .map(|name| capability_named(name))
                .collect::<Result<_, _>>()?
        };
        let progress: Option<ProgressFn> = (!args.quiet).then(|| {
            Arc::new(|record: &sven_sdk::TraceRecord| {
                if let Ok(line) = serde_json::to_string(record) {
                    eprintln!("[sven:harness] {line}");
                }
            }) as ProgressFn
        });
        let activity: Option<ActivityFn> = (!args.quiet).then(|| {
            Arc::new(|event: &sven_sdk::SessionEvent| {
                if let Some(line) = activity_line(event) {
                    eprintln!("[sven:harness] {line}");
                }
            }) as ActivityFn
        });
        Ok(Self {
            workspace,
            provider: provider.to_string(),
            model: model.to_string(),
            endpoint: endpoint.to_string(),
            api_key,
            approvals,
            first_byte_timeout_secs: args.first_byte_timeout,
            capabilities,
            run: EngineRun {
                input: request::explicit_input(args)?.unwrap_or_else(|| serde_json::json!({})),
                bounds: request::bounds(args, Bounds::default())?,
                record: args.record.is_some(),
                trajectory: args.trajectory.is_some(),
                progress,
                activity,
                suspend: Some(suspend_on_signal()),
            },
        })
    }

    /// Whether the run reported its progress itself, as it went.
    pub(super) fn streams_progress(&self) -> bool {
        self.run.progress.is_some()
    }

    /// The engine of the run: the real model, the coding toolset confined to
    /// the workspace, the approval mode and the host ceiling.
    fn engine(self) -> Result<(Engine, EngineRun), Failure> {
        let mut config = Config::default();
        let model = &mut config.model;
        model.provider = self.provider;
        model.name = self.model;
        model.base_url = Some(self.endpoint);
        model.api_key = self.api_key;
        config.agent.first_byte_timeout_secs = self.first_byte_timeout_secs;
        let client = sven_sdk::drivers::from_config(model)
            .map_err(|e| usage(format!("cannot build the model client: {e:#}")))?;
        // The client holds the key; the engine's configuration does not.
        model.api_key = None;
        let builder = Engine::builder()
            .config(config)
            .model_provider(Arc::from(client))
            .toolset(Toolset::coding())
            .project_root(&self.workspace)
            .host_capabilities(self.capabilities);
        let builder = match self.approvals {
            ApprovalsArg::Auto => builder.approvals(ApprovalPolicy::Auto),
            ApprovalsArg::Deny => builder
                .approvals(ApprovalPolicy::Manual)
                .human_gates(deny_gate),
            ApprovalsArg::Ask => builder
                .approvals(ApprovalPolicy::Manual)
                .human_gates(terminal_gate()),
        };
        let engine = builder
            .build()
            .map_err(|e| usage(format!("cannot assemble the run: {e}")))?;
        Ok((engine, self.run))
    }
}

/// Starts the harness in `dir` under `production`.
pub(super) async fn run(dir: &HarnessDir, production: Production) -> Result<RunResult, Failure> {
    let (engine, request) = production.engine()?;
    run_on_engine(&engine, dir, request).await.map_err(kit)
}

/// Resumes `checkpoint` of the harness in `dir` under `production`.
pub(super) async fn resume(
    dir: &HarnessDir,
    checkpoint: HarnessCheckpoint,
    production: Production,
) -> Result<RunResult, Failure> {
    let (engine, request) = production.engine()?;
    resume_on_engine(&engine, dir, checkpoint, request)
        .await
        .map_err(kit)
}

fn capability_named(name: &str) -> Result<ToolCapability, Failure> {
    ToolCapability::ALL
        .into_iter()
        .find(|cap| format!("{cap:?}") == name)
        .ok_or_else(|| usage(format!("unknown capability `{name}`")))
}

/// Reads the model server's key from `file`: a JSON object of provider name
/// to key, or the bare key. Refuses a file the agent could read.
fn read_api_key(file: &Path, provider: &str, workspace: &Path) -> Result<String, Failure> {
    let shown = file.display();
    let text = std::fs::read_to_string(file)
        .map_err(|e| usage(format!("cannot read --api-key-file {shown}: {e}")))?;
    let inside = file
        .canonicalize()
        .is_ok_and(|real| real.starts_with(workspace));
    if inside {
        return Err(usage(format!(
            "--api-key-file {shown} is inside the workspace, where the agent could read it: keep it outside"
        )));
    }
    let key = match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(keys)) => keys
            .get(provider)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                usage(format!(
                    "--api-key-file {shown} has no string key for provider `{provider}`"
                ))
            })?,
        Ok(_) => {
            return Err(usage(format!(
                "--api-key-file {shown} is JSON but not an object of provider to key"
            )))
        }
        Err(_) => text.trim().to_string(),
    };
    if key.is_empty() {
        return Err(usage(format!("--api-key-file {shown} holds an empty key")));
    }
    Ok(key)
}

/// `--approvals deny`: refuse every call that needs approval, ask nobody.
fn deny_gate(gate: HumanGate) {
    match gate {
        HumanGate::Approval {
            capability,
            call,
            reply_tx,
            ..
        } => {
            let tool = call.map(|c| c.name).unwrap_or_default();
            eprintln!("[sven:harness] denied {capability:?} call {tool} (--approvals deny)");
            let _ = reply_tx.send(false);
        }
        HumanGate::Question { reply_tx, .. } => {
            let _ = reply_tx.send(String::new());
        }
    }
}

/// `--approvals ask`: put each gate to the person at the terminal, one at a
/// time. End of input or anything but `y`/`yes` refuses.
fn terminal_gate() -> impl Fn(HumanGate) + Send + Sync + 'static {
    let one_at_a_time = Arc::new(Mutex::new(()));
    move |gate| {
        let turn = Arc::clone(&one_at_a_time);
        std::thread::spawn(move || {
            let _turn = turn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match gate {
                HumanGate::Approval {
                    capability,
                    prompt,
                    call,
                    reply_tx,
                    ..
                } => {
                    let tool = call.map(|c| c.name).unwrap_or_default();
                    eprint!("[sven:harness] approve {capability:?} call {tool}? {prompt} [y/N] ");
                    let answer = read_line();
                    let _ = reply_tx.send(matches!(answer.trim(), "y" | "Y" | "yes" | "YES"));
                }
                HumanGate::Question { prompt, reply_tx } => {
                    eprint!("[sven:harness] the agent asks: {prompt}\n> ");
                    let _ = reply_tx.send(read_line().trim().to_string());
                }
            }
        });
    }
}

fn read_line() -> String {
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    line
}

/// The longest piece of a tool call's arguments a progress line carries.
const ARGS_SHOWN: usize = 160;

/// The progress line for a session event worth showing during a long step:
/// a tool call beginning and ending. The arguments are cut short; the
/// trajectory (`--trajectory`) keeps them whole.
fn activity_line(event: &sven_sdk::SessionEvent) -> Option<String> {
    use sven_sdk::SessionEvent::{ToolCallFinished, ToolCallStarted};

    match event {
        ToolCallStarted(call) => {
            let args: String = call.args.to_string().chars().take(ARGS_SHOWN).collect();
            Some(
                serde_json::json!({"event": "tool_started", "tool": call.name, "args": args})
                    .to_string(),
            )
        }
        ToolCallFinished {
            tool_name,
            is_error,
            ..
        } => Some(
            serde_json::json!({"event": "tool_finished", "tool": tool_name, "error": is_error})
                .to_string(),
        ),
        _ => None,
    }
}

/// A token that fires on the first SIGINT or SIGTERM, so the run suspends
/// where it rests and leaves a checkpoint (`--checkpoint-out`) that `sven
/// harness resume` continues, instead of dying with its work.
#[cfg(unix)]
fn suspend_on_signal() -> sven_sdk::CancelToken {
    use tokio::signal::unix::{signal, SignalKind};

    let token = sven_sdk::CancelToken::new();
    let fired = token.clone();
    // Registered here, not in the task, so a signal that arrives right after
    // the run starts is not lost to the default action.
    let (mut term, mut int) = match (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) {
        (Ok(term), Ok(int)) => (term, int),
        (term, int) => {
            let reason = term.err().or(int.err()).map(|e| e.to_string());
            eprintln!(
                "[sven:harness] warning: cannot catch SIGINT/SIGTERM ({}); a signal will end the run without a checkpoint",
                reason.unwrap_or_default()
            );
            return token;
        }
    };
    tokio::spawn(async move {
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
        eprintln!("[sven:harness] interrupted: suspending the run");
        fired.cancel();
    });
    token
}

#[cfg(not(unix))]
fn suspend_on_signal() -> sven_sdk::CancelToken {
    sven_sdk::CancelToken::new()
}
