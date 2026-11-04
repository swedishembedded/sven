// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a session asks a person about a tool call only when the user asked
//! for manual approval.
//!
//! Driven through the session the TUI runs (`RuntimeBuilder` +
//! `KernelAgentSession`): by default every call the mode allows runs with no
//! prompt; under manual approval every call that is not read-only is put to
//! the person, one call at a time, showing the tool and its command; the
//! mode's capability ceiling applies either way; and an explicit question
//! still reaches the person in an interactive session.
//!
//! Swedish Embedded AB implements human-in-the-loop agent runtimes for its
//! clients. If your team needs expertise in approval models for autonomous
//! agents then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sven_bootstrap::Config;
use sven_bootstrap::{BuiltinTools, KernelAgentSession, RuntimeBuilder};
use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_tool_api::{ApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput};
use sven_tools_agent::QuestionRequest;
use sven_vocab::{AgentMode, ApprovalMode};
use tokio::sync::mpsc;

/// A tool that records how often it ran.
struct Recorder {
    name: &'static str,
    capability: ToolCapability,
    ran: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Tool for Recorder {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "records that it ran"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Ask
    }
    fn kernel_capability(&self) -> ToolCapability {
        self.capability
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        self.ran.fetch_add(1, Ordering::SeqCst);
        ToolOutput::ok(&call.id, "done")
    }
}

/// What a session did with one scripted turn.
struct Outcome {
    /// Runs of the recording tool.
    ran: usize,
    /// Every prompt the person was shown.
    prompts: Vec<String>,
}

fn call(id: &str, tool: &str, args: serde_json::Value) -> Vec<ResponseEvent> {
    vec![
        ResponseEvent::ToolCall {
            index: 0,
            id: id.into(),
            name: tool.into(),
            arguments: args.to_string(),
        },
        ResponseEvent::Done,
    ]
}

fn finish() -> Vec<ResponseEvent> {
    vec![
        ResponseEvent::TextDelta("finished".into()),
        ResponseEvent::Done,
    ]
}

/// Runs one turn in which the model makes `calls` (one per round) to a
/// recording `tool`, in `mode` under `approval`, with the person answering
/// every prompt `answer`.
async fn session(
    approval: ApprovalMode,
    mode: AgentMode,
    tool: (&'static str, ToolCapability),
    calls: usize,
    answer: &'static str,
) -> Outcome {
    let commands: Vec<String> = (0..calls).map(|i| format!("make step{i}")).collect();
    session_running(approval, mode, tool, &commands, answer).await
}

/// [`session`] with the model running each of `commands` in turn.
async fn session_running(
    approval: ApprovalMode,
    mode: AgentMode,
    tool: (&'static str, ToolCapability),
    commands: &[String],
    answer: &'static str,
) -> Outcome {
    let mut scripts: Vec<_> = commands
        .iter()
        .enumerate()
        .map(|(i, command)| {
            call(
                &format!("c{i}"),
                tool.0,
                serde_json::json!({"shell_command": command, "path": "src/lib.rs"}),
            )
        })
        .collect();
    scripts.push(finish());
    let ran = Arc::new(AtomicUsize::new(0));
    let bundle = RuntimeBuilder::new(Arc::new(Config::default()), "agent")
        .with_agent_mode(mode)
        .with_approval_mode(approval)
        .with_builtin_tools(BuiltinTools::None)
        .with_extra_tools(vec![Arc::new(Recorder {
            name: tool.0,
            capability: tool.1,
            ran: Arc::clone(&ran),
        }) as Arc<dyn Tool>])
        .with_model_provider(Box::new(ScriptedMockProvider::new(scripts)))
        .build_session()
        .await
        .expect("a session builds");

    let (event_tx, mut event_rx) = mpsc::channel(256);
    let (question_tx, mut question_rx) = mpsc::channel::<QuestionRequest>(8);
    let (session, _mcp) = KernelAgentSession::spawn(bundle, event_tx, question_tx);
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&prompts);
    tokio::spawn(async move {
        while let Some(request) = question_rx.recv().await {
            seen.lock()
                .unwrap()
                .push(request.questions[0].prompt.clone());
            let _ = request.answer_tx.send(answer.to_string());
        }
    });

    assert!(session.send_user_message("go".into()).await);
    tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(event) = event_rx.recv().await {
            if matches!(event, sven_machines::AgentEvent::TurnComplete) {
                break;
            }
        }
    })
    .await
    .expect("the turn completes without waiting for anyone");
    let prompts = prompts.lock().unwrap().clone();
    Outcome {
        ran: ran.load(Ordering::SeqCst),
        prompts,
    }
}

