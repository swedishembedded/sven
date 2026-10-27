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
//! A browser has none of the native tools (no shell, no files); the session
//! offers none until a page registers browser tools of its own.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use sven_executors::{
    history_messages, CompositeExecutorBuilder, ThreadStore, ToolExecutor, TurnExecutor,
};
use sven_hsm::{
    dispatch::Hsm, submachine::ErasedMachine, Context, Event, PermissionPolicy, UiEvent,
};
use sven_kernel::ErasedRuntime;
use sven_machines::machines::reactive_agent::CHAT_THREAD;
use sven_machines::ReactiveAgentMachine;
use sven_model::ModelProvider;
use sven_model_drivers::OpenAiProvider;
use sven_tool_registry::ToolRegistry;
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
}

/// One agent session, running in this page (or worker).
#[wasm_bindgen]
pub struct SvenSession {
    runtime: ErasedRuntime,
}

#[wasm_bindgen]
impl SvenSession {
    /// Starts a session from `options`, a JSON object: `base_url`, `model`,
    /// and optionally `api_key` and `history`.
    ///
    /// # Errors
    ///
    /// `options` is not that object.
    #[wasm_bindgen(constructor)]
    pub fn new(options: &str) -> Result<SvenSession, JsError> {
        let options: SessionOptions = serde_json::from_str(options)
            .map_err(|e| JsError::new(&format!("session options: {e}")))?;
        let model: Arc<dyn ModelProvider> = Arc::new(OpenAiProvider::new(
            options.model,
            options.api_key,
            Some(options.base_url),
            None,
            None,
            serde_json::Value::Null,
        ));
        let registry = Arc::new(ToolRegistry::new());
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
        let tools = ToolExecutor::with_shared_store(registry, HashSet::new(), call_threads, store);
        let executor = CompositeExecutorBuilder::default()
            .with_turn(turn)
            .with_tool_executor(tools)
            .build();
        let machine: Box<dyn ErasedMachine> = Box::new(Hsm::new(ReactiveAgentMachine::new()));
        let runtime = ErasedRuntime::spawn(
            machine,
            Context::new(),
            PermissionPolicy::builder().build(),
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
