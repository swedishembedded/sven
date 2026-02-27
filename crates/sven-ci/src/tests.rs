// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT

/// Tests for CI-mode step processing and pipe composition.
///
/// These are unit-level tests that exercise the logic that translates stdin
/// input and CLI options into step queues and history seeds, without starting
/// a real network connection.
///
/// ## Pipe composition test matrix
///
/// | Stdin content          | CLI prompt  | Expected behaviour                          |
/// |------------------------|-------------|---------------------------------------------|
/// | (empty)                | "task"      | Single step with "task"                     |
/// | (empty)                | (none)      | Single step with "" (empty, model handles)  |
/// | plain text             | (none)      | Single step with plain text (workflow fallback) |
/// | plain text             | "task"      | Step "task" prepended; plain text is second |
/// | workflow markdown      | (none)      | Steps from ## sections                      |
/// | workflow markdown      | "task"      | "task" prepended, then ## sections          |
/// | conversation markdown  | "task2"     | History seeded, single step "task2"         |
/// | conversation markdown  | (none) + pending | History seeded, step = pending user    |
/// | conversation markdown  | (none) + no pending | Error exit                         |
/// | JSONL                  | "task2"     | History seeded, single step "task2"         |
/// | JSONL                  | (none) + pending | History seeded, step = pending user    |
/// | JSONL                  | (none) + no pending | Error exit                         |
#[cfg(test)]
mod tests {
    use sven_input::{
        parse_conversation, parse_jsonl_full, parse_workflow,
        serialize_conversation_turn, serialize_jsonl_records,
        ConversationRecord, Step, StepQueue,
    };
    use sven_model::{Message, Role};

    // ── Helper aliases ────────────────────────────────────────────────────────

    fn is_conv(s: &str) -> bool {
        crate::runner::is_conversation_format(s)
    }

    fn is_jsonl(s: &str) -> bool {
        crate::runner::is_jsonl_format(s)
    }

    // ── is_conversation_format ────────────────────────────────────────────────

    #[test]
    fn conversation_format_detected_by_user_heading() {
        let md = "## User\n\nping\n\n## Sven\n\npong\n";
        assert!(is_conv(md), "## User heading should mark conversation format");
    }

    #[test]
    fn conversation_format_detected_by_sven_heading() {
        let md = "Some preamble.\n\n## Sven\n\nThe answer.\n";
        assert!(is_conv(md), "## Sven heading should mark conversation format");
    }

    #[test]
    fn conversation_format_detected_by_tool_heading() {
        let md = "## Tool\n\n```json\n{}\n```\n";
        assert!(is_conv(md), "## Tool heading should mark conversation format");
    }

    #[test]
    fn conversation_format_detected_by_tool_result_heading() {
        let md = "## Tool Result\n\n```\nok\n```\n";
        assert!(is_conv(md), "## Tool Result heading should mark conversation format");
    }

    #[test]
    fn plain_text_not_conversation_format() {
        assert!(!is_conv("Just a plain prompt with no markdown headings."));
    }

    #[test]
    fn workflow_markdown_not_conversation_format() {
        // Standard workflow markdown uses H2 headings for step labels but
        // none of the reserved conversation heading names.
        let md = "# My Workflow\n\n## Step one\nDo it.\n\n## Step two\nAnd this.";
        assert!(!is_conv(md), "workflow markdown should not be detected as conversation");
    }

    #[test]
    fn empty_string_not_conversation_format() {
        assert!(!is_conv(""));
        assert!(!is_conv("   \n  "));
    }

    #[test]
    fn h2_user_anywhere_in_document_is_sufficient() {
        let md = "# Title\n\nSome preamble text.\n\n## User\n\nhello\n";
        assert!(is_conv(md));
    }

    #[test]
    fn partial_match_user_in_body_not_detected() {
        // "## UserLogin" should not match "## User"
        let md = "## UserLogin\n\nsome content\n";
        assert!(!is_conv(md));
    }

    #[test]
    fn conversation_format_full_multi_turn() {
        let md = concat!(
            "## User\n\nping\n\n",
            "<!-- provider: mock, model: gpt-4o -->\n",
            "## Sven\n\npong\n\n",
            "## User\n\nsummarise the above\n\n",
            "## Sven\n\nSummary: pong was the response.\n",
        );
        assert!(is_conv(md));
    }

    // ── Typical sven stdout includes step label before ## User ────────────────

