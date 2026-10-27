// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for running the same coding agent
// in a terminal, on a server and in a web page for its clients. If your team
// needs expertise in WebAssembly agent runtimes then you can procure our
// services by sending an email to info@swedishembedded.com.
//! A sven agent session for JavaScript.
//!
//! ```js
//! import init, { SvenSession } from "./sven_wasm.js";
//! await init();
//! const session = new SvenSession(JSON.stringify({
//!   base_url: "https://example.com/v1", model: "some/model",
//! }));
//! session.onEvent((json) => render(JSON.parse(json)));
//! await session.send("hello");
//! ```
//!
//! Nothing here is a second agent: the session is the same reactive agent
//! machine, kernel loop, turn executor and tool executor native sven runs
//! (`make test/wasm` proves the same turn behaves identically on both). The
//! model is sven's OpenAI-compatible driver over the page's own `fetch`. The
//! page renders what [`SvenSession::on_event`] delivers - sven's own
//! `SessionEvent`, as JSON - and never interprets model output itself.
//!
//! A browser has none of the native tools (no shell, no files). A page offers
//! its own: declared in the options, run by the page's handler, and allowed
//! exactly the effects they declare.
//!
//! ```js
//! const session = new SvenSession(JSON.stringify({
//!   base_url, model,
//!   tools: [{ name: "page_title", description: "The page's title.",
//!             parameters: { type: "object" }, effect: "read" }],
//! }), async (name, argumentsJson) => document.title);
//! ```

// The page's tools hold JavaScript values, so the registry is not Send in a
// browser build; it runs on the page's one thread, where Arc is merely shared.
#![cfg_attr(target_arch = "wasm32", allow(clippy::arc_with_non_send_sync))]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use sven_executors::{
    history_messages, CompositeExecutorBuilder, ThreadStore, ToolExecutor, TurnExecutor,
};
use sven_hsm::{
    dispatch::Hsm, submachine::ErasedMachine, Context, Event, PermissionPolicy, ToolCapability,
    UiEvent,
};
use sven_kernel::ErasedRuntime;
use sven_machines::machines::reactive_agent::CHAT_THREAD;
use sven_machines::ReactiveAgentMachine;
use sven_model::ModelProvider;
use sven_model_drivers::OpenAiProvider;
#[cfg(target_arch = "wasm32")]
use sven_tool_api::{ApprovalPolicy, Tool};
use sven_tool_registry::ToolRegistry;
#[cfg(target_arch = "wasm32")]
use sven_vocab::{ToolCall, ToolOutput};
use tokio::sync::broadcast::error::RecvError;
use wasm_bindgen::prelude::*;

/// What a page gives a new session.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionOptions {
    /// An OpenAI-compatible API, ending in `/v1`.
    base_url: String,
    /// The model id that API serves.
    model: String,
    /// A bearer key, when the API needs one. A page normally leaves it out
    /// and calls an endpoint of its own origin that its session cookie
    /// authenticates, so no key ever reaches the page.
    #[serde(default)]
    api_key: Option<String>,
    /// The conversation so far, oldest first, in the remote-control
    /// protocol's shape (`{"role": "user" | "assistant", "text": ...}`): how a
    /// reopened conversation continues where it stopped.
    #[serde(default)]
    history: Vec<sven_control::HistoryEntry>,
    /// Tools the page offers the agent, each run by the page's handler.
    #[serde(default)]
    tools: Vec<PageToolSpec>,
}

