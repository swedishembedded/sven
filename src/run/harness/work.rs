// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven harness work`: the worker side of a served harness run
//! ([`sven_control::work`]).
//!
//! The worker fetches its run's descriptor, lays the package's skills and
//! scripts and the run's files out, and runs the harness (resuming the
//! run's checkpoint when it has one). At every publishing task's boundary it
//! uploads the task's files and the run's checkpoint and commits them
//! together; a heartbeat tells it whether to go on, pause at the next
//! boundary, or stop. Every request carries the generation it was started
//! for: once the server answers `409` another worker owns the run, and this
//! one stops without committing or finishing anything more.
//!
//! Two levels of trust share the process. The worker is trusted: it holds the
//! run's token and model key and runs the plan, the model calls, the gates,
//! the checkpoints and the run protocol, unconfined. The agent's commands are
//! not: the `shell` tool starts each of them under a Linux Landlock domain
//! ([`agent_confinement`]) that reaches only the workspace and a private tmp
//! for writing, the package's `bin`, the `PATH`'s directories and the system
//! set for running programs, and no network. The login file, the package's
//! gate scripts and data, the scratchpad and the worker's scratch are outside
//! it, and the worker is non-dumpable so a command cannot read its memory.
//! A kernel that cannot enforce all of it refuses the run before it starts.
//!
//! Swedish Embedded AB implements solutions for running agent workflows on
//! disposable workers without losing work for its clients. If your team
//! needs expertise in durable agent execution then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::work_package;
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sven_control::work::{
    Commit, CommittedFile, Declared, Desired, FileRef, Finish, Heartbeat, HeartbeatAnswer,
    PackageFormat, RunDescriptor, Transfer, UploadsGranted, UploadsRequest, WorkLogin,
};
use sven_model::ModelProvider;
use sven_sdk::{CancelToken, Engine, HarnessCheckpoint, Publication, RunOptions, Toolset};
use sven_tool_api::{OpenMode, PathScope};
use sven_tools_exec::confine::CommandConfinement;

/// How a worker's run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkEnd {
    /// The harness reached its verdict, and the server has it.
    Finished,
    /// Paused at a boundary; the server has the checkpoint.
    Paused,
    /// The server cancelled the run.
    Cancelled,
    /// Another worker owns the run now; nothing more was committed.
    Superseded,
}

/// What a worker runs with.
pub(crate) struct WorkOptions {
    pub login: WorkLogin,
    /// Where the run's files are laid out and its tools work.
    pub workspace: PathBuf,
    /// The run's private directory: the package's scripts, the scratchpad.
    pub scratch: PathBuf,
    pub heartbeat: Duration,
    /// Every model call goes here instead of the run's endpoint (tests).
    pub provider: Option<Arc<dyn ModelProvider>>,
}

/// The server answered 409: this worker no longer owns the run.
#[derive(Debug)]
struct Superseded;

impl std::fmt::Display for Superseded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("another worker owns this run now")
    }
}

impl std::error::Error for Superseded {}

#[derive(Clone)]
struct Client {
    http: reqwest::Client,
    login: Arc<WorkLogin>,
}

impl Client {
    fn url(&self, endpoint: &str) -> String {
        format!("{}/{endpoint}", self.login.run_url.trim_end_matches('/'))
    }

    async fn answer<T: DeserializeOwned>(response: reqwest::Response) -> Result<T> {
        match response.status().as_u16() {
            409 => Err(anyhow!(Superseded)),
            200..=299 => Ok(response.json().await?),
            status => bail!("the server answered {status}"),
        }
    }

    async fn descriptor(&self) -> Result<RunDescriptor> {
        let response = self
            .http
            .get(self.url("descriptor"))
            .bearer_auth(&self.login.token)
            .send()
            .await?;
        Self::answer(response).await
    }

    async fn post<B: Serialize, T: DeserializeOwned>(&self, endpoint: &str, body: &B) -> Result<T> {
        let response = self
            .http
            .post(self.url(endpoint))
            .bearer_auth(&self.login.token)
            .json(body)
            .send()
            .await?;
        Self::answer(response).await
    }

