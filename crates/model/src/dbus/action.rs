// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Generic capability-action calls against brain's `Brain1.Manager`.
//!
//! Swedish Embedded AB implements model-serving integration seams like this
//! one for teams running inference on their own hardware. If your team needs
//! expertise in wiring an agent to a local model server without a subprocess
//! per request, you can procure our services by sending an email to
//! info@swedishembedded.com.
//!
//! [`DbusProvider`](super::DbusProvider) is one action (`generate`) dressed as
//! a chat completion. brain's `Run` is more general than that: every model
//! advertises named actions taking typed params plus named binary blobs, and
//! returning scalars plus named binary blobs. Transcription, image generation
//! and depth estimation are all the same call with different names.
//!
//! This is the transport half of that: [`ActionClient::run`] issues one `Run`
//! against an already-running server. It deliberately knows nothing about
//! which action it is invoking.
//!
//! # Why not shell out
//!
//! The obvious alternative is `brain <arch> <action>` per request. That pays a
//! process spawn, a device handshake and a full checkpoint load every single
//! time -- for the ASR model this repo uses, 2.4 GiB from disk onto the GPU
//! before a single sample is looked at. A resident server loads once and every
//! later call is the inference alone. It also means the model is chosen by a
//! manifest id the server actually serves, rather than by a CLI spelling that
//! has to be kept in step with brain's argument parser.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use super::blob;
use super::proxy::ManagerProxy;
use super::BusKind;

/// One named binary input: the raw payload plus the metadata describing it.
///
/// brain does no format sniffing on the receiving side, so `meta` is what
/// tells it how to read the bytes (`{"media":"audio","sample_rate":16000}`).
/// Getting it wrong yields a decode failure inside the model, not a transport
/// error.
#[derive(Debug, Clone)]
pub struct ActionInput {
    pub bytes: Vec<u8>,
    pub meta: Value,
}

impl ActionInput {
    /// Mono `f32` little-endian PCM at `sample_rate`, the layout every audio
    /// action in brain expects on the wire.
    pub fn audio_pcm_f32(bytes: Vec<u8>, sample_rate: u32) -> ActionInput {
        ActionInput {
            bytes,
            meta: serde_json::json!({ "media": "audio", "sample_rate": sample_rate }),
        }
    }
}

/// What one action produced: its scalar outputs, plus any named blobs.
#[derive(Debug, Clone, Default)]
pub struct ActionOutcome {
    /// The `result` JSON object.
    pub outputs: Value,
    /// Named output blobs, already read off their fds.
    pub blobs: HashMap<String, Vec<u8>>,
}

impl ActionOutcome {
    /// A named text output, preferring the blob over the scalar.
    ///
    /// brain may return short text inline in `result` or as a blob on an fd,
    /// and which one it picks is a property of the action, not of the caller.
    pub fn text(&self, name: &str) -> Option<String> {
        if let Some(bytes) = self.blobs.get(name) {
            return Some(String::from_utf8_lossy(bytes).into_owned());
        }
        self.outputs
            .get(name)
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }
}

/// A client for one brain server's generic action surface.
#[derive(Debug, Clone)]
pub struct ActionClient {
    pub bus: BusKind,
    pub service: String,
    pub object_path: String,
    /// Transport tag passed as `Run`'s last argument.
    pub transport: String,
    /// A connection to use instead of dialling `bus`.
    ///
    /// The seam a test drives: a peer-to-peer connection over a `UnixStream`
    /// pair reaches a fake `Brain1.Manager` with no bus daemon and no
    /// well-known name involved, which is how the wire contract is pinned
    /// without a real server. Mirrors `DbusProvider::with_connection`.
    conn: Option<zbus::Connection>,
}

impl Default for ActionClient {
    fn default() -> Self {
        Self {
            bus: BusKind::Session,
            service: super::proxy::DEFAULT_SERVICE.to_string(),
            object_path: super::proxy::DEFAULT_PATH.to_string(),
            transport: "memfd".to_string(),
            conn: None,
        }
    }
}

impl ActionClient {
    /// A client for the bus named by `address`, or the session bus when `None`.
    pub fn new(address: Option<&str>) -> Self {
        Self {
            bus: address.map(BusKind::parse).unwrap_or(BusKind::Session),
            ..Self::default()
        }
    }

    /// A client that talks over an existing connection rather than dialling.
    pub fn with_connection(conn: zbus::Connection) -> Self {
        Self {
            conn: Some(conn),
            ..Self::default()
        }
    }

    /// The connection to use for one call.
    async fn connection(&self) -> Result<zbus::Connection> {
        match &self.conn {
            Some(conn) => Ok(conn.clone()),
            None => self.bus.connect().await,
        }
    }

    async fn proxy(&self) -> Result<ManagerProxy<'static>> {
        let conn = self.connection().await?;
        ManagerProxy::builder(&conn)
            .destination(self.service.clone())
            .context("invalid D-Bus service name")?
            .path(self.object_path.clone())
            .context("invalid D-Bus object path")?
            .build()
            .await
            .context("building the Brain1.Manager proxy")
    }

    /// Run one action and return everything it produced.
    ///
    /// Inputs travel as sealed `memfd`s: brain `fstat`s each received fd for
    /// its length, so an anonymous pipe would report zero and silently deliver
    /// an empty blob.
    pub async fn run(
        &self,
        model: &str,
        action: &str,
        params: &Value,
        inputs: &HashMap<String, ActionInput>,
    ) -> Result<ActionOutcome> {
        let proxy = self.proxy().await?;

        let mut in_fds = HashMap::new();
        let mut in_meta = serde_json::Map::new();
        for (name, input) in inputs {
            in_fds.insert(name.clone(), blob::memfd_with_bytes(name, &input.bytes)?);
            in_meta.insert(name.clone(), input.meta.clone());
        }

        let params_str = serde_json::to_string(params).context("serialising action params")?;
        let meta_str =
            serde_json::to_string(&Value::Object(in_meta)).context("serialising blob metadata")?;

        let (result_json, out_fds, _out_meta) = proxy
            .run(
                model,
                action,
                &params_str,
                in_fds,
                &meta_str,
                &self.transport,
            )
            .await
            .with_context(|| format!("Brain1.Manager.Run failed for {model}/{action}"))?;

        let outputs: Value = serde_json::from_str(&result_json)
            .with_context(|| format!("parsing Run result JSON: {result_json:.256}"))?;

        let mut blobs = HashMap::new();
        for (name, fd) in &out_fds {
            let text = blob::read_fd_to_string(fd)
                .with_context(|| format!("reading output blob {name:?}"))?;
            blobs.insert(name.clone(), text.into_bytes());
        }

        Ok(ActionOutcome { outputs, blobs })
    }

    /// Model ids the server currently serves.
    pub async fn list_models(&self) -> Result<Vec<String>> {
        let proxy = self.proxy().await?;
        proxy
            .list_models()
            .await
            .context("Brain1.Manager.ListModels failed")
    }

    /// Fail with a message naming what IS served, when `model` is not.
    ///
    /// A bare "no model" from the server does not say what the caller should
    /// have asked for, and the two ids are easy to confuse: the CLI dispatches
    /// on a bare architecture id while this surface serves manifest ids.
    pub async fn ensure_served(&self, model: &str) -> Result<()> {
        let served = self.list_models().await?;
        if served.iter().any(|m| m == model) {
            return Ok(());
        }
        bail!(
            "brain does not serve {model:?}; it currently serves: {}",
            if served.is_empty() {
                "nothing".to_string()
            } else {
                served.join(", ")
            }
        )
    }
}
