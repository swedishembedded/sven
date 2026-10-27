// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Runs of tool activity: the stretches of a conversation where the agent
//! only called tools, with no text of its own in between. A frontend shows
//! each run as one collapsible "Ran N tools" entry instead of a row per call.
//!
//! Swedish Embedded AB implements solutions for keeping autonomous agents'
//! work readable at a glance for its clients. If your team needs expertise
//! in agent interfaces and conversation models then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::ops::Range;

use sven_model::{MessageContent, Role};

use crate::ChatSegment;

/// A maximal stretch of consecutive tool calls and results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRun {
    /// The segments the run covers.
    pub segments: Range<usize>,
    /// How many tools ran in it: every call, plus every result whose call is
    /// not in the run (a history that kept the result but not the call).
    pub tools: usize,
}

enum Kind {
    Call,
    Result,
    Thinking,
    Other,
}

fn kind(seg: &ChatSegment) -> Kind {
    match seg {
        ChatSegment::Message(m) => match (&m.role, &m.content) {
            (Role::Assistant, MessageContent::ToolCall { .. }) => Kind::Call,
            (Role::Tool, MessageContent::ToolResult { .. }) => Kind::Result,
            _ => Kind::Other,
        },
        ChatSegment::Thinking { .. } => Kind::Thinking,
        _ => Kind::Other,
    }
}

/// Every run of tool activity in `segments`, in order.
///
/// Anything the reader sees between two tools - the agent's text, a user
/// message, a note - ends the run. Thinking ends it only when the frontend
/// shows thinking (`thinking_shown`); hidden thinking is invisible, so tools
/// on either side of it read as one run. A run never starts or ends on
/// thinking.
pub fn tool_runs(segments: &[ChatSegment], thinking_shown: bool) -> Vec<ToolRun> {
    let mut runs = Vec::new();
    let mut i = 0;
    while i < segments.len() {
        if !matches!(kind(&segments[i]), Kind::Call | Kind::Result) {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i; // one past the last tool segment seen
        let mut calls = std::collections::HashSet::new();
        let mut tools = 0;
        while i < segments.len() {
            match kind(&segments[i]) {
                Kind::Call => {
                    tools += 1;
                    if let Some(id) = crate::segment_tool_call_id(&segments[i]) {
                        calls.insert(id);
                    }
                    end = i + 1;
                }
                Kind::Result => {
                    let id = crate::segment_tool_call_id(&segments[i]);
                    if !id.is_some_and(|id| calls.contains(id)) {
                        tools += 1;
                    }
                    end = i + 1;
                }
                Kind::Thinking if !thinking_shown => {}
                Kind::Thinking | Kind::Other => break,
            }
            i += 1;
        }
        runs.push(ToolRun {
            segments: start..end,
            tools,
        });
        i = end;
    }
    runs
}

#[cfg(test)]
mod tests {
    use sven_model::{FunctionCall, Message};

    use super::*;

    fn call(id: &str) -> ChatSegment {
        ChatSegment::Message(Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: id.into(),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: "{}".into(),
                },
            },
            reasoning: None,
        })
    }

    fn result(id: &str) -> ChatSegment {
        ChatSegment::Message(Message::tool_result(id, "ok"))
    }

    fn text(t: &str) -> ChatSegment {
        ChatSegment::Message(Message::assistant(t))
    }

    fn thinking() -> ChatSegment {
        ChatSegment::Thinking {
            content: "hmm".into(),
        }
    }

    #[test]
    fn text_between_tools_separates_runs() {
        let segs = vec![
            ChatSegment::Message(Message::user("go")),
            call("a"),
            result("a"),
            call("b"),
            result("b"),
            text("halfway"),
            call("c"),
            result("c"),
        ];
        assert_eq!(
            tool_runs(&segs, false),
            vec![
                ToolRun {
                    segments: 1..5,
                    tools: 2
                },
                ToolRun {
                    segments: 6..8,
                    tools: 1
                },
            ]
        );
    }

    #[test]
    fn hidden_thinking_joins_runs_and_shown_thinking_splits_them() {
        let segs = vec![call("a"), result("a"), thinking(), call("b"), thinking()];
        assert_eq!(
            tool_runs(&segs, false),
            vec![ToolRun {
                segments: 0..4,
                tools: 2
            }],
            "the trailing thinking is not part of the run"
        );
        assert_eq!(tool_runs(&segs, true).len(), 2);
    }

    #[test]
    fn parallel_calls_and_results_without_calls_count_once_each() {
        // Two calls issued together, their results after; then a result
        // whose call is not in the history.
        let segs = vec![call("a"), call("b"), result("a"), result("b"), result("z")];
        assert_eq!(tool_runs(&segs, false)[0].tools, 3);
    }
}