    #[test]
    fn sven_stdout_with_step_label_detected_as_conversation() {
        // The CI runner emits: "## (unlabelled)\n\n## User\n<content>\n\n## Sven\n..."
        // The step-label line is an Unknown H2 and must not block detection.
        let md = concat!(
            "## (unlabelled)\n\n",
            "## User\nhi\n\n",
            "## Sven\nHello!\n",
        );
        assert!(
            is_conv(md),
            "sven's own step-label prefix must not prevent conversation detection"
        );
    }

    // ── is_jsonl_format ───────────────────────────────────────────────────────

    #[test]
    fn jsonl_format_detected_for_all_json_objects() {
        let jsonl = "{\"role\":\"user\",\"content\":\"hello\"}\n\
                     {\"role\":\"assistant\",\"content\":\"hi\"}\n";
        assert!(is_jsonl(jsonl), "all-JSON-object lines should be JSONL");
    }

    #[test]
    fn jsonl_with_blank_lines_detected() {
        let jsonl = "{\"a\":1}\n\n{\"b\":2}\n";
        assert!(is_jsonl(jsonl));
    }

    #[test]
    fn plain_text_not_jsonl() {
        assert!(!is_jsonl("hello world"));
    }

    #[test]
    fn conversation_markdown_not_jsonl() {
        let md = "## User\nhello\n\n## Sven\nhi\n";
        assert!(!is_jsonl(md));
    }

    #[test]
    fn empty_not_jsonl() {
        assert!(!is_jsonl(""));
        assert!(!is_jsonl("   \n  \n"));
    }

    #[test]
    fn mixed_json_and_text_not_jsonl() {
        // One non-JSON line makes the whole thing not JSONL.
        let s = "{\"a\":1}\nsome plain text\n";
        assert!(!is_jsonl(s));
    }

    #[test]
    fn single_json_line_detected_as_jsonl() {
        let s = "{\"role\":\"user\",\"content\":\"task\"}\n";
        assert!(is_jsonl(s));
    }

    // ── Pipe: conversation format priority over JSONL ─────────────────────────

    #[test]
    fn conversation_takes_priority_over_jsonl_when_no_json_objects() {
        // A document with ## User / ## Sven but no JSON-object lines is
        // conversation, not JSONL.
        let md = "## User\nhello\n## Sven\nworld\n";
        assert!(is_conv(md));
        assert!(!is_jsonl(md));
    }

    // ── Input parsing: plain text → workflow fallback step ────────────────────

    #[test]
    fn plain_text_input_becomes_one_fallback_step() {
        let mut w = parse_workflow("Do something useful.");
        assert_eq!(w.steps.len(), 1);
        assert!(w.steps.pop().unwrap().content.contains("Do something useful"));
    }

    #[test]
    fn markdown_with_three_sections_becomes_three_steps() {
        let md = "## A\nStep A.\n\n## B\nStep B.\n\n## C\nStep C.";
        let w = parse_workflow(md);
        assert_eq!(w.steps.len(), 3);
    }

    #[test]
    fn empty_input_gives_single_fallback_step() {
        let w = parse_workflow("");
        assert_eq!(w.steps.len(), 1);
    }

    #[test]
    fn h1_becomes_title_and_preamble_goes_to_system_prompt() {
        let md = "# My Workflow\n\nThis context goes to the system prompt.\n\n## Step one\nDo it.";
        let mut w = parse_workflow(md);
        assert_eq!(w.title.as_deref(), Some("My Workflow"));
        assert!(w.system_prompt_append.as_deref()
            .map(|s| s.contains("context goes to the system prompt"))
            .unwrap_or(false));
        assert_eq!(w.steps.len(), 1);
        assert_eq!(w.steps.pop().unwrap().label.as_deref(), Some("Step one"));
    }

    // ── Extra prompt prepend logic ────────────────────────────────────────────

    #[test]
    fn extra_prompt_can_be_prepended_to_queue() {
        let mut base = parse_workflow("## Step 1\nDo it.").steps;
        let extra_step = Step {
            label: None,
            content: "Extra context.".into(),
            options: sven_input::StepOptions::default(),
        };
        let mut prepended = StepQueue::from(vec![extra_step]);
        while let Some(s) = base.pop() {
            prepended.push(s);
        }
        assert_eq!(prepended.len(), 2);
        let first = { let mut p = prepended; p.pop().unwrap() };
        assert_eq!(first.content, "Extra context.");
    }

