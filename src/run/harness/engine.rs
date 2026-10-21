// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The engine of `sven harness run` and `resume`: a real model with sven's
//! real coding tools on a real workspace.
//!
//! Every choice that widens what a run can do is a flag the caller must give.
//! Three caps bound the agent: the
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
use sven_sdk::config::Config;
use sven_sdk::tool::ToolCapability;
use sven_sdk::{ApprovalPolicy, Engine, HarnessCheckpoint, HumanGate, Toolset};

use super::args;
use super::engine_run::{
    resume_on_engine, run_on_engine, ActivityFn, CheckpointFn, EngineRun, ProgressFn, RunResult,
};
use super::stamps::{stamped_line, Stamps};
use super::{kit, usage, Failure};
use crate::cli::{ApprovalsArg, HarnessRunArgs};

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

/// A validated run: everything the flags decided, nothing read
/// from the environment or the user's configuration.
pub(super) struct Assembly {
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
    /// Where `--checkpoint-out` keeps the latest checkpoint while the run
    /// goes; removed when the run ends with a verdict.
    progress_checkpoint: Option<PathBuf>,
    quiet: bool,
    run: EngineRun,
}

impl Assembly {
    /// Validates the flags of `run` and `resume`.
    pub(super) fn from_args(
        args: &HarnessRunArgs,
        plan: Arc<sven_sdk::harness_plan::HarnessPlan>,
        stamps: &Stamps,
    ) -> Result<Self, Failure> {
        let workspace = args.workspace.as_deref().ok_or_else(|| {
            usage("--workspace DIR is required: the directory the agent works in")
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
            usage("--model <provider/name> and --model-endpoint <url> are required")
        })?;
        let endpoint = args
            .model_endpoint
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .ok_or_else(|| {
                usage("--model-endpoint <url> of a server you already run is required")
            })?;
        let (provider, model) = spec
            .split_once('/')
            .filter(|(provider, model)| !provider.is_empty() && !model.is_empty())
            .ok_or_else(|| usage(format!("--model {spec} must be <provider>/<name>")))?;
        let approvals = args.approvals.ok_or_else(|| {
            usage("--approvals auto|deny|ask is required: a run never inherits an approval mode")
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
        // Every record is noted as it arrives (the trace file is stamped from
        // these times); it is printed as well unless the run is quiet.
        let progress: Option<ProgressFn> = {
            let tracker = std::sync::Mutex::new(sven_sdk::SourceTracker::new(plan));
            let (stamps, quiet) = (stamps.clone(), args.quiet);
            Some(Arc::new(move |record: &sven_sdk::TraceRecord| {
                let ts_ms = stamps.note();
                let sourced = tracker
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .next(record);
                if let (false, Ok(line)) = (quiet, stamped_line(&sourced, ts_ms)) {
                    eprintln!("[sven:harness] {line}");
                }
            }) as ProgressFn)
        };
        let activity: Option<ActivityFn> = (!args.quiet).then(|| {
            Arc::new(|event: &sven_sdk::SessionEvent| {
                if let Some(line) = activity_line(event) {
                    eprintln!("[sven:harness] {line}");
                }
            }) as ActivityFn
        });
        let item_checkpoint: Option<CheckpointFn> = args.checkpoint_out.clone().map(|file| {
            Arc::new(move |checkpoint: &HarnessCheckpoint| {
                if let Err(e) = write_atomically(&file, checkpoint) {
                    eprintln!(
                        "[sven:harness] warning: cannot keep the progress checkpoint {}: {e}",
                        file.display()
                    );
                }
            }) as CheckpointFn
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
            progress_checkpoint: args.checkpoint_out.clone(),
            quiet: args.quiet,
            run: EngineRun {
                input: args::input(args)?,
                deadline_ms: args::deadline_ms(args)?,
                max_output_tokens: args.max_output_tokens,
                record: args.record.is_some(),
                trajectory: args.trajectory.is_some() || args.conversations.is_some(),
                scratchpad: args::scratchpad(args)?,
                progress,
                activity,
                suspend: Some(suspend_on_signal()),
                item_checkpoint,
            },
        })
    }

    /// Whether the run reported its progress itself, as it went.
    pub(super) fn streams_progress(&self) -> bool {
        !self.quiet
    }

    /// The engine of the run: the real model, the coding toolset confined to
    /// the workspace, the approval mode and the host ceiling.
    async fn engine(self) -> Result<(Engine, EngineRun, Option<PathBuf>), Failure> {
        let mut config = Config::default();
        let model = &mut config.model;
        model.provider = self.provider;
        model.name = self.model;
        model.base_url = Some(self.endpoint);
        model.api_key = self.api_key;
        config.agent.first_byte_timeout_secs = self.first_byte_timeout_secs;
        // Probed: the server's own context window is learned up front, so a
        // conversation is summarised before it outgrows the window rather
        // than trimmed when the server refuses it.
        let client = sven_sdk::drivers::from_config_probed(model)
            .await
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
        Ok((engine, self.run, self.progress_checkpoint))
    }
}

/// Starts the harness at `path` under `assembly`.
pub(super) async fn run(path: &Path, assembly: Assembly) -> Result<RunResult, Failure> {
    let (engine, request, progress_checkpoint) = assembly.engine().await?;
    let result = run_on_engine(&engine, path, request).await.map_err(kit)?;
    Ok(settled(result, progress_checkpoint))
}

/// Resumes `checkpoint` of the harness at `path` under `assembly`.
pub(super) async fn resume(
    path: &Path,
    checkpoint: HarnessCheckpoint,
    assembly: Assembly,
) -> Result<RunResult, Failure> {
    let (engine, request, progress_checkpoint) = assembly.engine().await?;
    let result = resume_on_engine(&engine, path, checkpoint, request)
        .await
        .map_err(kit)?;
    Ok(settled(result, progress_checkpoint))
}

/// A run that ended with a verdict has no use for the checkpoint kept while
/// it went: left behind, it would offer a resume of a finished run. A
/// suspended run's final checkpoint is written by the caller.
fn settled(result: RunResult, progress_checkpoint: Option<PathBuf>) -> RunResult {
    if let (None, Some(file)) = (&result.checkpoint, progress_checkpoint) {
        let _ = std::fs::remove_file(file);
    }
    result
}

/// Writes `checkpoint` to `file` so that a reader, or a crash, sees the
/// previous checkpoint or this one, never half of it.
fn write_atomically(file: &Path, checkpoint: &HarnessCheckpoint) -> std::io::Result<()> {
    let mut staging = file.as_os_str().to_owned();
    staging.push(".partial");
    let staging = PathBuf::from(staging);
    std::fs::write(&staging, serde_json::to_vec_pretty(checkpoint)?)?;
    std::fs::rename(&staging, file)
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