/// A tool the page offers: what the model is told about it, and what it does.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
struct PageToolSpec {
    name: String,
    description: String,
    /// The JSON Schema of its arguments.
    parameters: serde_json::Value,
    /// What a call can do, which the session's permissions are checked
    /// against: `read`, `write` or `network`.
    effect: PageToolEffect,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum PageToolEffect {
    Read,
    Write,
    Network,
}

impl PageToolEffect {
    fn capability(self) -> ToolCapability {
        match self {
            Self::Read => ToolCapability::ReadFile,
            Self::Write => ToolCapability::WriteFile,
            Self::Network => ToolCapability::NetworkAccess,
        }
    }
}

/// A page tool: the model's call goes to the page's handler as
/// `(name, argumentsJson)`, and the string its promise resolves to is the
/// result; a rejected promise is a failed call.
#[cfg(target_arch = "wasm32")]
struct PageTool {
    spec: PageToolSpec,
    handler: js_sys::Function,
}

#[cfg(target_arch = "wasm32")]
#[async_trait::async_trait(?Send)]
impl Tool for PageTool {
    fn name(&self) -> &str {
        &self.spec.name
    }
    fn description(&self) -> &str {
        &self.spec.description
    }
    fn parameters_schema(&self) -> serde_json::Value {
        self.spec.parameters.clone()
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn kernel_capability(&self) -> ToolCapability {
        self.spec.effect.capability()
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let args = JsValue::from_str(&call.args.to_string());
        let started =
            self.handler
                .call2(&JsValue::NULL, &JsValue::from_str(&self.spec.name), &args);
        let promise = match started {
            Ok(value) => js_sys::Promise::resolve(&value),
            Err(error) => return ToolOutput::err(call.id.clone(), format!("{error:?}")),
        };
        match wasm_bindgen_futures::JsFuture::from(promise).await {
            Ok(value) => ToolOutput::ok(call.id.clone(), value.as_string().unwrap_or_default()),
            Err(error) => ToolOutput::err(
                call.id.clone(),
                error.as_string().unwrap_or_else(|| format!("{error:?}")),
            ),
        }
    }
}

/// Puts the page's tools in `registry`, each answered by `handler`.
#[cfg(target_arch = "wasm32")]
fn register_page_tools(
    registry: &mut ToolRegistry,
    tools: Vec<PageToolSpec>,
    handler: Option<js_sys::Function>,
) -> Result<(), JsError> {
    if tools.is_empty() {
        return Ok(());
    }
    let handler = handler.ok_or_else(|| JsError::new("tools need a tool handler"))?;
    for spec in tools {
        registry.register(PageTool {
            spec,
            handler: handler.clone(),
        });
    }
    Ok(())
}

/// Natively there is no page to run a tool.
#[cfg(not(target_arch = "wasm32"))]
fn register_page_tools(
    _registry: &mut ToolRegistry,
    tools: Vec<PageToolSpec>,
    _handler: Option<js_sys::Function>,
) -> Result<(), JsError> {
    if tools.is_empty() {
        Ok(())
    } else {
        Err(JsError::new("page tools run only in a browser build"))
    }
}

/// One agent session, running in this page (or worker).
#[wasm_bindgen]
pub struct SvenSession {
    runtime: ErasedRuntime,
}

#[wasm_bindgen]
impl SvenSession {
    /// Starts a session from `options`, a JSON object: `base_url`, `model`,
    /// and optionally `api_key`, `history` and `tools`. `tool_handler` runs
    /// the page's tools: `(name, argumentsJson) => Promise<string>`.
    ///
    /// # Errors
    ///
    /// `options` is not that object, or it names tools without a handler.
    #[wasm_bindgen(constructor)]
    pub fn new(
        options: &str,
        tool_handler: Option<js_sys::Function>,
    ) -> Result<SvenSession, JsError> {
        let options: SessionOptions = serde_json::from_str(options)
            .map_err(|e| JsError::new(&format!("session options: {e}")))?;
        let model: Arc<dyn ModelProvider> = Arc::new(match options.api_key {
            Some(key) => OpenAiProvider::new(
                options.model,
                Some(key),
                Some(options.base_url),
                None,
                None,
                serde_json::Value::Null,
            ),
            None => OpenAiProvider::authenticated_by_origin(options.model, &options.base_url),
        });
        let capabilities: HashSet<ToolCapability> = options
            .tools
            .iter()
            .map(|tool| tool.effect.capability())
            .collect();
        let mut registry = ToolRegistry::new();
        register_page_tools(&mut registry, options.tools, tool_handler)?;
        let registry = Arc::new(registry);
        let mut thread = ThreadStore::new();
        thread.replace_thread(CHAT_THREAD, history_messages(options.history));
        let store = Arc::new(Mutex::new(thread));
        let call_threads = Arc::new(Mutex::new(HashMap::new()));
        let turn = TurnExecutor::new(
            model,
            None,
            Arc::clone(&registry),
            Arc::clone(&store),
            Arc::clone(&call_threads),
            Arc::new(tokio::sync::Mutex::new(None)),
        );
        let tools =
            ToolExecutor::with_shared_store(registry, capabilities.clone(), call_threads, store);
        let executor = CompositeExecutorBuilder::default()
            .with_turn(turn)
            .with_tool_executor(tools)
            .build();
        let machine: Box<dyn ErasedMachine> = Box::new(Hsm::new(ReactiveAgentMachine::new()));
        let runtime = ErasedRuntime::spawn(
            machine,
            Context::new(),
            PermissionPolicy::builder()
                .allow_globally(capabilities)
                .build(),
            executor,
            64,
        );
        Ok(SvenSession { runtime })
    }