    #[test]
    fn queue_without_extra_prompt_preserves_step_count() {
        let w = parse_workflow("## A\none\n\n## B\ntwo");
        assert_eq!(w.steps.len(), 2);
    }

    // ── Pipe: conversation → pending user input extraction ────────────────────

    /// When conversation markdown ends with a `## User` section that has no
    /// `## Sven` response, `parse_conversation` sets `pending_user_input`.
    /// The runner uses this as the step content when no CLI prompt is given.
    #[test]
    fn conversation_with_trailing_user_yields_pending() {
        let md = concat!(
            "## User\nFirst task\n\n",
            "## Sven\nDone.\n\n",
            "## User\nSecond task\n",
        );
        let conv = parse_conversation(md).unwrap();
        assert_eq!(conv.pending_user_input.as_deref(), Some("Second task"));
        assert_eq!(conv.history.len(), 2, "first two turns go to history");
    }

    #[test]
    fn conversation_without_trailing_user_has_no_pending() {
        let md = "## User\nTask\n\n## Sven\nDone.\n";
        let conv = parse_conversation(md).unwrap();
        assert!(conv.pending_user_input.is_none());
        assert_eq!(conv.history.len(), 2);
    }

    /// Simulates the exact pipe scenario that was broken:
    ///   echo "hi" | sven | sven
    ///
    /// First sven produces conversation markdown ending with `## Sven`.
    /// Second sven gets that markdown but no CLI prompt.
    /// Expected: pending_user_input is None, which the runner should reject.
    #[test]
    fn pipe_sven_to_sven_without_prompt_has_no_pending() {
        // Simulated output of `echo "hi" | sven --output-format conversation`
        let sven_stdout = concat!(
            "## (unlabelled)\n\n",
            "## User\nhi\n\n",
            "## Sven\nHello! How can I assist you today?\n",
        );
        assert!(is_conv(sven_stdout), "output is conversation format");

        let conv = parse_conversation(sven_stdout).unwrap();
        // No trailing ## User → no pending task for the second agent
        assert!(
            conv.pending_user_input.is_none(),
            "no pending input: runner must error rather than send empty message"
        );
    }

    /// Simulates the correct pipe pattern:
    ///   sven 'task1' | sven 'task2'
    ///
    /// Second sven receives conversation markdown and an explicit CLI prompt.
    /// Expected: history is seeded, step content = CLI prompt.
    #[test]
    fn pipe_sven_to_sven_with_cli_prompt_uses_prompt_as_step() {
        let sven_stdout = concat!(
            "## (unlabelled)\n\n",
            "## User\ntask1\n\n",
            "## Sven\nResult of task1.\n",
        );
        // Simulate what the runner does when extra_prompt is provided:
        // parse conversation, use extra_prompt as step content.
        let conv = parse_conversation(sven_stdout).unwrap();
        let history = conv.history;
        let extra_prompt = Some("task2".to_string());

        let step_content = extra_prompt
            .or(conv.pending_user_input)
            .unwrap_or_default();

        assert_eq!(step_content, "task2");
        assert_eq!(history.len(), 2, "first turn seeded into history");
    }

    /// Trailing ## User in piped conversation enables prompt-free relay.
    #[test]
    fn pipe_with_trailing_user_section_uses_pending_as_step() {
        let md = concat!(
            "## User\ntask1\n\n",
            "## Sven\nResult of task1. Next task: summarise.\n\n",
            "## User\nsummarise\n",
        );
        let conv = parse_conversation(md).unwrap();
        assert_eq!(conv.pending_user_input.as_deref(), Some("summarise"));

        // extra_prompt is None → fall back to pending
        let step_content = None::<String>
            .or(conv.pending_user_input)
            .unwrap_or_default();

        assert_eq!(step_content, "summarise");
        assert_eq!(conv.history.len(), 2);
    }

    // ── Pipe: JSONL → pending user input extraction ───────────────────────────

    fn make_jsonl_conversation(messages: &[Message]) -> String {
        let records: Vec<ConversationRecord> = messages
            .iter()
            .cloned()
            .map(ConversationRecord::Message)
            .collect();
        serialize_jsonl_records(&records)
    }

