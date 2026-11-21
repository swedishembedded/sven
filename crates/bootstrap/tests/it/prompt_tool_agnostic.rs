// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent prompts
// consistent with the tools an agent really has for its clients. If your team
// needs expertise in agent tool configuration then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Spec: the system prompt never names a tool.
//!
//! Tools reach the model only through the tool list, and only when the session
//! offers them; each tool's own description says what it does and how to use
//! it. The system prompt only asks the agent to use the available tools as
//! appropriate, with the fewest steps and calls that give a correct result.
//! Checked against the names a real registry holds, for a session with every
//! tool, one confined to a single MCP tool, one with no tools and one with
//! deferred disclosure.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use sven_bootstrap::{Config, Disclosure, RuntimeBuilder, RuntimeContext};
use sven_model::{CompletionRequest, Role};
use sven_model_mock::ScriptedMockProvider;
use sven_tool_api::{ApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput};

/// The one sentence every built-in prompt carries instead of tool names.
const GUIDANCE: &str = "Use the tools available to you as appropriate to complete the task \
                        correctly, with the fewest steps and tool calls that achieve a correct \
                        result.";

/// Stands in for an MCP server's tool, as a task environment brings it.
struct McpLike;

#[async_trait]
impl Tool for McpLike {
    fn name(&self) -> &str {
        "vim-vim"
    }
    fn description(&self) -> &str {
        "Drive a headless vim."
    }
    fn parameters_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn is_mcp(&self) -> bool {
        true
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        ToolOutput::ok(&call.id, "ok")
    }
}

struct Session {
    request: CompletionRequest,
    registry_names: Vec<String>,
}

impl Session {
    fn system(&self) -> &str {
        self.request
            .messages
            .iter()
            .find(|m| m.role == Role::System)
            .and_then(|m| m.as_text())
            .expect("a system message")
    }

    fn description(&self, tool: &str) -> &str {
        &self
            .request
            .tools
            .iter()
            .find(|t| t.name == tool)
            .unwrap_or_else(|| panic!("{tool} is offered"))
            .description
    }
}

fn mock_config() -> Config {
    let mut config = Config::default();
    config.model.provider = "mock".into();
    config.model.name = "mock-model".into();
    config
}

/// Builds a session in agent mode, sends one message and returns the request
/// the model received with the names its registry holds.
async fn first_request(config: Config, ctx: RuntimeContext, extra: Vec<Arc<dyn Tool>>) -> Session {
    let provider = ScriptedMockProvider::always_text("done");
    let last_request = provider.last_request.clone();
    let (runtime, handle, ..) = RuntimeBuilder::new(Arc::new(config), "agent")
        .with_runtime_context(ctx)
        .with_model_provider(Box::new(provider))
        .with_extra_tools(extra)
        .build()
        .await
        .expect("the session builds");
    let registry_names = handle.tool_registry().names();
    handle.send_user_message("hello".into()).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while last_request.lock().unwrap().is_none() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    runtime.abort();
    let request = last_request.lock().unwrap().clone().expect("a request");
    Session {
        request,
        registry_names,
    }
}

async fn full_session() -> Session {
    first_request(mock_config(), RuntimeContext::empty(), vec![]).await
}

/// Where `name` would appear as a tool name: in code format, as a call, or
/// next to the word "tool". A name with `_` or `-` is no English word, so it
/// must not appear at all.
fn mentions(prompt: &str, name: &str) -> bool {
    if name.contains(['_', '-']) && prompt.contains(name) {
        return true;
    }
    [
        format!("`{name}`"),
        format!("{name}("),
        format!("{name} tool"),
        format!("tool {name}"),
    ]
    .iter()
    .any(|form| prompt.contains(form.as_str()))
}

fn assert_names_no_tool(case: &str, session: &Session, universe: &[String]) {
    let prompt = session.system();
    let named: Vec<&String> = universe
        .iter()
        .chain(&session.registry_names)
        .filter(|name| mentions(prompt, name))
        .collect();
    assert!(
        named.is_empty(),
        "{case}: the prompt names {named:?}:\n{prompt}"
    );
    assert!(
        prompt.contains(GUIDANCE),
        "{case}: the guidance is missing:\n{prompt}"
    );
}

#[tokio::test]
async fn no_session_prompt_names_a_tool_of_the_registry() {
    let full = full_session().await;
    let universe = full.registry_names.clone();
    assert!(universe.iter().any(|n| n == "shell"), "{universe:?}");
    assert_names_no_tool("all tools", &full, &universe);

    let mut vim = mock_config();
    vim.tools.only = Some(vec!["vim-vim".into()]);
    let vim = first_request(vim, RuntimeContext::empty(), vec![Arc::new(McpLike)]).await;
    assert_eq!(vim.registry_names, ["vim-vim"]);
    assert_names_no_tool("one MCP tool", &vim, &universe);

    let none = first_request(
        mock_config(),
        RuntimeContext {
            no_tools: true,
            ..RuntimeContext::empty()
        },
        vec![],
    )
    .await;
    assert!(none.request.tools.is_empty());
    assert_names_no_tool("no tools", &none, &universe);

    let mut deferred = mock_config();
    deferred.tools.disclosure.mode = Disclosure::Deferred;
    let deferred = first_request(deferred, RuntimeContext::empty(), vec![]).await;
    assert_names_no_tool("deferred disclosure", &deferred, &universe);
}

#[tokio::test]
async fn tool_advice_lives_in_the_tool_that_owns_it() {
    let full = full_session().await;
    assert!(
        full.description("shell").contains("timeout_secs"),
        "{}",
        full.description("shell")
    );
    assert!(
        full.description("grep").contains("relevant ranges"),
        "{}",
        full.description("grep")
    );

    // Under deferred disclosure the `tools` tool says what it can load.
    let mut config = mock_config();
    config.tools.disclosure.mode = Disclosure::Deferred;
    let deferred = first_request(config, RuntimeContext::empty(), vec![]).await;
    let loader = deferred.description("tools");
    assert!(loader.contains("Not loaded:"), "{loader}");
    assert!(loader.contains("- web_fetch"), "{loader}");
    assert!(loader.contains("prefer it over a skill"), "{loader}");
}

#[tokio::test]
async fn a_custom_prompt_and_an_appended_one_are_sent_unchanged() {
    let session = first_request(
        mock_config(),
        RuntimeContext {
            system_prompt_override: Some("Custom prompt naming `shell`.".into()),
            append_system_prompt: Some("Appended.".into()),
            ..RuntimeContext::empty()
        },
        vec![],
    )
    .await;
    assert_eq!(
        session.system(),
        "Custom prompt naming `shell`.\n\nAppended."
    );
}
