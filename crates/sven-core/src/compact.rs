use sven_model::{Message, Role};

const SUMMARIZE_PROMPT: &str =
    "You are a context compaction assistant. Summarise the following conversation history \
     in a concise, information-dense way. Preserve all technical details, decisions, file \
     names, code snippets, and tool outputs that may be relevant to future work. \
     The summary will replace the original history to free up context space.";

/// Replace the conversation history with a single summarisation request
/// result so the session stays within its token budget.
///
/// In headless/CI mode the model is called synchronously to produce the
/// summary before the main agent continues.  The caller is responsible for
/// actually invoking the model; this function just restructures the message
/// list to contain the prompt.
pub fn compact_session(messages: &mut Vec<Message>, system_msg: Option<Message>) -> usize {
    let before = messages.len();

    // Serialise the current history into plain text
    let history_text: String = messages
        .iter()
        .filter(|m| !matches!(m.role, Role::System))
        .map(|m| {
            let role = match m.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
                Role::Tool => "Tool",
                Role::System => "System",
            };
            let text = match &m.content {
                sven_model::MessageContent::Text(t) => t.clone(),
                sven_model::MessageContent::ToolCall { function, .. } => {
                    format!("[tool_call: {}({})]", function.name, function.arguments)
                }
                sven_model::MessageContent::ToolResult { content, .. } => {
                    format!("[tool_result: {content}]")
                }
            };
            format!("{role}: {text}")
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let summary_request = Message::user(format!(
        "{SUMMARIZE_PROMPT}\n\n---\n\n{history_text}"
    ));

    messages.clear();
    if let Some(sys) = system_msg {
        messages.push(sys);
    }
    messages.push(summary_request);

    before
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use sven_model::{FunctionCall, Message, MessageContent, Role};
    use super::*;

    fn make_history() -> Vec<Message> {
        vec![
            Message::system("You are a helpful assistant."),
            Message::user("What is Rust?"),
            Message::assistant("Rust is a systems programming language."),
            Message::user("Show me an example."),
            Message::assistant("fn main() { println!(\"Hello\"); }"),
        ]
    }

    // ── Returns message count ─────────────────────────────────────────────────

    #[test]
    fn returns_original_message_count() {
        let mut msgs = make_history();
        let before = compact_session(&mut msgs, None);
        assert_eq!(before, 5);
    }

    // ── Output structure ──────────────────────────────────────────────────────

    #[test]
    fn output_has_single_user_summary_request_without_system() {
        let mut msgs = make_history();
        compact_session(&mut msgs, None);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::User);
    }

    #[test]
    fn output_with_system_message_has_two_messages() {
        let mut msgs = make_history();
        let sys = Message::system("Keep this system message.");
        compact_session(&mut msgs, Some(sys));
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, Role::System);
        assert_eq!(msgs[1].role, Role::User);
    }

    #[test]
    fn system_message_content_is_preserved() {
        let mut msgs = make_history();
        let sys = Message::system("Custom system prompt.");
        compact_session(&mut msgs, Some(sys));
        assert_eq!(msgs[0].as_text(), Some("Custom system prompt."));
    }

    // ── History serialisation in the summary request ──────────────────────────

    #[test]
    fn summary_request_contains_original_text() {
        let mut msgs = make_history();
        compact_session(&mut msgs, None);
        let summary_text = msgs[0].as_text().unwrap();
        assert!(summary_text.contains("What is Rust?"));
        assert!(summary_text.contains("systems programming language"));
    }

    #[test]
    fn system_messages_excluded_from_history_text() {
        let mut msgs = make_history();
        compact_session(&mut msgs, None);
        let summary_text = msgs[0].as_text().unwrap();
        // The system message text should not appear in the serialised history
        assert!(!summary_text.contains("You are a helpful assistant"));
    }

    #[test]
    fn tool_call_serialised_in_history() {
        let mut msgs = vec![
            Message::user("run ls"),
            Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: "id1".into(),
                    function: FunctionCall { name: "shell".into(), arguments: r#"{"command":"ls"}"#.into() },
                },
            },
        ];
        compact_session(&mut msgs, None);
        let text = msgs[0].as_text().unwrap();
        assert!(text.contains("shell"), "tool name should appear in history");
        assert!(text.contains("ls"), "tool arg should appear in history");
    }

    #[test]
    fn tool_result_serialised_in_history() {
        let mut msgs = vec![
            Message::user("run ls"),
            Message::tool_result("id1", "file1.txt\nfile2.txt"),
        ];
        compact_session(&mut msgs, None);
        let text = msgs[0].as_text().unwrap();
        assert!(text.contains("file1.txt"));
    }

    // ── Empty history ─────────────────────────────────────────────────────────

    #[test]
    fn compact_empty_history_returns_zero() {
        let mut msgs: Vec<Message> = vec![];
        let count = compact_session(&mut msgs, None);
        assert_eq!(count, 0);
    }

    #[test]
    fn compact_empty_history_produces_single_request() {
        let mut msgs: Vec<Message> = vec![];
        compact_session(&mut msgs, None);
        assert_eq!(msgs.len(), 1);
    }
}
