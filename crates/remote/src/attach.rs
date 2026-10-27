// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A conversation the server keeps, as an agent session the full-screen UI
//! can show: `sven rc attach --tui`.
//!
//! The UI talks to its agent through two channels, requests in and
//! [`AgentEvent`]s out. [`remote_agent_task`] is that agent for a
//! conversation that runs somewhere else: it keeps a replica of the
//! conversation's shared document, turns what changes in it into the same
//! events a local session emits ([`EventTranslator`]), and sends what the
//! person types through the server, which stores it and has the machine
//! the conversation runs on answer it. Nothing runs here.
//!
//! The document's messages carry their content as parts, a JSON array of
//! objects with a `type`: `text` (`text`), `reasoning` (`text`), `tool_call`
//! (`callId`, `name`, `arguments`), `tool_result` (`callId`, `result`,
//! `isError`), `approval` (`callId`, `name`, `arguments`, `status`) and
//! `error` (`message`). Any other type is not shown.
//!
//! A tool call that waits for permission is asked about in the UI the way a
//! local session asks (a yes/no question); the answer goes to the server,
//! which tells the machine. Decided elsewhere first, the question is
//! withdrawn.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use serde_json::Value;
use sven_frontend::agent::AgentRequest;
use sven_machines::AgentEvent;
use sven_session_sync::DocMessage;
use sven_tools_agent::{Question, QuestionRequest};
use sven_vocab::ToolCall;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::client::RemoteClient;
use crate::transcript::Reader;

/// How often the attached UI asks the server for what changed: often enough
/// that an answer reads as streaming.
pub const ATTACH_INTERVAL: Duration = Duration::from_millis(500);

/// How far one assistant message has been shown.
#[derive(Debug, Default)]
struct Shown {
    /// Parts shown in full.
    parts: usize,
    /// Bytes of the last part's text already streamed, while it grows.
    streamed: usize,
    /// A tool call's name, by call id, for its result.
    tools: HashMap<String, String>,
    ended: bool,
}

/// Turns a conversation, as it changes, into the events a local session
/// would have emitted for it.
#[derive(Debug, Default)]
pub struct EventTranslator {
    users: HashMap<String, ()>,
    answers: HashMap<String, Shown>,
    /// Messages this UI sent, not yet seen in the conversation: shown by the
    /// UI when sent, so never shown again when they arrive.
    own: VecDeque<String>,
    /// Tool calls waiting for permission, by call id.
    waiting: HashMap<String, ()>,
    /// What changed about waiting calls since [`Self::take_approvals`].
    approvals: Approvals,
}

/// A tool call that waits for the person's permission.
#[derive(Debug, Clone, PartialEq)]
pub struct Waiting {
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
}

/// Calls that started waiting, and calls no longer waiting (decided here or
/// elsewhere, or their answer ended).
#[derive(Debug, Default, PartialEq)]
pub struct Approvals {
    pub asked: Vec<Waiting>,
    pub settled: Vec<String>,
}

impl EventTranslator {
    /// `text` was sent from this UI.
    pub fn sent(&mut self, text: &str) {
        self.own.push_back(text.trim().to_string());
    }

    /// The calls that started or stopped waiting since the last time.
    pub fn take_approvals(&mut self) -> Approvals {
        std::mem::take(&mut self.approvals)
    }

    /// Notes which tool calls of `parts` wait for permission now.
    fn note_approvals(&mut self, parts: &[Value], streaming: bool) {
        for part in parts.iter().filter(|p| field(p, "type") == "approval") {
            let call_id = field(part, "callId").to_string();
            let pending = streaming && field(part, "status") == "pending";
            if pending && self.waiting.insert(call_id.clone(), ()).is_none() {
                self.approvals.asked.push(Waiting {
                    call_id,
                    name: field(part, "name").to_string(),
                    arguments: part.get("arguments").cloned().unwrap_or(Value::Null),
                });
            } else if !pending && self.waiting.remove(&call_id).is_some() {
                self.approvals.settled.push(call_id);
            }
        }
    }

