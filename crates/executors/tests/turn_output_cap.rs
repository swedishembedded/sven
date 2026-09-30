// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A turn's output-token limit reaches the provider whatever the provider
//! knows about its own model.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use sven_executors::{CompositeExecutorBuilder, ThreadStore, TurnExecutor};
use sven_hsm::{dispatch::Hsm, submachine::ErasedMachine, Context, Event, PermissionPolicy};
use sven_kernel::ErasedRuntime;
use sven_machines::ReactiveAgentMachine;

/// A provider that knows neither its context window nor its output cap, and
/// records the request it is sent.
struct UnknownModel {
    last: Arc<Mutex<Option<sven_model::CompletionRequest>>>,
}

#[async_trait::async_trait]
impl sven_model::ModelProvider for UnknownModel {
    fn name(&self) -> &str {
        "unknown"
    }
    fn model_name(&self) -> &str {
        "unknown"
    }
    async fn complete(
        &self,
        req: sven_model::CompletionRequest,
    ) -> anyhow::Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = anyhow::Result<sven_model::ResponseEvent>> + Send>,
        >,
    > {
        *self.last.lock().unwrap() = Some(req);
        let events: Vec<anyhow::Result<sven_model::ResponseEvent>> = vec![
            Ok(sven_model::ResponseEvent::TextDelta("pong".into())),
            Ok(sven_model::ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

#[tokio::test]
async fn the_output_cap_applies_when_the_context_window_is_unknown() {
    let last = Arc::new(Mutex::new(None));
    let turn = TurnExecutor::new(
        Arc::new(UnknownModel {
            last: Arc::clone(&last),
        }),
        None,
        Arc::new(sven_tool_registry::ToolRegistry::new()),
        Arc::new(std::sync::Mutex::new(ThreadStore::new())),
        Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        Arc::new(tokio::sync::Mutex::new(None)),
    )
    .with_turn_limits(sven_turn::TurnLimits::default().with_max_output_tokens(Some(123)));
    let rt = ErasedRuntime::spawn(
        Box::new(Hsm::new(ReactiveAgentMachine::new())) as Box<dyn ErasedMachine>,
        Context::new(),
        PermissionPolicy::builder().build(),
        CompositeExecutorBuilder::default().with_turn(turn).build(),
        64,
    );
    rt.post(Event::user_message("ping")).await;
    for _ in 0..200 {
        if last.lock().unwrap().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    rt.abort();
    let sent = last.lock().unwrap().take().expect("the model was called");
    assert_eq!(sent.max_output_tokens_override, Some(123));
}