    #[test]
    fn jsonl_with_trailing_user_yields_pending() {
        let messages = vec![
            Message::user("task1"),
            Message::assistant("done"),
            Message::user("task2"),
        ];
        let jsonl = make_jsonl_conversation(&messages);

        assert!(is_jsonl(&jsonl), "must be detected as JSONL");
        assert!(!is_conv(&jsonl), "must not be detected as conversation markdown");

        let parsed = parse_jsonl_full(&jsonl).unwrap();
        assert_eq!(parsed.pending_user_input.as_deref(), Some("task2"));
        assert_eq!(parsed.history.len(), 2, "first two messages go to history");
    }

    #[test]
    fn jsonl_without_trailing_user_has_no_pending() {
        let messages = vec![
            Message::user("task1"),
            Message::assistant("done"),
        ];
        let jsonl = make_jsonl_conversation(&messages);

        let parsed = parse_jsonl_full(&jsonl).unwrap();
        assert!(parsed.pending_user_input.is_none());
        assert_eq!(parsed.history.len(), 2);
    }

    #[test]
    fn pipe_jsonl_with_cli_prompt_uses_prompt_as_step() {
        let messages = vec![
            Message::user("task1"),
            Message::assistant("Result of task1."),
        ];
        let jsonl = make_jsonl_conversation(&messages);

        let parsed = parse_jsonl_full(&jsonl).unwrap();
        let extra_prompt = Some("task2".to_string());

        let step_content = extra_prompt
            .or(parsed.pending_user_input)
            .unwrap_or_default();

        assert_eq!(step_content, "task2");
        assert_eq!(parsed.history.len(), 2);
    }

    #[test]
    fn pipe_jsonl_with_trailing_user_enables_prompt_free_relay() {
        let messages = vec![
            Message::user("task1"),
            Message::assistant("Here is the plan. Next step: execute."),
            Message::user("execute"),
        ];
        let jsonl = make_jsonl_conversation(&messages);

        assert!(is_jsonl(&jsonl));

        let parsed = parse_jsonl_full(&jsonl).unwrap();
        let step_content = None::<String>
            .or(parsed.pending_user_input)
            .unwrap_or_default();

        assert_eq!(step_content, "execute");
        assert_eq!(parsed.history.len(), 2);
    }

    // ── Pipe: compact output relay ────────────────────────────────────────────

    /// `--output-format compact` emits only the agent's final text.
    /// When piped, that text is plain (no ## markers) → treated as a plain
    /// workflow fallback step, which becomes the user message for the next agent.
    #[test]
    fn compact_output_becomes_plain_text_step() {
        let compact_output = "Here is a bug report:\n- missing null check on line 42\n";

        assert!(!is_conv(compact_output), "compact output is not conversation format");
        assert!(!is_jsonl(compact_output), "compact output is not JSONL");

        // parse_workflow fallback: no ## sections → single step with full body
        let mut w = parse_workflow(compact_output);
        assert_eq!(w.steps.len(), 1);
        assert!(w.steps.pop().unwrap().content.contains("missing null check"));
    }

    #[test]
    fn compact_relay_with_extra_prompt_prepends_task() {
        // Simulate: sven 'find bugs' --output-format compact | sven 'fix each bug'
        // The compact output becomes a single step, and 'fix each bug' is prepended.
        let compact = "- line 10: null deref\n- line 42: off-by-one\n";
        let extra_prompt = Some("Fix each of the following bugs:".to_string());

        assert!(!is_conv(compact));
        assert!(!is_jsonl(compact));

        let workflow = parse_workflow(compact);
        let mut q = workflow.steps;

        if let Some(prompt) = extra_prompt {
            let mut prepended = StepQueue::from(vec![Step {
                label: None,
                content: prompt,
                options: sven_input::StepOptions::default(),
            }]);
            while let Some(s) = q.pop() {
                prepended.push(s);
            }
            assert_eq!(prepended.len(), 2);
            let mut it = prepended;
            let first = it.pop().unwrap();
            let second = it.pop().unwrap();
            assert!(first.content.contains("Fix each"));
            assert!(second.content.contains("null deref"));
        }
    }

    // ── Pipe: multi-turn conversation round-trip ──────────────────────────────

    /// Full round-trip: build a conversation, serialize it, detect format,
    /// parse it back, and verify history + pending extraction.
    #[test]
    fn conversation_round_trip_with_pending_user() {
        let messages = vec![
            Message::user("Step one"),
            Message::assistant("Step one done."),
        ];
        let mut md = serialize_conversation_turn(&messages);
        md.push_str("## User\nStep two\n");

        assert!(is_conv(&md));
        assert!(!is_jsonl(&md));

        let conv = parse_conversation(&md).unwrap();
        assert_eq!(conv.history.len(), 2);
        assert_eq!(conv.pending_user_input.as_deref(), Some("Step two"));
    }