    /// Calls `callback` with every event of the session from now on, each a
    /// JSON string of sven's `SessionEvent` (`{"TextDelta":"..."}`,
    /// `"TurnComplete"`, ...). Events a slow page missed arrive as one
    /// `{"EventsDropped":{"count":n}}`, never silently.
    #[wasm_bindgen(js_name = onEvent)]
    pub fn on_event(&self, callback: js_sys::Function) {
        let mut events = self.runtime.subscribe_observations();
        sven_rt::spawn(async move {
            loop {
                let event = match events.recv().await {
                    // The kernel's own dispatch trace is for a debugger.
                    Ok(UiEvent::Transition { .. }) => continue,
                    Ok(event) => event,
                    Err(RecvError::Lagged(count)) => UiEvent::EventsDropped { count },
                    Err(RecvError::Closed) => break,
                };
                if let Ok(json) = serde_json::to_string(&event) {
                    let _ = callback.call1(&JsValue::NULL, &JsValue::from_str(&json));
                }
            }
        });
    }

    /// Sends the person's message. Resolves to whether the session took it.
    pub fn send(&self, text: String) -> js_sys::Promise {
        let sink = self.runtime.sink();
        wasm_bindgen_futures::future_to_promise(async move {
            Ok(JsValue::from_bool(
                sink.emit(Event::user_message(text)).await,
            ))
        })
    }

    /// Stops the answer being written; the session stays open.
    pub fn cancel(&self) -> js_sys::Promise {
        let sink = self.runtime.sink();
        wasm_bindgen_futures::future_to_promise(async move {
            Ok(JsValue::from_bool(sink.emit(Event::UserCancelled).await))
        })
    }
}

/// One replica of a conversation's shared document (`sven-session-sync`):
/// the same implementation every replica runs, so they all merge alike.
#[wasm_bindgen(js_name = ConversationDoc)]
pub struct JsConversationDoc {
    inner: sven_session_sync::ConversationDoc,
}

impl Default for JsConversationDoc {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen(js_class = ConversationDoc)]
impl JsConversationDoc {
    /// An empty replica.
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new() -> JsConversationDoc {
        JsConversationDoc {
            inner: sven_session_sync::ConversationDoc::new(),
        }
    }

    /// Adds a message (`{id, role, author, state, parts}` as JSON, `parts`
    /// itself JSON text); an id already present is updated instead.
    ///
    /// # Errors
    ///
    /// `message` is not that object.
    #[wasm_bindgen(js_name = addMessage)]
    pub fn add_message(&self, message: &str) -> Result<(), JsError> {
        let message: sven_session_sync::DocMessage =
            serde_json::from_str(message).map_err(|e| JsError::new(&format!("message: {e}")))?;
        self.inner.add_message(&message);
        Ok(())
    }

    /// Replaces message `id`'s state and content.
    #[wasm_bindgen(js_name = updateMessage)]
    pub fn update_message(&self, id: &str, state: &str, parts: &str) {
        self.inner.update_message(id, state, parts);
    }

    /// Sets `key` of what is known about the conversation (`model`, ...).
    #[wasm_bindgen(js_name = setMeta)]
    pub fn set_meta(&self, key: &str, value: &str) {
        self.inner.set_meta(key, value);
    }

    /// `key` of what is known about the conversation, if set.
    pub fn meta(&self, key: &str) -> Option<String> {
        self.inner.meta(key)
    }

    /// The conversation in order, as a JSON array of messages.
    #[must_use]
    pub fn messages(&self) -> String {
        serde_json::to_string(&self.inner.messages()).unwrap_or_else(|_| "[]".into())
    }

    /// What this replica has; another replica's `diff` of it is what it lacks.
    #[wasm_bindgen(js_name = stateVector)]
    #[must_use]
    pub fn state_vector(&self) -> Vec<u8> {
        self.inner.state_vector()
    }

    /// Everything this replica has that one with `state_vector` lacks.
    ///
    /// # Errors
    ///
    /// `state_vector` is not one.
    pub fn diff(&self, state_vector: &[u8]) -> Result<Vec<u8>, JsError> {
        self.inner
            .diff(state_vector)
            .map_err(|e| JsError::new(&e.to_string()))
    }

    /// Merges an update from another replica; any order, any number of times.
    ///
    /// # Errors
    ///
    /// `update` is not one.
    pub fn apply(&self, update: &[u8]) -> Result<(), JsError> {
        self.inner
            .apply(update)
            .map_err(|e| JsError::new(&e.to_string()))
    }
}
