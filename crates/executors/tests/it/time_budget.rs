// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for coding agents that run out of
// wall-clock time before they finish for its clients. If your team needs
// expertise in running agents under deadlines then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Spec: a run with a deadline tells the model how much of it is left, on the
//! newest message of each request, without disturbing the cached prefix and
//! without entering the conversation the session stores.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use sven_executors::{CompositeExecutorBuilder, ThreadStore, TurnExecutor};
use sven_hsm::{dispatch::Hsm, submachine::ErasedMachine, Context, Event, PermissionPolicy};
use sven_kernel::ErasedRuntime;
use sven_machines::ReactiveAgentMachine;
use sven_model::{Message, MessageContent};
use sven_turn::TimeBudget;

/// Records every request it is sent and answers each with a short reply.
struct Recording {
    requests: Arc<Mutex<Vec<sven_model::CompletionRequest>>>,
}

#[async_trait::async_trait]
impl sven_model::ModelProvider for Recording {
    fn name(&self) -> &str {
        "recording"
    }
    fn model_name(&self) -> &str {
        "recording"
    }
    async fn complete(
        &self,
        req: sven_model::CompletionRequest,
    ) -> anyhow::Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = anyhow::Result<sven_model::ResponseEvent>> + Send>,
        >,
    > {
        self.requests.lock().unwrap().push(req);
        let events: Vec<anyhow::Result<sven_model::ResponseEvent>> = vec![
            Ok(sven_model::ResponseEvent::TextDelta("ok".into())),
            Ok(sven_model::ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

struct Run {
    rt: ErasedRuntime,
    requests: Arc<Mutex<Vec<sven_model::CompletionRequest>>>,
    store: Arc<Mutex<ThreadStore>>,
}

fn run(budget: Option<TimeBudget>) -> Run {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::new(Mutex::new(ThreadStore::new()));
    let turn = TurnExecutor::new(
        Arc::new(Recording {
            requests: Arc::clone(&requests),
        }),
        None,
        Arc::new(sven_tool_registry::ToolRegistry::new()),
        Arc::clone(&store),
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        Arc::new(tokio::sync::Mutex::new(None)),
    )
    .with_time_budget(budget);
    let rt = ErasedRuntime::spawn(
        Box::new(Hsm::new(ReactiveAgentMachine::new())) as Box<dyn ErasedMachine>,
        Context::new(),
        PermissionPolicy::builder().build(),
        CompositeExecutorBuilder::default().with_turn(turn).build(),
        64,
    );
    Run {
        rt,
        requests,
        store,
    }
}

impl Run {
    /// Posts `text` and waits until the model has been asked `n` times.
    async fn say(&self, text: &str, n: usize) {
        self.rt.post(Event::user_message(text)).await;
        for _ in 0..500 {
            if self.requests.lock().unwrap().len() >= n {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Let the reply land in the thread before the next message.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    fn sent(&self, i: usize) -> Vec<Message> {
        self.requests.lock().unwrap()[i].messages.clone()
    }
}

fn text(m: &Message) -> String {
    match &m.content {
        MessageContent::Text(t) => t.clone(),
        other => panic!("expected text, got {other:?}"),
    }
}

const MIN: Duration = Duration::from_secs(60);

#[tokio::test(start_paused = true)]
async fn the_newest_message_of_each_request_ends_with_the_time_left() {
    let r = run(Some(TimeBudget::starting_now(58 * MIN)));
    r.say("first", 1).await;
    tokio::time::advance(12 * MIN).await;
    r.say("second", 2).await;
    r.rt.abort();

    let first = text(r.sent(0).last().unwrap());
    assert!(first.starts_with("first"), "{first}");
    assert!(
        first.ends_with("Time: 0 of 58 min used, 58 min left; leave time to verify and finish."),
        "{first}"
    );
    let second = text(r.sent(1).last().unwrap());
    assert!(second.starts_with("second"), "{second}");
    assert!(
        second.ends_with("Time: 12 of 58 min used, 46 min left; leave time to verify and finish."),
        "{second}"
    );
}

#[tokio::test(start_paused = true)]
async fn each_request_begins_with_the_whole_previous_request() {
    // A server that caches prompts resumes a recurrent model only from the
    // exact end of an earlier prompt, so a notice once sent stays in every
    // later request where it was: requests only ever grow at the end.
    let r = run(Some(TimeBudget::starting_now(58 * MIN)));
    r.say("first", 1).await;
    tokio::time::advance(7 * MIN).await;
    r.say("second", 2).await;
    tokio::time::advance(3 * MIN).await;
    r.say("third", 3).await;
    r.rt.abort();

    for n in 1..3 {
        let (a, b) = (r.sent(n - 1), r.sent(n));
        assert!(b.len() > a.len(), "request {n} extends request {}", n - 1);
        for i in 0..a.len() {
            assert_eq!(
                serde_json::to_string(&a[i]).unwrap(),
                serde_json::to_string(&b[i]).unwrap(),
                "message {i} of request {} changed in request {n}",
                n - 1
            );
        }
    }
    // Each message keeps the notice of the request it was newest in.
    let last = r.sent(2);
    let notices: Vec<_> = last
        .iter()
        .filter(|m| format!("{:?}", m.content).contains("Time: "))
        .collect();
    assert_eq!(notices.len(), 3, "{last:?}");
    assert!(text(last.last().unwrap())
        .ends_with("Time: 10 of 58 min used, 48 min left; leave time to verify and finish."));
}

#[tokio::test(start_paused = true)]
async fn a_run_without_a_deadline_carries_no_notice() {
    let r = run(None);
    r.say("first", 1).await;
    r.rt.abort();
    assert_eq!(text(r.sent(0).last().unwrap()), "first");
    assert!(r.requests.lock().unwrap()[0]
        .system_dynamic_suffix
        .is_none());
}

#[tokio::test(start_paused = true)]
async fn the_notice_never_enters_the_stored_thread() {
    let r = run(Some(TimeBudget::starting_now(58 * MIN)));
    r.say("first", 1).await;
    r.rt.abort();

    let sent = text(r.sent(0).last().unwrap());
    assert!(sent.contains("Time: "), "the request carried it: {sent}");
    let store = r.store.lock().unwrap();
    for id in store.transcript_threads() {
        for m in store.transcript(&id.to_string()) {
            assert!(
                !format!("{:?}", m.content).contains("Time: "),
                "stored message carries the notice: {m:?}"
            );
        }
    }
    assert!(!store.transcript_threads().is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_system_message_and_dynamic_suffix_are_untouched() {
    let r = run(Some(TimeBudget::starting_now(58 * MIN)));
    r.say("first", 1).await;
    r.rt.abort();
    let req = &r.requests.lock().unwrap()[0];
    assert!(req
        .messages
        .iter()
        .filter(|m| m.role == sven_model::Role::System)
        .all(|m| !format!("{:?}", m.content).contains("Time: ")));
    assert!(req
        .system_dynamic_suffix
        .as_deref()
        .is_none_or(|s| !s.contains("Time: ")));
}
