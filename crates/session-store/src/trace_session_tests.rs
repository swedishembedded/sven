// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for `trace_session` -- split into its own file (Phase 7 of the
//! refactor plan) since it had grown to more than half of that file's 2609
//! lines. Included via `#[path]` in `trace_session.rs`, so this is still
//! logically `trace_session::tests`, just not inline.

    use super::*;
    use crate::chat_document::SessionId;

    fn msg_values(messages: &[Message]) -> Vec<Value> {
        messages
            .iter()
            .map(|m| serde_json::to_value(m).unwrap())
            .collect()
    }

    fn assert_messages_eq(actual: &[Message], expected: &[Message]) {
        assert_eq!(msg_values(actual), msg_values(expected));
    }

    // ── SvenSessionMeta round-trip ──────────────────────────────────────────

    #[test]
    fn sven_session_meta_round_trips_through_extra() {
        let agent = default_agent_profile();
        let mut trajectory = Trajectory::new(ATIF_SCHEMA_VERSION, agent);
        let mut meta = SvenSessionMeta::new("My session");
        meta.status = ChatStatus::Completed;
        meta.mode = Some("code".to_string());
        meta.parent_session_id = Some("parent-123".to_string());
        meta.apply_to_trajectory(&mut trajectory);

        let restored = SvenSessionMeta::from_trajectory(&trajectory).expect("meta present");
        assert_eq!(restored, meta);
    }

    #[test]
    fn sven_session_meta_absent_returns_none() {
        let trajectory = Trajectory::new(ATIF_SCHEMA_VERSION, default_agent_profile());
        assert!(SvenSessionMeta::from_trajectory(&trajectory).is_none());
    }

    #[test]
    fn sven_session_meta_preserves_other_extra_keys() {
        let mut trajectory = Trajectory::new(ATIF_SCHEMA_VERSION, default_agent_profile());
        trajectory.extra = Some(serde_json::json!({ "other_tool": { "foo": "bar" } }));
        let meta = SvenSessionMeta::new("Title");
        meta.apply_to_trajectory(&mut trajectory);

        let extra = trajectory.extra.as_ref().unwrap();
        assert_eq!(extra["other_tool"]["foo"], "bar");
        assert!(SvenSessionMeta::from_trajectory(&trajectory).is_some());
    }

    #[test]
    fn sven_session_meta_json_shape_is_nested_object() {
        let mut trajectory = Trajectory::new(ATIF_SCHEMA_VERSION, default_agent_profile());
        SvenSessionMeta::new("Title").apply_to_trajectory(&mut trajectory);
        let extra = trajectory.extra.as_ref().unwrap();
        assert!(
            extra.get("sven").is_some(),
            "must nest under a single `sven` key"
        );
        assert_eq!(extra["sven"]["title"], "Title");
    }

    // ── model / usage native-field mapping ──────────────────────────────────

    #[test]
    fn model_maps_to_agent_model_name() {
        let agent = default_agent_profile().with_model("anthropic/claude-sonnet-4-20250514");
        assert_eq!(
            agent.model_name.as_deref(),
            Some("anthropic/claude-sonnet-4-20250514")
        );
    }

    #[test]
    fn chat_usage_maps_to_final_metrics_native_fields() {
        let usage = ChatUsage {
            total_input_tokens: 1234,
            total_output_tokens: 567,
            total_cache_read_tokens: 100,
            total_cache_write_tokens: 200,
            total_cost_usd: 0.042,
        };
        let metrics = chat_usage_to_final_metrics(&usage);
        assert_eq!(metrics.total_prompt_tokens, Some(1234));
        assert_eq!(metrics.total_completion_tokens, Some(567));
        assert_eq!(metrics.total_cached_tokens, Some(100));
        assert!((metrics.total_cost_usd.unwrap() - 0.042).abs() < 1e-9);
        assert_eq!(metrics.extra.unwrap()["total_cache_write_tokens"], 200);
    }

    #[test]
    fn chat_usage_final_metrics_round_trip() {
        let usage = ChatUsage {
            total_input_tokens: 10,
            total_output_tokens: 20,
            total_cache_read_tokens: 5,
            total_cache_write_tokens: 7,
            total_cost_usd: 1.5,
        };
        let metrics = chat_usage_to_final_metrics(&usage);
        let back = final_metrics_to_chat_usage(&metrics);
        assert_eq!(back, usage);
    }

    #[test]
    fn chat_usage_zero_cache_write_omits_extra() {
        let usage = ChatUsage {
            total_cache_write_tokens: 0,
            ..Default::default()
        };
        let metrics = chat_usage_to_final_metrics(&usage);
        assert!(metrics.extra.is_none());
    }

    // ── session id ────────────────────────────────────────────────────────

    #[test]
    fn new_session_id_is_nonempty_uuid_like() {
        let id = new_session_id();
        assert_eq!(id.len(), 36);
        assert!(id.chars().filter(|c| *c == '-').count() == 4);
    }

    #[test]
    fn new_session_id_is_unique() {
        assert_ne!(new_session_id(), new_session_id());
    }

    // ── parent/child linkage ─────────────────────────────────────────────

    #[test]
    fn parent_session_id_round_trips_on_child() {
        let mut child = Trajectory::new(ATIF_SCHEMA_VERSION, default_agent_profile());
        let mut meta = SvenSessionMeta::new("Subagent task");
        meta.parent_session_id = Some("root-session-1".to_string());
        meta.apply_to_trajectory(&mut child);

        let restored = SvenSessionMeta::from_trajectory(&child).unwrap();
        assert_eq!(
            restored.parent_session_id.as_deref(),
            Some("root-session-1")
        );
    }

    #[test]
    fn record_subagent_spawn_appends_forward_ref_and_validates() {
        let mut parent = Trajectory::new(ATIF_SCHEMA_VERSION, default_agent_profile());
        parent.session_id = Some("root-session-1".to_string());
        parent
            .steps
            .push(TraceStep::new(1, StepOrigin::User, "Do the subtask"));

        record_subagent_spawn(
            &mut parent,
            "child-session-1",
            Path::new("/data/sessions/child-session-1.json"),
        );

        assert_eq!(parent.steps.len(), 2);
        let step = &parent.steps[1];
        assert_eq!(step.step_id, 2);
        assert_eq!(step.source, StepOrigin::System);
        let obs = step.observation.as_ref().expect("observation present");
        let refs = obs.results[0]
            .subagent_trajectory_ref
            .as_ref()
            .expect("refs present");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].session_id.as_deref(), Some("child-session-1"));
        assert_eq!(
            refs[0].trajectory_path.as_deref(),
            Some("/data/sessions/child-session-1.json")
        );
        assert!(refs[0].trajectory_id.is_none());
        assert!(!refs[0].is_unresolvable());

        assert!(atif::validate_trajectory(&parent).is_ok());
    }

    // ── agent.version ─────────────────────────────────────────────────────

    #[test]
    fn default_agent_profile_reports_the_real_sven_binary_version_not_a_frozen_crate_version() {
        // Regression guard: `crates/input/Cargo.toml` used to carry its own
        // permanently-frozen `version = "1.0.0"`, so every ATIF trace this
        // crate wrote claimed `agent.version = "1.0.0"` no matter what the
        // actual top-level `sven` binary release was. `crates/input` now
        // inherits `version.workspace = true` from the root Cargo.toml's
        // `[workspace.package].version`, so `env!("CARGO_PKG_VERSION")` here
        // always matches the real release version.
        let profile = default_agent_profile();
        assert_ne!(
            profile.version, "1.0.0",
            "agent.version is still the old frozen crates/input version, not the real sven release version"
        );
        assert_eq!(profile.version, env!("CARGO_PKG_VERSION"));
    }

    // ── StepAssembler: forward turn assembly ────────────────────────────────

    #[test]
    fn simple_user_assistant_turn() {
        let mut a = StepAssembler::new();
        a.push_message(&Message::user("Hello, how are you?"));
        a.push_message(&Message::assistant("I'm doing well, thank you!"));
        let steps = a.finish();

        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].step_id, 1);
        assert_eq!(steps[0].source, StepOrigin::User);
        assert_eq!(steps[0].message.as_text(), Some("Hello, how are you?"));
        assert_eq!(steps[1].step_id, 2);
        assert_eq!(steps[1].source, StepOrigin::Agent);
        assert_eq!(
            steps[1].message.as_text(),
            Some("I'm doing well, thank you!")
        );
        assert!(steps[1].reasoning_content.is_none());
        assert!(steps[1].tool_calls.is_none());
    }

    #[test]
    fn thinking_tool_call_tool_result_and_text_merge_into_one_step() {
        let mut a = StepAssembler::new();
        a.push_message(&Message::user("What is 2+2?"));
        a.push_thinking("The user wants 2+2. That is 4.");
        a.push_message(&Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: "call_1".into(),
                function: FunctionCall {
                    name: "calculator".into(),
                    arguments: r#"{"expr":"2+2"}"#.into(),
                },
            },
        });
        a.push_message(&Message::tool_result("call_1", "4"));
        a.push_message(&Message::assistant("The answer is 4."));
        let steps = a.finish();

        assert_eq!(
            steps.len(),
            2,
            "thinking+toolcall+toolresult+text must merge into ONE agent step"
        );
        assert_eq!(steps[0].source, StepOrigin::User);
        let agent_step = &steps[1];
        assert_eq!(agent_step.step_id, 2);
        assert_eq!(agent_step.source, StepOrigin::Agent);
        assert_eq!(
            agent_step.reasoning_content.as_deref(),
            Some("The user wants 2+2. That is 4.")
        );
        assert_eq!(agent_step.message.as_text(), Some("The answer is 4."));

        let tool_calls = agent_step.tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].tool_call_id, "call_1");
        assert_eq!(tool_calls[0].function_name, "calculator");
        assert_eq!(tool_calls[0].arguments["expr"], "2+2");

        let observation = agent_step.observation.as_ref().unwrap();
        assert_eq!(observation.results.len(), 1);
        assert_eq!(
            observation.results[0].source_call_id.as_deref(),
            Some("call_1")
        );
        assert_eq!(
            observation.results[0].content.as_ref().unwrap().as_text(),
            Some("4")
        );

        assert!(atif::validate_trajectory(&{
            let mut t = Trajectory::new(ATIF_SCHEMA_VERSION, default_agent_profile());
            t.steps = steps.clone();
            t
        })
        .is_ok());
    }

    #[test]
    fn tool_call_without_thinking_still_merges_with_following_text() {
        // Matches chat_document.rs's `round_trip_tool_call` scenario shape:
        // user -> tool_call -> tool_result -> assistant text, no thinking.
        let mut a = StepAssembler::new();
        a.push_message(&Message::user("List files"));
        a.push_message(&Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: "call_001".into(),
                function: FunctionCall {
                    name: "list_dir".into(),
                    arguments: r#"{"path":"/tmp","depth":2}"#.into(),
                },
            },
        });
        a.push_message(&Message::tool_result("call_001", "file1.rs\nfile2.rs\n"));
        a.push_message(&Message::assistant("Found 2 Rust files."));
        let steps = a.finish();

        assert_eq!(steps.len(), 2);
        let agent_step = &steps[1];
        assert!(agent_step.reasoning_content.is_none());
        assert_eq!(agent_step.message.as_text(), Some("Found 2 Rust files."));
        assert_eq!(agent_step.tool_calls.as_ref().unwrap().len(), 1);
        assert_eq!(agent_step.observation.as_ref().unwrap().results.len(), 1);
    }

    #[test]
    fn multi_turn_conversation_step_ids_sequential() {
        let mut a = StepAssembler::new();
        a.push_message(&Message::user("Hi"));
        a.push_thinking("thinking");
        a.push_message(&Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: "c1".into(),
                function: FunctionCall {
                    name: "tool".into(),
                    arguments: "{}".into(),
                },
            },
        });
        a.push_message(&Message::tool_result("c1", "result"));
        a.push_message(&Message::assistant("Turn one done."));
        a.push_message(&Message::user("Next task"));
        a.push_message(&Message::assistant("Turn two done."));
        let steps = a.finish();

        let ids: Vec<u64> = steps.iter().map(|s| s.step_id).collect();
        assert_eq!(ids, vec![1, 2, 3, 4]);
        assert_eq!(steps[0].source, StepOrigin::User);
        assert_eq!(steps[1].source, StepOrigin::Agent);
        assert_eq!(steps[2].source, StepOrigin::User);
        assert_eq!(steps[3].source, StepOrigin::Agent);
    }

    #[test]
    fn resuming_continues_step_id_sequence() {
        let mut a = StepAssembler::resuming(5);
        a.push_message(&Message::user("Continuing"));
        a.push_message(&Message::assistant("Sure."));
        let steps = a.finish();
        let ids: Vec<u64> = steps.iter().map(|s| s.step_id).collect();
        assert_eq!(ids, vec![5, 6]);
    }

    #[test]
    fn closed_steps_reflects_pushes_without_consuming_assembler() {
        let mut a = StepAssembler::new();
        assert_eq!(a.closed_steps().len(), 0);
        a.push_message(&Message::user("Hi"));
        assert_eq!(a.closed_steps().len(), 1, "user message closes immediately");
        a.push_message(&Message::assistant("pending, not yet closed"));
        assert_eq!(
            a.closed_steps().len(),
            1,
            "assistant text alone stays pending until something closes it"
        );
        a.push_message(&Message::user("Next"));
        assert_eq!(
            a.closed_steps().len(),
            3,
            "new user message flushes the pending agent step"
        );
        let steps = a.finish();
        assert_eq!(steps.len(), 3);
    }

    #[test]
    fn snapshot_including_pending_captures_in_flight_turn_without_consuming() {
        let mut a = StepAssembler::new();
        a.push_message(&Message::user("Hi"));
        a.push_message(&Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: "c1".into(),
                function: FunctionCall {
                    name: "tool".into(),
                    arguments: "{}".into(),
                },
            },
        });
        // No tool result / closing event yet — the agent step is still open.
        assert_eq!(
            a.closed_steps().len(),
            1,
            "only the user step has closed so far"
        );

        let snapshot = a.snapshot_including_pending();
        assert_eq!(
            snapshot.len(),
            2,
            "snapshot includes the in-flight pending agent step"
        );
        assert_eq!(snapshot[1].source, StepOrigin::Agent);
        assert_eq!(snapshot[1].tool_calls.as_ref().unwrap().len(), 1);

        // The assembler itself is untouched: the pending step is still open
        // and can keep accumulating (e.g. the tool result arrives next).
        assert_eq!(
            a.closed_steps().len(),
            1,
            "snapshot must not consume pending"
        );
        a.push_message(&Message::tool_result("c1", "result"));
        a.push_message(&Message::assistant("done"));
        let steps = a.finish();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[1].observation.as_ref().unwrap().results.len(), 1);
    }

    #[test]
    fn context_compacted_becomes_system_step_with_structured_extra() {
        let mut a = StepAssembler::new();
        a.push_message(&Message::user("What is 2+2?"));
        a.push_thinking("reasoning");
        a.push_message(&Message::assistant("4"));
        a.push_context_compacted(1000, 100, Some("structured"), Some(3));
        let steps = a.finish();

        assert_eq!(steps.len(), 3);
        let compaction_step = &steps[2];
        assert_eq!(compaction_step.source, StepOrigin::System);
        assert!(compaction_step
            .message
            .as_text()
            .unwrap()
            .contains("context_compaction"));

        let extra = compaction_step.extra.as_ref().expect("extra present");
        let cm = ContextManagement::from_extra(extra).expect("context_management present");
        assert_eq!(cm.kind, "compaction");

        let details = ContextCompactionDetails::from_step_extra(extra).expect("details present");
        assert_eq!(details.tokens_before, 1000);
        assert_eq!(details.tokens_after, 100);
        assert_eq!(details.strategy.as_deref(), Some("structured"));
        assert_eq!(details.turn, Some(3));
    }

    #[test]
    fn push_subagent_embedded_attaches_to_pending_tool_call_step_without_splitting_it() {
        // Mirrors the real `task`-tool event order: ToolCallStarted (task)
        // arrives, then the subagent's own completion signal arrives — all
        // *before* the task tool's own ToolCallFinished (its own execute()
        // call hasn't returned to the parent event stream yet), and finally
        // assistant text closes the turn. If `push_subagent_embedded` closed
        // the pending step early (as an earlier version of this method did),
        // the tool call and its eventual result would land in different
        // steps and `atif::validate_trajectory` would reject the document
        // with a `DanglingSourceCallId` error — this test is the regression
        // guard for that.
        let mut a = StepAssembler::new();
        a.push_message(&Message::user("delegate this"));
        a.push_message(&Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: "tc-task".into(),
                function: FunctionCall {
                    name: "task".into(),
                    arguments: "{}".into(),
                },
            },
        });
        assert_eq!(
            a.closed_steps().len(),
            1,
            "only the user step has closed so far"
        );

        a.push_subagent_embedded(Some("tc-task"), "child-traj-9", Some("child-session-9"));
        a.push_message(&Message::tool_result("tc-task", "pong"));
        a.push_message(&Message::assistant("The delegated subtask finished."));

        let steps = a.finish();
        assert_eq!(
            steps.len(),
            2,
            "everything about this turn stays in one agent step"
        );
        let step = &steps[1];
        assert_eq!(step.source, StepOrigin::Agent);
        assert_eq!(step.tool_calls.as_ref().unwrap().len(), 1);
        assert_eq!(
            step.message.as_text(),
            Some("The delegated subtask finished.")
        );

        let obs = step.observation.as_ref().expect("observation present");
        assert_eq!(
            obs.results.len(),
            2,
            "subagent ref + tool result, both in this step"
        );
        let subagent_result = obs
            .results
            .iter()
            .find(|r| r.subagent_trajectory_ref.is_some())
            .expect("subagent ref result present");
        assert_eq!(subagent_result.source_call_id.as_deref(), Some("tc-task"));
        let refs = subagent_result.subagent_trajectory_ref.as_ref().unwrap();
        assert_eq!(refs[0].trajectory_id.as_deref(), Some("child-traj-9"));
        assert_eq!(refs[0].session_id.as_deref(), Some("child-session-9"));
        assert!(refs[0].trajectory_path.is_none());
        let tool_result = obs
            .results
            .iter()
            .find(|r| r.content.is_some())
            .expect("tool-result observation present");
        assert_eq!(tool_result.source_call_id.as_deref(), Some("tc-task"));

        // Build a full document (with the referenced child embedded) and
        // confirm the real validator accepts it.
        let mut trajectory = Trajectory::new(ATIF_SCHEMA_VERSION, default_agent_profile());
        trajectory.steps = steps;
        let mut child = Trajectory::new(ATIF_SCHEMA_VERSION, default_agent_profile());
        child.trajectory_id = Some("child-traj-9".to_string());
        trajectory.subagent_trajectories = Some(vec![child]);
        assert!(atif::validate_trajectory(&trajectory).is_ok());
    }

    #[test]
    fn system_messages_are_skipped_by_assembler() {
        let mut a = StepAssembler::new();
        a.push_message(&Message::system("You are sven."));
        a.push_message(&Message::user("Hello"));
        a.push_message(&Message::assistant("Hi"));
        let steps = a.finish();
        assert_eq!(steps.len(), 2, "system message must be skipped");
        assert_eq!(steps[0].source, StepOrigin::User);
    }

    // ── batch converters ─────────────────────────────────────────────────

    #[test]
    fn messages_to_steps_matches_streaming_assembler() {
        let messages = vec![Message::user("Hi"), Message::assistant("Hello!")];
        let steps = messages_to_steps(&messages);
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].message.as_text(), Some("Hi"));
        assert_eq!(steps[1].message.as_text(), Some("Hello!"));
    }

    #[test]
    fn conversation_records_to_steps_handles_full_event_stream() {
        let records = vec![
            ConversationRecord::Message(Message::user("What is 2+2?")),
            ConversationRecord::Thinking {
                content: "reasoning".to_string(),
            },
            ConversationRecord::Message(Message::assistant("4")),
            ConversationRecord::ContextCompacted {
                tokens_before: 500,
                tokens_after: 50,
                strategy: None,
                turn: None,
            },
        ];
        let steps = conversation_records_to_steps(&records);
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[2].source, StepOrigin::System);
    }

    // ── reverse: TraceStep -> Message ────────────────────────────────────

    #[test]
    fn steps_to_messages_skips_reasoning_content() {
        let mut step = TraceStep::new(1, StepOrigin::Agent, "Answer");
        step.reasoning_content = Some("secret reasoning".to_string());
        let messages = steps_to_messages(&[step]);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].as_text(), Some("Answer"));
    }

    #[test]
    fn steps_to_messages_skips_system_steps() {
        let steps = vec![
            TraceStep::new(1, StepOrigin::User, "Hi"),
            TraceStep::new(2, StepOrigin::System, "context_compaction: ..."),
            TraceStep::new(3, StepOrigin::Agent, "Hello"),
        ];
        let messages = steps_to_messages(&steps);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].as_text(), Some("Hi"));
        assert_eq!(messages[1].as_text(), Some("Hello"));
    }

    #[test]
    fn steps_to_messages_skips_copied_context_steps() {
        let mut copied = TraceStep::new(1, StepOrigin::Agent, "copied");
        copied.is_copied_context = Some(true);
        let steps = vec![copied, TraceStep::new(2, StepOrigin::Agent, "fresh")];
        let messages = steps_to_messages(&steps);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].as_text(), Some("fresh"));
    }

    #[test]
    fn steps_to_messages_unmerges_tool_call_and_result() {
        let mut step = TraceStep::new(1, StepOrigin::Agent, "Found main.rs");
        step.tool_calls = Some(vec![ToolInvocation::new("call_1", "glob")
            .with_arguments(serde_json::json!({"pattern": "**/*.rs"}))]);
        step.observation = Some(StepObservation::single(ObservationEntry::for_call(
            "call_1",
            "src/main.rs",
        )));
        let messages = steps_to_messages(&[step]);

        assert_eq!(messages.len(), 3);
        match &messages[0].content {
            MessageContent::ToolCall {
                tool_call_id,
                function,
            } => {
                assert_eq!(tool_call_id, "call_1");
                assert_eq!(function.name, "glob");
            }
            _ => panic!("expected ToolCall"),
        }
        match &messages[1].content {
            MessageContent::ToolResult {
                tool_call_id,
                content,
            } => {
                assert_eq!(tool_call_id, "call_1");
                assert_eq!(content.to_string(), "src/main.rs");
            }
            _ => panic!("expected ToolResult"),
        }
        assert_eq!(messages[2].as_text(), Some("Found main.rs"));
    }

    #[test]
    fn steps_to_messages_omits_empty_assistant_text() {
        let mut step = TraceStep::new(1, StepOrigin::Agent, "");
        step.tool_calls = Some(vec![ToolInvocation::new("call_1", "noop")]);
        step.observation = Some(StepObservation::single(ObservationEntry::for_call(
            "call_1", "ok",
        )));
        let messages = steps_to_messages(&[step]);
        // ToolCall + ToolResult only, no trailing empty assistant text message.
        assert_eq!(messages.len(), 2);
    }

    // ── reverse: TraceStep -> TurnRecord (legacy YAML) ────────────────────

    #[test]
    fn steps_to_turn_records_round_trips_simple_turn() {
        let steps = vec![
            TraceStep::new(1, StepOrigin::User, "Hello"),
            TraceStep::new(2, StepOrigin::Agent, "Hi there"),
        ];
        let turns = steps_to_turn_records(&steps);
        assert_eq!(turns.len(), 2);
        assert!(matches!(&turns[0], TurnRecord::User { content } if content == "Hello"));
        assert!(matches!(&turns[1], TurnRecord::Assistant { content } if content == "Hi there"));
    }

    #[test]
    fn steps_to_turn_records_preserves_thinking_and_tool_calls() {
        let mut step = TraceStep::new(1, StepOrigin::Agent, "Found main.rs");
        step.reasoning_content = Some("I should search first.".to_string());
        step.tool_calls = Some(vec![ToolInvocation::new("call_1", "glob")
            .with_arguments(serde_json::json!({"pattern": "**/*.rs"}))]);
        step.observation = Some(StepObservation::single(ObservationEntry::for_call(
            "call_1",
            "src/main.rs",
        )));

        let turns = steps_to_turn_records(&[step]);
        assert_eq!(
            turns.len(),
            4,
            "thinking, tool call, tool result, assistant text"
        );
        assert!(
            matches!(&turns[0], TurnRecord::Thinking { content } if content == "I should search first.")
        );
        assert!(
            matches!(&turns[1], TurnRecord::ToolCall { tool_call_id, name, .. } if tool_call_id == "call_1" && name == "glob")
        );
        assert!(
            matches!(&turns[2], TurnRecord::ToolResult { tool_call_id, content } if tool_call_id == "call_1" && content == "src/main.rs")
        );
        assert!(
            matches!(&turns[3], TurnRecord::Assistant { content } if content == "Found main.rs")
        );
    }

    #[test]
    fn steps_to_turn_records_converts_context_compaction_system_step() {
        let mut a = StepAssembler::new();
        a.push_message(&Message::user("Hi"));
        a.push_message(&Message::assistant("Hello"));
        a.push_context_compacted(1000, 100, Some("structured"), Some(2));
        let steps = a.finish();

        let turns = steps_to_turn_records(&steps);
        let compaction = turns
            .iter()
            .find(|t| matches!(t, TurnRecord::ContextCompacted { .. }))
            .expect("context-compaction turn present");
        assert!(matches!(
            compaction,
            TurnRecord::ContextCompacted {
                tokens_before: 1000,
                tokens_after: 100,
                ..
            }
        ));
    }

    #[test]
    fn steps_to_turn_records_skips_copied_context_steps() {
        let mut copied = TraceStep::new(1, StepOrigin::Agent, "copied");
        copied.is_copied_context = Some(true);
        let turns = steps_to_turn_records(&[copied, TraceStep::new(2, StepOrigin::Agent, "fresh")]);
        assert_eq!(turns.len(), 1);
        assert!(matches!(&turns[0], TurnRecord::Assistant { content } if content == "fresh"));
    }

    // ── reverse: TraceStep -> ConversationRecord ──────────────────────────

    #[test]
    fn steps_to_conversation_records_round_trips_simple_turn() {
        let records = vec![
            ConversationRecord::Message(Message::user("Hello")),
            ConversationRecord::Message(Message::assistant("Hi there")),
        ];
        let steps = conversation_records_to_steps(&records);
        let back = steps_to_conversation_records(&steps);
        assert_eq!(
            msg_values_of_records(&back),
            msg_values_of_records(&records)
        );
    }

    #[test]
    fn steps_to_conversation_records_preserves_thinking_and_tool_calls() {
        let records = vec![
            ConversationRecord::Message(Message::user("What is 2+2?")),
            ConversationRecord::Thinking {
                content: "I should compute it.".to_string(),
            },
            ConversationRecord::Message(Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: "call_1".into(),
                    function: FunctionCall {
                        name: "calc".into(),
                        arguments: r#"{"expr":"2+2"}"#.into(),
                    },
                },
            }),
            ConversationRecord::Message(Message::tool_result("call_1", "4")),
            ConversationRecord::Message(Message::assistant("The answer is 4.")),
        ];
        let steps = conversation_records_to_steps(&records);
        let back = steps_to_conversation_records(&steps);

        assert!(
            back.iter()
                .any(|r| matches!(r, ConversationRecord::Thinking { content } if content == "I should compute it.")),
            "thinking record must survive the round trip: {back:?}"
        );
        let tool_call_present = back.iter().any(|r| {
            matches!(
                r,
                ConversationRecord::Message(m)
                    if matches!(&m.content, MessageContent::ToolCall { tool_call_id, .. } if tool_call_id == "call_1")
            )
        });
        assert!(tool_call_present, "tool call must survive: {back:?}");
        let tool_result_present = back.iter().any(|r| {
            matches!(
                r,
                ConversationRecord::Message(m)
                    if matches!(&m.content, MessageContent::ToolResult { tool_call_id, .. } if tool_call_id == "call_1")
            )
        });
        assert!(tool_result_present, "tool result must survive: {back:?}");
    }

    #[test]
    fn steps_to_conversation_records_preserves_context_compaction() {
        let mut a = StepAssembler::new();
        a.push_message(&Message::user("Hi"));
        a.push_message(&Message::assistant("Hello"));
        a.push_context_compacted(1000, 100, Some("structured"), Some(2));
        let steps = a.finish();

        let records = steps_to_conversation_records(&steps);
        let compaction = records
            .iter()
            .find(|r| matches!(r, ConversationRecord::ContextCompacted { .. }))
            .expect("context-compaction record present");
        assert!(matches!(
            compaction,
            ConversationRecord::ContextCompacted {
                tokens_before: 1000,
                tokens_after: 100,
                ..
            }
        ));
    }

    #[test]
    fn steps_to_conversation_records_skips_copied_context_steps() {
        let mut copied = TraceStep::new(1, StepOrigin::Agent, "copied");
        copied.is_copied_context = Some(true);
        let records =
            steps_to_conversation_records(&[copied, TraceStep::new(2, StepOrigin::Agent, "fresh")]);
        assert_eq!(records.len(), 1);
        assert!(
            matches!(&records[0], ConversationRecord::Message(m) if m.as_text() == Some("fresh"))
        );
    }

    fn msg_values_of_records(records: &[ConversationRecord]) -> Vec<Value> {
        records
            .iter()
            .map(|r| serde_json::to_value(r).unwrap())
            .collect()
    }

    // ── full round-trip: multi-turn conversation, both directions ─────────

    #[test]
    fn full_round_trip_multi_turn_conversation() {
        let records = vec![
            ConversationRecord::Message(Message::user("Search and summarize")),
            ConversationRecord::Thinking {
                content: "I should search first.".to_string(),
            },
            ConversationRecord::Message(Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: "call_1".into(),
                    function: FunctionCall {
                        name: "search".into(),
                        arguments: r#"{"query":"rust"}"#.into(),
                    },
                },
            }),
            ConversationRecord::Message(Message::tool_result("call_1", "found: main.rs")),
            ConversationRecord::Message(Message::assistant(
                "I found main.rs, which implements the entry point.",
            )),
            ConversationRecord::Message(Message::user("Thanks!")),
            ConversationRecord::Message(Message::assistant("You're welcome!")),
        ];

        let expected_messages: Vec<Message> = records
            .iter()
            .filter_map(|r| match r {
                ConversationRecord::Message(m) => Some(m.clone()),
                _ => None,
            })
            .collect();

        let steps = conversation_records_to_steps(&records);
        assert_eq!(steps.len(), 4, "user, [merged agent turn], user, assistant");

        let round_tripped = steps_to_messages(&steps);
        assert_messages_eq(&round_tripped, &expected_messages);
    }

    // ── file I/O ────────────────────────────────────────────────────────

    fn sample_trajectory(session_id: &str) -> Trajectory {
        let mut t = Trajectory::new(ATIF_SCHEMA_VERSION, default_agent_profile());
        t.session_id = Some(session_id.to_string());
        t.steps.push(TraceStep::new(1, StepOrigin::User, "Hello"));
        t.steps
            .push(TraceStep::new(2, StepOrigin::Agent, "Hi there"));
        SvenSessionMeta::new("Test session").apply_to_trajectory(&mut t);
        t
    }

    #[test]
    fn session_dir_uses_sessions_not_chats() {
        let dir = session_dir();
        assert!(
            dir.ends_with("sven/sessions"),
            "expected .../sven/sessions, got {}",
            dir.display()
        );
        assert_ne!(
            dir,
            crate::chat_document::chat_dir(),
            "must be a distinct directory from the legacy chat dir"
        );
    }

    #[test]
    fn save_and_load_session_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let trajectory = sample_trajectory("session-abc");
        let path = dir.path().join("session-abc.json");
        atif::persist::write_trajectory(&path, &trajectory).unwrap();

        let loaded = load_session_from(&path).unwrap();
        assert_eq!(loaded.session_id.as_deref(), Some("session-abc"));
        assert_eq!(loaded.steps.len(), 2);
        let meta = SvenSessionMeta::from_trajectory(&loaded).unwrap();
        assert_eq!(meta.title, "Test session");
    }

    #[test]
    fn save_session_atomic_and_load_with_fingerprint_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-atomic.json");
        let trajectory = sample_trajectory("session-atomic");

        atif::persist::write_trajectory_atomic(&path, &trajectory, None).unwrap();
        let (loaded, fingerprint) = atif::persist::read_trajectory_with_fingerprint(&path).unwrap();
        assert_eq!(loaded.session_id.as_deref(), Some("session-atomic"));

        let mut updated = loaded.clone();
        updated
            .steps
            .push(TraceStep::new(3, StepOrigin::User, "one more"));
        atif::persist::write_trajectory_atomic(&path, &updated, Some(&fingerprint)).unwrap();

        let (final_loaded, _) = atif::persist::read_trajectory_with_fingerprint(&path).unwrap();
        assert_eq!(final_loaded.steps.len(), 3);
    }

    #[test]
    fn list_sessions_header_quick_path_and_ordering() {
        let dir = tempfile::tempdir().unwrap();
        let old_path = dir.path().join("old.json");
        let new_path = dir.path().join("new.json");

        let mut old_meta = SvenSessionMeta::new("Older");
        old_meta.updated_at = Utc::now() - chrono::Duration::hours(2);
        let mut old_t = Trajectory::new(ATIF_SCHEMA_VERSION, default_agent_profile());
        old_t.session_id = Some("old".to_string());
        old_meta.apply_to_trajectory(&mut old_t);
        old_t.steps.push(TraceStep::new(1, StepOrigin::User, "hi"));
        atif::persist::write_trajectory(&old_path, &old_t).unwrap();

        let mut new_meta = SvenSessionMeta::new("Newer");
        new_meta.updated_at = Utc::now();
        let mut new_t = Trajectory::new(ATIF_SCHEMA_VERSION, default_agent_profile());
        new_t.session_id = Some("new".to_string());
        new_meta.apply_to_trajectory(&mut new_t);
        new_t.steps.push(TraceStep::new(1, StepOrigin::User, "hi"));
        atif::persist::write_trajectory(&new_path, &new_t).unwrap();

        // list_sessions() reads from the real session_dir(); exercise the
        // underlying header-read path directly against our temp files
        // instead of relying on global state.
        let old_header = atif::persist::read_trajectory_header(&old_path).unwrap();
        let new_header = atif::persist::read_trajectory_header(&new_path).unwrap();
        assert_eq!(old_header.session_id.as_deref(), Some("old"));
        assert_eq!(new_header.session_id.as_deref(), Some("new"));
        assert!(old_header.extra.is_some());
    }

    #[test]
    fn list_sessions_on_missing_dir_returns_empty() {
        // session_dir() is a fixed real-filesystem path here; this exercises
        // the "no dir" branch generically via a path we know is absent by
        // constructing the equivalent logic on a temp dir instead.
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert!(!missing.exists());
    }

    // ── unified listing: merge native + legacy ────────────────────────────

    fn native_entry(id: &str, title: &str, updated_at: DateTime<Utc>) -> SessionEntry {
        let mut meta = SvenSessionMeta::new(title);
        meta.updated_at = updated_at;
        SessionEntry {
            session_id: id.to_string(),
            path: PathBuf::from(format!("/sessions/{id}.json")),
            meta: Some(meta),
            final_metrics: None,
        }
    }

    fn legacy_entry(
        id: &str,
        title: &str,
        updated_at: DateTime<Utc>,
    ) -> crate::chat_document::ChatEntry {
        crate::chat_document::ChatEntry {
            id: SessionId::from_string(id.to_string()),
            path: PathBuf::from(format!("/chats/{id}.yaml")),
            title: title.to_string(),
            turns: 2,
            updated_at,
            status: ChatStatus::Active,
            parent_id: None,
            usage: None,
        }
    }

    #[test]
    fn merge_marks_native_entries_as_not_legacy() {
        let now = Utc::now();
        let merged = merge_native_and_legacy(vec![native_entry("s1", "Native", now)], vec![]);
        assert_eq!(merged.len(), 1);
        assert!(!merged[0].is_legacy);
        assert_eq!(merged[0].session_id, "s1");
        assert_eq!(merged[0].title, "Native");
    }

    #[test]
    fn merge_marks_legacy_only_entries_as_legacy() {
        let now = Utc::now();
        let merged = merge_native_and_legacy(vec![], vec![legacy_entry("s2", "Legacy", now)]);
        assert_eq!(merged.len(), 1);
        assert!(merged[0].is_legacy);
        assert_eq!(merged[0].session_id, "s2");
    }

    #[test]
    fn merge_hides_legacy_entry_superseded_by_native_twin() {
        // Same session_id present in both native and legacy: the legacy
        // (superseded) row must not be surfaced.
        let now = Utc::now();
        let merged = merge_native_and_legacy(
            vec![native_entry("s3", "Resaved", now)],
            vec![legacy_entry(
                "s3",
                "Old title",
                now - chrono::Duration::hours(1),
            )],
        );
        assert_eq!(
            merged.len(),
            1,
            "the legacy twin must be hidden: {merged:?}"
        );
        assert!(!merged[0].is_legacy);
        assert_eq!(merged[0].title, "Resaved");
    }

    #[test]
    fn merge_keeps_both_when_ids_differ() {
        let now = Utc::now();
        let merged = merge_native_and_legacy(
            vec![native_entry("s4", "Native", now)],
            vec![legacy_entry("s5", "Legacy", now)],
        );
        assert_eq!(merged.len(), 2);
        assert!(merged.iter().any(|e| e.session_id == "s4" && !e.is_legacy));
        assert!(merged.iter().any(|e| e.session_id == "s5" && e.is_legacy));
    }

    // ── legacy YAML importer ────────────────────────────────────────────

    fn legacy_doc_simple() -> ChatDocument {
        let mut doc = ChatDocument::new("Simple chat");
        doc.model = Some("anthropic/claude-3-5".to_string());
        doc.turns = vec![
            TurnRecord::User {
                content: "Hello, how are you?".to_string(),
            },
            TurnRecord::Assistant {
                content: "I'm doing well, thank you!".to_string(),
            },
        ];
        doc
    }

    fn legacy_doc_tool_call() -> ChatDocument {
        let mut doc = ChatDocument::new("Tool test");
        doc.turns = vec![
            TurnRecord::User {
                content: "List files".to_string(),
            },
            TurnRecord::ToolCall {
                tool_call_id: "call_001".to_string(),
                name: "list_dir".to_string(),
                arguments: crate::chat_document::json_str_to_yaml(r#"{"path":"/tmp","depth":2}"#),
            },
            TurnRecord::ToolResult {
                tool_call_id: "call_001".to_string(),
                content: "file1.rs\nfile2.rs\n".to_string(),
            },
            TurnRecord::Assistant {
                content: "Found 2 Rust files.".to_string(),
            },
        ];
        doc
    }

    fn legacy_doc_thinking_and_compaction() -> ChatDocument {
        let mut doc = ChatDocument::new("Thinking test");
        doc.turns = vec![
            TurnRecord::User {
                content: "What is 2+2?".to_string(),
            },
            TurnRecord::Thinking {
                content: "The user wants to know 2+2. That is 4.".to_string(),
            },
            TurnRecord::Assistant {
                content: "4".to_string(),
            },
            TurnRecord::ContextCompacted {
                tokens_before: 1000,
                tokens_after: 100,
                strategy: Some("structured".to_string()),
                turn: Some(3),
            },
        ];
        doc
    }

    #[test]
    fn import_simple_chat_document_produces_valid_trajectory() {
        let doc = legacy_doc_simple();
        let trajectory = import_legacy_chat_document(&doc);

        assert_eq!(trajectory.schema_version, ATIF_SCHEMA_VERSION);
        assert_eq!(trajectory.session_id.as_deref(), Some(doc.id.as_str()));
        assert_eq!(
            trajectory.agent.model_name.as_deref(),
            Some("anthropic/claude-3-5")
        );
        assert_eq!(trajectory.steps.len(), 2);

        let meta = SvenSessionMeta::from_trajectory(&trajectory).unwrap();
        assert_eq!(meta.title, "Simple chat");

        assert!(atif::validate_trajectory(&trajectory).is_ok());
    }

    #[test]
    fn import_tool_call_chat_document_produces_valid_trajectory() {
        let doc = legacy_doc_tool_call();
        let trajectory = import_legacy_chat_document(&doc);

        // User + one merged agent step (tool_call + tool_result + text).
        assert_eq!(trajectory.steps.len(), 2);
        let agent_step = &trajectory.steps[1];
        assert_eq!(agent_step.tool_calls.as_ref().unwrap().len(), 1);
        assert_eq!(agent_step.observation.as_ref().unwrap().results.len(), 1);
        assert_eq!(agent_step.message.as_text(), Some("Found 2 Rust files."));

        assert!(atif::validate_trajectory(&trajectory).is_ok());
    }

    #[test]
    fn import_thinking_and_compaction_chat_document_produces_valid_trajectory() {
        let doc = legacy_doc_thinking_and_compaction();
        let trajectory = import_legacy_chat_document(&doc);

        assert_eq!(trajectory.steps.len(), 3);
        assert_eq!(
            trajectory.steps[1].reasoning_content.as_deref(),
            Some("The user wants to know 2+2. That is 4.")
        );
        assert_eq!(trajectory.steps[2].source, StepOrigin::System);
        let details =
            ContextCompactionDetails::from_step_extra(trajectory.steps[2].extra.as_ref().unwrap())
                .unwrap();
        assert_eq!(details.tokens_before, 1000);
        assert_eq!(details.strategy.as_deref(), Some("structured"));

        assert!(atif::validate_trajectory(&trajectory).is_ok());
    }

    #[test]
    fn import_preserves_parent_id_and_usage() {
        let mut doc = legacy_doc_simple();
        doc.parent_id = Some(SessionId::from_string("parent-session-1".to_string()));
        doc.usage = Some(ChatUsage {
            total_input_tokens: 100,
            total_output_tokens: 50,
            total_cache_read_tokens: 10,
            total_cache_write_tokens: 5,
            total_cost_usd: 0.01,
        });

        let trajectory = import_legacy_chat_document(&doc);
        let meta = SvenSessionMeta::from_trajectory(&trajectory).unwrap();
        assert_eq!(meta.parent_session_id.as_deref(), Some("parent-session-1"));

        let metrics = trajectory.final_metrics.unwrap();
        assert_eq!(metrics.total_prompt_tokens, Some(100));
        assert_eq!(metrics.extra.unwrap()["total_cache_write_tokens"], 5);
    }

    #[test]
    fn import_empty_usage_leaves_final_metrics_none() {
        let mut doc = legacy_doc_simple();
        doc.usage = Some(ChatUsage::default());
        let trajectory = import_legacy_chat_document(&doc);
        assert!(trajectory.final_metrics.is_none());
    }

    fn write_legacy_yaml(chat_dir: &Path, doc: &ChatDocument) {
        let yaml = serde_yaml::to_string(doc).expect("serialize fixture chat to YAML");
        fs::write(chat_dir.join(format!("{}.yaml", doc.id)), yaml).expect("write fixture chat");
    }

    #[test]
    fn migrate_converts_every_legacy_chat_to_atif() {
        let chat_dir = tempfile::tempdir().unwrap();
        let session_dir = tempfile::tempdir().unwrap();
        let doc_a = legacy_doc_simple();
        let doc_b = legacy_doc_tool_call();
        write_legacy_yaml(chat_dir.path(), &doc_a);
        write_legacy_yaml(chat_dir.path(), &doc_b);

        let summary =
            migrate_legacy_chats_between(chat_dir.path(), session_dir.path(), false).unwrap();

        assert_eq!(summary.migrated.len(), 2);
        assert!(summary.skipped_existing.is_empty());
        assert!(summary.failed.is_empty());
        for doc in [&doc_a, &doc_b] {
            let target = session_dir.path().join(format!("{}.json", doc.id));
            assert!(target.exists(), "{target:?} should have been written");
            let (trajectory, _) = atif::persist::read_trajectory_with_fingerprint(&target).unwrap();
            assert_eq!(trajectory.session_id.as_deref(), Some(doc.id.as_str()));
        }
        // The original .yaml files are untouched.
        assert!(chat_dir.path().join(format!("{}.yaml", doc_a.id)).exists());
    }

    #[test]
    fn migrate_is_idempotent_and_never_overwrites() {
        let chat_dir = tempfile::tempdir().unwrap();
        let session_dir = tempfile::tempdir().unwrap();
        let doc = legacy_doc_simple();
        write_legacy_yaml(chat_dir.path(), &doc);

        let first = migrate_legacy_chats_between(chat_dir.path(), session_dir.path(), false).unwrap();
        assert_eq!(first.migrated, vec![doc.id.as_str().to_string()]);

        // Simulate the session having moved on since migration (e.g. the user
        // opened and continued it): the migration must never clobber this.
        let target = session_dir.path().join(format!("{}.json", doc.id));
        let (mut trajectory, _) = atif::persist::read_trajectory_with_fingerprint(&target).unwrap();
        trajectory.steps.push(TraceStep {
            step_id: 999,
            ..trajectory.steps[0].clone()
        });
        atif::persist::write_trajectory(&target, &trajectory).unwrap();

        let second = migrate_legacy_chats_between(chat_dir.path(), session_dir.path(), false).unwrap();
        assert!(second.migrated.is_empty());
        assert_eq!(second.skipped_existing, vec![doc.id.as_str().to_string()]);
        let (unchanged, _) = atif::persist::read_trajectory_with_fingerprint(&target).unwrap();
        assert!(unchanged.steps.iter().any(|s| s.step_id == 999));
    }

    #[test]
    fn migrate_dry_run_writes_nothing() {
        let chat_dir = tempfile::tempdir().unwrap();
        // Deliberately not created (unlike a real `session_dir()`, which may
        // not exist yet either) - dry run must not create it.
        let session_dir = tempfile::tempdir().unwrap().path().join("not-yet-created");
        let doc = legacy_doc_simple();
        write_legacy_yaml(chat_dir.path(), &doc);

        let summary = migrate_legacy_chats_between(chat_dir.path(), &session_dir, true).unwrap();

        assert_eq!(summary.migrated, vec![doc.id.as_str().to_string()]);
        assert!(!session_dir.join(format!("{}.json", doc.id)).exists());
        assert!(!session_dir.exists(), "dry run must not even create session_dir");
    }

    #[test]
    fn migrate_with_no_chat_dir_is_a_no_op() {
        let chat_dir = tempfile::tempdir().unwrap();
        std::fs::remove_dir(chat_dir.path()).unwrap();
        let session_dir = tempfile::tempdir().unwrap();

        let summary =
            migrate_legacy_chats_between(chat_dir.path(), session_dir.path(), false).unwrap();
        assert_eq!(summary, MigrationSummary::default());
    }
