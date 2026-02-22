// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
/// Comprehensive tests for the Agent agentic loop.
///
/// Uses ScriptedMockProvider so every scenario is deterministic and
/// requires no network access.
#[cfg(test)]
mod agent_tests {
    use std::sync::Arc;

    use sven_config::{AgentConfig, AgentMode};
    use sven_model::{ResponseEvent, ScriptedMockProvider};
    use sven_tools::{FsTool, ReadImageTool, ShellTool, ToolRegistry, events::ToolEvent};
    use tokio::sync::{mpsc, Mutex};

    use crate::{Agent, AgentEvent, AgentRuntimeContext};

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn agent_with(
        model: ScriptedMockProvider,
        tools: ToolRegistry,
        config: AgentConfig,
        mode: AgentMode,
    ) -> Agent {
        let mode_lock = Arc::new(Mutex::new(mode));
        let (_tx, tool_event_rx) = mpsc::channel::<ToolEvent>(64);
        Agent::new(Arc::new(model), Arc::new(tools), Arc::new(config), AgentRuntimeContext::default(), mode_lock, tool_event_rx, 128_000)
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
            ResponseEvent::Usage { input_tokens: 42, output_tokens: 17, cache_read_tokens: 0, cache_write_tokens: 0 },
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

    #[tokio::test]
    async fn cache_tokens_propagated_to_agent_event() {
        let model = ScriptedMockProvider::new(vec![vec![
            ResponseEvent::TextDelta("cached reply".into()),
            ResponseEvent::Usage {
                input_tokens: 1000,
                output_tokens: 50,
                cache_read_tokens: 800,
                cache_write_tokens: 200,
            },
            ResponseEvent::Done,
        ]]);
        let mut agent = default_agent(model);
        let (tx, rx) = mpsc::channel(64);

        agent.submit("q", tx).await.unwrap();
        let events = collect_events(rx).await;

        let cache = events.iter().find_map(|e| {
            if let AgentEvent::TokenUsage { cache_read, cache_write, .. } = e {
                Some((*cache_read, *cache_write))
            } else {
                None
            }
        });
        assert_eq!(cache, Some((800, 200)));
    }

    // ── Multimodal input ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn submit_with_parts_creates_content_parts_message() {
        use std::sync::Arc;
        use sven_model::{ContentPart, MessageContent};

        let mock = ScriptedMockProvider::always_text("ok").with_vision();
        let last_req = Arc::clone(&mock.last_request);
        let mut agent = agent_with(mock, ToolRegistry::default(), AgentConfig::default(), AgentMode::Agent);
        let (tx, rx) = mpsc::channel(64);

        agent.submit_with_parts(vec![
            ContentPart::Text { text: "describe this".into() },
            ContentPart::image("data:image/png;base64,abc="),
        ], tx).await.unwrap();
        let _ = collect_events(rx).await;

        // Inspect what was actually sent to the provider
        let req = last_req.lock().unwrap().take().unwrap();
        let user_msg = req.messages.iter().find(|m| m.role == sven_model::Role::User).unwrap();
        assert!(
            matches!(&user_msg.content, MessageContent::ContentParts(parts) if parts.len() == 2),
            "user message should have ContentParts with 2 parts; got: {:?}", user_msg.content
        );
    }

    #[tokio::test]
    async fn text_only_model_strips_images_before_send() {
        use std::sync::Arc;
        use sven_model::{ContentPart, MessageContent};

        // Default mock has no vision capability → images should be stripped
        let mock = ScriptedMockProvider::always_text("ok");
        let last_req = Arc::clone(&mock.last_request);
        let mut agent = agent_with(mock, ToolRegistry::default(), AgentConfig::default(), AgentMode::Agent);
        let (tx, rx) = mpsc::channel(64);

        agent.submit_with_parts(vec![
            ContentPart::Text { text: "what is in this image?".into() },
            ContentPart::image("data:image/png;base64,abc="),
        ], tx).await.unwrap();
        let _ = collect_events(rx).await;

        let req = last_req.lock().unwrap().take().unwrap();
        let user_msg = req.messages.iter().find(|m| m.role == sven_model::Role::User).unwrap();
        // The image should have been replaced with a text placeholder
        let text = match &user_msg.content {
            MessageContent::ContentParts(parts) => {
                parts.iter().filter_map(|p| if let ContentPart::Text { text } = p { Some(text.as_str()) } else { None })
                    .collect::<Vec<_>>()
                    .join(" ")
            }
            MessageContent::Text(t) => t.clone(),
            other => panic!("unexpected content: {other:?}"),
        };
        assert!(!text.contains("base64"), "raw base64 must not reach a text-only model");
        assert!(text.contains("omitted") || text.contains("[image"), "image should be replaced with placeholder; got: {text}");
    }

    #[tokio::test]
    async fn tool_result_with_image_stored_as_parts_in_session() {
        use std::io::Write;
        use sven_model::MessageContent;
        use sven_tools::ReadImageTool;

        // Write a valid 1×1 red PNG to a temp file (bytes verified by Python zlib)
        let tmp = tempfile::NamedTempFile::with_suffix(".png").unwrap();
        let png_bytes: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, // PNG signature
            0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1×1
            0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, // bit depth 8, RGB
            0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, // IDAT length + "IDAT"
            0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00, // compressed pixel (red)
            0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, // IDAT CRC
            0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, // IEND
            0x44, 0xae, 0x42, 0x60, 0x82,                   // IEND CRC
        ];
        tmp.as_file().write_all(png_bytes).unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        // Vision model so images are not stripped
        let mock = ScriptedMockProvider::tool_then_text(
            "ri-1", "read_image", &format!(r#"{{"file_path":"{path}"}}"#), "I see a red pixel",
        ).with_vision();
        let mut reg = ToolRegistry::new();
        reg.register(ReadImageTool);
        let mut agent = agent_with(mock, reg, AgentConfig::default(), AgentMode::Agent);
        let (tx, rx) = mpsc::channel(64);

        agent.submit("read the image file", tx).await.unwrap();
        let _ = collect_events(rx).await;

        let has_image_in_tool_result = agent.session().messages.iter().any(|m| {
            matches!(&m.content, MessageContent::ToolResult { content, .. }
                if matches!(content, sven_model::ToolResultContent::Parts(parts)
                    if parts.iter().any(|p| matches!(p, sven_model::ToolContentPart::Image { .. }))))
        });
        assert!(has_image_in_tool_result, "tool result should contain an image part in session history");
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
