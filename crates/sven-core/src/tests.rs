/// Comprehensive tests for the Agent agentic loop.
///
/// Uses ScriptedMockProvider so every scenario is deterministic and
/// requires no network access.
#[cfg(test)]
mod agent_tests {
    use std::sync::Arc;

    use sven_config::{AgentConfig, AgentMode};
    use sven_model::{ResponseEvent, ScriptedMockProvider};
    use sven_tools::{FsTool, ShellTool, ToolRegistry};
    use tokio::sync::mpsc;

    use crate::{Agent, AgentEvent};

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn agent_with(
        model: ScriptedMockProvider,
        tools: ToolRegistry,
        config: AgentConfig,
        mode: AgentMode,
    ) -> Agent {
        Agent::new(Arc::new(model), Arc::new(tools), Arc::new(config), mode, 128_000)
    }

    fn default_agent(model: ScriptedMockProvider) -> Agent {
        agent_with(model, ToolRegistry::default(), AgentConfig::default(), AgentMode::Agent)
    }

    /// Drain the channel into a Vec of events, waiting for TurnComplete or channel close.
    async fn collect_events(mut rx: mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            let done = matches!(ev, AgentEvent::TurnComplete);
            events.push(ev);
            if done { break; }
        }
        events
    }

    // ── Basic text turn ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn single_text_turn_emits_text_delta_and_complete() {
        let model = ScriptedMockProvider::always_text("hello from agent");
        let mut agent = default_agent(model);
        let (tx, rx) = mpsc::channel(64);

        agent.submit("hi", tx).await.unwrap();
        let events = collect_events(rx).await;

        let has_delta = events.iter().any(|e| matches!(e, AgentEvent::TextDelta(t) if t.contains("hello")));
        let has_complete = events.iter().any(|e| matches!(e, AgentEvent::TurnComplete));
        assert!(has_delta, "should have emitted at least one TextDelta");
        assert!(has_complete, "should have emitted TurnComplete");
    }

    #[tokio::test]
    async fn text_complete_event_contains_full_response() {
        let model = ScriptedMockProvider::always_text("full response text");
        let mut agent = default_agent(model);
        let (tx, rx) = mpsc::channel(64);

        agent.submit("hi", tx).await.unwrap();
        let events = collect_events(rx).await;

        let complete_text = events.iter().find_map(|e| {
            if let AgentEvent::TextComplete(t) = e { Some(t.as_str()) } else { None }
        });
        assert_eq!(complete_text, Some("full response text"));
    }

    // ── Session history ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn system_message_injected_on_first_turn() {
        let model = ScriptedMockProvider::always_text("ok");
        let mut agent = default_agent(model);
        let (tx, rx) = mpsc::channel(64);

        agent.submit("go", tx).await.unwrap();
        let _ = collect_events(rx).await;

        let msgs = &agent.session().messages;
        assert!(msgs[0].role == sven_model::Role::System, "first message must be system");
    }

    #[tokio::test]
    async fn user_message_appended_to_session() {
        let model = ScriptedMockProvider::always_text("reply");
        let mut agent = default_agent(model);
        let (tx, rx) = mpsc::channel(64);

        agent.submit("my question", tx).await.unwrap();
        let _ = collect_events(rx).await;

        let msgs = &agent.session().messages;
        let user_msg = msgs.iter().find(|m| m.role == sven_model::Role::User);
        assert!(user_msg.is_some());
        assert_eq!(user_msg.unwrap().as_text(), Some("my question"));
    }

    #[tokio::test]
    async fn assistant_reply_appended_to_session() {
        let model = ScriptedMockProvider::always_text("my reply");
        let mut agent = default_agent(model);
        let (tx, rx) = mpsc::channel(64);

        agent.submit("q", tx).await.unwrap();
        let _ = collect_events(rx).await;

        let msgs = &agent.session().messages;
        let asst = msgs.iter().find(|m| m.role == sven_model::Role::Assistant);
        assert!(asst.is_some());
        assert!(asst.unwrap().as_text().unwrap().contains("my reply"));
    }

    // ── Tool call round-trip ──────────────────────────────────────────────────

    #[tokio::test]
    async fn tool_call_started_event_emitted() {
        let model = ScriptedMockProvider::tool_then_text(
            "tc-1", "shell", r#"{"command":"echo ok"}"#, "done",
        );
        let mut reg = ToolRegistry::new();
        reg.register(ShellTool::default());
        let mut agent = agent_with(model, reg, AgentConfig::default(), AgentMode::Agent);
        let (tx, rx) = mpsc::channel(64);

        agent.submit("run something", tx).await.unwrap();
        let events = collect_events(rx).await;

        let started = events.iter().any(|e| matches!(e, AgentEvent::ToolCallStarted(tc) if tc.name == "shell"));
        assert!(started, "should emit ToolCallStarted for shell tool");
    }

    #[tokio::test]
    async fn tool_call_finished_event_emitted() {
        let model = ScriptedMockProvider::tool_then_text(
            "tc-1", "shell", r#"{"command":"echo finished"}"#, "done",
        );
        let mut reg = ToolRegistry::new();
        reg.register(ShellTool::default());
        let mut agent = agent_with(model, reg, AgentConfig::default(), AgentMode::Agent);
        let (tx, rx) = mpsc::channel(64);

        agent.submit("run it", tx).await.unwrap();
        let events = collect_events(rx).await;

        let finished = events.iter().any(|e| matches!(e, AgentEvent::ToolCallFinished { tool_name, .. } if tool_name == "shell"));
        assert!(finished, "should emit ToolCallFinished");
    }

    #[tokio::test]
    async fn tool_output_included_in_finished_event() {
        let model = ScriptedMockProvider::tool_then_text(
            "tc-1", "shell", r#"{"command":"echo expected_output"}"#, "done",
        );
        let mut reg = ToolRegistry::new();
        reg.register(ShellTool::default());
        let mut agent = agent_with(model, reg, AgentConfig::default(), AgentMode::Agent);
        let (tx, rx) = mpsc::channel(64);

        agent.submit("run", tx).await.unwrap();
        let events = collect_events(rx).await;

        let output = events.iter().find_map(|e| {
            if let AgentEvent::ToolCallFinished { output, .. } = e { Some(output.as_str()) } else { None }
        });
        assert!(output.is_some());
        assert!(output.unwrap().contains("expected_output"),
            "tool output should contain the echoed text");
    }

    #[tokio::test]
    async fn tool_result_appended_to_session_history() {
        let model = ScriptedMockProvider::tool_then_text(
            "tc-1", "shell", r#"{"command":"echo hi"}"#, "done",
        );
        let mut reg = ToolRegistry::new();
        reg.register(ShellTool::default());
        let mut agent = agent_with(model, reg, AgentConfig::default(), AgentMode::Agent);
        let (tx, rx) = mpsc::channel(64);

        agent.submit("run", tx).await.unwrap();
        let _ = collect_events(rx).await;

        let has_tool_result = agent.session().messages.iter().any(|m| m.role == sven_model::Role::Tool);
        assert!(has_tool_result, "tool result should be appended to session");
    }

    // ── File tool integration ─────────────────────────────────────────────────

    #[tokio::test]
    async fn fs_tool_write_via_agent_turn() {
        let path = format!("/tmp/sven_agent_test_{}.txt", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos());
        let args = format!(r#"{{"operation":"write","path":"{path}","content":"agent wrote this"}}"#);

        let model = ScriptedMockProvider::tool_then_text("fs-1", "fs", &args, "file written");
        let mut reg = ToolRegistry::new();
        reg.register(FsTool);
        let mut agent = agent_with(model, reg, AgentConfig::default(), AgentMode::Agent);
        let (tx, rx) = mpsc::channel(64);

        agent.submit("write the file", tx).await.unwrap();
        let _ = collect_events(rx).await;

        let content = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(content, "agent wrote this");
        let _ = std::fs::remove_file(&path);
    }

    // ── Max rounds enforcement ────────────────────────────────────────────────

    #[tokio::test]
    async fn max_rounds_emits_error_event() {
        // Scripted to always return a tool call – will exhaust rounds
        let scripts: Vec<Vec<ResponseEvent>> = (0..=5).map(|_| vec![
            ResponseEvent::ToolCall {
                id: "x".into(),
                name: "shell".into(),
                arguments: r#"{"command":"echo loop"}"#.into(),
            },
            ResponseEvent::Done,
        ]).collect();

        let model = ScriptedMockProvider::new(scripts);
        let config = AgentConfig { max_tool_rounds: 2, ..AgentConfig::default() };
        let mut reg = ToolRegistry::new();
        reg.register(ShellTool::default());
        let mut agent = agent_with(model, reg, config, AgentMode::Agent);
        let (tx, mut rx) = mpsc::channel(256);

        agent.submit("loop forever", tx).await.unwrap();

        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }
        // Drain any remaining
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }

        let has_error = events.iter().any(|e| matches!(e, AgentEvent::Error(msg) if msg.contains("max tool rounds")));
        assert!(has_error, "should emit Error event when max rounds exceeded; got: {events:?}");
    }

    // ── Token usage events ────────────────────────────────────────────────────

    #[tokio::test]
    async fn token_usage_event_emitted() {
        let model = ScriptedMockProvider::new(vec![vec![
            ResponseEvent::TextDelta("reply".into()),
            ResponseEvent::Usage { input_tokens: 42, output_tokens: 17 },
            ResponseEvent::Done,
        ]]);
        let mut agent = default_agent(model);
        let (tx, rx) = mpsc::channel(64);

        agent.submit("q", tx).await.unwrap();
        let events = collect_events(rx).await;

        let usage = events.iter().find_map(|e| {
            if let AgentEvent::TokenUsage { input, output, .. } = e { Some((*input, *output)) } else { None }
        });
        assert_eq!(usage, Some((42, 17)));
    }

    // ── Mode is accessible ────────────────────────────────────────────────────

    #[test]
    fn agent_mode_is_accessible() {
        let model = ScriptedMockProvider::always_text("x");
        let agent = agent_with(
            model, ToolRegistry::default(), AgentConfig::default(), AgentMode::Research,
        );
        assert_eq!(agent.mode(), AgentMode::Research);
    }

    // ── Multi-turn conversation ───────────────────────────────────────────────

    #[tokio::test]
    async fn second_turn_adds_to_existing_history() {
        let model = ScriptedMockProvider::new(vec![
            vec![ResponseEvent::TextDelta("first reply".into()), ResponseEvent::Done],
            vec![ResponseEvent::TextDelta("second reply".into()), ResponseEvent::Done],
        ]);
        let mut agent = default_agent(model);

        let (tx1, rx1) = mpsc::channel(64);
        agent.submit("turn one", tx1).await.unwrap();
        let _ = collect_events(rx1).await;

        let msgs_after_first = agent.session().messages.len();

        let (tx2, rx2) = mpsc::channel(64);
        agent.submit("turn two", tx2).await.unwrap();
        let _ = collect_events(rx2).await;

        assert!(agent.session().messages.len() > msgs_after_first,
            "second turn should append more messages");
    }
}