#[tokio::test]
async fn by_default_shell_and_write_calls_run_without_a_prompt() {
    for tool in [
        ("shell", ToolCapability::ExecuteShell),
        ("write_file", ToolCapability::WriteFile),
    ] {
        let outcome = session(ApprovalMode::Auto, AgentMode::Agent, tool, 1, "no").await;
        assert_eq!(outcome.ran, 1, "{}", tool.0);
        assert!(outcome.prompts.is_empty(), "{:?}", outcome.prompts);
    }
}

#[tokio::test]
async fn manual_approval_puts_each_shell_call_to_the_person() {
    let outcome = session(
        ApprovalMode::Manual,
        AgentMode::Agent,
        ("shell", ToolCapability::ExecuteShell),
        2,
        "yes",
    )
    .await;
    assert_eq!(outcome.ran, 2, "approved, so both ran");
    assert_eq!(
        outcome.prompts.len(),
        2,
        "one prompt per call, not per capability"
    );
    assert!(
        outcome.prompts[0].contains("shell") && outcome.prompts[0].contains("make step0"),
        "the prompt shows the tool and its command: {}",
        outcome.prompts[0]
    );
    assert!(outcome.prompts[1].contains("make step1"));

    let outcome = session(
        ApprovalMode::Manual,
        AgentMode::Agent,
        ("shell", ToolCapability::ExecuteShell),
        1,
        "no",
    )
    .await;
    assert_eq!(outcome.ran, 0, "denied, so it never ran");
    assert_eq!(outcome.prompts.len(), 1);
}

/// `tools.auto_approve_patterns` (by default `ls *`, `cat *`, `grep *`, ...)
/// are the shell commands manual approval does not ask about.
#[tokio::test]
async fn manual_approval_does_not_ask_about_an_auto_approved_command() {
    let outcome = session_running(
        ApprovalMode::Manual,
        AgentMode::Agent,
        ("shell", ToolCapability::ExecuteShell),
        &["ls -la".to_string(), "rm -rf build".to_string()],
        "yes",
    )
    .await;
    assert_eq!(outcome.ran, 2);
    assert_eq!(outcome.prompts.len(), 1, "{:?}", outcome.prompts);
    assert!(outcome.prompts[0].contains("rm -rf build"));
}

#[tokio::test]
async fn manual_approval_does_not_ask_about_a_read_only_call() {
    let outcome = session(
        ApprovalMode::Manual,
        AgentMode::Agent,
        ("read_file", ToolCapability::ReadFile),
        1,
        "no",
    )
    .await;
    assert_eq!(outcome.ran, 1);
    assert!(outcome.prompts.is_empty(), "{:?}", outcome.prompts);
}

#[tokio::test]
async fn the_mode_ceiling_still_refuses_a_write_in_research_mode() {
    for approval in [ApprovalMode::Auto, ApprovalMode::Manual] {
        let outcome = session(
            approval,
            AgentMode::Research,
            ("write_file", ToolCapability::WriteFile),
            1,
            "yes",
        )
        .await;
        assert_eq!(outcome.ran, 0, "{approval}: research mode never writes");
        assert!(outcome.prompts.is_empty(), "refused, not asked about");
    }
}

