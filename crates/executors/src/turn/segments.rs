// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent runtimes whose recorded sessions can be
// trained on as they ran, for its clients. If your team needs expertise in
// agent trajectories as training data then you can procure our services by
// sending an email to info@swedishembedded.com.
//! Where a conversation stops extending what it sent before.
//!
//! A session is a list of segments: within one, every request sends the
//! previous request's system prompt and tools unchanged and its history
//! followed by what came since. A request that does not (a compaction, a
//! system prompt or a tool list that changed) starts the next segment, and
//! [`Segments::observe`] says so, with the cause, so the rewrite is an event
//! of its own and a trainer merges each segment into one record.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use sven_model::{Message, Role, ToolSchema};
use sven_vocab::ContextRewrite;

/// What each thread sent with its last request.
#[derive(Default)]
pub(crate) struct Segments {
    threads: HashMap<String, Sent>,
}

struct Sent {
    segment: u32,
    system: u64,
    tools: u64,
    /// A digest of each history message, system prompt excluded.
    history: Vec<u64>,
}

/// A digest of `value`'s JSON text.
macro_rules! digest {
    ($value:expr) => {{
        let mut hasher = DefaultHasher::new();
        serde_json::to_string($value)
            .unwrap_or_default()
            .hash(&mut hasher);
        hasher.finish()
    }};
}

impl Segments {
    /// Notes the request `thread` is about to send. When it does not extend
    /// the thread's previous request, the segment it starts and why.
    pub(crate) fn observe(
        &mut self,
        thread: &str,
        messages: &[Message],
        tools: &[ToolSchema],
    ) -> Option<(u32, ContextRewrite)> {
        let (system_messages, history): (Vec<&Message>, Vec<&Message>) =
            messages.iter().partition(|m| m.role == Role::System);
        let tool_digests: Vec<(&str, u64)> = tools
            .iter()
            .map(|t| (t.name.as_str(), digest!(&(&t.description, &t.parameters))))
            .collect();
        let now = Sent {
            segment: 0,
            system: digest!(&system_messages),
            tools: digest!(&tool_digests),
            history: history.iter().map(|m| digest!(m)).collect(),
        };
        let Some(before) = self.threads.get(thread) else {
            self.threads.insert(thread.to_string(), now);
            return None;
        };
        let cause = if before.system != now.system {
            Some(ContextRewrite::System)
        } else if before.tools != now.tools {
            Some(ContextRewrite::Tools)
        } else if !now.history.starts_with(&before.history) {
            Some(ContextRewrite::History)
        } else {
            None
        };
        let segment = before.segment + u32::from(cause.is_some());
        self.threads
            .insert(thread.to_string(), Sent { segment, ..now });
        cause.map(|cause| (segment, cause))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str) -> ToolSchema {
        ToolSchema {
            name: name.into(),
            description: "d".into(),
            parameters: serde_json::json!({"type": "object"}),
            is_mcp: false,
        }
    }

    #[test]
    fn a_request_that_does_not_extend_the_last_starts_a_segment_and_says_why() {
        let mut segments = Segments::default();
        let mut history = vec![Message::system("you are an agent"), Message::user("go")];
        let tools = [tool("shell")];
        let more = [tool("shell"), tool("read")];
        assert_eq!(segments.observe("t", &history, &tools), None);
        history.push(Message::assistant("ok"));
        history.push(Message::user("more"));
        assert_eq!(
            segments.observe("t", &history, &tools),
            None,
            "an extension"
        );
        history[0] = Message::system("you are an agent; it is 12:00");
        assert_eq!(
            segments.observe("t", &history, &tools),
            Some((1, ContextRewrite::System))
        );
        assert_eq!(
            segments.observe("t", &history, &more),
            Some((2, ContextRewrite::Tools))
        );
        let compacted = vec![
            history[0].clone(),
            Message::user("notes: did things"),
            Message::user("more"),
        ];
        assert_eq!(
            segments.observe("t", &compacted, &more),
            Some((3, ContextRewrite::History))
        );
        assert_eq!(
            segments.observe("other", &compacted, &tools),
            None,
            "threads are apart"
        );
    }
}
