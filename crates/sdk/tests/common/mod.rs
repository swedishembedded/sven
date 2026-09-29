// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Test doubles shared by the SDK's spec tests.

use std::collections::HashSet;

use sven_model::{CompletionRequest, MessageContent, ModelProvider, ResponseStream};
use sven_model_mock::ScriptedMockProvider;

/// Refuses a request whose history has a tool call with no result, as the
/// hosted providers do; otherwise delegates to the script.
pub struct Strict(pub ScriptedMockProvider);

#[async_trait::async_trait]
impl ModelProvider for Strict {
    fn name(&self) -> &str {
        "strict"
    }
    fn model_name(&self) -> &str {
        "strict"
    }
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        let mut open: HashSet<String> = HashSet::new();
        for m in &req.messages {
            match &m.content {
                MessageContent::ToolCall { tool_call_id, .. } => {
                    open.insert(tool_call_id.clone());
                }
                MessageContent::ToolResult { tool_call_id, .. } => {
                    open.remove(tool_call_id);
                }
                _ => {}
            }
        }
        anyhow::ensure!(open.is_empty(), "tool calls without a result: {open:?}");
        self.0.complete(req).await
    }
}
