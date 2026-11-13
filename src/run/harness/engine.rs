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
    resume_on_engine, run_on_engine, ActivityFn, CheckpointFn, ConversationsFn, EngineRun,
    ProgressFn, RunResult,
};
use super::live_files::{append_conversations, TraceAppender};
use super::progress::{Progress, Style};
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
    /// The model server the run's turns go to; none for a plan that takes no
    /// model turn, which has nothing to ask one.
    server: Option<ModelServer>,
    /// The models that answer the roles the workflow names, served by the
    /// same endpoint.
    roles: Vec<RoleModel>,
    approvals: ApprovalsArg,
    /// The video API the run's agents generate with, if any.
    media: Option<sven_tools_media::MediaEndpoint>,
    first_byte_timeout_secs: Option<u64>,
    capabilities: Vec<ToolCapability>,
    /// Where `--checkpoint-out` keeps the latest checkpoint while the run
    /// goes; removed when the run ends with a verdict.
    progress_checkpoint: Option<PathBuf>,
    quiet: bool,
    /// Where the progress lines go; also used for the ones printed after
    /// the run when it did not stream them.
    sink: Arc<Progress>,
    run: EngineRun,
}

/// The model a run's turns go to, as `--model` and `--model-endpoint` name it.
struct ModelServer {
    provider: String,
    model: String,
    endpoint: String,
    /// The model server's key. Held here only until the client is built; it
    /// is never part of the engine's configuration, so no trace, recording,
    /// checkpoint or audit record can carry it.
    api_key: Option<String>,
}