    async fn fetch(&self, file: &FileRef) -> Result<Vec<u8>> {
        let bytes = self.transfer(&file.download, None).await?;
        if digest(&bytes) != file.sha256.trim_end_matches('=') {
            bail!("{} did not match its digest", file.name);
        }
        Ok(bytes)
    }

    async fn transfer(&self, transfer: &Transfer, body: Option<Vec<u8>>) -> Result<Vec<u8>> {
        let method = reqwest::Method::from_bytes(transfer.method.as_bytes())?;
        let mut request = self.http.request(method, &transfer.url);
        for (name, value) in &transfer.headers {
            if !name.eq_ignore_ascii_case("content-length") {
                request = request.header(name, value);
            }
        }
        if let Some(body) = body {
            request = request.body(body);
        }
        let response = request.send().await?;
        if !response.status().is_success() {
            bail!("the store answered {}", response.status());
        }
        Ok(response.bytes().await?.to_vec())
    }
}

fn digest(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(bytes))
}

/// `path` under `root`, refused when it climbs out.
fn inside(root: &Path, path: &str) -> Result<PathBuf> {
    if path.is_empty() || path.starts_with('/') || path.split('/').any(|part| part == "..") {
        bail!("{path} is not a path inside the workspace");
    }
    Ok(root.join(path))
}

/// Uploads a boundary's files (read from the workspace) and `checkpoint`,
/// and commits them together.
async fn commit(
    client: &Client,
    workspace: &Path,
    task: Option<String>,
    paths: &[String],
    checkpoint: &HarnessCheckpoint,
) -> Result<()> {
    let generation = client.login.generation;
    // The workspace is written by the agent's confined processes: each file
    // is opened beneath it inside the kernel, so a link swapped in cannot make
    // this unconfined worker upload a file from outside it.
    let scope = PathScope::confined(workspace)?.race_free()?;
    let mut contents = Vec::new();
    for path in paths {
        inside(workspace, path)?;
        let mut bytes = Vec::new();
        scope
            .open(path, OpenMode::Read)
            .map_err(anyhow::Error::from)
            .and_then(|mut file| Ok(std::io::Read::read_to_end(&mut file, &mut bytes)?))
            .with_context(|| format!("reading {path}"))?;
        contents.push(bytes);
    }
    let checkpoint_bytes = serde_json::to_vec(checkpoint)?;
    let declare = |path: &str, bytes: &[u8], mime: &str| Declared {
        path: path.to_string(),
        size: bytes.len() as u64,
        sha256: digest(bytes),
        mime_type: mime.to_string(),
    };
    let request = UploadsRequest {
        generation,
        files: paths
            .iter()
            .zip(&contents)
            .map(|(path, bytes)| declare(path, bytes, "application/octet-stream"))
            .collect(),
        checkpoint: declare("checkpoint.json", &checkpoint_bytes, "application/json"),
    };
    let granted: UploadsGranted = client.post("uploads", &request).await?;
    if granted.files.len() != paths.len() {
        bail!(
            "the server granted {} uploads for {} files",
            granted.files.len(),
            paths.len()
        );
    }
    for (grant, bytes) in granted.files.iter().zip(contents) {
        client.transfer(&grant.upload, Some(bytes)).await?;
    }
    client
        .transfer(&granted.checkpoint.upload, Some(checkpoint_bytes))
        .await?;
    let _: Value = client
        .post(
            "commit",
            &Commit {
                generation,
                task,
                files: granted
                    .files
                    .iter()
                    .map(|grant| CommittedFile {
                        path: grant.path.clone(),
                        uri: grant.uri.clone(),
                    })
                    .collect(),
                checkpoint_uri: granted.checkpoint.uri,
            },
        )
        .await?;
    Ok(())
}

/// What a laid-out package gives the run.
struct LaidOut {
    /// The text the engine loads the harness from.
    harness_text: String,
    /// The package's `bin`, which goes first on the `PATH` of the commands
    /// the run's tools start; none for a bundle or a tar without one.
    bin: Option<PathBuf>,
}

