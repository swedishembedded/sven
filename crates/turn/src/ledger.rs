// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Attributes the input tokens of a model request to their sources.
//!
//! [`estimate_request`] is the pure function behind the token ledger: given
//! the [`CompletionRequest`] about to be sent, it returns a
//! [`RequestBreakdown`] saying how many (estimated) tokens went to the system
//! instructions, each tool schema, the user's turns, replayed assistant
//! turns, tool-call arguments, tool results and attached media. `stream_turn`
//! calls it (through `LedgerRecorder`) at the one place a request is
//! assembled, then reconciles it with the provider's report
//! ([`RequestBreakdown::reconcile`]).
//!
//! # Estimator
//!
//! No tokenizer for the active provider is available in the workspace, and
//! providers disagree with one another, so the estimator is deterministic and
//! provider-independent: text costs [`BYTES_PER_TOKEN`] bytes per token,
//! rounded up. Images and audio use the same approximations as
//! [`sven_model::Message::approx_tokens`]. The figures are for *attribution*;
//! the provider's reported prompt size remains authoritative and the residual
//! recorded by the reconciliation says how far off the estimate was.

use std::collections::{BTreeMap, HashMap, HashSet};

use sven_model::{
    audio_approx_tokens, image_approx_tokens, CompletionRequest, ContentPart, Message,
    MessageContent, Role, ToolContentPart, ToolResultContent, ToolSchema,
};
use sven_vocab::{RequestBreakdown, ToolActivity};

/// Bytes of UTF-8 text per token.
///
/// Four is the long-standing rule of thumb for English prose and source code
/// under BPE vocabularies, and it is the constant behind
/// [`Message::approx_tokens`] and the request-size gate in
/// `sven_model::budget`, so the ledger and the gate agree on what a token is.
/// Dense text (non-Latin scripts, minified code) costs more per byte; that
/// shows up as a positive residual, not as a hidden correction here.
pub const BYTES_PER_TOKEN: usize = 4;

/// Name recorded in [`RequestBreakdown::estimator`].
pub const ESTIMATOR: &str = "bytes_div_4";

/// Headings of system-prompt sections built from the runtime context rather
/// than from sven's fixed instructions. They match what `prompts` and the
/// workspace discovery code emit as `## <heading>` lines.
const RUNTIME_SECTIONS: [&str; 7] = [
    "Skills",
    "Subagents",
    "Knowledge Base",
    "Project Context",
    "Project Instructions",
    "Git Context",
    "CI Environment",
];

/// Section name under which the request's dynamic system suffix is counted.
const DYNAMIC_SUFFIX_SECTION: &str = "dynamic_suffix";

/// Tool name given to a tool result whose call is not in the history.
const UNKNOWN_TOOL: &str = "unknown";

/// Estimated tokens of `text`.
#[must_use]
pub fn text_tokens(text: &str) -> u32 {
    saturating_u32(text.len().div_ceil(BYTES_PER_TOKEN))
}

/// Estimated tokens of one tool's schema as it is sent: name, description and
/// parameter schema.
#[must_use]
pub fn schema_tokens(schema: &ToolSchema) -> u32 {
    text_tokens(&schema.name)
        .saturating_add(text_tokens(&schema.description))
        .saturating_add(text_tokens(&schema.parameters.to_string()))
}

fn saturating_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Attribute the input tokens of `request` to their sources.
///
/// Pure: the result depends only on `request`. `estimated_total` is the sum
/// of the attributed parts, and the tool counters describe the history in
/// `request.messages` (see [`ToolActivity`]). The reconciliation fields are
/// left empty for the caller to fill once the provider has reported.
#[must_use]
pub fn estimate_request(request: &CompletionRequest) -> RequestBreakdown {
    let mut b = RequestBreakdown {
        estimator: ESTIMATOR.to_string(),
        ..RequestBreakdown::default()
    };

    for schema in &request.tools {
        *b.tool_schemas.entry(schema.name.clone()).or_default() += schema_tokens(schema);
    }
    if let Some(suffix) = &request.system_dynamic_suffix {
        *b.system_runtime
            .entry(DYNAMIC_SUFFIX_SECTION.to_string())
            .or_default() += text_tokens(suffix);
    }

    let tool_names = tool_names_by_call_id(&request.messages);
    for message in &request.messages {
        add_message(&mut b, message, &tool_names);
    }
    b.tool_activity = tool_activity(&request.messages);

    b.estimated_total = b.attributed_total();
    b
}

fn tool_names_by_call_id(messages: &[Message]) -> HashMap<&str, &str> {
    messages
        .iter()
        .filter_map(|m| match &m.content {
            MessageContent::ToolCall {
                tool_call_id,
                function,
            } => Some((tool_call_id.as_str(), function.name.as_str())),
            _ => None,
        })
        .collect()
}