    /// The events `messages` add to what was shown.
    pub fn events(&mut self, messages: &[DocMessage]) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        for message in messages {
            let Ok(Value::Array(parts)) = serde_json::from_str::<Value>(&message.parts) else {
                continue;
            };
            if message.role == "user" {
                if self.users.insert(message.id.clone(), ()).is_some() {
                    continue;
                }
                let text = texts(&parts);
                if self.own.front().is_some_and(|own| *own == text.trim()) {
                    self.own.pop_front();
                } else {
                    events.push(AgentEvent::UserMessage(text));
                }
                continue;
            }
            if message.role != "assistant" {
                continue;
            }
            let streaming = message.state == "streaming";
            self.note_approvals(&parts, streaming);
            let shown = self.answers.entry(message.id.clone()).or_default();
            show_parts(shown, &parts, streaming, &mut events);
            if !streaming && !shown.ended {
                shown.ended = true;
                events.push(AgentEvent::TurnComplete);
            }
        }
        events
    }
}

fn texts(parts: &[Value]) -> String {
    parts
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn field<'a>(part: &'a Value, name: &str) -> &'a str {
    part.get(name).and_then(Value::as_str).unwrap_or_default()
}

/// Shows what of `parts` was not shown yet. The last part of a message still
/// being written may grow, so its text is streamed and it is finished only
/// once a part follows it or the message ends.
fn show_parts(shown: &mut Shown, parts: &[Value], streaming: bool, events: &mut Vec<AgentEvent>) {
    while shown.parts < parts.len() {
        let part = &parts[shown.parts];
        let open = streaming && shown.parts + 1 == parts.len();
        match field(part, "type") {
            "text" => {
                let text = field(part, "text");
                if let Some(rest) = text.get(shown.streamed..).filter(|rest| !rest.is_empty()) {
                    events.push(AgentEvent::TextDelta(rest.to_string()));
                    shown.streamed = text.len();
                }
                if open {
                    return;
                }
                events.push(AgentEvent::TextComplete(text.to_string()));
            }
            "reasoning" => {
                if open {
                    return;
                }
                events.push(AgentEvent::ThinkingComplete(
                    field(part, "text").to_string(),
                ));
            }
            "tool_call" => {
                let id = field(part, "callId").to_string();
                let name = field(part, "name").to_string();
                shown.tools.insert(id.clone(), name.clone());
                let args = part.get("arguments").cloned().unwrap_or(Value::Null);
                events.push(AgentEvent::ToolCallStarted(ToolCall { id, name, args }));
            }
            "tool_result" => {
                let call_id = field(part, "callId").to_string();
                let output = match part.get("result") {
                    Some(Value::String(text)) => text.clone(),
                    Some(other) => other.to_string(),
                    None => String::new(),
                };
                events.push(AgentEvent::ToolCallFinished {
                    tool_name: shown.tools.get(&call_id).cloned().unwrap_or_default(),
                    call_id,
                    output,
                    is_error: part
                        .get("isError")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                });
            }
            "error" => events.push(AgentEvent::Error(field(part, "message").to_string())),
            _ => {}
        }
        shown.parts += 1;
        shown.streamed = 0;
    }
}