/// Lays the run's package out.
///
/// A bundle is written to `package_dir`. A tar is unpacked there read-only,
/// and its main workflow is read from its place in the unpacked tree, so the
/// workflows it imports are found beside it. In both, the skills are copied
/// to where the agent finds skills in `workspace`.
fn lay_out_package(
    descriptor: &RunDescriptor,
    package: &[u8],
    package_dir: &Path,
    workspace: &Path,
) -> Result<LaidOut> {
    let skills = match descriptor.package_format {
        PackageFormat::Bundle => {
            if descriptor.harness.is_some()
                || descriptor.skills.is_some()
                || descriptor.bin.is_some()
            {
                bail!("harness, skills and bin name paths of a tar package, not of a bundle");
            }
            let bundle: sven_harness_spec::HarnessBundle =
                serde_json::from_slice(package).context("the package")?;
            sven_harness_spec::resources::materialize(&bundle.resources, package_dir)?;
            let text = serde_json::to_string(&bundle)?;
            install_skills(&package_dir.join("skills"), workspace)?;
            return Ok(LaidOut {
                harness_text: text,
                bin: None,
            });
        }
        PackageFormat::Tar => descriptor.skills.as_deref(),
    };
    let main = descriptor
        .harness
        .as_deref()
        .ok_or_else(|| anyhow!("a tar package names its main workflow in `harness`"))?;
    work_package::unpack_tar(package, package_dir).context("the package")?;
    let main = work_package::package_path(package_dir, main).context("harness")?;
    if let Some(skills) = skills {
        let skills = work_package::package_path(package_dir, skills).context("skills")?;
        install_skills(&skills, workspace)?;
    }
    let bin = descriptor
        .bin
        .as_deref()
        .map(|bin| work_package::package_path(package_dir, bin).context("bin"))
        .transpose()?;
    Ok(LaidOut {
        harness_text: super::source::Source::open(&main)?.text,
        bin,
    })
}

/// Copies the skills in `from`, when it is a directory, to where the agent
/// finds skills in `workspace`.
fn install_skills(from: &Path, workspace: &Path) -> Result<()> {
    if from.is_dir() {
        work_package::copy_tree(from, &workspace.join(".agents").join("skills"))?;
    }
    Ok(())
}

/// What the agent's commands may reach (see the module docs): the workspace
/// and a private tmp in `scratch` for writing; the package's `bin` and the
/// toolchain directories of this process's `PATH` for running programs, and
/// the system set. The run's login, `scratch` itself (the package, the
/// scratchpad, the gate scripts) and everything else are out of reach.
fn agent_confinement(
    workspace: &Path,
    scratch: &Path,
    bin: Option<&Path>,
) -> Result<CommandConfinement> {
    let tmpdir = scratch.join("agent-tmp");
    std::fs::create_dir_all(&tmpdir).with_context(|| tmpdir.display().to_string())?;
    Ok(CommandConfinement {
        writable: vec![workspace.to_path_buf(), tmpdir.clone()],
        read_exec: bin
            .map(Path::to_path_buf)
            .into_iter()
            .chain(sven_tools_exec::search_path_dirs())
            .collect(),
        tmpdir: Some(tmpdir),
    })
}