fn add_message(b: &mut RequestBreakdown, message: &Message, tool_names: &HashMap<&str, &str>) {
    match &message.content {
        MessageContent::Text(text) => add_text(b, &message.role, text),
        MessageContent::ContentParts(parts) => {
            for part in parts {
                match part {
                    ContentPart::Text { text } => add_text(b, &message.role, text),
                    ContentPart::Image { detail, .. } => {
                        b.media += saturating_u32(image_approx_tokens(detail.as_deref()));
                    }
                    ContentPart::Audio { audio_url, .. } => {
                        b.media += saturating_u32(audio_approx_tokens(audio_url));
                    }
                }
            }
        }
        MessageContent::ToolCall { function, .. } => {
            *b.tool_call_arguments
                .entry(function.name.clone())
                .or_default() += text_tokens(&function.name) + text_tokens(&function.arguments);
        }
        MessageContent::ToolResult {
            tool_call_id,
            content,
        } => {
            let tool = tool_names
                .get(tool_call_id.as_str())
                .copied()
                .unwrap_or(UNKNOWN_TOOL);
            let (text, media) = tool_result_tokens(content);
            *b.tool_results.entry(tool.to_string()).or_default() += text;
            b.media += media;
        }
    }
}

fn add_text(b: &mut RequestBreakdown, role: &Role, text: &str) {
    match role {
        Role::System => add_system_prompt(b, text),
        Role::User => b.user += text_tokens(text),
        Role::Assistant => b.assistant += text_tokens(text),
        // A tool message is a `ToolResult`, which has its own arm; plain text
        // under this role is a result whose call cannot be named.
        Role::Tool => {
            *b.tool_results.entry(UNKNOWN_TOOL.to_string()).or_default() += text_tokens(text);
        }
    }
}

/// Split system text into its `## ` sections and count each as stable or
/// runtime-context. Text before the first heading is stable, as is any
/// section not named in [`RUNTIME_SECTIONS`].
fn add_system_prompt(b: &mut RequestBreakdown, text: &str) {
    let mut section: Option<&str> = None;
    let mut start = 0;
    let mut offset = 0;
    let mut close = |b: &mut RequestBreakdown, section: Option<&str>, end: usize| {
        let tokens = text_tokens(&text[start..end]);
        match section {
            Some(name) if RUNTIME_SECTIONS.contains(&name) => {
                *b.system_runtime.entry(name.to_string()).or_default() += tokens;
            }
            _ => b.system_stable += tokens,
        }
        start = end;
    };
    for line in text.split_inclusive('\n') {
        if let Some(heading) = line.strip_prefix("## ") {
            close(b, section, offset);
            section = Some(heading.trim());
        }
        offset += line.len();
    }
    close(b, section, text.len());
}

/// Estimated (text, media) tokens of a tool result.
fn tool_result_tokens(content: &ToolResultContent) -> (u32, u32) {
    match content {
        ToolResultContent::Text(text) => (text_tokens(text), 0),
        ToolResultContent::Parts(parts) => {
            let mut text = 0;
            let mut media = 0;
            for part in parts {
                match part {
                    ToolContentPart::Text { text: t } => text += text_tokens(t),
                    ToolContentPart::Image { .. } => {
                        media += saturating_u32(image_approx_tokens(None));
                    }
                    ToolContentPart::Audio { audio_url } => {
                        media += saturating_u32(audio_approx_tokens(audio_url));
                    }
                }
            }
            (text, media)
        }
    }
}

fn tool_activity(messages: &[Message]) -> ToolActivity {
    let mut seen = HashSet::new();
    let mut activity = ToolActivity::default();
    for message in messages {
        if let MessageContent::ToolCall { function, .. } = &message.content {
            activity.calls += 1;
            if !seen.insert((function.name.as_str(), canonical_json(&function.arguments))) {
                activity.duplicate_calls += 1;
            }
        }
    }
    activity
}

