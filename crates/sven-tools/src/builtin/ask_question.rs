use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

/// Interactively ask the user one or more questions and collect their answers.
/// In CI / headless mode, questions are printed to stderr and answers are read
/// from stdin line by line.
pub struct AskQuestionTool;

#[async_trait]
impl Tool for AskQuestionTool {
    fn name(&self) -> &str { "ask_question" }

    fn description(&self) -> &str {
        "Ask the user one to three clarifying questions and return their answers. \
         Use sparingly – only when the task is genuinely ambiguous and you cannot proceed \
         without more information."
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

        debug!(count = questions.len(), "ask_question tool");

        // Print questions to stderr so they appear on the terminal
        eprintln!();
        eprintln!("╔══ Questions from agent ══════════════════════════╗");
        for (i, q) in questions.iter().enumerate() {
            eprintln!("  {}. {}", i + 1, q);
        }
        eprintln!("╚════════════════════════════════════════════════════╝");

        // Read answers from stdin
        let mut answers: Vec<String> = Vec::new();
        for (i, q) in questions.iter().enumerate() {
            eprint!("  Answer {}: ", i + 1);
            let answer = read_stdin_line().await;
            if answer.is_empty() && questions.len() == 1 {
                eprintln!("(no answer provided)");
            }
            answers.push(format!("Q: {}\nA: {}", q, answer));
        }

        ToolOutput::ok(&call.id, answers.join("\n\n"))
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
}
