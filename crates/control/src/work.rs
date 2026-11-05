// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The HTTP binding of **served harness runs**: a server runs a harness
//! package for one of its users on a worker it starts, and the worker
//! (`sven harness work`) reports back to it, the way a `sven rc` host does
//! for remote control ([`crate::remote`]).
//!
//! The worker is given a [`WorkLogin`] in a file: the run's endpoint and a
//! token good for that run alone. Every request carries the token as a
//! bearer token and the run's `generation`; the server answers `409` to a
//! generation that is no longer the run's (another worker took it over, or
//! it was cancelled), and the worker stops at once.
//!
//! 1. `GET {run_url}/descriptor` answers the [`RunDescriptor`]: the package,
//!    the inputs, the files to lay out in the workspace before running (the
//!    input files, and on a resume the files published so far), the
//!    checkpoint to resume from, and the model endpoint. Each file comes with
//!    a download good for minutes, and is checked against its digest.
//!
//!    The package is one of two [`PackageFormat`]s. A `bundle` (the default)
//!    is a serialized harness bundle, its skills and scripts included. A
//!    `tar` is a plain tar archive of a package directory: the descriptor's
//!    `harness` names the main workflow inside it (its imports are read from
//!    beside it), `skills` the directory of skills and `bin` the directory of
//!    tools, both optional. The worker unpacks it into the run's private
//!    package directory (regular files and directories only; an absolute
//!    path, a `..` component, a link or a device refuses the whole archive;
//!    at most [`MAX_TAR_ENTRIES`] entries and [`MAX_TAR_BYTES`] bytes), and
//!    makes it read-only. `bin` goes first on the `PATH` of every program the
//!    run executes, tools and gates alike. A server cannot know where the
//!    package is unpacked, so in a tar package any input value that is
//!    exactly `{"package": "<path>"}` is replaced, before the run starts, by
//!    the absolute path of that entry of the archive (the run fails when the
//!    path leaves the package or names nothing). The `package_dir` input
//!    holds the package's root for both formats.
//! 2. At every publishing task's boundary the worker asks
//!    `POST {run_url}/uploads` ([`UploadsRequest`]) for where to put the
//!    task's files and the run's checkpoint, uploads them, and commits them
//!    together with `POST {run_url}/commit` ([`Commit`]). Nothing a worker
//!    uploaded counts until its commit is accepted, so a worker that dies in
//!    between leaves the run at its previous commit.
//! 3. `POST {run_url}/heartbeat` ([`Heartbeat`]) every few seconds answers
//!    what the server wants ([`Desired`]): go on, pause (stop at the next
//!    boundary, commit, exit) or cancel.
//! 4. `POST {run_url}/finish` ([`Finish`]) reports the run's verdict.
//!
//! Swedish Embedded AB implements solutions for running agent workflows on
//! disposable workers without losing work for its clients. If your team
//! needs expertise in durable agent execution then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// The protocol revision this build speaks.
pub const WORK_PROTOCOL_VERSION: u32 = 1;

/// The most entries a `tar` package may hold.
pub const MAX_TAR_ENTRIES: usize = 10_000;

/// The most bytes of file content a `tar` package may unpack to.
pub const MAX_TAR_BYTES: u64 = 256 * 1024 * 1024;

/// What a worker is started with, in a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkLogin {
    /// The run's endpoint; the requests below are relative to it.
    pub run_url: String,
    /// The bearer token, good for this run alone.
    pub token: String,
    /// The generation this worker was started for.
    pub generation: u64,
}

/// A file the server holds, and how to fetch it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRef {
    pub uri: String,
    pub name: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    pub size: u64,
    /// The SHA-256 of the file, base64url.
    pub sha256: String,
    /// A download good for minutes.
    pub download: Transfer,
}

/// One HTTP request moving a file's bytes: a presigned URL and the headers
/// to send exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transfer {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

/// A file to lay out in the workspace before the run starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceFile {
    /// Where it goes, relative to the workspace.
    pub path: String,
    pub file: FileRef,
}

/// The model endpoint the run calls, with the run's token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Models {
    /// An OpenAI-compatible API, ending in `/v1`.
    pub openai_base_url: String,
    /// The model of tasks that name no role.
    pub default: String,
    /// The model of each role the plan names.
    #[serde(default)]
    pub roles: BTreeMap<String, String>,
}

/// What the descriptor's `package` file is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageFormat {
    /// A serialized harness bundle (the default).
    #[default]
    Bundle,
    /// A tar archive of a package directory; see the module documentation.
    Tar,
}

