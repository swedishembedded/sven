// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use std::collections::HashMap;

use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

#[derive(Default)]
pub struct UpdateMemoryTool {
    /// Path override for the memory file (falls back to ~/.config/sven/memory.json)
    pub memory_file: Option<String>,
}

#[async_trait]
impl Tool for UpdateMemoryTool {
    fn name(&self) -> &str { "update_memory" }

    fn description(&self) -> &str {
        "Persist key-value pairs across sessions. Operations: set (upsert), get (retrieve), \
         delete (remove), list (all keys). Memory is stored in ~/.config/sven/memory.json."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["set", "get", "delete", "list"],
                    "description": "Memory operation to perform"
                },
                "key": {
                    "type": "string",
                    "description": "Memory key (required for set/get/delete)"
                },
                "value": {
                    "type": "string",
                    "description": "Value to store (required for set)"
                }
            },
            "required": ["operation"]
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Auto }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let op = match call.args.get("operation").and_then(|v| v.as_str()) {
            Some(o) => o.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'operation'"),
        };

        debug!(op = %op, "update_memory tool");

        let path = self.memory_path();

        match op.as_str() {
            "set" => {
                let key = match call.args.get("key").and_then(|v| v.as_str()) {
                    Some(k) => k.to_string(),
                    None => return ToolOutput::err(&call.id, "missing 'key' for set"),
                };
                let value = match call.args.get("value").and_then(|v| v.as_str()) {
                    Some(v) => v.to_string(),
                    None => return ToolOutput::err(&call.id, "missing 'value' for set"),
                };
                let mut store = load_store(&path).await;
                store.insert(key.clone(), value);
                match save_store(&path, &store).await {
                    Ok(_) => ToolOutput::ok(&call.id, format!("set {key}")),
                    Err(e) => ToolOutput::err(&call.id, format!("save error: {e}")),
                }
            }
            "get" => {
                let key = match call.args.get("key").and_then(|v| v.as_str()) {
                    Some(k) => k.to_string(),
                    None => return ToolOutput::err(&call.id, "missing 'key' for get"),
                };
                let store = load_store(&path).await;
                match store.get(&key) {
                    Some(v) => ToolOutput::ok(&call.id, v.clone()),
                    None => ToolOutput::err(&call.id, format!("key not found: {key}")),
                }
            }
            "delete" => {
                let key = match call.args.get("key").and_then(|v| v.as_str()) {
                    Some(k) => k.to_string(),
                    None => return ToolOutput::err(&call.id, "missing 'key' for delete"),
                };
                let mut store = load_store(&path).await;
                if store.remove(&key).is_none() {
                    return ToolOutput::err(&call.id, format!("key not found: {key}"));
                }
                match save_store(&path, &store).await {
                    Ok(_) => ToolOutput::ok(&call.id, format!("deleted {key}")),
                    Err(e) => ToolOutput::err(&call.id, format!("save error: {e}")),
                }
            }
            "list" => {
                let store = load_store(&path).await;
                if store.is_empty() {
                    ToolOutput::ok(&call.id, "(no keys stored)")
                } else {
                    let mut keys: Vec<&str> = store.keys().map(String::as_str).collect();
                    keys.sort();
                    ToolOutput::ok(&call.id, keys.join("\n"))
                }
            }
            other => ToolOutput::err(&call.id, format!("unknown operation: {other}")),
        }
    }
}

impl UpdateMemoryTool {
    fn memory_path(&self) -> String {
        if let Some(path) = &self.memory_file {
            return path.clone();
        }
        let home = dirs::home_dir()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|| "/tmp".to_string());
        format!("{home}/.config/sven/memory.json")
    }
}

async fn load_store(path: &str) -> HashMap<String, String> {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

async fn save_store(path: &str, store: &HashMap<String, String>) -> anyhow::Result<()> {
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }
    let json = serde_json::to_string_pretty(store)?;
    tokio::fs::write(path, json).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn tmp_memory_tool() -> UpdateMemoryTool {
        use std::sync::atomic::{AtomicU32, Ordering};
        static CTR: AtomicU32 = AtomicU32::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        UpdateMemoryTool {
            memory_file: Some(format!("/tmp/sven_memory_test_{}_{n}.json", std::process::id())),
        }
    }

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall { id: "m1".into(), name: "update_memory".into(), args }
    }

    #[tokio::test]
    async fn set_and_get_value() {
        let t = tmp_memory_tool();
        let path = t.memory_file.clone().unwrap();

        t.execute(&call(json!({"operation": "set", "key": "name", "value": "sven"}))).await;
        let out = t.execute(&call(json!({"operation": "get", "key": "name"}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(out.content, "sven");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn delete_key() {
        let t = tmp_memory_tool();
        let path = t.memory_file.clone().unwrap();

        t.execute(&call(json!({"operation": "set", "key": "x", "value": "1"}))).await;
        t.execute(&call(json!({"operation": "delete", "key": "x"}))).await;
        let out = t.execute(&call(json!({"operation": "get", "key": "x"}))).await;
        assert!(out.is_error);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn list_returns_keys() {
        let t = tmp_memory_tool();
        let path = t.memory_file.clone().unwrap();

        t.execute(&call(json!({"operation": "set", "key": "a", "value": "1"}))).await;
        t.execute(&call(json!({"operation": "set", "key": "b", "value": "2"}))).await;
        let out = t.execute(&call(json!({"operation": "list"}))).await;
        assert!(out.content.contains("a"));
        assert!(out.content.contains("b"));

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn missing_operation_is_error() {
        let t = tmp_memory_tool();
        let out = t.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing 'operation'"));
    }
}