/// An interactive session still routes the model's explicit question to the
/// person, and the answer is what the model reads.
#[tokio::test]
async fn an_interactive_session_puts_an_explicit_question_to_the_person() {
    let (question_tx, mut question_rx) = mpsc::channel::<QuestionRequest>(8);
    let bundle = RuntimeBuilder::new(Arc::new(Config::default()), "agent")
        .with_builtin_tools(BuiltinTools::None)
        .with_extra_tools(vec![Arc::new(sven_tools_agent::AskQuestionTool::new_tui(
            question_tx.clone(),
        )) as Arc<dyn Tool>])
        .with_model_provider(Box::new(ScriptedMockProvider::new(vec![
            call(
                "q1",
                "ask_question",
                serde_json::json!({"questions": [
                    {"prompt": "Which framework?", "options": ["Axum", "Actix"]}
                ]}),
            ),
            finish(),
        ])))
        .build_session()
        .await
        .expect("a session builds");
    let (event_tx, mut event_rx) = mpsc::channel(256);
    let (session, _mcp) = KernelAgentSession::spawn(bundle, event_tx, question_tx);
    assert!(session.send_user_message("start".into()).await);

    let request = tokio::time::timeout(Duration::from_secs(20), question_rx.recv())
        .await
        .expect("the person is asked")
        .expect("question channel open");
    assert_eq!(request.questions[0].prompt, "Which framework?");
    request.answer_tx.send("Axum".into()).unwrap();

    let answered = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(event) = event_rx.recv().await {
            if let sven_machines::AgentEvent::ToolCallFinished { output, .. } = &event {
                return output.clone();
            }
        }
        String::new()
    })
    .await
    .expect("the call finishes");
    assert_eq!(answered, "Axum");
}

/// Manual approval asks by what a call does, not by the tool's name: the
/// in-session todo list and reading memory are not asked about, writing
/// memory is.
#[tokio::test]
async fn manual_approval_asks_about_a_memory_write_but_not_the_todo_list() {
    let dir = tempfile::tempdir().unwrap();
    let memory_file = dir.path().join("memory.json").display().to_string();
    let (event_tx, _events) = mpsc::channel(16);
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(sven_tools_agent::TodoTool::new(
            Arc::new(tokio::sync::Mutex::new(Vec::new())),
            event_tx,
        )),
        Arc::new(sven_bootstrap::MemoryTool::new(
            Some(memory_file),
            sven_workspace::SharedKnowledge::empty(),
        )),
    ];
    let scripts = vec![
        call(
            "t1",
            "todo",
            serde_json::json!({"action": "set", "todos": [
                {"id": "1", "content": "build", "status": "pending"}
            ]}),
        ),
        call(
            "m1",
            "memory",
            serde_json::json!({"action": "set", "key": "k", "value": "v"}),
        ),
        call(
            "m2",
            "memory",
            serde_json::json!({"action": "get", "key": "k"}),
        ),
        finish(),
    ];
    let bundle = RuntimeBuilder::new(Arc::new(Config::default()), "agent")
        .with_approval_mode(ApprovalMode::Manual)
        .with_builtin_tools(BuiltinTools::None)
        .with_extra_tools(tools)
        .with_model_provider(Box::new(ScriptedMockProvider::new(scripts)))
        .build_session()
        .await
        .expect("a session builds");
    let (event_tx, mut event_rx) = mpsc::channel(256);
    let (question_tx, mut question_rx) = mpsc::channel::<QuestionRequest>(8);
    let (session, _mcp) = KernelAgentSession::spawn(bundle, event_tx, question_tx);
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&prompts);
    tokio::spawn(async move {
        while let Some(request) = question_rx.recv().await {
            seen.lock()
                .unwrap()
                .push(request.questions[0].prompt.clone());
            let _ = request.answer_tx.send("yes".into());
        }
    });
    assert!(session.send_user_message("go".into()).await);
    tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(event) = event_rx.recv().await {
            if matches!(event, sven_machines::AgentEvent::TurnComplete) {
                break;
            }
        }
    })
    .await
    .expect("the turn completes");
    let prompts = prompts.lock().unwrap().clone();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(
        prompts[0].contains("memory") && prompts[0].contains("WriteFile"),
        "{}",
        prompts[0]
    );
}

/// A ui-test step runs dispatched and its machine does not hold a call for
/// an approval: manual approval is refused when the session is built, never
/// left to stall at the first device call.
#[cfg(feature = "android")]
#[tokio::test]
async fn manual_approval_of_a_ui_test_session_is_refused_at_build() {
    let err = RuntimeBuilder::new(Arc::new(Config::default()), "ui-test")
        .with_approval_mode(ApprovalMode::Manual)
        .with_builtin_tools(BuiltinTools::None)
        .with_model_provider(Box::new(ScriptedMockProvider::new(vec![])))
        .build_session()
        .await
        .err()
        .expect("refused");
    assert!(err.to_string().contains("ui-test"), "{err}");
}
