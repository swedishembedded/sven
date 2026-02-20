use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

/// Interactively ask the user one or more questions and collect their answers.
///
/// Only works when stdin is an interactive TTY (i.e. the user is at a real
/// terminal). In headless / CI / piped mode stdin is not a TTY and no answers
/// can be collected; the tool returns an error so the model knows to proceed
/// with its best judgement rather than silently receiving empty answers.
pub struct AskQuestionTool;

#[async_trait]
impl Tool for AskQuestionTool {
    fn name(&self) -> &str { "ask_question" }

    fn description(&self) -> &str {
        "Ask the user one to three clarifying questions and return their answers. \
         Only available when running interactively (stdin is a terminal). \
         In headless / CI / piped mode this tool is unavailable — proceed with \
         your best judgement and note any assumptions in your response. \
         Use sparingly: only when the task is genuinely ambiguous and you cannot \
         make a reasonable assumption without more information."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "List of 1-3 questions to ask the user",
                    "minItems": 1,
                    "maxItems": 3
                }
            },
            "required": ["questions"]
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Auto }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let questions: Vec<String> = match call.args.get("questions").and_then(|v| v.as_array()) {
            Some(arr) => arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            None => return ToolOutput::err(&call.id, "missing 'questions' array"),
        };

        if questions.is_empty() {
            return ToolOutput::err(&call.id, "questions array must not be empty");
        }
        if questions.len() > 3 {
            return ToolOutput::err(&call.id, "at most 3 questions may be asked at a time");
        }

        // Refuse to block waiting for input when stdin is not an interactive
        // terminal.  Piped / CI / headless runs have stdin already consumed or
        // redirected; blindly reading would return empty strings immediately,
        // which would silently corrupt the model's context.
        if !stdin_is_tty() {
            let question_list = questions.iter()
                .enumerate()
                .map(|(i, q)| format!("  {}. {}", i + 1, q))
                .collect::<Vec<_>>()
                .join("\n");
            return ToolOutput::err(
                &call.id,
                format!(
                    "ask_question is unavailable in non-interactive (headless/CI/piped) mode.\n\
                     The following questions could not be answered:\n{question_list}\n\
                     Proceed with your best judgement and state your assumptions clearly."
                ),
            );
        }

        debug!(count = questions.len(), "ask_question tool");

        // Print questions to stderr so they appear on the terminal
        eprintln!();
        eprintln!("╔══ Questions from agent ══════════════════════════╗");
        for (i, q) in questions.iter().enumerate() {
            eprintln!("  {}. {}", i + 1, q);
        }
        eprintln!("╚══════════════════════════════════════════════════╝");

        // Read answers from the interactive terminal
        let mut answers: Vec<String> = Vec::new();
        for (i, q) in questions.iter().enumerate() {
            eprint!("  Answer {}: ", i + 1);
            let answer = read_stdin_line().await;
            answers.push(format!("Q: {}\nA: {}", q, answer));
        }
        eprintln!();

        ToolOutput::ok(&call.id, answers.join("\n\n"))
    }
}

/// Returns true only when stdin is connected to an interactive terminal.
/// Uses `libc::isatty` on Unix; always false on other platforms.
fn stdin_is_tty() -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        // SAFETY: isatty is async-signal-safe and only reads an fd number.
        unsafe { libc::isatty(std::io::stdin().as_raw_fd()) != 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

async fn read_stdin_line() -> String {
    use tokio::io::AsyncBufReadExt;
    let stdin = tokio::io::stdin();
    let mut reader = tokio::io::BufReader::new(stdin);
    let mut line = String::new();
    match reader.read_line(&mut line).await {
        Ok(_) => line.trim_end_matches('\n').trim_end_matches('\r').to_string(),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::Tool;

    #[test]
    fn schema_requires_questions() {
        let t = AskQuestionTool;
        let schema = t.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("questions")));
    }

    #[tokio::test]
    async fn missing_questions_is_error() {
        use serde_json::json;
        use crate::tool::ToolCall;
        let t = AskQuestionTool;
        let call = ToolCall { id: "1".into(), name: "ask_question".into(), args: json!({}) };
        let out = t.execute(&call).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing 'questions'"));
    }

    #[tokio::test]
    async fn too_many_questions_is_error() {
        use serde_json::json;
        use crate::tool::ToolCall;
        let t = AskQuestionTool;
        let call = ToolCall {
            id: "1".into(),
            name: "ask_question".into(),
            args: json!({
                "questions": ["q1", "q2", "q3", "q4"]
            }),
        };
        let out = t.execute(&call).await;
        assert!(out.is_error);
        assert!(out.content.contains("at most 3"));
    }

    /// In CI / test environments stdin is never a real TTY, so the tool must
    /// return a descriptive error rather than blocking forever on empty stdin.
    #[tokio::test]
    async fn headless_mode_returns_error_with_question_list() {
        use serde_json::json;
        use crate::tool::ToolCall;

        // Tests always run with stdin as a pipe (not a TTY), so we don't need
        // to mock anything — stdin_is_tty() will return false naturally.
        let t = AskQuestionTool;
        let call = ToolCall {
            id: "1".into(),
            name: "ask_question".into(),
            args: json!({ "questions": ["What language?", "What framework?"] }),
        };
        let out = t.execute(&call).await;
        // In non-TTY (test) environments the tool must fail gracefully.
        assert!(out.is_error);
        assert!(out.content.contains("non-interactive"));
        assert!(out.content.contains("What language?"));
        assert!(out.content.contains("What framework?"));
        assert!(out.content.contains("best judgement"));
    }
}