/// Runs the run the login names to its end.
pub(crate) async fn work(options: WorkOptions) -> Result<WorkEnd> {
    // Two levels of trust in this process: it is the trusted worker, and the
    // commands of the agent's tools are confined by the kernel. Neither may
    // be skipped: a kernel that cannot confine fully refuses the run, and
    // this process's memory (the run's token and model key) is kept from the
    // commands it starts.
    CommandConfinement::require_kernel_support().map_err(|e| anyhow!(e))?;
    sven_remote::sandbox::keep_memory_private()?;
    let client = Client {
        http: sven_http::client(),
        login: Arc::new(options.login),
    };
    let descriptor = match client.descriptor().await {
        Err(e) if e.is::<Superseded>() => return Ok(WorkEnd::Superseded),
        other => other?,
    };
    if descriptor.generation != client.login.generation {
        return Ok(WorkEnd::Superseded);
    }

    // The package: its skills where the agent finds skills, its scripts and
    // data in the run's private directory, which a gate names through
    // `package_dir`.
    let package_dir = options.scratch.join("package");
    let LaidOut { harness_text, bin } = lay_out_package(
        &descriptor,
        &client.fetch(&descriptor.package).await?,
        &package_dir,
        &options.workspace,
    )?;
    for placed in &descriptor.files {
        let target = inside(&options.workspace, &placed.path)?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, client.fetch(&placed.file).await?)?;
    }

    let model = |name: &str| -> Result<Arc<dyn ModelProvider>> {
        if let Some(provider) = &options.provider {
            return Ok(Arc::clone(provider));
        }
        let config = sven_model_drivers::ModelConfig {
            provider: "openai".into(),
            name: name.to_string(),
            base_url: Some(descriptor.models.openai_base_url.clone()),
            api_key: Some(client.login.token.clone()),
            ..sven_model_drivers::ModelConfig::default()
        };
        Ok(Arc::from(sven_model_drivers::from_config(&config)?))
    };
    // The package's tools are found by the run's commands through the tools'
    // own configuration; this process's environment is not touched.
    let mut config = sven_sdk::config::Config::default();
    config.tools.confine = Some(agent_confinement(
        &options.workspace,
        &options.scratch,
        bin.as_deref(),
    )?);
    config.tools.path_prefix = bin.into_iter().collect();
    let mut builder = Engine::builder()
        .config(config)
        .model_provider(model(&descriptor.models.default)?)
        .toolset(Toolset::coding());
    for (role, name) in &descriptor.models.roles {
        builder = builder.model_role(role.clone(), model(name)?);
    }
    // Video, when the server lets the run make it: generated with the run's
    // token, in this trusted process, never by the confined commands.
    if let Some(base_url) = &descriptor.models.media_base_url {
        let endpoint = sven_tools_media::MediaEndpoint {
            base_url: base_url.clone(),
            api_key: client.login.token.clone(),
        };
        builder = builder
            .tool(Arc::new(sven_tools_media::ModelsTool::new(
                endpoint.clone(),
            )))
            .tool(Arc::new(sven_tools_media::RunModelTool::new(
                endpoint,
                sven_tools_media::MediaConfig::default(),
                options.workspace.clone(),
            )));
    }
    let engine = builder.build()?;
    let harness = engine.load_harness_from_str(&harness_text)?;

    let mut inputs = descriptor.inputs.clone();
    let declares_package_dir = harness
        .plan()
        .inputs
        .as_ref()
        .is_some_and(|i| i.contains_key("package_dir"));
    if declares_package_dir {
        inputs["package_dir"] = json!(package_dir.display().to_string());
    }
    if descriptor.package_format == PackageFormat::Tar {
        work_package::resolve_inputs(&mut inputs, &package_dir)?;
    }
    let run = match &descriptor.checkpoint {
        Some(file) => harness.resume(serde_json::from_slice(&client.fetch(file).await?)?)?,
        None => harness.start(inputs)?,
    };

    let superseded = Arc::new(AtomicBool::new(false));
    let suspend = CancelToken::new();
    let cancel = CancelToken::new();
    let cancelled = Arc::new(AtomicBool::new(false));

    // Publications are committed in order, off the run's own task.
    let (publications, mut queue) = tokio::sync::mpsc::unbounded_channel::<Publication>();
    let committer = {
        let (client, workspace, superseded, cancel) = (
            client.clone(),
            options.workspace.clone(),
            Arc::clone(&superseded),
            cancel.clone(),
        );
        tokio::spawn(async move {
            while let Some(publication) = queue.recv().await {
                if superseded.load(Ordering::SeqCst) {
                    continue;
                }
                let paths: Vec<String> = publication.files.iter().map(|f| f.path.clone()).collect();
                if let Err(e) = commit(
                    &client,
                    &workspace,
                    Some(publication.task),
                    &paths,
                    &publication.checkpoint,
                )
                .await
                {
                    if e.is::<Superseded>() {
                        superseded.store(true, Ordering::SeqCst);
                        cancel.cancel();
                    } else {
                        tracing::warn!("a publication was not committed: {e:#}");
                    }
                }
            }
        })
    };
    let heartbeat = {
        let (client, superseded, suspend, cancel, cancelled) = (
            client.clone(),
            Arc::clone(&superseded),
            suspend.clone(),
            cancel.clone(),
            Arc::clone(&cancelled),
        );
        let every = options.heartbeat;
        tokio::spawn(async move {
            loop {
                let answer: Result<HeartbeatAnswer> = client
                    .post(
                        "heartbeat",
                        &Heartbeat {
                            generation: client.login.generation,
                        },
                    )
                    .await;
                match answer {
                    Ok(HeartbeatAnswer {
                        desired: Desired::Running,
                    }) => {}
                    Ok(HeartbeatAnswer {
                        desired: Desired::Paused,
                    }) => suspend.cancel(),
                    Ok(HeartbeatAnswer {
                        desired: Desired::Cancelled,
                    }) => {
                        cancelled.store(true, Ordering::SeqCst);
                        cancel.cancel();
                    }
                    Err(e) if e.is::<Superseded>() => {
                        superseded.store(true, Ordering::SeqCst);
                        cancel.cancel();
                        return;
                    }
                    Err(e) => tracing::warn!("heartbeat: {e:#}"),
                }
                tokio::time::sleep(every).await;
            }
        })
    };

    let sink = publications.clone();
    let mut run = run
        .in_workspace(&options.workspace)?
        .with_scratchpad(options.scratch.join("scratchpad"))
        .on_publish(move |publication| {
            let _ = sink.send(publication);
        });
    let outcome = run
        .execute(RunOptions::new().suspend_on(suspend).cancel(cancel))
        .await;
    heartbeat.abort();
    // The run holds the other end of the publication queue: once it is gone
    // nothing more can be published, and the committer finishes the queue.
    let paused_at = run.checkpoint();
    drop(run);
    drop(publications);
    let _ = committer.await;

    if superseded.load(Ordering::SeqCst) {
        return Ok(WorkEnd::Superseded);
    }
    if cancelled.load(Ordering::SeqCst) {
        return Ok(WorkEnd::Cancelled);
    }
    let outcome = outcome?;
    match outcome.result {
        None => {
            let checkpoint = paused_at.ok_or_else(|| anyhow!("a paused run left no checkpoint"))?;
            match commit(&client, &options.workspace, None, &[], &checkpoint).await {
                Err(e) if e.is::<Superseded>() => Ok(WorkEnd::Superseded),
                Err(e) => Err(e),
                Ok(()) => Ok(WorkEnd::Paused),
            }
        }
        Some(result) => {
            let verdict = match result {
                Ok(value) => json!({ "ok": true, "value": value }),
                Err(failure) => {
                    json!({ "ok": false, "code": failure.code, "message": failure.message })
                }
            };
            let finished: Result<Value> = client
                .post(
                    "finish",
                    &Finish {
                        generation: client.login.generation,
                        outcome: verdict,
                    },
                )
                .await;
            match finished {
                Err(e) if e.is::<Superseded>() => Ok(WorkEnd::Superseded),
                Err(e) => Err(e),
                Ok(_) => Ok(WorkEnd::Finished),
            }
        }
    }
}