/// Everything a worker needs to run its run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunDescriptor {
    pub protocol_version: u32,
    pub run_id: Uuid,
    pub generation: u64,
    /// The harness package, in [`Self::package_format`].
    pub package: FileRef,
    #[serde(default)]
    pub package_format: PackageFormat,
    /// `tar` only, required: the main workflow's path inside the archive.
    #[serde(default)]
    pub harness: Option<String>,
    /// `tar` only: the directory of skills inside the archive.
    #[serde(default)]
    pub skills: Option<String>,
    /// `tar` only: the directory of tools inside the archive, put first on
    /// the `PATH` of everything the run executes.
    #[serde(default)]
    pub bin: Option<String>,
    /// The run's inputs; a `file` input names a path of [`Self::files`], and
    /// in a `tar` package `{"package": "<path>"}` names an entry of it.
    pub inputs: Value,
    #[serde(default)]
    pub files: Vec<WorkspaceFile>,
    /// The checkpoint to resume from, when the run has one.
    #[serde(default)]
    pub checkpoint: Option<FileRef>,
    pub models: Models,
}

/// A file the worker is about to upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declared {
    pub path: String,
    pub size: u64,
    /// base64url.
    pub sha256: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
}

/// `POST {run_url}/uploads`: where to put a boundary's files and checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadsRequest {
    pub generation: u64,
    pub files: Vec<Declared>,
    pub checkpoint: Declared,
}

/// Where one declared file goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Granted {
    pub path: String,
    pub uri: String,
    pub upload: Transfer,
}

/// The answer to [`UploadsRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadsGranted {
    pub files: Vec<Granted>,
    pub checkpoint: Granted,
}

/// `POST {run_url}/commit`: a boundary's uploaded files and checkpoint,
/// counted together or not at all. A pause commits its checkpoint with no
/// task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    pub generation: u64,
    /// The publishing task, or none for a pause.
    #[serde(default)]
    pub task: Option<String>,
    /// Each published file: its workspace path and the URI it was uploaded to.
    #[serde(default)]
    pub files: Vec<CommittedFile>,
    pub checkpoint_uri: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommittedFile {
    pub path: String,
    pub uri: String,
}

/// `POST {run_url}/heartbeat`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub generation: u64,
}

/// What the server wants of a running worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Desired {
    Running,
    /// Stop at the next boundary, commit the checkpoint, exit.
    Paused,
    /// Stop now.
    Cancelled,
}

/// The answer to [`Heartbeat`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeartbeatAnswer {
    pub desired: Desired,
}

/// `POST {run_url}/finish`: the run's verdict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finish {
    pub generation: u64,
    /// `{ok: true, value}` or `{ok: false, code, message}`.
    pub outcome: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The JSON a server writes, as the worker parses it: spelled out so a
    /// server in another language is held to these literals.
    #[test]
    fn a_server_descriptor_parses() {
        let parsed: RunDescriptor = serde_json::from_value(serde_json::json!({
            "protocol_version": 1,
            "run_id": "6f1c1d1e-6c1a-4d43-9a43-6f2a9d3b2a10",
            "generation": 3,
            "package": { "uri": "mcp-file://files/p", "name": "cad.json", "mimeType": "application/json",
                         "size": 10, "sha256": "x", "download": { "method": "GET", "url": "https://s/p" } },
            "inputs": { "drawing": "front.png" },
            "files": [{ "path": "front.png", "file": { "uri": "mcp-file://files/d", "name": "front.png",
                        "mimeType": "image/png", "size": 4, "sha256": "y",
                        "download": { "method": "GET", "url": "https://s/d", "headers": {} } } }],
            "models": { "openai_base_url": "https://s/v1", "default": "m", "roles": { "vision": "v" } }
        }))
        .unwrap();
        assert_eq!(parsed.generation, 3);
        assert_eq!(parsed.package_format, PackageFormat::Bundle);
        assert!(parsed.harness.is_none() && parsed.skills.is_none() && parsed.bin.is_none());
        assert_eq!(parsed.files[0].path, "front.png");
        assert!(parsed.checkpoint.is_none());
        assert_eq!(
            serde_json::to_value(HeartbeatAnswer {
                desired: Desired::Paused
            })
            .unwrap(),
            serde_json::json!({ "desired": "paused" })
        );
    }

    #[test]
    fn a_tar_descriptor_names_its_workflow_skills_and_tools() {
        let parsed: RunDescriptor = serde_json::from_value(serde_json::json!({
            "protocol_version": 1,
            "run_id": "6f1c1d1e-6c1a-4d43-9a43-6f2a9d3b2a10",
            "generation": 1,
            "package": { "uri": "mcp-file://files/p", "name": "cad.tar", "mimeType": "application/x-tar",
                         "size": 10, "sha256": "x", "download": { "method": "GET", "url": "https://s/p" } },
            "package_format": "tar",
            "harness": "package/harness.yaml",
            "skills": "package/skills",
            "bin": "package/bin",
            "inputs": { "printer": { "package": "package/printers/a1" } },
            "models": { "openai_base_url": "https://s/v1", "default": "m" }
        }))
        .unwrap();
        assert_eq!(parsed.package_format, PackageFormat::Tar);
        assert_eq!(parsed.harness.as_deref(), Some("package/harness.yaml"));
        assert_eq!(parsed.skills.as_deref(), Some("package/skills"));
        assert_eq!(parsed.bin.as_deref(), Some("package/bin"));
    }
}
