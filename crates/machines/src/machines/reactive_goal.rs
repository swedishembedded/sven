// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that keep working toward an
// objective until it is verifiably done for its clients. If your team needs
// expertise in supervised, long-running agent work then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Where the reactive agent's turn ends meet its standing goal: the goal's
//! next turn, its check, its end, and a goal the model set at the person's
//! request (`super::goal_flow` keeps the state, `super::goal` decides).

use serde_json::json;
use sven_hsm::{context::Context, effect::Effect, status::Reaction};

use super::goal::Next;
use super::goal_flow;
use super::loop_core::build_turn_effect;
use super::reactive_agent::{start_loop, turn_ended, ReactiveState, AGENT_MODE, CHAT_THREAD};

/// Starts a goal turn with `instruction`, announcing the goal.
pub(super) fn goal_turn(
    ctx: &mut Context,
    announce: Effect,
    instruction: &str,
) -> Reaction<ReactiveState> {
    let ls = start_loop(ctx);
    let turn = ls.stamp(build_turn_effect(
        CHAT_THREAD,
        &[],
        AGENT_MODE,
        None,
        Some(instruction),
        None,
        ls.max_rounds,
        None,
        None,
    ));
    Reaction::transition(
        ReactiveState::Generating,
        vec![announce, turn],
        "the goal takes a turn",
    )
}

/// A person's turn ended with an answer: at rest, unless the model set the
/// goal the person asked it to, which starts in the same answer.
pub(super) fn person_turn_ended(ctx: &mut Context) -> Reaction<ReactiveState> {
    match goal_flow::proposed(ctx) {
        Some(goal_flow::Answer::Start {
            announce,
            instruction,
        }) => goal_turn(ctx, announce, &instruction),
        // Its answer ends the person's turn: the person waited for it.
        Some(goal_flow::Answer::Stay(effects)) => {
            Reaction::transition(ReactiveState::Idle, effects, "the goal was not set")
        }
        None => Reaction::transition(
            ReactiveState::Idle,
            vec![turn_ended()],
            "turn complete; final answer",
        ),
    }
}

/// Does what the goal policy decided after a goal turn or its check.
pub(super) fn goal_step(
    ctx: &mut Context,
    announce: Effect,
    next: Next,
) -> Reaction<ReactiveState> {
    match next {
        Next::Check(spec) => Reaction::transition(
            ReactiveState::Checking,
            vec![announce, Effect::Verify { spec }],
            "checking the goal",
        ),
        Next::Continue(instruction) => goal_turn(ctx, announce, &instruction),
        Next::Stop => Reaction::transition(
            ReactiveState::Idle,
            vec![announce, turn_ended()],
            "the goal stopped",
        ),
    }
}

/// The goal's check answered.
pub(super) fn checked(
    ctx: &mut Context,
    verdict: &sven_vocab::verify::VerifierVerdict,
) -> Reaction<ReactiveState> {
    match goal_flow::checked(ctx, verdict) {
        Some((announce, next)) => goal_step(ctx, announce, next),
        // Cleared while it ran.
        None => Reaction::transition(ReactiveState::Idle, vec![turn_ended()], "goal cleared"),
    }
}

/// A turn failed beyond recovery: it ends, and an active goal pauses.
pub(super) fn failed(ctx: &mut Context, error: &str) -> Reaction<ReactiveState> {
    ctx.set_fact("last_error", json!(error));
    let mut effects: Vec<Effect> =
        goal_flow::interrupted(ctx, &format!("the turn failed: {error}"))
            .into_iter()
            .collect();
    effects.push(turn_ended());
    Reaction::transition(ReactiveState::Idle, effects, "turn failed")
}