/// The agent of an attached UI: shows conversation `conversation` as it
/// changes and sends what the person types, until the UI hangs up.
///
/// A message the server will not take now (an answer is being written) is
/// reported as an error and ends the UI's turn; a sync that fails is
/// reported once and retried.
pub async fn remote_agent_task(
    client: RemoteClient,
    conversation: Uuid,
    mut requests: mpsc::Receiver<AgentRequest>,
    events: mpsc::Sender<AgentEvent>,
    questions: mpsc::Sender<QuestionRequest>,
) {
    let mut reader = Reader::new(conversation);
    let mut translator = EventTranslator::default();
    // One question per waiting call; aborting it withdraws it from the UI.
    let mut asking: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
    let mut tick = tokio::time::interval(ATTACH_INTERVAL);
    let mut failing: Option<String> = None;
    let mut model: Option<String> = None;
    loop {
        let mut out = Vec::new();
        tokio::select! {
            _ = tick.tick() => match reader.sync(&client).await {
                Ok(messages) => {
                    failing = None;
                    // The status bar shows the model the conversation answers
                    // with, not this machine's default.
                    if let Some(now) = reader.model().filter(|now| model.as_ref() != Some(now)) {
                        out.push(AgentEvent::ModelChanged(now.clone()));
                        model = Some(now);
                    }
                    out.extend(translator.events(&messages));
                    let approvals = translator.take_approvals();
                    for call_id in approvals.settled {
                        if let Some(question) = asking.remove(&call_id) {
                            question.abort();
                        }
                    }
                    for waiting in approvals.asked {
                        let call_id = waiting.call_id.clone();
                        let question = tokio::spawn(ask(
                            client.clone(),
                            conversation,
                            waiting,
                            questions.clone(),
                            events.clone(),
                        ));
                        asking.insert(call_id, question);
                    }
                }
                Err(e) => {
                    let message = format!("{e:#}");
                    if failing.as_deref() != Some(message.as_str()) {
                        out.push(AgentEvent::Warning(format!("cannot reach the conversation: {message}")));
                        failing = Some(message);
                    }
                }
            },
            request = requests.recv() => match request {
                None => return,
                Some(
                    AgentRequest::Submit { content: text, .. }
                    | AgentRequest::Resubmit { new_user_content: text, .. },
                ) => {
                    translator.sent(&text);
                    if let Err(e) = client.send_to_conversation(conversation, &text).await {
                        translator.own.pop_back();
                        out.push(AgentEvent::Error(format!("{e:#}")));
                        out.push(AgentEvent::TurnComplete);
                    }
                }
                // History is the conversation's own; titles, tools and
                // models belong to the machine it runs on.
                Some(_) => {}
            },
        }
        for event in out {
            if events.send(event).await.is_err() {
                return;
            }
        }
    }
}

