// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that never lose what a person says
// to them for its clients. If your team needs expertise in reliable
// conversational agents then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The reactive agent's messages that arrive while a turn runs: held, never
//! dropped, and answered once the machine is at rest, one turn each, in the
//! order they came. A client that took an error for the turn's end sends
//! one; so can any race between a turn ending and a message being sent.
//!
//! They are kept in the machine's context, so a snapshot keeps them too.

use serde_json::json;
use sven_hsm::{Context, Effect, Event, InternalEvent, Reaction};

use super::reactive_agent::ReactiveState;

/// The signal that asks the machine at rest to answer the oldest held
/// message; posted on entering rest while one is held.
const SIGNAL: &str = "agent.held_message";

/// The fact holding the messages, oldest first.
const FACT: &str = "agent.held_messages";

fn held(ctx: &Context) -> Vec<String> {
    ctx.fact(FACT)
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

/// Holds a message that arrived while a turn ran, behind any already held.
pub(super) fn hold(ctx: &mut Context, text: &str) {
    let mut messages = held(ctx);
    messages.push(text.to_string());
    ctx.set_fact(FACT, json!(messages));
}

fn take_oldest(ctx: &mut Context) -> Option<String> {
    let mut messages = held(ctx);
    if messages.is_empty() {
        return None;
    }
    let oldest = messages.remove(0);
    ctx.set_fact(FACT, json!(messages));
    Some(oldest)
}

/// What the machine at rest does with `event` for its held messages, or
/// `None` when `event` is none of theirs. Entering rest with one held asks
/// for it; a message that comes while some are held waits behind them, the
/// oldest starting its turn (`start`) instead; the signal starts the oldest's,
/// and changes nothing once they are all answered.
pub(super) fn at_rest(
    ctx: &mut Context,
    event: &Event,
    start: impl FnOnce(&mut Context, &str) -> Reaction<ReactiveState>,
) -> Option<Reaction<ReactiveState>> {
    match event {
        Event::Internal(InternalEvent::Entry) if !held(ctx).is_empty() => {
            Some(Reaction::effects(vec![Effect::EmitInternal {
                name: SIGNAL.to_string(),
                payload: json!(null),
            }]))
        }
        Event::UserMessage { text } if !held(ctx).is_empty() => {
            hold(ctx, text);
            let oldest = take_oldest(ctx).unwrap_or_default();
            Some(start(ctx, &oldest))
        }
        Event::Internal(InternalEvent::Custom { name, .. }) if name == SIGNAL => {
            Some(match take_oldest(ctx) {
                Some(oldest) => start(ctx, &oldest),
                None => Reaction::effects(vec![]),
            })
        }
        _ => None,
    }
}
