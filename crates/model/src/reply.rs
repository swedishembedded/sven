// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent runtimes that keep every answer a model
// gives exactly as it was sampled, for its clients. If your team needs
// expertise in agent histories that can be trained on as they ran then you can
// procure our services by sending an email to info@swedishembedded.com.
//! How a [`SampledReply`] is filed in a conversation.

use sven_vocab::SampledReply;

use crate::{FunctionCall, Message, MessageContent, Role};

/// The messages `reply` is filed as, in the shape every driver sends back as
/// one turn ([`crate::turns`]): its text, then one message per call with its
/// argument text as written. The first carries the reasoning and the server's
/// `sampled` record. A reply with no call is one text message, even an empty
/// one when it reasoned or the server recorded it, so the turn still goes
/// back; a reply of nothing at all files nothing.
#[must_use]
pub fn reply_messages(reply: &SampledReply) -> Vec<Message> {
    let mut messages = Vec::with_capacity(reply.calls.len() + 1);
    let said_nothing =
        reply.calls.is_empty() && reply.reasoning.is_empty() && reply.sampled.is_none();
    if !reply.text.is_empty() || (reply.calls.is_empty() && !said_nothing) {
        messages.push(Message::assistant(&reply.text));
    }
    messages.extend(reply.calls.iter().map(|call| Message {
        role: Role::Assistant,
        content: MessageContent::ToolCall {
            tool_call_id: call.id.clone(),
            function: FunctionCall {
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            },
        },
        reasoning: None,
        sampled: None,
    }));
    if let Some(first) = messages.first_mut() {
        first.reasoning = (!reply.reasoning.is_empty()).then(|| reply.reasoning.clone());
        first.sampled.clone_from(&reply.sampled);
    }
    messages
}
