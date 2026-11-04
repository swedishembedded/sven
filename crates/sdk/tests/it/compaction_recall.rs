// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: an agent that reads more than its context window holds still has
//! what it read when it answers.
//!
//! Six files are read one tool call at a time through a window that holds
//! about three of them. Every compaction is a summary written by the model
//! from a fork of the conversation (never an emergency trim), every request
//! is a valid conversation, and the final answer holds all six codewords.
//!
//! Swedish Embedded AB implements agents that keep working inside a small
//! model context window for its clients. If your team needs expertise in
//! running agents on local models you can procure our services by sending an
//! email to info@swedishembedded.com.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use sven_model::{CompletionRequest, MessageContent, ModelProvider, ResponseEvent, ResponseStream};
use sven_sdk::config::Config;
use sven_sdk::tool::{ApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput};
use sven_sdk::Engine;

const FILES: usize = 6;

struct ReadFile;

#[async_trait::async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }
    fn description(&self) -> &str {
        "Reads file n."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {"n": {"type": "integer"}}})
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn modes(&self) -> &[sven_vocab::AgentMode] {
        &[sven_vocab::AgentMode::Agent]
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let n = call.args["n"].as_u64().unwrap_or(0);
        let filler = "lorem ipsum dolor sit amet ".repeat(110);
        ToolOutput::ok(
            &call.id,
            format!("CODEWORD for file {n}: KEY-{n}{n}7\n{filler}"),
        )
    }
}

/// Reads the files in order, writes notes when asked for them, and answers
/// from whatever the conversation it is shown still holds.
struct Reader {
    window: u32,
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

fn codewords(req: &CompletionRequest) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for m in &req.messages {
        let body = match &m.content {
            MessageContent::Text(t) => t.clone(),
            MessageContent::ToolResult { content, .. } => format!("{content:?}"),
            _ => continue,
        };
        for part in body.split("CODEWORD for file ").skip(1) {
            let line: String = part
                .chars()
                .take_while(|c| *c != '\n' && *c != '\\')
                .collect();
            if !found.contains(&line) {
                found.push(line);
            }
        }
    }
    found.sort();
    found
}

#[async_trait::async_trait]
impl ModelProvider for Reader {
    fn name(&self) -> &str {
        "reader"
    }
    fn model_name(&self) -> &str {
        "reader"
    }
    fn catalog_context_window(&self) -> Option<u32> {
        Some(self.window)
    }
    fn catalog_max_output_tokens(&self) -> Option<u32> {
        Some(self.window)
    }
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        // A valid conversation: every tool result answers an earlier call.
        let mut open = HashSet::new();
        for m in &req.messages {
            match &m.content {
                MessageContent::ToolCall { tool_call_id, .. } => {
                    open.insert(tool_call_id.clone());
                }
                MessageContent::ToolResult { tool_call_id, .. } => {
                    anyhow::ensure!(open.remove(tool_call_id), "orphan result {tool_call_id}");
                }
                _ => {}
            }
        }
        anyhow::ensure!(open.is_empty(), "calls without results: {open:?}");

        self.requests.lock().unwrap().push(req.clone());
        let is_fork = req
            .messages
            .last()
            .and_then(|m| m.as_text())
            .is_some_and(|t| t == sven_turn::COMPACTION_PROMPT);
        let seen = codewords(&req);
        let events = if is_fork {
            vec![
                ResponseEvent::TextDelta(format!(
                    "NOTES. Codewords read so far:\nCODEWORD for file {}",
                    seen.join("\nCODEWORD for file ")
                )),
                ResponseEvent::Done,
            ]
        } else {
            let done = seen.len();
            if done < FILES {
                vec![
                    ResponseEvent::ToolCall {
                        index: 0,
                        id: format!("call-{done}"),
                        name: "read_file".into(),
                        arguments: format!("{{\"n\":{done}}}"),
                    },
                    ResponseEvent::Done,
                ]
            } else {
                vec![
                    ResponseEvent::TextDelta(seen.join("\n")),
                    ResponseEvent::Done,
                ]
            }
        };
        Ok(Box::pin(futures::stream::iter(
            events.into_iter().map(Ok::<_, anyhow::Error>),
        )))
    }
}

#[tokio::test]
async fn six_files_through_a_window_that_holds_three_are_all_recalled_by_summary() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let engine = Engine::builder()
        .config(Config::default())
        .model_provider(Arc::new(Reader {
            window: 6000,
            requests: Arc::clone(&requests),
        }))
        .tool(Arc::new(ReadFile))
        .build()
        .expect("an engine builds");
    let mut agent = engine.agent("agent");
    let outcome = agent
        .send("Read files 0..5 in order, then list the codewords.")
        .await
        .expect("an outcome");

    for n in 0..FILES {
        assert!(
            outcome.reply.contains(&format!("{n}: KEY-{n}{n}7")),
            "codeword {n} missing from the answer: {}",
            outcome.reply
        );
    }
    let forks = requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| {
            r.messages
                .last()
                .and_then(|m| m.as_text())
                .is_some_and(|t| t == sven_turn::COMPACTION_PROMPT)
        })
        .count();
    assert!(forks >= 1, "the window forced no compaction");
    let json = serde_json::to_value(agent.trajectory()).unwrap();
    let strategies: Vec<String> = json["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["extra"]["context_management"]["type"] == "compaction")
        .map(|s| s["extra"]["sven"]["strategy"].to_string())
        .collect();
    assert!(!strategies.is_empty());
    assert!(
        strategies.iter().all(|s| s.contains("fork")),
        "every compaction is a fork summary: {strategies:?}"
    );
}
