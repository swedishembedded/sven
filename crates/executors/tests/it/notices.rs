// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent runtimes that take word from the system
// around them without mistaking it for the person they serve. If your team
// needs expertise in embedding agents in a larger service then you can
// procure our services by sending an email to info@swedishembedded.com.
//! Spec: a notice from the host.
//!
//! A notice is shown as a notice, starts no turn unless asked, and leaves the
//! system prompt intact on every driver. A driver that keeps only the last
//! system message as the system prompt (Anthropic's) stays correct exactly
//! when the model is never shown a second `System` message, so that is what
//! is checked: whatever a request carries after a notice, the thread holds one
//! system message, the first, and the notice is a `User`-role message.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sven_executors::{CompositeExecutorBuilder, ThreadStore, TurnExecutor};
use sven_hsm::{
    dispatch::Hsm, submachine::ErasedMachine, Context, Event, PermissionPolicy, UiEvent,
};
use sven_kernel::ErasedRuntime;
use sven_machines::machines::reactive_agent::CHAT_THREAD;
use sven_machines::ReactiveAgentMachine;
use sven_model::{CompletionRequest, Message, ResponseEvent, Role};
use sven_vocab::Notice;

type Stream = std::pin::Pin<Box<dyn futures::Stream<Item = anyhow::Result<ResponseEvent>> + Send>>;

const SYSTEM_PROMPT: &str = "You are the agent. Follow these instructions.";

/// Answers "done" and keeps every request it was asked.
struct Recording {
    requests: Arc<Mutex<Vec<Vec<Message>>>>,
}

#[async_trait]
impl sven_model::ModelProvider for Recording {
    fn name(&self) -> &str {
        "recording"
    }
    fn model_name(&self) -> &str {
        "recording"
    }
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<Stream> {
        self.requests.lock().unwrap().push(req.messages);
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(ResponseEvent::TextDelta("done".into())),
            Ok(ResponseEvent::Done),
        ])))
    }
}

struct Session {
    rt: ErasedRuntime,
    requests: Arc<Mutex<Vec<Vec<Message>>>>,
    store: Arc<Mutex<ThreadStore>>,
    events: tokio::sync::broadcast::Receiver<UiEvent>,
}

fn session() -> Session {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::new(Mutex::new(ThreadStore::new()));
    store
        .lock()
        .unwrap()
        .append(CHAT_THREAD, Message::system(SYSTEM_PROMPT));
    let turn = TurnExecutor::new(
        Arc::new(Recording {
            requests: Arc::clone(&requests),
        }),
        None,
        Arc::new(sven_tool_registry::ToolRegistry::new()),
        Arc::clone(&store),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        Arc::new(tokio::sync::Mutex::new(None)),
    );
    let rt = ErasedRuntime::spawn(
        Box::new(Hsm::new(ReactiveAgentMachine::new())) as Box<dyn ErasedMachine>,
        Context::new(),
        PermissionPolicy::builder().build(),
        CompositeExecutorBuilder::default().with_turn(turn).build(),
        64,
    );
    let events = rt.subscribe_observations();
    Session {
        rt,
        requests,
        store,
        events,
    }
}

impl Session {
    /// The events up to and including the first one `stop` accepts.
    async fn until(&mut self, stop: impl Fn(&UiEvent) -> bool) -> Vec<UiEvent> {
        let mut seen = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        while let Ok(Ok(ev)) = tokio::time::timeout_at(deadline, self.events.recv()).await {
            let over = stop(&ev);
            seen.push(ev);
            if over {
                return seen;
            }
        }
        panic!("the awaited event never came: {seen:?}");
    }

    fn thread(&self) -> Vec<Message> {
        self.store.lock().unwrap().snapshot(CHAT_THREAD)
    }

    fn model_requests(&self) -> Vec<Vec<Message>> {
        self.requests.lock().unwrap().clone()
    }
}

fn notice() -> Notice {
    Notice::new("job.finished", "The render is ready.").unwrap()
}

fn system_messages(messages: &[Message]) -> Vec<&str> {
    messages
        .iter()
        .filter(|m| m.role == Role::System)
        .filter_map(Message::as_text)
        .collect()
}

#[tokio::test]
async fn a_notice_is_shown_as_a_notice_and_starts_no_turn_unless_asked() {
    let mut s = session();
    s.rt.post(Event::Notice {
        notice: notice(),
        start_turn: false,
    })
    .await;
    let seen = s.until(|e| matches!(e, UiEvent::Notice(_))).await;
    assert!(
        !seen.iter().any(|e| matches!(e, UiEvent::UserMessage(_))),
        "a notice is never shown as the person's message: {seen:?}"
    );
    assert!(s.model_requests().is_empty(), "the model was asked");

    // It is in the conversation as a user-role message that names its sender,
    // and the instructions are as they were.
    let thread = s.thread();
    assert_eq!(system_messages(&thread), [SYSTEM_PROMPT]);
    let filed = thread.last().unwrap();
    assert_eq!(filed.role, Role::User);
    assert_eq!(Notice::parse(filed.as_text().unwrap()), Some(notice()));

    // The person's next message is the first the model hears of it.
    s.rt.post(Event::user_message("what is new?")).await;
    s.until(|e| *e == UiEvent::TurnComplete).await;
    let requests = s.model_requests();
    assert_eq!(requests.len(), 1, "one turn, the person's");
    assert_eq!(system_messages(&requests[0]), [SYSTEM_PROMPT]);
    assert_eq!(requests[0][0].role, Role::System, "the prompt stays first");
    assert!(requests[0][1..]
        .iter()
        .any(|m| m.role == Role::User && Notice::parse(m.as_text().unwrap_or("")).is_some()));
    s.rt.abort();
}

#[tokio::test]
async fn a_notice_that_asks_for_a_turn_gets_one_with_the_system_prompt_intact() {
    let mut s = session();
    s.rt.post(Event::Notice {
        notice: notice(),
        start_turn: true,
    })
    .await;
    let seen = s.until(|e| *e == UiEvent::TurnComplete).await;
    assert!(seen.iter().any(|e| matches!(e, UiEvent::Notice(_))));
    let requests = s.model_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(system_messages(&requests[0]), [SYSTEM_PROMPT]);
    let last = requests[0].last().unwrap();
    assert_eq!(last.role, Role::User);
    assert_eq!(Notice::parse(last.as_text().unwrap()), Some(notice()));
    s.rt.abort();
}
