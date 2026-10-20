// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent runtimes that notice when a model is
// going in circles and say so, for its clients. If your team needs expertise
// in unattended agents you can procure our services by sending an email to
// info@swedishembedded.com.
//! When a model asks the same question twice.
//!
//! A tool call and its result are a question and its answer. A model that
//! repeats a call and gets the same answer learns nothing from it, and often
//! cannot tell that it did not move: the second answer looks like progress.
//! This module decides, from the transcript alone (no clock, no I/O), whether
//! a call is a repeat, so that the same decision is made in a live run and in
//! a replay, and by a caller that wants to bound repetition from outside.
//!
//! Two calls are the same call when they name the same tool and carry the same
//! arguments once cosmetic differences are removed ([`call_identity`]): a
//! `description` field (shown to a person, never acted on), the order of keys
//! and runs of whitespace in a string.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::{Message, MessageContent, Role, ToolResultContent};

/// Starts the note [`repeat_note`] appends; what [`strip_repeat_note`] cuts at.
pub const REPEAT_NOTE_MARK: &str = "\n\n[sven note: ";

/// The identity of a call: tool name and canonical arguments.
///
/// `arguments` is the JSON text the model sent. Text that is not JSON is
/// compared as it is, whitespace collapsed.
#[must_use]
pub fn call_identity(name: &str, arguments: &str) -> String {
    let canonical = match serde_json::from_str::<Value>(arguments) {
        Ok(value) => canonical(value, true),
        Err(_) => Value::String(collapse_whitespace(arguments)),
    };
    format!("{name}\u{1}{canonical}")
}

fn canonical(value: Value, top: bool) -> Value {
    match value {
        Value::String(s) => Value::String(collapse_whitespace(&s)),
        Value::Array(items) => {
            Value::Array(items.into_iter().map(|v| canonical(v, false)).collect())
        }
        Value::Object(map) => {
            let sorted: BTreeMap<String, Value> = map
                .into_iter()
                .filter(|(key, _)| !(top && key == "description"))
                .map(|(key, v)| (key, canonical(v, false)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        other => other,
    }
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The result with a note [`repeat_note`] appended removed.
#[must_use]
pub fn strip_repeat_note(result: &str) -> &str {
    result
        .rfind(REPEAT_NOTE_MARK)
        .map_or(result, |at| &result[..at])
}

/// The note appended to the result of a call that repeats `earlier` earlier
/// identical calls with identical results in this run.
#[must_use]
pub fn repeat_note(earlier: usize) -> String {
    let times = if earlier == 1 {
        "once before".to_string()
    } else {
        format!("{earlier} times before")
    };
    format!(
        "{REPEAT_NOTE_MARK}this exact call returned this exact result {times} in this run, and \
         the result is unchanged. Repeating it will not give you new information; take a \
         different action.]"
    )
}

fn result_text(content: &ToolResultContent) -> String {
    match content {
        ToolResultContent::Text(t) => t.clone(),
        ToolResultContent::Parts(_) => content.to_string(),
    }
}

/// How many calls of the current run (the turns after the last user message of
/// `messages`) were this call and got this result. The notes this module
/// appends are not part of what is compared.
#[must_use]
pub fn earlier_identical_results(
    messages: &[Message],
    name: &str,
    arguments: &str,
    result: &str,
) -> usize {
    let start = messages
        .iter()
        .rposition(|m| m.role == Role::User)
        .map_or(0, |at| at + 1);
    let wanted = call_identity(name, arguments);
    let result = strip_repeat_note(result);
    let mut asked: BTreeMap<&str, String> = BTreeMap::new();
    let mut same = 0;
    for message in &messages[start..] {
        match &message.content {
            MessageContent::ToolCall {
                tool_call_id,
                function,
            } => {
                asked.insert(
                    tool_call_id,
                    call_identity(&function.name, &function.arguments),
                );
            }
            MessageContent::ToolResult {
                tool_call_id,
                content,
            } if asked.get(tool_call_id.as_str()) == Some(&wanted)
                && strip_repeat_note(&result_text(content)) == result =>
            {
                same += 1;
            }
            _ => {}
        }
    }
    same
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FunctionCall;

    fn call(id: &str, name: &str, arguments: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: id.into(),
                function: FunctionCall {
                    name: name.into(),
                    arguments: arguments.into(),
                },
            },
        }
    }

    #[test]
    fn cosmetic_differences_do_not_make_a_different_call() {
        let a = call_identity(
            "shell",
            r#"{"shell_command":"ls  -la\n","description":"list"}"#,
        );
        let b = call_identity(
            "shell",
            r#"{"description":"look around","shell_command":"ls -la"}"#,
        );
        assert_eq!(a, b);
    }

    #[test]
    fn a_different_command_or_tool_is_a_different_call() {
        let base = call_identity("shell", r#"{"shell_command":"ls"}"#);
        assert_ne!(base, call_identity("shell", r#"{"shell_command":"ls -a"}"#));
        assert_ne!(base, call_identity("grep", r#"{"shell_command":"ls"}"#));
    }

    #[test]
    fn only_a_top_level_description_is_cosmetic() {
        let a = call_identity("write_file", r#"{"path":"a","text":{"description":"x"}}"#);
        let b = call_identity("write_file", r#"{"path":"a","text":{"description":"y"}}"#);
        assert_ne!(a, b);
    }

    #[test]
    fn a_repeat_is_counted_within_the_current_run_only() {
        let args = r#"{"path":"a.txt"}"#;
        let history = vec![
            Message::user("first task"),
            call("c0", "read_file", args),
            Message::tool_result("c0", "same"),
            Message::user("second task"),
            call("c1", "read_file", r#"{ "path": "a.txt" }"#),
            Message::tool_result("c1", "same"),
            call("c2", "read_file", args),
            Message::tool_result("c2", "other"),
        ];
        assert_eq!(
            earlier_identical_results(&history, "read_file", args, "same"),
            1
        );
        assert_eq!(
            earlier_identical_results(&history, "read_file", args, "new"),
            0
        );
    }

    #[test]
    fn an_earlier_note_is_not_part_of_what_is_compared() {
        let args = r#"{"path":"a.txt"}"#;
        let noted = format!("same{}", repeat_note(1));
        let history = vec![
            Message::user("task"),
            call("c0", "read_file", args),
            Message::tool_result("c0", noted.clone()),
        ];
        assert_eq!(
            earlier_identical_results(&history, "read_file", args, "same"),
            1
        );
        assert_eq!(strip_repeat_note(&noted), "same");
    }

    #[test]
    fn the_note_says_how_often_and_what_to_do() {
        assert!(repeat_note(1).contains("once before"));
        let many = repeat_note(3);
        assert!(many.contains("3 times before") && many.contains("different action"));
    }
}