// A worker confines its agent's commands with Landlock and refuses to run
// without it, so it runs, and is tested, on Linux alone.
#[cfg(all(test, target_os = "linux"))]
mod tests {
    //! Spec: against a server, a worker commits each publication with its
    //! checkpoint and reports its verdict; when the server answers 409 (a
    //! newer generation owns the run) it stops and reports nothing more.

    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use sven_model::ResponseEvent;
    use sven_model_mock::CallbackProvider;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    const DOC: &str = r#"
api_version: "2"
name: part
inputs: { drawing: file }
execution: { limits: { model_turns: 4 } }
workflow:
  - name: model
    task: "Model {{ inputs.drawing }}."
    publish: [drawing.png]
"#;

    /// A package directory as a server distributes it: the main workflow, a
    /// skill, a tool its gate runs, and a data directory an input names.
    const TAR_DOC: &str = r#"
api_version: "2"
name: part
inputs: { profile: string }
execution: { limits: { model_turns: 4 } }
gates:
  render: { command: [probe-tool, "{{ inputs.profile }}"] }
workflow:
  - name: model
    task: "Model it."
    verify: [render]
    publish: [out.txt]
"#;

    /// The tool: fails unless the skill is where the agent finds skills,
    /// and publishes the profile it was pointed at.
    const PROBE_TOOL: &str =
        "#!/bin/sh\nset -e\ntest -f .agents/skills/probe/SKILL.md\ncp \"$1/printer.cfg\" out.txt\n";

