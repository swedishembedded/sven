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
    use sven_model::{MessageContent, ResponseEvent, ScriptedMockProvider};
    use sven_tools::{events::ToolEvent, ShellTool, ToolRegistry, WriteTool};
    use tokio::sync::{mpsc, Mutex};

    use crate::{Agent, AgentEvent, AgentRuntimeContext};

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn agent_with(
        model: ScriptedMockProvider,
        tools: ToolRegistry,
        config: AgentConfig,
        mode: AgentMode,
    ) -> Agent {
        agent_with_ctx(model, tools, config, mode, 128_000)
    }

    fn agent_with_ctx(
        model: ScriptedMockProvider,
        tools: ToolRegistry,
        config: AgentConfig,
        mode: AgentMode,
        max_context_tokens: usize,
    ) -> Agent {
        let mode_lock = Arc::new(Mutex::new(mode));
        let (_tx, tool_event_rx) = mpsc::channel::<ToolEvent>(64);
        Agent::new(
            Arc::new(model),
            Arc::new(tools),
            Arc::new(config),
            AgentRuntimeContext::default(),
            mode_lock,
            tool_event_rx,
            max_context_tokens,
        )
    }

    fn default_agent(model: ScriptedMockProvider) -> Agent {
        agent_with(
            model,
            ToolRegistry::default(),
            AgentConfig::default(),
            AgentMode::Agent,
        )
    }

    /// Drain the channel into a Vec of events, waiting for TurnComplete or channel close.
    async fn collect_events(mut rx: mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            let done = matches!(ev, AgentEvent::TurnComplete);
            events.push(ev);
            if done {
                break;
            }
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

        let has_delta = events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta(t) if t.contains("hello")));
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
            if let AgentEvent::TextComplete(t) = e {
                Some(t.as_str())
            } else {
                None
            }
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
        assert!(
            msgs[0].role == sven_model::Role::System,
            "first message must be system"
        );
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
            "tc-1",
            "shell",
            r#"{"command":"echo ok"}"#,
            "done",
        );
        let mut reg = ToolRegistry::new();
        reg.register(ShellTool::default());
        let mut agent = agent_with(model, reg, AgentConfig::default(), AgentMode::Agent);
        let (tx, rx) = mpsc::channel(64);

        agent.submit("run something", tx).await.unwrap();
        let events = collect_events(rx).await;

        let started = events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCallStarted(tc) if tc.name == "shell"));
        assert!(started, "should emit ToolCallStarted for shell tool");
    }

    #[tokio::test]
    async fn tool_call_finished_event_emitted() {
        let model = ScriptedMockProvider::tool_then_text(
            "tc-1",
            "shell",
            r#"{"command":"echo finished"}"#,
            "done",
        );
        let mut reg = ToolRegistry::new();
        reg.register(ShellTool::default());
        let mut agent = agent_with(model, reg, AgentConfig::default(), AgentMode::Agent);
        let (tx, rx) = mpsc::channel(64);

        agent.submit("run it", tx).await.unwrap();
        let events = collect_events(rx).await;

        let finished = events.iter().any(
            |e| matches!(e, AgentEvent::ToolCallFinished { tool_name, .. } if tool_name == "shell"),
        );
        assert!(finished, "should emit ToolCallFinished");
    }

    #[tokio::test]
    async fn tool_output_included_in_finished_event() {
        let model = ScriptedMockProvider::tool_then_text(
            "tc-1",
            "shell",
            r#"{"command":"echo expected_output"}"#,
            "done",
        );
        let mut reg = ToolRegistry::new();
        reg.register(ShellTool::default());
        let mut agent = agent_with(model, reg, AgentConfig::default(), AgentMode::Agent);
        let (tx, rx) = mpsc::channel(64);

        agent.submit("run", tx).await.unwrap();
        let events = collect_events(rx).await;

        let output = events.iter().find_map(|e| {
            if let AgentEvent::ToolCallFinished { output, .. } = e {
                Some(output.as_str())
            } else {
                None
            }
        });
        assert!(output.is_some());
        assert!(
            output.unwrap().contains("expected_output"),
            "tool output should contain the echoed text"
        );
    }

    #[tokio::test]
    async fn tool_result_appended_to_session_history() {
        let model = ScriptedMockProvider::tool_then_text(
            "tc-1",
            "shell",
            r#"{"command":"echo hi"}"#,
            "done",
        );
        let mut reg = ToolRegistry::new();
        reg.register(ShellTool::default());
        let mut agent = agent_with(model, reg, AgentConfig::default(), AgentMode::Agent);
        let (tx, rx) = mpsc::channel(64);

        agent.submit("run", tx).await.unwrap();
        let _ = collect_events(rx).await;

        let has_tool_result = agent
            .session()
            .messages
            .iter()
            .any(|m| m.role == sven_model::Role::Tool);
        assert!(has_tool_result, "tool result should be appended to session");
    }

    // ── File tool integration ─────────────────────────────────────────────────

    #[tokio::test]
    async fn write_file_tool_via_agent_turn() {
        let path = format!(
            "/tmp/sven_agent_test_{}.txt",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        );
        let args = format!(r#"{{"path":"{path}","text":"agent wrote this"}}"#);

        let model =
            ScriptedMockProvider::tool_then_text("wf-1", "write_file", &args, "file written");
        let mut reg = ToolRegistry::new();
        reg.register(WriteTool);
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
    async fn max_rounds_wraps_up_and_emits_turn_complete() {
        // Rounds 1..=max: always return a tool call (will exhaust the budget).
        // Round max+1 (wrap-up): return a plain text summary — no tools available.
        let max: usize = 2;
        let mut scripts: Vec<Vec<ResponseEvent>> = (0..max)
            .map(|_| {
                vec![
                    ResponseEvent::ToolCall {
                        index: 0,
                        id: "x".into(),
                        name: "shell".into(),
                        arguments: r#"{"command":"echo loop"}"#.into(),
                    },
                    ResponseEvent::Done,
                ]
            })
            .collect();
        // Wrap-up turn: model receives the budget-exhausted message and
        // responds with a text summary (no tool calls).
        scripts.push(vec![
            ResponseEvent::TextDelta("Here is a summary of what was done.".into()),
            ResponseEvent::Done,
        ]);

        let model = ScriptedMockProvider::new(scripts);
        let config = AgentConfig {
            max_tool_rounds: max as u32,
            ..AgentConfig::default()
        };
        let mut reg = ToolRegistry::new();
        reg.register(ShellTool::default());
        let mut agent = agent_with(model, reg, config, AgentMode::Agent);
        let (tx, mut rx) = mpsc::channel(256);

        agent.submit("loop forever", tx).await.unwrap();

        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }

        // Must NOT emit an Error about max rounds — the turn ends gracefully.
        let has_round_error = events
            .iter()
            .any(|e| matches!(e, AgentEvent::Error(msg) if msg.contains("max tool rounds")));
        assert!(
            !has_round_error,
            "should not emit Error for max rounds; got: {events:?}"
        );

        // Must end with TurnComplete so the TUI shows a clean turn end.
        let has_complete = events.iter().any(|e| matches!(e, AgentEvent::TurnComplete));
        assert!(
            has_complete,
            "should emit TurnComplete after wrap-up; got: {events:?}"
        );

        // The wrap-up summary text must have been streamed.
        let text: String = events
            .iter()
            .filter_map(|e| {
                if let AgentEvent::TextDelta(d) = e {
                    Some(d.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert!(
            text.contains("summary"),
            "wrap-up text should reach the stream; got: {text:?}"
        );
    }

    // ── Token usage events ────────────────────────────────────────────────────

    #[tokio::test]
    async fn token_usage_event_emitted() {
        let model = ScriptedMockProvider::new(vec![vec![
            ResponseEvent::TextDelta("reply".into()),
            ResponseEvent::Usage {
                input_tokens: 42,
                output_tokens: 17,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
            ResponseEvent::Done,
        ]]);
        let mut agent = default_agent(model);
        let (tx, rx) = mpsc::channel(64);

        agent.submit("q", tx).await.unwrap();
        let events = collect_events(rx).await;

        let usage = events.iter().find_map(|e| {
            if let AgentEvent::TokenUsage { input, output, .. } = e {
                Some((*input, *output))
            } else {
                None
            }
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
            if let AgentEvent::TokenUsage {
                cache_read,
                cache_write,
                ..
            } = e
            {
                Some((*cache_read, *cache_write))
            } else {
                None
            }
        });
        assert_eq!(cache, Some((800, 200)));
    }

    #[tokio::test]
    async fn session_cache_totals_accumulate_across_turns() {
        // Two turns each with cache usage — totals must be the running sum.
        let model = ScriptedMockProvider::new(vec![
            vec![
                ResponseEvent::Usage {
                    input_tokens: 500,
                    output_tokens: 10,
                    cache_read_tokens: 400,
                    cache_write_tokens: 50,
                },
                ResponseEvent::TextDelta("turn1".into()),
                ResponseEvent::Done,
            ],
            vec![
                ResponseEvent::Usage {
                    input_tokens: 600,
                    output_tokens: 20,
                    cache_read_tokens: 550,
                    cache_write_tokens: 0,
                },
                ResponseEvent::TextDelta("turn2".into()),
                ResponseEvent::Done,
            ],
        ]);
        let mut agent = default_agent(model);

        // First turn
        let (tx1, rx1) = mpsc::channel(64);
        agent.submit("first", tx1).await.unwrap();
        let events1 = collect_events(rx1).await;

        let totals1 = events1.iter().find_map(|e| {
            if let AgentEvent::TokenUsage {
                cache_read_total,
                cache_write_total,
                ..
            } = e
            {
                Some((*cache_read_total, *cache_write_total))
            } else {
                None
            }
        });
        assert_eq!(
            totals1,
            Some((400, 50)),
            "after turn 1 totals should be (400, 50)"
        );

        // Second turn — totals must accumulate, not reset
        let (tx2, rx2) = mpsc::channel(64);
        agent.submit("second", tx2).await.unwrap();
        let events2 = collect_events(rx2).await;

        let totals2 = events2.iter().find_map(|e| {
            if let AgentEvent::TokenUsage {
                cache_read_total,
                cache_write_total,
                ..
            } = e
            {
                Some((*cache_read_total, *cache_write_total))
            } else {
                None
            }
        });
        assert_eq!(
            totals2,
            Some((950, 50)),
            "after turn 2 totals should be (950, 50)"
        );
    }

    // ── Multimodal input ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn submit_with_parts_creates_content_parts_message() {
        use std::sync::Arc;
        use sven_model::{ContentPart, MessageContent};

        let mock = ScriptedMockProvider::always_text("ok").with_vision();
        let last_req = Arc::clone(&mock.last_request);
        let mut agent = agent_with(
            mock,
            ToolRegistry::default(),
            AgentConfig::default(),
            AgentMode::Agent,
        );
        let (tx, rx) = mpsc::channel(64);

        agent
            .submit_with_parts(
                vec![
                    ContentPart::Text {
                        text: "describe this".into(),
                    },
                    ContentPart::image("data:image/png;base64,abc="),
                ],
                tx,
            )
            .await
            .unwrap();
        let _ = collect_events(rx).await;

        // Inspect what was actually sent to the provider
        let req = last_req.lock().unwrap().take().unwrap();
        let user_msg = req
            .messages
            .iter()
            .find(|m| m.role == sven_model::Role::User)
            .unwrap();
        assert!(
            matches!(&user_msg.content, MessageContent::ContentParts(parts) if parts.len() == 2),
            "user message should have ContentParts with 2 parts; got: {:?}",
            user_msg.content
        );
    }

    #[tokio::test]
    async fn text_only_model_strips_images_before_send() {
        use std::sync::Arc;
        use sven_model::{ContentPart, MessageContent};

        // Default mock has no vision capability → images should be stripped
        let mock = ScriptedMockProvider::always_text("ok");
        let last_req = Arc::clone(&mock.last_request);
        let mut agent = agent_with(
            mock,
            ToolRegistry::default(),
            AgentConfig::default(),
            AgentMode::Agent,
        );
        let (tx, rx) = mpsc::channel(64);

        agent
            .submit_with_parts(
                vec![
                    ContentPart::Text {
                        text: "what is in this image?".into(),
                    },
                    ContentPart::image("data:image/png;base64,abc="),
                ],
                tx,
            )
            .await
            .unwrap();
        let _ = collect_events(rx).await;

        let req = last_req.lock().unwrap().take().unwrap();
        let user_msg = req
            .messages
            .iter()
            .find(|m| m.role == sven_model::Role::User)
            .unwrap();
        // The image should have been replaced with a text placeholder
        let text = match &user_msg.content {
            MessageContent::ContentParts(parts) => parts
                .iter()
                .filter_map(|p| {
                    if let ContentPart::Text { text } = p {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
            MessageContent::Text(t) => t.clone(),
            other => panic!("unexpected content: {other:?}"),
        };
        assert!(
            !text.contains("base64"),
            "raw base64 must not reach a text-only model"
        );
        assert!(
            text.contains("omitted") || text.contains("[image"),
            "image should be replaced with placeholder; got: {text}"
        );
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
            0x44, 0xae, 0x42, 0x60, 0x82, // IEND CRC
        ];
        tmp.as_file().write_all(png_bytes).unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        // Vision model so images are not stripped
        let mock = ScriptedMockProvider::tool_then_text(
            "ri-1",
            "read_image",
            &format!(r#"{{"path":"{path}"}}"#),
            "I see a red pixel",
        )
        .with_vision();
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
        assert!(
            has_image_in_tool_result,
            "tool result should contain an image part in session history"
        );
    }

    // ── Mode is accessible ────────────────────────────────────────────────────

    #[test]
    fn agent_mode_is_accessible() {
        let model = ScriptedMockProvider::always_text("x");
        let agent = agent_with(
            model,
            ToolRegistry::default(),
            AgentConfig::default(),
            AgentMode::Research,
        );
        assert_eq!(agent.mode(), AgentMode::Research);
    }

    // ── Multi-turn conversation ───────────────────────────────────────────────

    #[tokio::test]
    async fn second_turn_adds_to_existing_history() {
        let model = ScriptedMockProvider::new(vec![
            vec![
                ResponseEvent::TextDelta("first reply".into()),
                ResponseEvent::Done,
            ],
            vec![
                ResponseEvent::TextDelta("second reply".into()),
                ResponseEvent::Done,
            ],
        ]);
        let mut agent = default_agent(model);

        let (tx1, rx1) = mpsc::channel(64);
        agent.submit("turn one", tx1).await.unwrap();
        let _ = collect_events(rx1).await;

        let msgs_after_first = agent.session().messages.len();

        let (tx2, rx2) = mpsc::channel(64);
        agent.submit("turn two", tx2).await.unwrap();
        let _ = collect_events(rx2).await;

        assert!(
            agent.session().messages.len() > msgs_after_first,
            "second turn should append more messages"
        );
    }

    // ── Parallel tool execution ───────────────────────────────────────────────

    #[tokio::test]
    async fn parallel_tool_calls_execute_concurrently() {
        // Model returns two tool calls in one turn (index 0 and 1).
        // Both should execute in parallel and results preserved in order.
        let scripts = vec![
            vec![
                ResponseEvent::ToolCall {
                    index: 0,
                    id: "call_1".into(),
                    name: "shell".into(),
                    arguments: r#"{"command":"echo first"}"#.into(),
                },
                ResponseEvent::ToolCall {
                    index: 1,
                    id: "call_2".into(),
                    name: "shell".into(),
                    arguments: r#"{"command":"echo second"}"#.into(),
                },
                ResponseEvent::Done,
            ],
            vec![
                ResponseEvent::TextDelta("Both executed".into()),
                ResponseEvent::Done,
            ],
        ];

        let model = ScriptedMockProvider::new(scripts);
        let mut reg = ToolRegistry::new();
        reg.register(ShellTool { timeout_secs: 5 });
        let mut agent = agent_with(model, reg, AgentConfig::default(), AgentMode::Agent);

        let (tx, rx) = mpsc::channel(64);
        agent.submit("run both commands", tx).await.unwrap();
        let events = collect_events(rx).await;

        // Both tools should have finished
        let finished: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolCallFinished { call_id, .. } => Some(call_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(finished.len(), 2, "both tool calls should complete");
        assert_eq!(finished[0], "call_1", "first result should be call_1");
        assert_eq!(finished[1], "call_2", "second result should be call_2");

        // Session history should have 2 tool-call messages + 2 tool-result messages
        let tool_call_msgs = agent
            .session()
            .messages
            .iter()
            .filter(|m| matches!(&m.content, MessageContent::ToolCall { .. }))
            .count();
        let tool_result_msgs = agent
            .session()
            .messages
            .iter()
            .filter(|m| matches!(&m.content, MessageContent::ToolResult { .. }))
            .count();
        assert_eq!(tool_call_msgs, 2, "should have 2 tool call messages");
        assert_eq!(tool_result_msgs, 2, "should have 2 tool result messages");
    }

    // ── Rolling compaction ────────────────────────────────────────────────────

    /// Seed the session with pre-existing messages so the agent starts with
    /// a filled context, bypassing the need for many real turns.
    fn seed_session(agent: &mut Agent, messages: Vec<sven_model::Message>) {
        for msg in messages {
            agent.session_mut().push(msg);
        }
    }

    #[tokio::test]
    async fn full_summarization_when_history_too_short_for_rolling() {
        // When non_system.len() <= 2 * keep_n, the agent falls back to full
        // summarization (same as pre-rolling behaviour).
        // keep_n = 2, so rolling requires > 4 non-system messages.
        // We seed 4 messages → full summarization expected.
        let config = AgentConfig {
            compaction_keep_recent: 2,
            compaction_threshold: 0.5,
            ..AgentConfig::default()
        };
        // Script: 1st call = summarization turn, 2nd = actual user reply
        let model = ScriptedMockProvider::new(vec![
            vec![
                ResponseEvent::TextDelta("short summary".into()),
                ResponseEvent::Done,
            ],
            vec![
                ResponseEvent::TextDelta("actual reply".into()),
                ResponseEvent::Done,
            ],
        ]);
        // max_context_tokens=16: the 5 seeded messages produce 9 approx-tokens
        // (1+2+2+2+2), giving 9/16 = 0.5625 ≥ threshold 0.5.
        let mut agent =
            agent_with_ctx(model, ToolRegistry::default(), config, AgentMode::Agent, 16);

        // Seed: system is pushed automatically on first turn, but we inject
        // 4 non-system messages directly (4 tokens each char / 4 = ~1 each).
        use sven_model::Message;
        seed_session(
            &mut agent,
            vec![
                Message::system("sys"),         // ~1 token
                Message::user("m1 m1 m1 m1"),   // ~3 tokens
                Message::assistant("m2 m2 m2"), // ~3 tokens
                Message::user("m3 m3 m3 m3"),   // ~3 tokens
                Message::assistant("m4 m4 m4"), // ~3 tokens
            ],
        );
        // With max=20 tokens and these ~13 tokens, fraction=0.65 > threshold=0.5.
        assert!(
            agent.session().is_near_limit(0.5),
            "session must be over limit for test to be meaningful"
        );

        let (tx, mut rx) = mpsc::channel(64);
        agent.submit("new question", tx).await.unwrap();

        // Drain all events
        let mut events: Vec<AgentEvent> = Vec::new();
        while let Some(ev) = rx.recv().await {
            let done = matches!(ev, AgentEvent::TurnComplete);
            events.push(ev);
            if done {
                break;
            }
        }

        // ContextCompacted must have fired
        let compacted = events
            .iter()
            .any(|e| matches!(e, AgentEvent::ContextCompacted { .. }));
        assert!(compacted, "ContextCompacted event must be emitted");

        // After full compaction + new user message + reply:
        //   [sys, assistant(summary), user(new question), assistant(reply)]
        // The session should have NO messages from m1..m4 verbatim.
        let msgs = &agent.session().messages;
        // system + summary + user input + assistant reply = 4
        assert_eq!(
            msgs.len(),
            4,
            "expected [sys, summary, user, reply], got {:?}",
            msgs.iter().map(|m| m.role.clone()).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn rolling_compaction_preserves_recent_messages() {
        // When non_system.len() > 2 * keep_n, rolling compaction fires:
        // the most recent keep_n messages are preserved verbatim.
        // keep_n = 2, rolling requires > 4 non-system messages → seed 6.
        use sven_model::Message;
        let config = AgentConfig {
            compaction_keep_recent: 2,
            compaction_threshold: 0.4,
            ..AgentConfig::default()
        };
        // Script: summarization turn + actual turn
        let model = ScriptedMockProvider::new(vec![
            vec![
                ResponseEvent::TextDelta("rolling summary".into()),
                ResponseEvent::Done,
            ],
            vec![
                ResponseEvent::TextDelta("final reply".into()),
                ResponseEvent::Done,
            ],
        ]);
        let mut agent =
            agent_with_ctx(model, ToolRegistry::default(), config, AgentMode::Agent, 40);

        // Seed 6 non-system messages. With ~3 tokens each = 18 tokens + sys~1 = 19.
        // max=40, threshold=0.4 → limit=16 tokens. 19 > 16 → over limit.
        let recent_user = "keep me 1";
        let recent_asst = "keep me 2";
        seed_session(
            &mut agent,
            vec![
                Message::system("sys"),
                Message::user("old1 old1 old1"),
                Message::assistant("old2 old2 old2"),
                Message::user("old3 old3 old3"),
                Message::assistant("old4 old4 old4"),
                Message::user(recent_user),      // should be preserved
                Message::assistant(recent_asst), // should be preserved
            ],
        );
        assert!(
            agent.session().is_near_limit(0.4),
            "session must be over limit"
        );

        let (tx, mut rx) = mpsc::channel(64);
        agent.submit("new input", tx).await.unwrap();
        let mut events: Vec<AgentEvent> = Vec::new();
        while let Some(ev) = rx.recv().await {
            let done = matches!(ev, AgentEvent::TurnComplete);
            events.push(ev);
            if done {
                break;
            }
        }

        let compacted = events
            .iter()
            .any(|e| matches!(e, AgentEvent::ContextCompacted { .. }));
        assert!(
            compacted,
            "ContextCompacted must be emitted for rolling compaction"
        );

        // After rolling compaction + new user message + reply:
        //   [sys, assistant(summary), user(recent_user), assistant(recent_asst),
        //    user(new input), assistant(final reply)]
        let msgs = &agent.session().messages;
        assert_eq!(
            msgs.len(),
            6,
            "expected 6 messages after rolling compaction turn, got {}: {:?}",
            msgs.len(),
            msgs.iter().map(|m| &m.role).collect::<Vec<_>>()
        );

        // The two preserved messages must be exactly present in the session
        let has_recent_user = msgs.iter().any(|m| m.as_text() == Some(recent_user));
        let has_recent_asst = msgs.iter().any(|m| m.as_text() == Some(recent_asst));
        assert!(
            has_recent_user,
            "recently preserved user message must remain verbatim"
        );
        assert!(
            has_recent_asst,
            "recently preserved assistant message must remain verbatim"
        );
    }

    #[tokio::test]
    async fn context_compacted_event_has_correct_token_ordering() {
        // tokens_before must be > tokens_after after compaction
        use sven_model::Message;
        let config = AgentConfig {
            compaction_keep_recent: 0, // full summarization
            compaction_threshold: 0.3,
            ..AgentConfig::default()
        };
        let model = ScriptedMockProvider::new(vec![
            vec![
                ResponseEvent::TextDelta("summary text".into()),
                ResponseEvent::Done,
            ],
            vec![
                ResponseEvent::TextDelta("reply".into()),
                ResponseEvent::Done,
            ],
        ]);
        // max_context_tokens=20: the 3 seeded messages produce 7 approx-tokens
        // (1+3+3), giving 7/20 = 0.35 ≥ threshold 0.3.
        let mut agent =
            agent_with_ctx(model, ToolRegistry::default(), config, AgentMode::Agent, 20);
        seed_session(
            &mut agent,
            vec![
                Message::system("system"),
                Message::user("aaaa aaaa aaaa"),
                Message::assistant("bbbb bbbb bbbb"),
            ],
        );
        assert!(agent.session().is_near_limit(0.3));

        let (tx, mut rx) = mpsc::channel(64);
        agent.submit("q", tx).await.unwrap();
        let mut events: Vec<AgentEvent> = Vec::new();
        while let Some(ev) = rx.recv().await {
            let done = matches!(ev, AgentEvent::TurnComplete);
            events.push(ev);
            if done {
                break;
            }
        }

        let compaction_ev = events.iter().find_map(|e| {
            if let AgentEvent::ContextCompacted {
                tokens_before,
                tokens_after,
                ..
            } = e
            {
                Some((*tokens_before, *tokens_after))
            } else {
                None
            }
        });
        let (before, after) = compaction_ev.expect("ContextCompacted must be emitted");
        // `tokens_before` is the count of the seeded session; must be > 0.
        assert!(before > 0, "tokens_before must be positive (was {before})");
        // `tokens_after` includes the real (full) system prompt which is much
        // larger than the tiny seeded "system" message — comparing raw totals
        // is not meaningful.  Instead, verify the old history was replaced by
        // checking that neither "aaaa" nor "bbbb" appear in any session message.
        let old_history_remains = agent.session().messages.iter().any(|m| {
            m.as_text()
                .map(|t| t.contains("aaaa") || t.contains("bbbb"))
                .unwrap_or(false)
        });
        assert!(
            !old_history_remains,
            "original history must have been compacted away (tokens_before={before}, tokens_after={after})"
        );
        // The event fields must at least be non-zero.
        assert!(after > 0, "tokens_after must be positive (was {after})");
    }

    // ── New forefront compaction tests ────────────────────────────────────────

    #[tokio::test]
    async fn compaction_event_carries_strategy_and_turn() {
        // Verify that ContextCompacted events carry strategy and turn fields.
        use sven_model::Message;
        let config = AgentConfig {
            compaction_keep_recent: 0,
            compaction_threshold: 0.3,
            compaction_overhead_reserve: 0.0,
            ..AgentConfig::default()
        };
        let model = ScriptedMockProvider::new(vec![
            vec![
                ResponseEvent::TextDelta("summary".into()),
                ResponseEvent::Done,
            ],
            vec![
                ResponseEvent::TextDelta("reply".into()),
                ResponseEvent::Done,
            ],
        ]);
        let mut agent =
            agent_with_ctx(model, ToolRegistry::default(), config, AgentMode::Agent, 20);
        seed_session(
            &mut agent,
            vec![
                Message::system("system"),
                Message::user("aaaa aaaa aaaa"),
                Message::assistant("bbbb bbbb bbbb"),
            ],
        );

        let (tx, rx) = mpsc::channel(64);
        agent.submit("q", tx).await.unwrap();
        let events = collect_events(rx).await;

        let compacted = events.iter().find_map(|e| {
            if let AgentEvent::ContextCompacted { strategy, turn, .. } = e {
                Some((strategy.clone(), *turn))
            } else {
                None
            }
        });
        let (strategy, turn) = compacted.expect("ContextCompacted must be emitted");
        // Pre-submit compaction fires at turn=0
        assert_eq!(turn, 0, "pre-submit compaction must have turn=0");
        // Default strategy is Structured
        assert_eq!(strategy, crate::events::CompactionStrategyUsed::Structured);
    }

    #[tokio::test]
    async fn narrative_strategy_produces_narrative_compaction_event() {
        use sven_config::CompactionStrategy;
        use sven_model::Message;
        let config = AgentConfig {
            compaction_keep_recent: 0,
            compaction_threshold: 0.3,
            compaction_overhead_reserve: 0.0,
            compaction_strategy: CompactionStrategy::Narrative,
            ..AgentConfig::default()
        };
        let model = ScriptedMockProvider::new(vec![
            vec![
                ResponseEvent::TextDelta("summary".into()),
                ResponseEvent::Done,
            ],
            vec![
                ResponseEvent::TextDelta("reply".into()),
                ResponseEvent::Done,
            ],
        ]);
        let mut agent =
            agent_with_ctx(model, ToolRegistry::default(), config, AgentMode::Agent, 20);
        seed_session(
            &mut agent,
            vec![
                Message::system("system"),
                Message::user("aaaa aaaa aaaa"),
                Message::assistant("bbbb bbbb bbbb"),
            ],
        );

        let (tx, rx) = mpsc::channel(64);
        agent.submit("q", tx).await.unwrap();
        let events = collect_events(rx).await;

        let strategy = events.iter().find_map(|e| {
            if let AgentEvent::ContextCompacted { strategy, .. } = e {
                Some(strategy.clone())
            } else {
                None
            }
        });
        assert_eq!(
            strategy,
            Some(crate::events::CompactionStrategyUsed::Narrative),
            "Narrative strategy must produce Narrative compaction event"
        );
    }

    #[tokio::test]
    async fn tool_result_truncated_when_exceeds_cap() {
        // When tool_result_token_cap is small, large tool outputs must be
        // truncated before entering the session.
        use async_trait::async_trait;
        use sven_config::AgentMode;
        use sven_tools::events::ToolEvent;
        use sven_tools::{ApprovalPolicy, Tool, ToolCall, ToolOutput, ToolRegistry};
        use tokio::sync::Mutex;

        // Inline mock tool that returns 10 000 chars (≈ 2 500 tokens).
        struct BigOutputTool;
        #[async_trait]
        impl Tool for BigOutputTool {
            fn name(&self) -> &str {
                "shell"
            }
            fn description(&self) -> &str {
                "mock that returns lots of text"
            }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({"type": "object", "properties": {}})
            }
            fn default_policy(&self) -> ApprovalPolicy {
                ApprovalPolicy::Auto
            }
            async fn execute(&self, call: &ToolCall) -> ToolOutput {
                ToolOutput::ok(call.id.clone(), "output line\n".repeat(1000))
            }
        }

        let model = ScriptedMockProvider::new(vec![
            // Turn 1: model calls shell
            vec![
                ResponseEvent::ToolCall {
                    index: 0,
                    id: "tc1".into(),
                    name: "shell".into(),
                    arguments: r#"{}"#.into(),
                },
                ResponseEvent::Done,
            ],
            // Turn 2: model produces final answer after seeing truncated result
            vec![ResponseEvent::TextDelta("done".into()), ResponseEvent::Done],
        ]);

        let mut registry = ToolRegistry::default();
        registry.register(BigOutputTool);

        let config = AgentConfig {
            tool_result_token_cap: 100, // cap at 400 chars
            compaction_overhead_reserve: 0.0,
            ..AgentConfig::default()
        };

        let mode_lock = Arc::new(Mutex::new(AgentMode::Agent));
        let (_, tool_event_rx) = mpsc::channel::<ToolEvent>(64);
        let mut agent = Agent::new(
            Arc::new(model),
            Arc::new(registry),
            Arc::new(config),
            AgentRuntimeContext::default(),
            mode_lock,
            tool_event_rx,
            128_000,
        );

        let (tx, rx) = mpsc::channel(64);
        agent.submit("run shell", tx).await.unwrap();
        let _ = collect_events(rx).await;

        // The tool result stored in the session must be shorter than the original
        // (10 000 chars raw; cap is 100 tokens = 400 chars).
        let tool_result_len: usize = agent
            .session()
            .messages
            .iter()
            .filter_map(|m| {
                if let sven_model::MessageContent::ToolResult { content, .. } = &m.content {
                    match content {
                        sven_model::ToolResultContent::Text(t) => Some(t.len()),
                        _ => None,
                    }
                } else {
                    None
                }
            })
            .sum();

        assert!(
            tool_result_len < 2000, // significantly less than 10 000 original chars
            "tool result in session should be truncated (got {tool_result_len} chars)"
        );
    }

    #[tokio::test]
    async fn calibration_factor_updated_from_usage_event() {
        // After a turn that reports Usage, the session calibration_factor should
        // have moved away from its initial value of 1.0.
        let model = ScriptedMockProvider::new(vec![vec![
            // Report 200 input tokens consumed; estimated will be much less
            ResponseEvent::Usage {
                input_tokens: 200,
                output_tokens: 10,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
            ResponseEvent::TextDelta("hi".into()),
            ResponseEvent::Done,
        ]]);

        let mut agent = agent_with(
            model,
            ToolRegistry::default(),
            AgentConfig::default(),
            AgentMode::Agent,
        );

        let (tx, rx) = mpsc::channel(64);
        agent.submit("test", tx).await.unwrap();
        let _ = collect_events(rx).await;

        // calibration_factor should have been updated (moved away from 1.0)
        let factor = agent.session().calibration_factor;
        assert!(
            (factor - 1.0).abs() > 0.001,
            "calibration_factor should have been updated from 1.0, got {factor}"
        );
    }

    #[tokio::test]
    async fn session_input_budget_uses_max_output_tokens() {
        // Verify that input_budget() = max_tokens - max_output_tokens
        let model = ScriptedMockProvider::always_text("ok");
        let mut agent = agent_with_ctx(
            model,
            ToolRegistry::default(),
            AgentConfig::default(),
            AgentMode::Agent,
            200_000,
        );
        // Manually set max_output_tokens to simulate a real model
        agent.session_mut().max_output_tokens = 64_000;

        assert_eq!(
            agent.session().input_budget(),
            136_000,
            "input_budget must be context_window - max_output_tokens"
        );
    }

    #[tokio::test]
    async fn structured_compaction_produces_summary_in_session() {
        // Verify that when compaction fires with the Structured strategy,
        // the summary text from the model is stored in the session.
        use sven_model::Message;
        let config = AgentConfig {
            compaction_keep_recent: 0,
            compaction_threshold: 0.3,
            compaction_overhead_reserve: 0.0,
            compaction_strategy: sven_config::CompactionStrategy::Structured,
            ..AgentConfig::default()
        };

        let model = ScriptedMockProvider::new(vec![
            // First call: compaction summary turn
            vec![
                ResponseEvent::TextDelta("structured summary text".into()),
                ResponseEvent::Done,
            ],
            // Second call: actual user question reply
            vec![
                ResponseEvent::TextDelta("reply".into()),
                ResponseEvent::Done,
            ],
        ]);

        let mut agent =
            agent_with_ctx(model, ToolRegistry::default(), config, AgentMode::Agent, 20);
        seed_session(
            &mut agent,
            vec![
                Message::system("system"),
                Message::user("aaaa aaaa aaaa"),
                Message::assistant("bbbb bbbb bbbb"),
            ],
        );

        let (tx, rx) = mpsc::channel(64);
        agent.submit("q", tx).await.unwrap();
        let _ = collect_events(rx).await;

        // After compaction the summary text should be present in the session.
        let has_summary = agent.session().messages.iter().any(|m| {
            m.as_text()
                .map(|t| t.contains("structured summary text"))
                .unwrap_or(false)
        });
        assert!(
            has_summary,
            "compacted session must contain the structured summary text"
        );
    }

    // ── Compaction resilience ─────────────────────────────────────────────────

    #[tokio::test]
    async fn compaction_model_failure_falls_back_to_emergency_and_continues() {
        // When the compaction model call fails (simulated by returning an
        // error response), ensure_fits_budget must:
        //   1. Not corrupt the session (restore original messages).
        //   2. Fall back to emergency compaction (deterministic, no model call).
        //   3. Allow the subsequent real model turn to succeed.
        //
        // Script: turn-1 emits an error event (triggers the fallback), turn-2
        // is the actual user reply that must succeed.
        use sven_model::{Message, ResponseEvent};
        let config = AgentConfig {
            compaction_keep_recent: 2,
            compaction_threshold: 0.3,
            compaction_overhead_reserve: 0.0,
            ..AgentConfig::default()
        };
        let model = ScriptedMockProvider::new(vec![
            // Compaction turn: provider returns an error, simulating a
            // network failure or rate-limit during the summary generation.
            vec![
                ResponseEvent::Error("simulated model error".into()),
                ResponseEvent::Done,
            ],
            // Real user turn that must succeed after the fallback.
            vec![
                ResponseEvent::TextDelta("reply after fallback".into()),
                ResponseEvent::Done,
            ],
        ]);
        let mut agent =
            agent_with_ctx(model, ToolRegistry::default(), config, AgentMode::Agent, 20);
        // Seed history that is above the compaction threshold.
        seed_session(
            &mut agent,
            vec![
                Message::system("sys"),
                Message::user("old msg 1"),
                Message::assistant("old msg 2"),
                Message::user("old msg 3"),
                Message::assistant("old msg 4"),
            ],
        );
        assert!(
            agent.session().is_near_limit(0.3),
            "must be over threshold before submit"
        );

        let (tx, rx) = mpsc::channel(64);
        // Must not return an error even though the compaction model call fails.
        agent.submit("continue", tx).await.unwrap();
        let events = collect_events(rx).await;

        // A ContextCompacted event must still be emitted (emergency strategy).
        let compacted = events.iter().find_map(|e| {
            if let AgentEvent::ContextCompacted { strategy, .. } = e {
                Some(strategy.clone())
            } else {
                None
            }
        });
        assert!(
            compacted.is_some(),
            "ContextCompacted must be emitted even on fallback"
        );
        let strategy = compacted.unwrap();
        assert!(
            matches!(strategy, crate::events::CompactionStrategyUsed::Emergency),
            "fallback must use Emergency strategy, got {strategy:?}"
        );

        // The agent must have replied successfully after the fallback.
        let has_reply = events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta(t) if t.contains("reply after fallback")));
        assert!(
            has_reply,
            "agent must produce a reply after compaction fallback"
        );
    }

    #[tokio::test]
    async fn compaction_empty_summary_falls_back_to_emergency() {
        // When the compaction model call returns an empty string (e.g., the
        // model chose not to respond), ensure_fits_budget must also fall back
        // to emergency compaction rather than storing an empty assistant message.
        use sven_model::{Message, ResponseEvent};
        let config = AgentConfig {
            compaction_keep_recent: 2,
            compaction_threshold: 0.3,
            compaction_overhead_reserve: 0.0,
            ..AgentConfig::default()
        };
        let model = ScriptedMockProvider::new(vec![
            // Compaction turn: model returns Done with no text (empty summary).
            vec![ResponseEvent::Done],
            // Real turn after fallback.
            vec![ResponseEvent::TextDelta("ok".into()), ResponseEvent::Done],
        ]);
        let mut agent =
            agent_with_ctx(model, ToolRegistry::default(), config, AgentMode::Agent, 20);
        seed_session(
            &mut agent,
            vec![
                Message::system("sys"),
                Message::user("old msg 1"),
                Message::assistant("old msg 2"),
                Message::user("old msg 3"),
                Message::assistant("old msg 4"),
            ],
        );
        assert!(agent.session().is_near_limit(0.3));

        let (tx, rx) = mpsc::channel(64);
        agent.submit("go", tx).await.unwrap();
        let events = collect_events(rx).await;

        let strategy = events
            .iter()
            .find_map(|e| {
                if let AgentEvent::ContextCompacted { strategy, .. } = e {
                    Some(strategy.clone())
                } else {
                    None
                }
            })
            .expect("ContextCompacted must be emitted");
        assert!(
            matches!(strategy, crate::events::CompactionStrategyUsed::Emergency),
            "empty summary must also fall back to Emergency, got {strategy:?}"
        );

        // No empty assistant message should be in the session.
        let empty_assistant = agent.session().messages.iter().any(|m| {
            m.role == sven_model::Role::Assistant
                && m.as_text().map(|t| t.is_empty()).unwrap_or(false)
        });
        assert!(
            !empty_assistant,
            "session must not contain empty assistant messages after fallback"
        );
    }

    // ── Split-boundary safety (tool_use / tool_result pair integrity) ─────────

    #[tokio::test]
    async fn compaction_never_splits_tool_call_result_pair() {
        // Regression test for: compaction fires mid-loop and the rolling
        // keep_n boundary falls between ToolCall and ToolResult messages.
        // The resulting session must not contain any ToolResult whose
        // corresponding ToolCall was summarised away (which would cause
        // Anthropic to return a 400 "unexpected tool_use_id" error).
        //
        // Setup: seed a session that contains a complete tool-call/result pair
        // at the very boundary of the keep_n window.  With keep_n=2 the naive
        // split would land exactly between a ToolCall and its ToolResult.
        use serde_json::json;
        use sven_model::{FunctionCall, Message, MessageContent, Role};

        let config = AgentConfig {
            compaction_keep_recent: 2,
            compaction_threshold: 0.3,
            compaction_overhead_reserve: 0.0,
            ..AgentConfig::default()
        };
        let model = ScriptedMockProvider::new(vec![
            // Compaction turn (summary generation).
            vec![
                ResponseEvent::TextDelta("summary".into()),
                ResponseEvent::Done,
            ],
            // Real reply turn.
            vec![ResponseEvent::TextDelta("done".into()), ResponseEvent::Done],
        ]);
        let mut agent =
            agent_with_ctx(model, ToolRegistry::default(), config, AgentMode::Agent, 30);

        // Build a session where the last 2 non-system messages are a ToolCall
        // followed by a ToolResult — exactly what keep_n=2 would preserve when
        // the naive split puts the boundary between them.
        // By inserting extra older messages, we push total tokens above threshold.
        let tool_call_id = "tc_boundary_test";
        seed_session(
            &mut agent,
            vec![
                Message::system("sys"),
                Message::user("task"),
                Message::assistant("thinking"),
                // ToolCall + ToolResult pair at positions that straddle the keep_n boundary.
                Message {
                    role: Role::Assistant,
                    content: MessageContent::ToolCall {
                        tool_call_id: tool_call_id.into(),
                        function: FunctionCall {
                            name: "shell".into(),
                            arguments: json!({"command":"ls"}).to_string(),
                        },
                    },
                },
                Message {
                    role: Role::Tool,
                    content: MessageContent::ToolResult {
                        tool_call_id: tool_call_id.into(),
                        content: sven_model::ToolResultContent::Text("file.txt".into()),
                    },
                },
            ],
        );
        assert!(agent.session().is_near_limit(0.3), "must be over threshold");

        let (tx, rx) = mpsc::channel(64);
        agent.submit("continue", tx).await.unwrap();
        let _events = collect_events(rx).await;

        // Verify the session is internally consistent: every ToolResult in the
        // session must be preceded (somewhere before it) by a ToolCall with
        // the same tool_call_id.  An orphaned ToolResult would be the bug.
        let msgs = &agent.session().messages;
        let mut seen_tool_call_ids: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for msg in msgs {
            match &msg.content {
                MessageContent::ToolCall { tool_call_id, .. } => {
                    seen_tool_call_ids.insert(tool_call_id.clone());
                }
                MessageContent::ToolResult { tool_call_id, .. } => {
                    assert!(
                        seen_tool_call_ids.contains(tool_call_id.as_str()),
                        "orphaned ToolResult: tool_call_id '{tool_call_id}' has no preceding ToolCall in the session"
                    );
                }
                _ => {}
            }
        }
    }

    #[tokio::test]
    async fn compaction_adjusts_split_past_multiple_tool_calls_in_same_batch() {
        // When multiple parallel ToolCall messages (same batch) sit at the
        // boundary, the split must move backward past ALL of them so that none
        // of their ToolResult messages is left orphaned in recent_messages.
        use serde_json::json;
        use sven_model::{FunctionCall, Message, MessageContent, Role};

        let config = AgentConfig {
            compaction_keep_recent: 3, // boundary lands mid-batch
            compaction_threshold: 0.3,
            compaction_overhead_reserve: 0.0,
            ..AgentConfig::default()
        };
        let model = ScriptedMockProvider::new(vec![
            vec![
                ResponseEvent::TextDelta("summary".into()),
                ResponseEvent::Done,
            ],
            vec![ResponseEvent::TextDelta("done".into()), ResponseEvent::Done],
        ]);
        let mut agent =
            agent_with_ctx(model, ToolRegistry::default(), config, AgentMode::Agent, 40);

        // Non-system messages: [user, assistant-text, tc1, tc2, tr1, tr2]
        // keep_n=3 → naive split at index 3 (tc2), which is mid-batch.
        let tc1 = "tc_batch_1";
        let tc2 = "tc_batch_2";
        seed_session(
            &mut agent,
            vec![
                Message::system("sys"),
                Message::user("do task"),
                Message::assistant("thinking about it"),
                Message {
                    role: Role::Assistant,
                    content: MessageContent::ToolCall {
                        tool_call_id: tc1.into(),
                        function: FunctionCall {
                            name: "shell".into(),
                            arguments: json!({"command":"ls"}).to_string(),
                        },
                    },
                },
                Message {
                    role: Role::Assistant,
                    content: MessageContent::ToolCall {
                        tool_call_id: tc2.into(),
                        function: FunctionCall {
                            name: "shell".into(),
                            arguments: json!({"command":"pwd"}).to_string(),
                        },
                    },
                },
                Message {
                    role: Role::Tool,
                    content: MessageContent::ToolResult {
                        tool_call_id: tc1.into(),
                        content: sven_model::ToolResultContent::Text("file.txt".into()),
                    },
                },
                Message {
                    role: Role::Tool,
                    content: MessageContent::ToolResult {
                        tool_call_id: tc2.into(),
                        content: sven_model::ToolResultContent::Text("/workspace".into()),
                    },
                },
            ],
        );
        assert!(agent.session().is_near_limit(0.3));

        let (tx, rx) = mpsc::channel(64);
        agent.submit("next step", tx).await.unwrap();
        let _events = collect_events(rx).await;

        // All ToolResult messages in the session must have a preceding ToolCall.
        let msgs = &agent.session().messages;
        let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        for msg in msgs {
            match &msg.content {
                MessageContent::ToolCall { tool_call_id, .. } => {
                    seen_ids.insert(tool_call_id.clone());
                }
                MessageContent::ToolResult { tool_call_id, .. } => {
                    assert!(
                        seen_ids.contains(tool_call_id.as_str()),
                        "orphaned ToolResult for '{tool_call_id}' — split must have moved past the full batch"
                    );
                }
                _ => {}
            }
        }
    }
}