/// Arguments in a form that is equal for equal JSON values: object keys
/// sorted at every depth. Text that is not JSON is compared as trimmed text.
fn canonical_json(arguments: &str) -> String {
    fn sorted(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => serde_json::Value::Object(
                map.into_iter()
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .map(|(k, v)| (k, sorted(v)))
                    .collect(),
            ),
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.into_iter().map(sorted).collect())
            }
            other => other,
        }
    }
    serde_json::from_str(arguments)
        .map_or_else(|_| arguments.trim().to_string(), |v| sorted(v).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompts::{system_prompt, PromptContext};
    use serde_json::json;
    use sven_model::FunctionCall;
    use sven_vocab::{AgentMode, ReportedUsage};

    fn request(messages: Vec<Message>, tools: Vec<ToolSchema>) -> CompletionRequest {
        CompletionRequest {
            messages,
            tools,
            stream: true,
            system_dynamic_suffix: None,
            cache_key: None,
            max_output_tokens_override: None,
            core_tool_count: 0,
            response_format: None,
        }
    }

    fn schema(name: &str, description: &str) -> ToolSchema {
        ToolSchema {
            name: name.into(),
            description: description.into(),
            parameters: json!({"type": "object", "properties": {}}),
            is_mcp: false,
        }
    }

    fn call(id: &str, name: &str, arguments: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: id.into(),
                function: FunctionCall {
                    name: name.into(),
                    arguments: arguments.into(),
                },
            },
        }
    }

    #[test]
    fn every_source_is_attributed_and_the_parts_sum_to_the_estimate() {
        let mut req = request(
            vec![
                Message::system(
                    "You are Sven.\n\n## Skills\nskill list here\n\n## Guidelines\nbe brief",
                ),
                Message::user("a".repeat(40)),
                call("c1", "read_file", r#"{"path":"a.rs"}"#),
                Message::tool_result("c1", "b".repeat(400)),
                Message::assistant("c".repeat(80)),
                Message::user_with_parts(vec![ContentPart::image("data:image/png;base64,AA==")]),
            ],
            vec![schema("read_file", "reads a file"), schema("shell", "runs")],
        );
        req.system_dynamic_suffix = Some("## Git Context\nbranch main".into());

        let b = estimate_request(&req);

        assert_eq!(b.user, 10);
        assert_eq!(b.assistant, 20);
        assert_eq!(b.tool_results["read_file"], 100);
        assert_eq!(b.media, 765);
        assert!(b.tool_call_arguments["read_file"] > 0);
        assert!(b.system_stable > 0);
        assert!(b.system_runtime["Skills"] > 0);
        assert!(b.system_runtime["dynamic_suffix"] > 0);
        assert_eq!(b.tool_schemas.len(), 2);
        assert_eq!(b.estimated_total, b.attributed_total());
        assert!(b.estimated_total > 895, "{}", b.estimated_total);
    }

    #[test]
    fn a_tool_schema_is_charged_to_its_tool_by_name() {
        let big = schema("big", &"x".repeat(4000));
        let small = schema("small", "x");
        let b = estimate_request(&request(vec![], vec![big.clone(), small.clone()]));
        assert_eq!(b.tool_schemas["big"], schema_tokens(&big));
        assert!(b.tool_schemas["big"] > 1000 && b.tool_schemas["small"] < 50);
        assert_eq!(b.estimated_total, b.tool_schemas.values().sum::<u32>());
    }

    #[test]
    fn tool_results_are_charged_to_the_tool_that_produced_them() {
        let b = estimate_request(&request(
            vec![
                call("1", "shell", "{}"),
                Message::tool_result("1", "x".repeat(4000)),
                call("2", "read_file", "{}"),
                Message::tool_result("2", "y".repeat(40)),
                Message::tool_result("orphan", "z".repeat(8)),
            ],
            vec![],
        ));
        assert_eq!(b.tool_results["shell"], 1000);
        assert_eq!(b.tool_results["read_file"], 10);
        assert_eq!(b.tool_results["unknown"], 2);
    }

    #[test]
    fn runtime_context_sections_of_the_real_system_prompt_are_not_counted_as_stable() {
        let skills: std::sync::Arc<[sven_workspace::SkillInfo]> =
            std::sync::Arc::from(vec![sven_workspace::SkillInfo {
                command: "deploy".into(),
                name: "deploy".into(),
                description: "Deploys the service to production".into(),
                version: None,
                skill_md_path: std::path::PathBuf::new(),
                skill_dir: std::path::PathBuf::new(),
                content: String::new(),
                sven_meta: None,
            }]);
        let with = system_prompt(
            AgentMode::Agent,
            None,
            PromptContext {
                skills,
                ..PromptContext::default()
            },
        );
        let without = system_prompt(AgentMode::Agent, None, PromptContext::default());
        let b_with = estimate_request(&request(vec![Message::system(with)], vec![]));
        let b_without = estimate_request(&request(vec![Message::system(without)], vec![]));
        assert!(b_with.system_runtime["Skills"] > 0);
        assert!(!b_without.system_runtime.contains_key("Skills"));
        assert_eq!(b_with.system_stable, b_without.system_stable);
    }

    #[test]
    fn duplicate_calls_are_same_tool_and_same_arguments_regardless_of_key_order() {
        let b = estimate_request(&request(
            vec![
                call("1", "grep", r#"{"pattern":"a","path":"."}"#),
                call("2", "grep", r#"{ "path": ".", "pattern": "a" }"#),
                call("3", "grep", r#"{"pattern":"b","path":"."}"#),
                call("4", "read_file", r#"{"pattern":"a","path":"."}"#),
            ],
            vec![],
        ));
        assert_eq!(
            (b.tool_activity.calls, b.tool_activity.duplicate_calls),
            (4, 1)
        );
        assert_eq!(
            (b.tool_activity.retries, b.tool_activity.errors),
            (None, None)
        );
    }

    #[test]
    fn reconciliation_exposes_the_estimator_residual() {
        let mut b = estimate_request(&request(vec![Message::user("a".repeat(400))], vec![]));
        assert_eq!(b.estimated_total, 100);
        b.reconcile(ReportedUsage {
            input: 90,
            cache_read: 30,
            ..ReportedUsage::default()
        });
        assert_eq!(b.residual, Some(20));
    }
}