    /// Multi-turn conversation with tool calls serializes and deserializes correctly.
    #[test]
    fn conversation_with_tool_calls_detected_and_parsed() {
        use sven_model::{FunctionCall, MessageContent};
        let messages = vec![
            Message::user("List files"),
            Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: "c1".into(),
                    function: FunctionCall {
                        name: "glob".into(),
                        arguments: r#"{"pattern":"**/*.rs"}"#.into(),
                    },
                },
            },
            Message::tool_result("c1", "src/main.rs"),
            Message::assistant("Found main.rs"),
        ];
        let md = serialize_conversation_turn(&messages);

        assert!(is_conv(&md));
        let conv = parse_conversation(&md).unwrap();
        assert_eq!(conv.history.len(), 4);
        assert!(conv.pending_user_input.is_none());
    }

    // ── Pipe: JSONL round-trip ────────────────────────────────────────────────

    #[test]
    fn jsonl_round_trip_preserves_all_messages() {
        let messages = vec![
            Message::user("task1"),
            Message::assistant("result1"),
            Message::user("task2"),
            Message::assistant("result2"),
        ];
        let jsonl = make_jsonl_conversation(&messages);

        assert!(is_jsonl(&jsonl));

        let parsed = parse_jsonl_full(&jsonl).unwrap();
        // last message is assistant → no pending
        assert!(parsed.pending_user_input.is_none());
        assert_eq!(parsed.history.len(), 4);
    }

    #[test]
    fn jsonl_skips_system_messages_in_history() {
        let messages = vec![
            Message::system("You are sven."),
            Message::user("hello"),
            Message::assistant("hi"),
        ];
        let jsonl = make_jsonl_conversation(&messages);

        let parsed = parse_jsonl_full(&jsonl).unwrap();
        // System messages are stripped from history
        assert_eq!(parsed.history.len(), 2);
        assert_eq!(parsed.history[0].role, Role::User);
    }

    // ── resolve_model_cfg ─────────────────────────────────────────────────────

    use sven_model::resolve_model_cfg;
    use sven_config::ModelConfig;

    fn openai_base() -> ModelConfig {
        ModelConfig {
            provider: "openai".into(),
            name: "gpt-4o".into(),
            api_key_env: Some("OPENAI_API_KEY".into()),
            api_key: None,
            ..ModelConfig::default()
        }
    }

    #[test]
    fn slash_separated_model_splits_provider_and_name() {
        let spec = "anthropic/claude-opus-4-5";
        let (provider, name) = spec.split_once('/').unwrap();
        assert_eq!(provider, "anthropic");
        assert_eq!(name, "claude-opus-4-5");
    }

    #[test]
    fn bare_model_name_has_no_slash() {
        let spec = "gpt-4o";
        assert!(spec.split_once('/').is_none(), "bare name has no slash");
    }

    #[test]
    fn provider_change_clears_inherited_api_key_env() {
        let cfg = resolve_model_cfg(&openai_base(), "anthropic/claude-sonnet-4-5");
        assert_eq!(cfg.provider, "anthropic");
        assert_eq!(cfg.name, "claude-sonnet-4-5");
        assert!(
            cfg.api_key_env.is_none(),
            "api_key_env must be cleared when provider changes"
        );
        assert!(cfg.api_key.is_none(), "api_key must be cleared when provider changes");
    }

    #[test]
    fn same_provider_model_override_keeps_api_key_env() {
        let cfg = resolve_model_cfg(&openai_base(), "gpt-4o-mini");
        assert_eq!(cfg.provider, "openai");
        assert_eq!(cfg.name, "gpt-4o-mini");
        assert_eq!(
            cfg.api_key_env.as_deref(),
            Some("OPENAI_API_KEY"),
            "api_key_env must be kept when provider does not change"
        );
    }

    #[test]
    fn bare_provider_override_clears_api_key_env() {
        let cfg = resolve_model_cfg(&openai_base(), "anthropic");
        assert_eq!(cfg.provider, "anthropic");
        assert!(cfg.api_key_env.is_none(), "api_key_env must be cleared for bare provider change");
    }

    // ── Step label extraction ─────────────────────────────────────────────────

    #[test]
    fn step_label_preserved_for_logging() {
        let md = "## Analyse codebase\nRead the files.";
        let mut w = parse_workflow(md);
        let step = w.steps.pop().unwrap();
        assert_eq!(step.label.as_deref(), Some("Analyse codebase"));
    }

    #[test]
    fn plain_text_fallback_step_has_none_label() {
        let mut w = parse_workflow("Just text, no heading.");
        let step = w.steps.pop().unwrap();
        assert!(step.label.is_none());
    }

    // ── Step content: piped conversation takes priority rules ─────────────────

    /// Verifies the full priority chain used in the runner:
    ///   CLI prompt > piped pending > error
    #[test]
    fn step_content_priority_cli_beats_pending() {
        let pending = Some("pending task from pipe".to_string());
        let cli_prompt = Some("explicit cli task".to_string());

        let step_content = cli_prompt.or(pending);
        assert_eq!(step_content.as_deref(), Some("explicit cli task"));
    }

    #[test]
    fn step_content_priority_pending_used_when_no_cli() {
        let pending = Some("pending task from pipe".to_string());
        let cli_prompt: Option<String> = None;

        let step_content = cli_prompt.or(pending);
        assert_eq!(step_content.as_deref(), Some("pending task from pipe"));
    }

    #[test]
    fn step_content_priority_none_when_both_absent() {
        let pending: Option<String> = None;
        let cli_prompt: Option<String> = None;

        let step_content = cli_prompt.or(pending);
        assert!(step_content.is_none(), "runner must exit with error when both are absent");
    }

    // ── JSONL with thinking blocks ────────────────────────────────────────────

    #[test]
    fn jsonl_with_thinking_records_parses_correctly() {
        // Use the actual ConversationRecord serialization format:
        //   Message  → {"type":"message","data":{"role":"...", "content":"..."}}
        //   Thinking → {"type":"thinking","data":{"content":"..."}}
        // MessageContent::Text is untagged, so it serializes as a plain string.
        use serde_json::json;
        let records = vec![
            json!({"type":"message","data":{"role":"user","content":"hello"}}),
            json!({"type":"thinking","data":{"content":"Let me think..."}}),
            json!({"type":"message","data":{"role":"assistant","content":"world"}}),
        ];
        let jsonl: String = records
            .iter()
            .map(|v| v.to_string() + "\n")
            .collect();

        assert!(is_jsonl(&jsonl));

        let parsed = parse_jsonl_full(&jsonl).unwrap();
        // Thinking blocks are stored as ConversationRecord::Thinking,
        // not as Messages, so only the two messages appear in history.
        assert_eq!(parsed.history.len(), 2);
        assert!(parsed.pending_user_input.is_none());
    }

    // ── Edge cases ────────────────────────────────────────────────────────────

    #[test]
    fn whitespace_only_input_is_not_conversation() {
        assert!(!is_conv("   \n\t\n   "));
    }

    #[test]
    fn whitespace_only_input_is_not_jsonl() {
        assert!(!is_jsonl("   \n\t\n   "));
    }

    #[test]
    fn conversation_with_metadata_comments_parsed_correctly() {
        // The CI runner emits HTML comments with provider/model metadata before
        // each Sven section.  These must not affect conversation detection.
        let md = concat!(
            "## User\nWhat is 2+2?\n\n",
            "<!-- provider: anthropic, model: claude-opus-4-5 -->\n",
            "## Sven\nThe answer is 4.\n",
        );
        assert!(is_conv(md));
        let conv = parse_conversation(md).unwrap();
        assert_eq!(conv.history.len(), 2);
        // The metadata comment must be stripped from the assistant message
        let response = conv.history[1].as_text().unwrap();
        assert!(!response.contains("<!--"), "metadata comment must be stripped");
        assert!(response.contains("The answer is 4."));
    }

    #[test]
    fn jsonl_detection_stops_at_10_lines() {
        // Build a 20-line JSONL with the 11th being invalid — must still pass
        // because we only check the first 10.
        let mut lines: Vec<String> = (0..10)
            .map(|i| format!("{{\"i\":{i}}}"))
            .collect();
        lines.push("not json".to_string());
        lines.extend((11..20).map(|i| format!("{{\"i\":{i}}}")));
        let s = lines.join("\n");
        assert!(
            is_jsonl(&s),
            "detection only checks first 10 lines; rest must be ignored"
        );
    }
}
