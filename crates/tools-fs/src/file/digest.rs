// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The digest tool: the content digest of groups of workspace files.
//!
//! A harness `for_each` with `covers` records, when an item completes, a
//! digest of the files the item covers, and compares it again when a run is
//! resumed: an item whose files changed is pending again. This tool computes
//! the digests. No agent mode lists it ([`Tool::modes`] is empty) and the
//! harness runtime never admits a model's call to it. A file that does not
//! exist digests as absent, so a deleted file changes the digest too.
//!
//! Swedish Embedded AB implements verifiable, resumable agent workflows for
//! its clients. If your team needs expertise in telling which finished work is
//! still valid after the code under it changed then you can procure our
//! services by sending an email to info@swedishembedded.com.

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use sven_harness_spec::{DIGEST_TOOL, MAX_COVERS};
use sven_hsm::ToolCapability;
use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{Tool, ToolCall, ToolOutput};
use sven_tool_api::PathScope;
use sven_vocab::AgentMode;

/// The largest file the tool digests.
const MAX_BYTES: u64 = 64 * 1024 * 1024;
/// The most groups one call may ask about.
const MAX_GROUPS: usize = 10_000;

/// Digests groups of workspace files.
pub struct DigestFilesTool {
    scope: PathScope,
}

impl DigestFilesTool {
    /// A tool confined to `root`.
    ///
    /// # Errors
    ///
    /// When `root` does not exist or is not a directory.
    pub fn new(root: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        Ok(Self {
            scope: PathScope::confined(root)?,
        })
    }

    /// One group's digest: each path with the SHA-256 of its bytes, or
    /// `absent` when there is no such file.
    async fn group(&self, call: &ToolCall, paths: &[Value]) -> Result<String, ToolOutput> {
        let mut hasher = Sha256::new();
        for path in paths {
            let Some(path) = path.as_str() else {
                return Err(ToolOutput::err(&call.id, "a path must be a string"));
            };
            let resolved = self.scope.resolve_for(call, path)?;
            let content = match tokio::fs::metadata(&resolved).await {
                Ok(meta) if meta.is_file() && meta.len() <= MAX_BYTES => {
                    match tokio::fs::read(&resolved).await {
                        Ok(bytes) => format!("{:x}", Sha256::digest(&bytes)),
                        Err(e) => return Err(ToolOutput::err(&call.id, format!("{path}: {e}"))),
                    }
                }
                Ok(meta) if meta.is_file() => {
                    return Err(ToolOutput::err(
                        &call.id,
                        format!("{path} is larger than the {MAX_BYTES} bytes a digest reads"),
                    ))
                }
                Ok(_) => "not-a-file".to_string(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => "absent".to_string(),
                Err(e) => return Err(ToolOutput::err(&call.id, format!("{path}: {e}"))),
            };
            hasher.update(path.as_bytes());
            hasher.update([0]);
            hasher.update(content.as_bytes());
            hasher.update(b"\n");
        }
        Ok(format!("{:x}", hasher.finalize()))
    }
}

#[async_trait]
impl Tool for DigestFilesTool {
    fn name(&self) -> &str {
        DIGEST_TOOL
    }

    fn summary(&self) -> &str {
        "Digest groups of workspace files (harness only)."
    }

    fn description(&self) -> &str {
        "Answers, for each named group of workspace-relative paths, a digest of the paths and the \
         bytes of the files; a missing file digests as absent. Called by a harness loop step, \
         never by a model: no agent mode lists it."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "groups": {
                    "type": "object",
                    "description": "Group name to the workspace-relative paths in it",
                    "additionalProperties": { "type": "array", "items": { "type": "string" } }
                }
            },
            "required": ["groups"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    /// Listed in no mode: the model is never offered this tool.
    fn modes(&self) -> &[AgentMode] {
        &[]
    }

    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let Some(groups) = call.args.get("groups").and_then(Value::as_object) else {
            return ToolOutput::err(&call.id, "`groups` is a required object");
        };
        if groups.len() > MAX_GROUPS {
            return ToolOutput::err(&call.id, format!("more than {MAX_GROUPS} groups"));
        }
        let mut answer = Map::new();
        for (name, paths) in groups {
            let Some(paths) = paths.as_array().filter(|p| p.len() <= MAX_COVERS) else {
                return ToolOutput::err(
                    &call.id,
                    format!("group `{name}` must be a list of at most {MAX_COVERS} paths"),
                );
            };
            match self.group(call, paths).await {
                Ok(digest) => {
                    answer.insert(name.clone(), Value::String(digest));
                }
                Err(refused) => return refused,
            }
        }
        ToolOutput::ok(&call.id, Value::Object(answer).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn digests(dir: &tempfile::TempDir, args: Value) -> (bool, Value) {
        let out = DigestFilesTool::new(dir.path())
            .expect("tool")
            .execute(&ToolCall {
                id: "d".into(),
                name: DIGEST_TOOL.into(),
                args,
            })
            .await;
        (
            out.is_error,
            serde_json::from_str(&out.content).unwrap_or(Value::String(out.content)),
        )
    }

    #[tokio::test]
    async fn a_digest_changes_with_the_content_the_names_and_the_presence_of_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.c"), "one").unwrap();
        let ask = json!({ "groups": { "m": ["a.c", "b.h"], "n": ["a.c"] } });
        let (is_error, first) = digests(&dir, ask.clone()).await;
        assert!(!is_error, "{first}");
        assert_ne!(first["m"], first["n"]);
        let (_, same) = digests(&dir, ask.clone()).await;
        assert_eq!(first, same, "a digest is stable");

        std::fs::write(dir.path().join("a.c"), "two").unwrap();
        let (_, changed) = digests(&dir, ask.clone()).await;
        assert_ne!(first["n"], changed["n"], "new content");

        std::fs::write(dir.path().join("b.h"), "x").unwrap();
        let (_, appeared) = digests(&dir, ask).await;
        assert_ne!(changed["m"], appeared["m"], "a file that was absent exists");
    }

    #[tokio::test]
    async fn a_path_outside_the_workspace_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (is_error, _) = digests(&dir, json!({ "groups": { "m": ["../etc/passwd"] } })).await;
        assert!(is_error);
        let (is_error, _) = digests(&dir, json!({ "groups": { "m": [3] } })).await;
        assert!(is_error);
    }
}
