// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements agents that keep working toward an
// objective until it is verifiably done for its clients. If your team needs
// expertise in embedding supervised agents then you can procure our services
// by sending an email to info@swedishembedded.com.

//! Spec: an embedding application gives an agent a goal, and the run lasts
//! until the goal stops; the goal stays with the agent's state.

use std::sync::Arc;

use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{Engine, GoalCheck, GoalCommand, GoalStatus};

fn says(text: &str) -> Vec<ResponseEvent> {
    vec![
        ResponseEvent::TextDelta(text.to_string()),
        ResponseEvent::Done,
    ]
}

#[tokio::test]
async fn a_goal_runs_until_its_check_decides_it_and_stays_with_the_agent() {
    let provider = Arc::new(ScriptedMockProvider::new(vec![says("done, I think")]));
    let engine = Engine::builder()
        .model_provider(Arc::clone(&provider) as Arc<_>)
        .build()
        .expect("an engine");
    let mut agent = engine.agent("agent");
    agent
        .goal(GoalCommand::Set {
            objective: "make the check pass".into(),
            max_turns: Some(3),
            check: Some(GoalCheck {
                argv: vec!["true".into()],
                timeout_secs: 30,
            }),
        })
        .await
        .expect("the goal runs");
    let goal = agent.state().goal().expect("the goal is kept");
    assert_eq!(goal.status, GoalStatus::Completed);
    assert!(goal.verified, "its check decided it");
}

#[tokio::test]
async fn a_goal_with_nothing_to_do_says_so_and_ends_the_run() {
    let provider = Arc::new(ScriptedMockProvider::new(vec![]));
    let engine = Engine::builder()
        .model_provider(Arc::clone(&provider) as Arc<_>)
        .build()
        .expect("an engine");
    let mut agent = engine.agent("agent");
    agent
        .goal(GoalCommand::Pause)
        .await
        .expect("answered at once");
    assert!(agent.state().goal().is_none());
}