impl Assembly {
    /// Validates the flags of `run` and `resume`.
    pub(super) fn from_args(
        args: &HarnessRunArgs,
        plan: Arc<sven_sdk::harness_plan::HarnessPlan>,
        stamps: &Stamps,
        resumed: bool,
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
        let model = match args.model.as_deref() {
            Some(spec) => Some(model_flag(spec, args.model_endpoint.as_deref())?),
            None if plan.takes_model_turns() => {
                return Err(usage(
                    "--model <provider/name> and --model-endpoint <url> are required",
                ))
            }
            None => None,
        };
        let approvals = args.approvals.ok_or_else(|| {
            usage("--approvals auto|deny|ask is required: a run never inherits an approval mode")
        })?;
        // Read once, whatever the number of roles: the file may be one a
        // host can read only once (a FIFO), so nothing that runs later can.
        let keys = args
            .api_key_file
            .as_deref()
            .map(|file| ApiKeys::read("--api-key-file", file, &workspace))
            .transpose()?;
        let media = media_endpoint(args, &workspace)?;
        let server = model
            .map(|(provider, model, endpoint)| {
                let api_key = keys
                    .as_ref()
                    .map(|k| k.for_provider(&provider))
                    .transpose()?;
                Ok::<_, Failure>(ModelServer {
                    provider,
                    model,
                    endpoint,
                    api_key,
                })
            })
            .transpose()?;
        let roles = role_models(&args.model_roles, &plan.model_roles())?
            .into_iter()
            .map(|(role, provider, model)| {
                let api_key = keys
                    .as_ref()
                    .map(|k| k.for_provider(&provider))
                    .transpose()?;
                Ok(RoleModel {
                    role,
                    provider,
                    model,
                    api_key,
                })
            })
            .collect::<Result<_, Failure>>()?;
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
        // The trace file is appended to as the records arrive, so a run that
        // is killed keeps the records so far; the run's end rewrites it whole.
        let trace = args
            .trace
            .as_deref()
            .map(|file| TraceAppender::open(file, resumed))
            .transpose()
            .map_err(|e| usage(format!("cannot write the trace file: {e}")))?;
        let sink = Arc::new(progress_sink(args));
        let progress: Option<ProgressFn> = {
            let tracker = std::sync::Mutex::new(sven_sdk::SourceTracker::new(plan));
            let (stamps, quiet, sink) = (stamps.clone(), args.quiet, sink.clone());
            Some(Arc::new(move |record: &sven_sdk::TraceRecord| {
                let ts_ms = stamps.note();
                let sourced = tracker
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .next(record);
                if let Ok(line) = stamped_line(&sourced, ts_ms) {
                    if let Some(trace) = &trace {
                        trace.append(&line);
                    }
                    if !quiet {
                        sink.emit(&line);
                    }
                }
            }) as ProgressFn)
        };
        if args.conversations.as_deref().is_some_and(Path::is_dir) {
            return Err(usage(
                "--conversations is one JSON-lines file now, not a directory (older versions \
                 wrote a file per conversation there); name a file",
            ));
        }
        let conversations_file = args
            .conversations
            .as_deref()
            .map(|file| {
                TraceAppender::open(file, resumed).map(|f| (Arc::new(f), file.to_path_buf()))
            })
            .transpose()
            .map_err(|e| usage(format!("cannot write the conversations file: {e}")))?;
        let item_conversations: Option<ConversationsFn> = conversations_file.map(|(file, path)| {
            Arc::new(move |updates: &[sven_sdk::ConversationUpdate]| {
                append_conversations(&file, &path, updates);
            }) as ConversationsFn
        });
        let activity: Option<ActivityFn> = (!args.quiet).then(|| {
            let sink = sink.clone();
            Arc::new(move |event: &sven_sdk::SessionEvent| {
                if let Some(line) = activity_line(event) {
                    sink.emit(&line);
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
            server,
            roles,
            approvals,
            media,
            first_byte_timeout_secs: args.first_byte_timeout,
            capabilities,
            progress_checkpoint: args.checkpoint_out.clone(),
            quiet: args.quiet,
            sink,
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
                item_conversations,
            },
        })
    }

    /// The progress sink of the run.
    pub(super) fn sink(&self) -> Arc<Progress> {
        self.sink.clone()
    }

    /// Whether the run reported its progress itself, as it went.
    pub(super) fn streams_progress(&self) -> bool {
        !self.quiet
    }

    /// The engine of the run: the real model, the coding toolset confined to
    /// the workspace, the approval mode and the host ceiling.
    async fn engine(self, harness: &Path) -> Result<(Engine, EngineRun, Option<PathBuf>), Failure> {
        let mut config = Config::default();
        // The toolkits the package declares beside its workflow, started as
        // the person running it; the run does not start without them.
        let servers =
            super::package_mcp::local_servers(harness, &self.workspace.join(".sven").join("mcp"))
                .map_err(|e| usage(format!("{e:#}")))?;
        super::package_mcp::apply(&mut config, servers);
        config.agent.first_byte_timeout_secs = self.first_byte_timeout_secs;
        let mut builder = Engine::builder();
        if let Some(server) = self.server {
            let model = &mut config.model;
            model.provider = server.provider;
            model.name = server.model;
            model.base_url = Some(server.endpoint);
            model.api_key = server.api_key;
            // Probed: the server's own context window is learned up front, so
            // a conversation is summarised before it outgrows the window
            // rather than trimmed when the server refuses it.
            let client = sven_sdk::drivers::from_config_probed(model)
                .await
                .map_err(|e| usage(format!("cannot build the model client: {e:#}")))?;
            // The client holds the key; the engine's configuration does not.
            model.api_key = None;
            builder = builder.model_provider(Arc::from(client));
        } else {
            // No agent step exists to ask it; without one the run would
            // build a model from the default configuration, and fail for
            // want of its key.
            builder = builder.model_provider(Arc::new(super::no_model::NoModel));
        }
        for role in self.roles {
            let mut role_config = config.model.clone();
            role_config.provider = role.provider;
            role_config.name = role.model;
            role_config.api_key = role.api_key;
            let client = sven_sdk::drivers::from_config_probed(&role_config)
                .await
                .map_err(|e| {
                    usage(format!(
                        "cannot build the model client of role `{}`: {e:#}",
                        role.role
                    ))
                })?;
            builder = builder.model_role(role.role, Arc::from(client));
        }
        if let Some(endpoint) = self.media {
            builder = builder
                .tool(Arc::new(sven_tools_media::ModelsTool::new(
                    endpoint.clone(),
                )))
                .tool(Arc::new(sven_tools_media::RunModelTool::new(
                    endpoint,
                    sven_tools_media::MediaConfig::default(),
                    self.workspace.clone(),
                )));
        }
        let builder = builder
            .config(config)
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
    let (engine, request, progress_checkpoint) = assembly.engine(path).await?;
    let result = run_on_engine(&engine, path, request).await.map_err(kit)?;
    Ok(settled(result, progress_checkpoint))
}

/// Resumes `checkpoint` of the harness at `path` under `assembly`.
pub(super) async fn resume(
    path: &Path,
    checkpoint: HarnessCheckpoint,
    assembly: Assembly,
) -> Result<RunResult, Failure> {
    let (engine, request, progress_checkpoint) = assembly.engine(path).await?;
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

/// `--model PROVIDER/NAME` with the `--model-endpoint` it is served from, as
/// provider, name and endpoint.
fn model_flag(spec: &str, endpoint: Option<&str>) -> Result<(String, String, String), Failure> {
    let endpoint = endpoint
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| usage("--model-endpoint <url> of a server you already run is required"))?;
    let (provider, model) = spec
        .split_once('/')
        .filter(|(provider, model)| !provider.is_empty() && !model.is_empty())
        .ok_or_else(|| usage(format!("--model {spec} must be <provider>/<name>")))?;
    Ok((provider.into(), model.into(), endpoint.into()))
}

/// One `--model-role`: the model that answers `role`.
struct RoleModel {
    role: String,
    provider: String,
    model: String,
    /// Held only until the role's client is built, like [`Assembly`]'s key.
    api_key: Option<String>,
}

/// Parses the `--model-role ROLE=provider/name` flags into `(role, provider,
/// name)`, refusing a malformed or repeated one, a role the workflow names
/// that is left unmapped (its turn would fail mid-run) and a mapped role it
/// does not name.
fn role_models(
    flags: &[String],
    named: &std::collections::BTreeSet<&str>,
) -> Result<Vec<(String, String, String)>, Failure> {
    let mut mapped = std::collections::BTreeMap::new();
    for flag in flags {
        let parsed = flag.split_once('=').and_then(|(role, spec)| {
            let (provider, model) = spec.split_once('/')?;
            let filled = [role, provider, model]
                .iter()
                .all(|part| !part.trim().is_empty());
            filled.then(|| (role.trim(), (provider.trim(), model.trim())))
        });
        let (role, spec) = parsed.ok_or_else(|| {
            usage(format!(
                "--model-role {flag} must be ROLE=<provider>/<name>"
            ))
        })?;
        if mapped.insert(role, spec).is_some() {
            return Err(usage(format!("--model-role {role} is given twice")));
        }
    }
    let unmapped: Vec<&str> = named
        .iter()
        .copied()
        .filter(|r| !mapped.contains_key(r))
        .collect();
    if !unmapped.is_empty() {
        return Err(usage(format!(
            "the workflow names model role(s) {}: map each with --model-role ROLE=<provider>/<name>",
            unmapped.join(", ")
        )));
    }
    if let Some(role) = mapped.keys().find(|r| !named.contains(*r)) {
        return Err(usage(format!(
            "--model-role {role}: the workflow names no such role (it names: {})",
            named.iter().copied().collect::<Vec<_>>().join(", ")
        )));
    }
    Ok(mapped
        .into_iter()
        .map(|(role, (provider, model))| {
            (role.to_string(), provider.to_string(), model.to_string())
        })
        .collect())
}

fn capability_named(name: &str) -> Result<ToolCapability, Failure> {
    ToolCapability::ALL
        .into_iter()
        .find(|cap| format!("{cap:?}") == name)
        .ok_or_else(|| usage(format!("unknown capability `{name}`")))
}

/// `--media-endpoint` with the key `--media-key-file` holds (none without
/// one: a local server may need none).
fn media_endpoint(
    args: &HarnessRunArgs,
    workspace: &Path,
) -> Result<Option<sven_tools_media::MediaEndpoint>, Failure> {
    let Some(base_url) = args.media_endpoint.as_deref().map(str::trim) else {
        return Ok(None);
    };
    if base_url.is_empty() {
        return Err(usage("--media-endpoint needs the video API's base URL"));
    }
    let api_key = args
        .media_key_file
        .as_deref()
        .map(|file| ApiKeys::read("--media-key-file", file, workspace)?.bare())
        .transpose()?
        .unwrap_or_default();
    Ok(Some(sven_tools_media::MediaEndpoint {
        base_url: base_url.to_string(),
        api_key,
    }))
}

/// The contents of a key file (`--api-key-file`, `--media-key-file`), read
/// once: a JSON object of provider name to key, or the bare key.
struct ApiKeys {
    shown: String,
    text: String,
}

impl ApiKeys {
    /// Reads `file`, given as `flag`, refusing one the agent could read.
    fn read(flag: &str, file: &Path, workspace: &Path) -> Result<Self, Failure> {
        let shown = format!("{flag} {}", file.display());
        let inside = file
            .canonicalize()
            .is_ok_and(|real| real.starts_with(workspace));
        if inside {
            return Err(usage(format!(
                "{shown} is inside the workspace, where the agent could read it: keep it outside"
            )));
        }
        let text = std::fs::read_to_string(file)
            .map_err(|e| usage(format!("cannot read {shown}: {e}")))?;
        Ok(Self { shown, text })
    }

    /// The file's one key.
    fn bare(&self) -> Result<String, Failure> {
        let key = self.text.trim();
        if key.is_empty() {
            return Err(usage(format!("{} holds an empty key", self.shown)));
        }
        Ok(key.to_string())
    }

    /// The key for `provider`.
    fn for_provider(&self, provider: &str) -> Result<String, Failure> {
        let shown = &self.shown;
        let key = match serde_json::from_str::<Value>(&self.text) {
            Ok(Value::Object(keys)) => keys
                .get(provider)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| {
                    usage(format!(
                        "{shown} has no string key for provider `{provider}`"
                    ))
                })?,
            Ok(_) => {
                return Err(usage(format!(
                    "{shown} is JSON but not an object of provider to key"
                )))
            }
            Err(_) => self.text.trim().to_string(),
        };
        if key.is_empty() {
            return Err(usage(format!("{shown} holds an empty key")));
        }
        Ok(key)
    }
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

/// The progress form the flags and stderr ask for. Text is coloured only on
/// a terminal; `--progress text` into a file stays plain.
fn progress_sink(args: &HarnessRunArgs) -> Progress {
    use std::io::IsTerminal;

    let terminal = std::io::stderr().is_terminal();
    match (args.text_progress(terminal), terminal) {
        (false, _) => Progress::json(),
        (true, true) => Progress::text(Style::Colour),
        (true, false) => Progress::text(Style::Plain),
    }
}