    fn tar_package() -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, mode, content) in [
            ("package/harness.yaml", 0o644, TAR_DOC),
            ("package/skills/probe/SKILL.md", 0o644, "# probe\n"),
            ("package/bin/probe-tool", 0o755, PROBE_TOOL),
            ("package/data/printer/printer.cfg", 0o644, "nozzle=0.4\n"),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(mode);
            builder
                .append_data(&mut header, path, content.as_bytes())
                .unwrap();
        }
        builder.into_inner().unwrap()
    }

    /// The content of the run's login file in the tests.
    const SECRET: &str = "bearer-secret-for-the-run";

    #[derive(Default)]
    struct Server {
        tar: bool,
        /// The descriptor names a video API.
        media: bool,
        stale_commit: bool,
        calls: Vec<String>,
        store: BTreeMap<String, Vec<u8>>,
        commits: Vec<Value>,
        finished: Option<Value>,
        /// What the agent's shell answered when it tried to read the login.
        agent_saw: Vec<String>,
        /// The tools the agent's first request offered.
        offered: Vec<String>,
    }

    async fn serve(state: Arc<Mutex<Server>>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let tar = state.lock().unwrap().tar;
        let package = if tar {
            tar_package()
        } else {
            serde_json::to_vec(&sven_harness_spec::HarnessBundle {
                document: DOC.to_string(),
                ..Default::default()
            })
            .unwrap()
        };
        state
            .lock()
            .unwrap()
            .store
            .insert("package".into(), package);
        state
            .lock()
            .unwrap()
            .store
            .insert("drawing".into(), b"png".to_vec());
        let base_url = base.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let (state, base) = (Arc::clone(&state), base_url.clone());
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 8192];
                    let end = loop {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i + 4;
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..end]).to_string();
                    let length: usize = head
                        .to_lowercase()
                        .lines()
                        .filter_map(|l| l.split_once(':'))
                        .find(|(k, _)| k.trim() == "content-length")
                        .and_then(|(_, v)| v.trim().parse().ok())
                        .unwrap_or(0);
                    while buf.len() < end + length {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let body = buf[end..].to_vec();
                    let line = head.lines().next().unwrap_or_default().to_string();
                    let mut parts = line.split(' ');
                    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                    let (status, reply) = {
                        let mut state = state.lock().unwrap();
                        state.calls.push(format!("{method} {path}"));
                        let file = |key: &str, bytes: &[u8]| {
                            json!({
                                "uri": format!("mcp-file://files/{key}"), "name": key, "mimeType": "application/octet-stream",
                                "size": bytes.len(), "sha256": digest(bytes),
                                "download": { "method": "GET", "url": format!("{base}/store/{key}") },
                            })
                        };
                        let (status, reply): (u16, Vec<u8>) = match (method, path) {
                            ("GET", "/run/descriptor") => {
                                let mut descriptor = json!({
                                    "protocol_version": 1, "run_id": "6f1c1d1e-6c1a-4d43-9a43-6f2a9d3b2a10", "generation": 2,
                                    "package": file("package", &state.store["package"]),
                                    "inputs": { "drawing": "drawing.png" },
                                    "files": [{ "path": "drawing.png", "file": file("drawing", &state.store["drawing"]) }],
                                    "models": { "openai_base_url": format!("{base}/v1"), "default": "m" },
                                });
                                if state.media {
                                    descriptor["models"]["media_base_url"] =
                                        json!(format!("{base}/v1"));
                                }
                                if state.tar {
                                    descriptor["package_format"] = json!("tar");
                                    descriptor["harness"] = json!("package/harness.yaml");
                                    descriptor["skills"] = json!("package/skills");
                                    descriptor["bin"] = json!("package/bin");
                                    descriptor["inputs"] =
                                        json!({ "profile": { "package": "package/data/printer" } });
                                    descriptor["files"] = json!([]);
                                }
                                (200, descriptor.to_string().into_bytes())
                            }
                            ("POST", "/run/uploads") => {
                                let request: Value = serde_json::from_slice(&body).unwrap();
                                let grant = |path: &str, key: &str| {
                                    json!({ "path": path, "uri": format!("mcp-file://files/{key}"),
                                "upload": { "method": "PUT", "url": format!("{base}/store/{key}") } })
                                };
                                let files: Vec<Value> = request["files"]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .enumerate()
                                    .map(|(i, f)| {
                                        grant(f["path"].as_str().unwrap(), &format!("out{i}"))
                                    })
                                    .collect();
                                (200, json!({ "files": files, "checkpoint": grant("checkpoint.json", "checkpoint") }).to_string().into_bytes())
                            }
                            ("POST", "/run/commit") if state.stale_commit => (409, b"{}".to_vec()),
                            ("POST", "/run/commit") => {
                                state.commits.push(serde_json::from_slice(&body).unwrap());
                                (200, b"{}".to_vec())
                            }
                            ("POST", "/run/heartbeat") => {
                                (200, br#"{"desired":"running"}"#.to_vec())
                            }
                            ("POST", "/run/finish") => {
                                state.finished = Some(serde_json::from_slice(&body).unwrap());
                                (200, b"{}".to_vec())
                            }
                            ("PUT", p) if p.starts_with("/store/") => {
                                state
                                    .store
                                    .insert(p.trim_start_matches("/store/").to_string(), body);
                                (200, Vec::new())
                            }
                            ("GET", p) if p.starts_with("/store/") => (
                                200,
                                state
                                    .store
                                    .get(p.trim_start_matches("/store/"))
                                    .cloned()
                                    .unwrap_or_default(),
                            ),
                            _ => (404, Vec::new()),
                        };
                        (status, reply)
                    };
                    let mut response = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        reply.len()
                    )
                    .into_bytes();
                    response.extend_from_slice(&reply);
                    let _ = socket.write_all(&response).await;
                });
            }
        });
        base
    }

    async fn worker(state: &Arc<Mutex<Server>>) -> WorkEnd {
        let base = serve(Arc::clone(state)).await;
        let workspace = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        // The agent's first act is to try to read the run's login (a file
        // the worker was started with); its shell's answer is recorded.
        let login_file = scratch.path().join("login.json");
        std::fs::write(&login_file, SECRET).unwrap();
        let recorded = Arc::clone(state);
        let provider: Arc<dyn ModelProvider> = Arc::new(CallbackProvider::new(move |request| {
            let answers: Vec<String> = request
                .messages
                .iter()
                .filter_map(|m| match &m.content {
                    sven_model::MessageContent::ToolResult { content, .. } => {
                        Some(format!("{content:?}"))
                    }
                    _ => None,
                })
                .collect();
            if answers.is_empty() {
                recorded.lock().unwrap().offered =
                    request.tools.iter().map(|t| t.name.clone()).collect();
                return vec![
                    ResponseEvent::ToolCall {
                        index: 0,
                        id: "peek".into(),
                        name: "shell".into(),
                        arguments:
                            json!({ "shell_command": format!("cat {}", login_file.display()) })
                                .to_string(),
                    },
                    ResponseEvent::Done,
                ];
            }
            recorded.lock().unwrap().agent_saw = answers;
            vec![
                ResponseEvent::TextDelta("modelled".into()),
                ResponseEvent::Done,
            ]
        }));
        let end = tokio::time::timeout(
            Duration::from_secs(60),
            work(WorkOptions {
                login: WorkLogin {
                    run_url: format!("{base}/run"),
                    token: "t".into(),
                    generation: 2,
                },
                workspace: workspace.path().to_path_buf(),
                scratch: scratch.path().to_path_buf(),
                heartbeat: Duration::from_millis(50),
                provider: Some(provider),
            }),
        )
        .await
        .expect("the worker ends")
        .expect("no error");
        // A tar package is read-only; make it removable with its directory.
        super::super::work_package::restore_write(scratch.path());
        end
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_worker_commits_each_publication_and_reports_its_verdict() {
        let state = Arc::new(Mutex::new(Server::default()));
        assert_eq!(worker(&state).await, WorkEnd::Finished);
        let state = state.lock().unwrap();
        assert_eq!(state.commits.len(), 1, "{:?}", state.calls);
        assert_eq!(state.commits[0]["task"], "model");
        assert_eq!(state.commits[0]["files"][0]["path"], "drawing.png");
        assert_eq!(
            state.store["out0"], b"png",
            "the published file was uploaded"
        );
        let checkpoint: HarnessCheckpoint =
            serde_json::from_slice(&state.store["checkpoint"]).unwrap();
        assert!(
            !checkpoint.plan_digest.is_empty(),
            "the checkpoint went with it"
        );
        assert_eq!(state.finished.as_ref().unwrap()["outcome"]["ok"], true);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_worker_offers_video_only_when_the_server_names_a_video_api() {
        let video = ["models", "run_model"];
        let state = Arc::new(Mutex::new(Server::default()));
        assert_eq!(worker(&state).await, WorkEnd::Finished);
        let offered = state.lock().unwrap().offered.clone();
        assert!(offered.iter().any(|t| t == "shell"), "{offered:?}");
        assert!(
            !offered.iter().any(|t| video.contains(&t.as_str())),
            "{offered:?}"
        );

        let state = Arc::new(Mutex::new(Server {
            media: true,
            ..Server::default()
        }));
        assert_eq!(worker(&state).await, WorkEnd::Finished);
        let offered = state.lock().unwrap().offered.clone();
        for tool in video {
            assert!(offered.iter().any(|t| t == tool), "{tool} in {offered:?}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_worker_runs_a_tar_package_with_its_skill_tool_and_data() {
        let state = Arc::new(Mutex::new(Server {
            tar: true,
            ..Server::default()
        }));
        // The PATH a program started from here inherits.
        let process_path = || {
            std::process::Command::new("sh")
                .args(["-c", "printf %s \"$PATH\""])
                .output()
                .unwrap()
                .stdout
        };
        let path_before = process_path();
        assert_eq!(worker(&state).await, WorkEnd::Finished);
        assert_eq!(
            process_path(),
            path_before,
            "the package's bin reaches the run's tools without changing this process's PATH"
        );
        let state = state.lock().unwrap();
        assert_eq!(state.finished.as_ref().unwrap()["outcome"]["ok"], true);
        let saw = state.agent_saw.join("\n");
        assert!(
            saw.contains("Permission denied") && !saw.contains(SECRET),
            "the agent's shell cannot read the run's login: {saw}"
        );
        assert_eq!(state.commits.len(), 1, "{:?}", state.calls);
        assert_eq!(state.commits[0]["task"], "model");
        assert_eq!(state.commits[0]["files"][0]["path"], "out.txt");
        assert_eq!(
            state.store["out0"], b"nozzle=0.4\n",
            "the gate ran the package's tool from its bin, on its data, after its skill was laid out"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_worker_whose_generation_is_stale_stops_and_reports_nothing_more() {
        let state = Arc::new(Mutex::new(Server {
            stale_commit: true,
            ..Server::default()
        }));
        assert_eq!(worker(&state).await, WorkEnd::Superseded);
        let state = state.lock().unwrap();
        assert!(state.commits.is_empty());
        assert!(
            state.finished.is_none(),
            "a superseded worker never reports a verdict: {:?}",
            state.calls
        );
    }
}
