// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The snapshot tool: read a value out of a JSON file, with the file's digest.
//!
//! This is the file half of a harness `for_each`: a collection an agent wrote
//! to the workspace is read once, when the loop begins, and the harness freezes
//! what this tool returned. No agent mode lists it ([`Tool::modes`] is empty)
//! and the harness runtime never admits a model's call to it. The digest is
//! that of the file's bytes, so a run's record says exactly which file content
//! its items came from.
//!
//! Swedish Embedded AB implements verifiable, resumable agent workflows for
//! its clients. If your team needs expertise in iterating agent work over a
//! discovered backlog without losing or repeating items then you can procure
//! our services by sending an email to info@swedishembedded.com.

use async_trait::async_trait;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sven_harness_spec::SNAPSHOT_TOOL;
use sven_hsm::ToolCapability;
use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{Tool, ToolCall, ToolOutput};
use sven_tool_api::PathScope;
use sven_vocab::AgentMode;

/// The largest file the tool reads. A backlog of thousands of items is a few
/// hundred kilobytes; anything near this is not a backlog.
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Reads a JSON value from a workspace file.
pub struct ReadJsonSnapshotTool {
    scope: PathScope,
}

impl ReadJsonSnapshotTool {
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
}

#[async_trait]
impl Tool for ReadJsonSnapshotTool {
    fn name(&self) -> &str {
        SNAPSHOT_TOOL
    }

    fn summary(&self) -> &str {
        "Read a value from a workspace JSON file, with the file's digest (harness only)."
    }

    fn description(&self) -> &str {
        "Reads a JSON file inside the workspace and answers {digest, value}: the SHA-256 of the \
         file's bytes and the value at an RFC 6901 pointer. Called by a harness loop step, \
         never by a model: no agent mode lists it."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Workspace-relative JSON file" },
                "pointer": { "type": "string", "description": "RFC 6901 pointer; empty is the whole document" }
            },
            "required": ["path", "pointer"],
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
        let (Some(path), Some(pointer)) = (
            call.args.get("path").and_then(Value::as_str),
            call.args.get("pointer").and_then(Value::as_str),
        ) else {
            return ToolOutput::err(&call.id, "`path` and `pointer` are required strings");
        };
        let resolved = match self.scope.resolve_for(call, path) {
            Ok(resolved) => resolved,
            Err(refused) => return refused,
        };
        let size = match tokio::fs::metadata(&resolved).await {
            Ok(meta) if meta.is_file() => meta.len(),
            Ok(_) => return ToolOutput::err(&call.id, format!("{path} is not a file")),
            Err(e) => return ToolOutput::err(&call.id, format!("{path} cannot be read: {e}")),
        };
        if size > MAX_BYTES {
            return ToolOutput::err(
                &call.id,
                format!("{path} is {size} bytes, more than the {MAX_BYTES} a snapshot may read"),
            );
        }
        let bytes = match tokio::fs::read(&resolved).await {
            Ok(bytes) => bytes,
            Err(e) => return ToolOutput::err(&call.id, format!("{path} cannot be read: {e}")),
        };
        let document: Value = match serde_json::from_slice(&bytes) {
            Ok(document) => document,
            Err(e) => return ToolOutput::err(&call.id, format!("{path} is not valid JSON: {e}")),
        };
        let Some(value) = document.pointer(pointer) else {
            return ToolOutput::err(
                &call.id,
                format!("{path} has nothing at pointer `{pointer}`"),
            );
        };
        ToolOutput::ok(
            &call.id,
            json!({ "digest": format!("{:x}", Sha256::digest(&bytes)), "value": value })
                .to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn read(dir: &tempfile::TempDir, args: Value) -> (bool, String) {
        let tool = ReadJsonSnapshotTool::new(dir.path()).expect("tool");
        let out = tool
            .execute(&ToolCall {
                id: "s".into(),
                name: SNAPSHOT_TOOL.into(),
                args,
            })
            .await;
        (out.is_error, out.content)
    }

    #[tokio::test]
    async fn it_answers_the_value_at_the_pointer_and_the_digest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let text = r#"{"items":[{"module":"a"},{"module":"b"}],"other":1}"#;
        std::fs::write(dir.path().join("backlog.json"), text).unwrap();
        let (is_error, content) =
            read(&dir, json!({ "path": "backlog.json", "pointer": "/items" })).await;
        assert!(!is_error, "{content}");
        let answer: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            answer["value"],
            json!([{ "module": "a" }, { "module": "b" }])
        );
        assert_eq!(
            answer["digest"],
            format!("{:x}", Sha256::digest(text.as_bytes()))
        );
    }

    #[tokio::test]
    async fn the_empty_pointer_is_the_whole_document() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.json"), "[1,2]").unwrap();
        let (_, content) = read(&dir, json!({ "path": "a.json", "pointer": "" })).await;
        assert_eq!(
            serde_json::from_str::<Value>(&content).unwrap()["value"],
            json!([1, 2])
        );
    }

    #[tokio::test]
    async fn every_way_the_read_can_go_wrong_is_a_tool_error_that_says_which() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bad.json"), "{not json").unwrap();
        std::fs::write(dir.path().join("ok.json"), "{}").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let cases = [
            (
                json!({ "path": "missing.json", "pointer": "" }),
                "cannot be read",
            ),
            (
                json!({ "path": "bad.json", "pointer": "" }),
                "not valid JSON",
            ),
            (
                json!({ "path": "ok.json", "pointer": "/nope" }),
                "nothing at pointer",
            ),
            (json!({ "path": "sub", "pointer": "" }), "not a file"),
            (json!({ "path": "../x.json", "pointer": "" }), "outside"),
            (json!({ "path": "ok.json" }), "required"),
        ];
        for (args, expect) in cases {
            let (is_error, content) = read(&dir, args.clone()).await;
            assert!(is_error, "{args}");
            assert!(content.contains(expect), "{args}: {content}");
        }
    }

    #[test]
    fn no_agent_mode_lists_it_and_it_only_reads() {
        let dir = tempfile::tempdir().unwrap();
        let tool = ReadJsonSnapshotTool::new(dir.path()).unwrap();
        assert!(tool.modes().is_empty());
        assert_eq!(tool.kernel_capability(), ToolCapability::ReadFile);
        assert_eq!(tool.name(), "read_json_snapshot");
    }
}