/// Asks the person whether `waiting` may run and tells the server. A
/// question the UI drops unanswered decides nothing.
async fn ask(
    client: RemoteClient,
    conversation: Uuid,
    waiting: Waiting,
    questions: mpsc::Sender<QuestionRequest>,
    events: mpsc::Sender<AgentEvent>,
) {
    let (answer_tx, answer_rx) = oneshot::channel();
    let request = QuestionRequest {
        id: waiting.call_id.clone(),
        questions: vec![Question {
            prompt: format!(
                "Allow {}?\n\nArguments: {}",
                waiting.name, waiting.arguments
            ),
            options: vec!["yes".to_string(), "no".to_string()],
            allow_multiple: false,
        }],
        answer_tx,
    };
    if questions.send(request).await.is_err() {
        return;
    }
    let Ok(answer) = answer_rx.await else { return };
    let approve = answer.trim().eq_ignore_ascii_case("yes");
    let report = match client
        .decide_approval(conversation, &waiting.call_id, approve)
        .await
    {
        Ok(true) => return,
        Ok(false) => AgentEvent::Warning(format!(
            "{} was already decided, or its answer ended",
            waiting.name
        )),
        Err(e) => AgentEvent::Error(format!("could not decide {}: {e:#}", waiting.name)),
    };
    let _ = events.send(report).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str, role: &str, state: &str, parts: Value) -> DocMessage {
        DocMessage {
            id: id.into(),
            role: role.into(),
            author: "a".into(),
            state: state.into(),
            parts: parts.to_string(),
        }
    }

    #[test]
    fn an_answer_streams_its_text_shows_its_tools_and_ends_once() {
        let mut translator = EventTranslator::default();
        let ask = message(
            "m1",
            "user",
            "complete",
            serde_json::json!([{"type": "text", "text": "list files"}]),
        );
        let writing = |state: &str, parts: Value| message("m2", "assistant", state, parts);

        assert_eq!(
            translator.events(&[
                ask.clone(),
                writing(
                    "streaming",
                    serde_json::json!([{"type": "text", "text": "Look"}])
                )
            ]),
            vec![
                AgentEvent::UserMessage("list files".into()),
                AgentEvent::TextDelta("Look".into())
            ]
        );
        let tools = serde_json::json!([
            {"type": "text", "text": "Looking."},
            {"type": "tool_call", "callId": "c1", "name": "list_dir", "arguments": {"path": "."}},
            {"type": "tool_result", "callId": "c1", "result": "README.md"},
        ]);
        assert_eq!(
            translator.events(&[ask.clone(), writing("streaming", tools.clone())]),
            vec![
                AgentEvent::TextDelta("ing.".into()),
                AgentEvent::TextComplete("Looking.".into()),
                AgentEvent::ToolCallStarted(ToolCall {
                    id: "c1".into(),
                    name: "list_dir".into(),
                    args: serde_json::json!({"path": "."})
                }),
                AgentEvent::ToolCallFinished {
                    call_id: "c1".into(),
                    tool_name: "list_dir".into(),
                    output: "README.md".into(),
                    is_error: false
                },
            ]
        );
        let mut done = tools.as_array().unwrap().clone();
        done.push(serde_json::json!({"type": "text", "text": "One file."}));
        let finished = writing("complete", Value::Array(done));
        assert_eq!(
            translator.events(&[ask.clone(), finished.clone()]),
            vec![
                AgentEvent::TextDelta("One file.".into()),
                AgentEvent::TextComplete("One file.".into()),
                AgentEvent::TurnComplete,
            ]
        );
        assert!(
            translator.events(&[ask, finished]).is_empty(),
            "nothing new, nothing shown"
        );
    }

    #[test]
    fn a_message_sent_from_here_is_not_shown_twice_but_one_from_elsewhere_is() {
        let mut translator = EventTranslator::default();
        translator.sent("run the tests ");
        let mine = message(
            "m1",
            "user",
            "complete",
            serde_json::json!([{"type": "text", "text": "run the tests"}]),
        );
        let theirs = message(
            "m2",
            "user",
            "complete",
            serde_json::json!([{"type": "text", "text": "and lint"}]),
        );
        assert_eq!(
            translator.events(&[mine, theirs]),
            vec![AgentEvent::UserMessage("and lint".into())]
        );
    }

    #[test]
    fn a_failed_answer_reports_its_error() {
        let mut translator = EventTranslator::default();
        let failed = message(
            "m1",
            "assistant",
            "failed",
            serde_json::json!([
                {"type": "approval", "callId": "c1", "name": "shell", "status": "pending"},
                {"type": "error", "code": "workspace_offline", "message": "the machine went offline"},
            ]),
        );
        let events = translator.events(&[failed]);
        assert_eq!(
            events,
            vec![
                AgentEvent::Error("the machine went offline".into()),
                AgentEvent::TurnComplete
            ]
        );
        assert_eq!(
            translator.take_approvals(),
            Approvals::default(),
            "an ended answer waits for nothing"
        );
    }

    #[tokio::test]
    async fn the_task_turns_the_servers_document_into_the_uis_events() {
        use sven_session_sync::ConversationDoc;
        let id = Uuid::new_v4();
        let server = std::sync::Arc::new(ConversationDoc::with_client_id(1));
        server.add_message(&message(
            "m1",
            "user",
            "complete",
            serde_json::json!([{"type": "text", "text": "hi"}]),
        ));
        server.add_message(&message(
            "m2",
            "assistant",
            "complete",
            serde_json::json!([{"type": "text", "text": "Hello."}]),
        ));
        server.set_meta("model", "some/model");
        let control = crate::transcript::over_http::serve(id, server, 8).await;
        let client = RemoteClient::new(&control, "k").unwrap();
        let (_requests, requests_rx) = mpsc::channel(4);
        let (events_tx, mut events) = mpsc::channel(16);
        let (questions, _questions_rx) = mpsc::channel(4);
        let task = tokio::spawn(remote_agent_task(
            client,
            id,
            requests_rx,
            events_tx,
            questions,
        ));
        let mut seen = Vec::new();
        while seen.last() != Some(&AgentEvent::TurnComplete) {
            seen.push(
                tokio::time::timeout(Duration::from_secs(5), events.recv())
                    .await
                    .expect("the task reports the conversation")
                    .expect("the task is running"),
            );
        }
        task.abort();
        assert_eq!(
            seen,
            vec![
                AgentEvent::ModelChanged("some/model".into()),
                AgentEvent::UserMessage("hi".into()),
                AgentEvent::TextDelta("Hello.".into()),
                AgentEvent::TextComplete("Hello.".into()),
                AgentEvent::TurnComplete,
            ]
        );
    }

    #[test]
    fn a_call_is_asked_about_once_while_it_waits_and_settled_when_it_stops() {
        let mut translator = EventTranslator::default();
        let approval = |status: &str| {
            serde_json::json!([{"type": "approval", "callId": "c1", "name": "shell",
                                "arguments": {"command": "make deploy"}, "status": status}])
        };
        translator.events(&[message("m1", "assistant", "streaming", approval("pending"))]);
        assert_eq!(
            translator.take_approvals(),
            Approvals {
                asked: vec![Waiting {
                    call_id: "c1".into(),
                    name: "shell".into(),
                    arguments: serde_json::json!({"command": "make deploy"})
                }],
                settled: vec![]
            }
        );
        translator.events(&[message("m1", "assistant", "streaming", approval("pending"))]);
        assert_eq!(
            translator.take_approvals(),
            Approvals::default(),
            "asked once"
        );
        translator.events(&[message(
            "m1",
            "assistant",
            "streaming",
            approval("approved"),
        )]);
        assert_eq!(
            translator.take_approvals(),
            Approvals {
                asked: vec![],
                settled: vec!["c1".into()]
            }
        );
    }

    #[tokio::test]
    async fn a_waiting_call_is_asked_in_the_ui_and_the_answer_reaches_the_server() {
        use sven_session_sync::ConversationDoc;
        let id = Uuid::new_v4();
        let server = std::sync::Arc::new(ConversationDoc::with_client_id(1));
        server.add_message(&message(
            "m1",
            "assistant",
            "streaming",
            serde_json::json!([{"type": "approval", "callId": "c1", "name": "shell",
                                "arguments": {"command": "make deploy"}, "status": "pending"}]),
        ));
        let (control, decisions) =
            crate::transcript::over_http::serve_recording(id, server, 64).await;
        let client = RemoteClient::new(&control, "k").unwrap();
        let (_requests, requests_rx) = mpsc::channel(4);
        let (events_tx, _events) = mpsc::channel(64);
        let (questions_tx, mut questions) = mpsc::channel(4);
        let task = tokio::spawn(remote_agent_task(
            client,
            id,
            requests_rx,
            events_tx,
            questions_tx,
        ));

        let question = tokio::time::timeout(Duration::from_secs(5), questions.recv())
            .await
            .expect("the UI is asked")
            .expect("the task is running");
        assert!(question.questions[0].prompt.contains("shell"));
        assert!(question.questions[0].prompt.contains("make deploy"));
        assert_eq!(question.questions[0].options, vec!["yes", "no"]);
        question.answer_tx.send("yes".into()).unwrap();
        for _ in 0..50 {
            if !decisions.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        task.abort();
        let decided: Vec<Value> = decisions
            .lock()
            .unwrap()
            .iter()
            .map(|body| serde_json::from_str(body).unwrap())
            .collect();
        assert_eq!(
            decided,
            vec![serde_json::json!({"callId": "c1", "approve": true})]
        );
    }
}
