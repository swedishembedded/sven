// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for coding agents that run out of
// wall-clock time before they finish for its clients. If your team needs
// expertise in running agents under deadlines then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The time a run has left, as a line the model reads.
//!
//! A model that is never told its deadline explores until the host kills it.
//! [`TimeBudget`] states the elapsed and remaining whole minutes, and
//! [`TimeBudget::append_to`] puts that line at the very end of the request
//! being sent: on the newest message, never earlier. A server that caches
//! the prompt's prefix then re-reads only the notice's few tokens each turn,
//! because every message before the newest one is byte-identical to the
//! previous request. The line is a property of one outgoing request: it is
//! applied to a copy of the history and never stored.

use std::time::Duration;

use sven_model::time_notice::{TIME_NOTICE_PREFIX, TIME_NOTICE_SEPARATOR, TIME_NOTICE_SUFFIX};
use sven_model::{ContentPart, Message, MessageContent, ToolContentPart, ToolResultContent};

use crate::clock::Instant;

/// The wall-clock a run was given and when it started counting.
#[derive(Debug, Clone, Copy)]
pub struct TimeBudget {
    started: Instant,
    total: Duration,
}

impl TimeBudget {
    /// A budget of `total` that starts now.
    pub fn starting_now(total: Duration) -> Self {
        Self {
            started: Instant::now(),
            total,
        }
    }

    /// The notice for the time that has passed: whole minutes, the total
    /// rounded up so a short budget never reads as zero.
    pub fn notice(&self) -> String {
        let total = self.total.as_secs().div_ceil(60);
        let used = (self.started.elapsed().as_secs() / 60).min(total);
        format!(
            "{TIME_NOTICE_PREFIX}{used} of {total} min used, {} min left{TIME_NOTICE_SUFFIX}",
            total - used
        )
    }

    /// Ends `messages` with the notice: appended to the text of the newest
    /// message, or as a message of its own when that one carries no text
    /// (an assistant's tool call). Nothing before the newest message changes.
    pub fn append_to(&self, messages: &mut Vec<Message>) {
        let notice = self.notice();
        let Some(last) = messages.last_mut() else {
            return;
        };
        let appended = match &mut last.content {
            MessageContent::Text(t) => {
                push_line(t, &notice);
                true
            }
            MessageContent::ContentParts(parts) => {
                parts.push(ContentPart::Text {
                    text: notice.clone(),
                });
                true
            }
            MessageContent::ToolResult { content, .. } => match content {
                ToolResultContent::Text(t) => {
                    push_line(t, &notice);
                    true
                }
                ToolResultContent::Parts(parts) => {
                    parts.push(ToolContentPart::Text {
                        text: notice.clone(),
                    });
                    true
                }
            },
            MessageContent::ToolCall { .. } => false,
        };
        if !appended {
            messages.push(Message::user(notice));
        }
    }
}

fn push_line(text: &mut String, notice: &str) {
    text.push_str(TIME_NOTICE_SEPARATOR);
    text.push_str(notice);
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_model::FunctionCall;

    const MIN: Duration = Duration::from_secs(60);

    #[tokio::test(start_paused = true)]
    async fn it_counts_whole_minutes_used_and_left() {
        let budget = TimeBudget::starting_now(58 * MIN);
        tokio::time::advance(12 * MIN + Duration::from_secs(59)).await;
        assert_eq!(
            budget.notice(),
            "Time: 12 of 58 min used, 46 min left; leave time to verify and finish."
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_passed_deadline_reads_as_nothing_left_and_a_short_budget_as_one_minute() {
        let budget = TimeBudget::starting_now(Duration::from_secs(30));
        assert!(budget
            .notice()
            .starts_with("Time: 0 of 1 min used, 1 min left"));
        tokio::time::advance(10 * MIN).await;
        assert!(budget
            .notice()
            .starts_with("Time: 1 of 1 min used, 0 min left"));
    }

    #[tokio::test(start_paused = true)]
    async fn the_notice_goes_on_the_text_of_the_newest_message_only() {
        let budget = TimeBudget::starting_now(10 * MIN);
        let mut messages = vec![Message::user("a"), Message::tool_result("c1", "out")];
        budget.append_to(&mut messages);
        assert_eq!(messages.len(), 2);
        assert!(matches!(&messages[0].content, MessageContent::Text(t) if t == "a"));
        match &messages[1].content {
            MessageContent::ToolResult {
                content: ToolResultContent::Text(t),
                ..
            } => assert!(t.starts_with("out\n\nTime: 0 of 10 min used"), "{t}"),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_newest_message_without_text_gets_a_message_of_its_own() {
        let budget = TimeBudget::starting_now(10 * MIN);
        let mut messages = vec![Message {
            role: sven_model::Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: "c1".into(),
                function: FunctionCall {
                    name: "f".into(),
                    arguments: "{}".into(),
                },
            },
            reasoning: None,
        }];
        budget.append_to(&mut messages);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].role, sven_model::Role::User);
    }
}
