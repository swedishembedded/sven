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
//! Swedish Embedded AB implements solutions for running agent workflows on
//! disposable workers without losing work for its clients. If your team
//! needs expertise in durable agent execution then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sven_control::work::{
    Commit, CommittedFile, Declared, Desired, FileRef, Finish, Heartbeat, HeartbeatAnswer,
    RunDescriptor, Transfer, UploadsGranted, UploadsRequest, WorkLogin,
};
use sven_model::ModelProvider;
use sven_sdk::{CancelToken, Engine, HarnessCheckpoint, Publication, RunOptions, Toolset};

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
    let mut contents = Vec::new();
    for path in paths {
        contents.push(
            std::fs::read(inside(workspace, path)?).with_context(|| format!("reading {path}"))?,
        );
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

/// Runs the run the login names to its end.
pub(crate) async fn work(options: WorkOptions) -> Result<WorkEnd> {
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

    // The package: its skills where the agent finds skills, its scripts in
    // the run's private directory, which a gate names through `package_dir`.
    let package: sven_harness_spec::HarnessBundle =
        serde_json::from_slice(&client.fetch(&descriptor.package).await?).context("the package")?;
    let package_dir = options.scratch.join("package");
    sven_harness_spec::resources::materialize(&package.resources, &package_dir)?;
    if package_dir.join("skills").is_dir() {
        let skills = options.workspace.join(".agents").join("skills");
        std::fs::create_dir_all(&skills)?;
        for entry in std::fs::read_dir(package_dir.join("skills"))? {
            let entry = entry?;
            std::fs::rename(entry.path(), skills.join(entry.file_name()))?;
        }
    }
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
    let mut builder = Engine::builder()
        .model_provider(model(&descriptor.models.default)?)
        .toolset(Toolset::coding());
    for (role, name) in &descriptor.models.roles {
        builder = builder.model_role(role.clone(), model(name)?);
    }
    let engine = builder.build()?;
    let harness = engine.load_harness_from_str(&serde_json::to_string(&package)?)?;

    let mut inputs = descriptor.inputs.clone();
    let declares_package_dir = harness
        .plan()
        .inputs
        .as_ref()
        .is_some_and(|i| i.contains_key("package_dir"));
    if declares_package_dir {
        inputs["package_dir"] = json!(package_dir.display().to_string());
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

#[cfg(test)]
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

    #[derive(Default)]
    struct Server {
        stale_commit: bool,
        calls: Vec<String>,
        store: BTreeMap<String, Vec<u8>>,
        commits: Vec<Value>,
        finished: Option<Value>,
    }

    async fn serve(state: Arc<Mutex<Server>>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let package = serde_json::to_vec(&sven_harness_spec::HarnessBundle {
            document: DOC.to_string(),
            ..Default::default()
        })
        .unwrap();
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
                        ("GET", "/run/descriptor") => (200, json!({
                            "protocol_version": 1, "run_id": "6f1c1d1e-6c1a-4d43-9a43-6f2a9d3b2a10", "generation": 2,
                            "package": file("package", &state.store["package"]),
                            "inputs": { "drawing": "drawing.png" },
                            "files": [{ "path": "drawing.png", "file": file("drawing", &state.store["drawing"]) }],
                            "models": { "openai_base_url": format!("{base}/v1"), "default": "m" },
                        }).to_string().into_bytes()),
                        ("POST", "/run/uploads") => {
                            let request: Value = serde_json::from_slice(&body).unwrap();
                            let grant = |path: &str, key: &str| json!({ "path": path, "uri": format!("mcp-file://files/{key}"),
                                "upload": { "method": "PUT", "url": format!("{base}/store/{key}") } });
                            let files: Vec<Value> = request["files"].as_array().unwrap().iter().enumerate()
                                .map(|(i, f)| grant(f["path"].as_str().unwrap(), &format!("out{i}"))).collect();
                            (200, json!({ "files": files, "checkpoint": grant("checkpoint.json", "checkpoint") }).to_string().into_bytes())
                        }
                        ("POST", "/run/commit") if state.stale_commit => (409, b"{}".to_vec()),
                        ("POST", "/run/commit") => {
                            state.commits.push(serde_json::from_slice(&body).unwrap());
                            (200, b"{}".to_vec())
                        }
                        ("POST", "/run/heartbeat") => (200, br#"{"desired":"running"}"#.to_vec()),
                        ("POST", "/run/finish") => {
                            state.finished = Some(serde_json::from_slice(&body).unwrap());
                            (200, b"{}".to_vec())
                        }
                        ("PUT", p) if p.starts_with("/store/") => {
                            state.store.insert(p.trim_start_matches("/store/").to_string(), body);
                            (200, Vec::new())
                        }
                        ("GET", p) if p.starts_with("/store/") => {
                            (200, state.store.get(p.trim_start_matches("/store/")).cloned().unwrap_or_default())
                        }
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
        let provider: Arc<dyn ModelProvider> = Arc::new(CallbackProvider::new(|_| {
            vec![
                ResponseEvent::TextDelta("modelled".into()),
                ResponseEvent::Done,
            ]
        }));
        tokio::time::timeout(
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
        .expect("no error")
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
