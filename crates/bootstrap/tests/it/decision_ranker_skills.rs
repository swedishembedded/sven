// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the instruction
// catalog of an LLM coding agent from taxing every request for its clients.
// If your team needs expertise in agent context budgets then you can procure
// our services by sending an email to info@swedishembedded.com.
//! Spec: a decision model re-ranks `skill search` under the
//! `skill_selection` flag, with the guarantees of tool search: BM25 stays the
//! candidate generator and the fallback, an abstention or a failure returns
//! BM25 exactly, and a winner that depends on the order of the options is not
//! trusted.

use std::sync::Arc;

use sven_bootstrap::decision_ranker::{for_point, DecisionRanker, RoutingPoint};
use sven_decision::config::DecisionConfig;
use sven_decision::{
    Answer, DecisionError, DecisionResult, DecisionRuntime, MockDecisionProvider, QuestionKind,
};
use sven_hsm::ToolCapability;
use sven_tool_api::events::ToolEvent;
use sven_tool_registry::{Bm25Ranker, RankedTool, ToolDescriptor, ToolRanker};
use tokio::sync::mpsc;

const NAMES: [&str; 4] = ["alpha_skill", "bravo_skill", "charlie_skill", "delta_skill"];

fn skills() -> Vec<ToolDescriptor> {
    NAMES
        .map(|n| ToolDescriptor {
            name: n.into(),
            summary: "Review a document.".into(),
            capability: ToolCapability::ReadFile,
            mcp: false,
            description: String::new(),
            parameter_descriptions: Vec::new(),
        })
        .into()
}

fn runtime(extra: &str) -> Arc<DecisionRuntime> {
    let config: DecisionConfig = serde_yaml::from_str(&format!("enabled: true\n{extra}")).unwrap();
    Arc::new(DecisionRuntime::new(config).unwrap())
}

struct Rig {
    ranker: DecisionRanker,
    mock: Arc<MockDecisionProvider>,
    runtime: Arc<DecisionRuntime>,
    notices: mpsc::Receiver<ToolEvent>,
}

fn rig() -> Rig {
    let mock = Arc::new(MockDecisionProvider::new("mock/model"));
    let runtime = runtime("");
    let (tx, notices) = mpsc::channel(8);
    let ranker = DecisionRanker::for_point(
        RoutingPoint::SKILLS,
        mock.clone(),
        runtime.clone(),
        Some(tx),
    );
    Rig {
        ranker,
        mock,
        runtime,
        notices,
    }
}

fn picks(id: &str, winner: &str, options: &[&str]) -> (String, QuestionKind, Answer) {
    let rest = 0.1 / (options.len() - 1).max(1) as f32;
    let candidates: Vec<(&str, f32)> = options
        .iter()
        .map(|o| (*o, if *o == winner { 0.9 } else { rest }))
        .collect();
    (
        id.into(),
        QuestionKind::Choice,
        Answer::choice(&candidates).unwrap(),
    )
}

fn script_both(rig: &Rig, first: &str, second: &str) {
    let ascending: Vec<&str> = NAMES.to_vec();
    let descending: Vec<&str> = ascending.iter().rev().copied().collect();
    let answers = [
        picks("0:tool", first, &ascending),
        picks("1:tool", second, &descending),
    ];
    rig.mock.push_result(DecisionResult::for_answers(
        "mock/model",
        answers
            .iter()
            .map(|(id, kind, answer)| (id.as_str(), *kind, answer.clone()))
            .collect(),
    ));
}

fn names(ranked: &[RankedTool]) -> Vec<&str> {
    ranked.iter().map(|r| r.name.as_str()).collect()
}

async fn bm25(query: &str, skills: &[ToolDescriptor]) -> Vec<RankedTool> {
    Bm25Ranker.rank(query, skills).await
}

#[tokio::test]
async fn the_model_can_promote_a_lower_ranked_skill() {
    let rig = rig();
    let skills = skills();
    assert_eq!(names(&bm25("review document", &skills).await), NAMES);
    script_both(&rig, "charlie_skill", "charlie_skill");

    let ranked = rig.ranker.rank("review document", &skills).await;

    assert_eq!(
        names(&ranked),
        ["charlie_skill", "alpha_skill", "bravo_skill", "delta_skill"]
    );
    let drained = rig.runtime.telemetry().drain();
    assert_eq!(drained.traces[0].policy, "skill_selection");
    assert!(drained.traces[0].applied);
}

#[tokio::test]
async fn an_order_sensitive_winner_returns_bm25_exactly() {
    let rig = rig();
    let skills = skills();
    script_both(&rig, "bravo_skill", "delta_skill");
    assert_eq!(
        rig.ranker.rank("review document", &skills).await,
        bm25("review document", &skills).await
    );
    let trace = rig.runtime.telemetry().drain().traces;
    assert_eq!(trace[0].abstain_reason.as_deref(), Some("order_sensitive"));
}

#[tokio::test]
async fn an_abstention_returns_bm25_exactly() {
    let rig = rig();
    let skills = skills();
    let flat: Vec<(&str, f32)> = NAMES.iter().map(|n| (*n, 0.25)).collect();
    let answer = Answer::choice(&flat).unwrap();
    rig.mock.push_result(DecisionResult::for_answers(
        "mock/model",
        vec![
            ("0:tool", QuestionKind::Choice, answer.clone()),
            ("1:tool", QuestionKind::Choice, answer),
        ],
    ));
    assert_eq!(
        rig.ranker.rank("review document", &skills).await,
        bm25("review document", &skills).await
    );
}

#[tokio::test]
async fn a_provider_failure_returns_bm25_exactly_and_is_reported_once() {
    let mut rig = rig();
    let skills = skills();
    for _ in 0..2 {
        rig.mock
            .push_error(DecisionError::Unreachable("refused".into()));
    }
    let expected = bm25("review document", &skills).await;
    assert_eq!(rig.ranker.rank("review document", &skills).await, expected);
    assert_eq!(rig.ranker.rank("review document", &skills).await, expected);
    assert!(rig.notices.try_recv().is_ok());
    assert!(rig.notices.try_recv().is_err(), "once per session");
}

#[tokio::test]
async fn a_clear_bm25_winner_asks_no_one() {
    let rig = rig();
    let mut skills = skills();
    skills.push(ToolDescriptor {
        name: "deploy".into(),
        summary: "Ship a release to production servers.".into(),
        capability: ToolCapability::ReadFile,
        mcp: false,
        description: String::new(),
        parameter_descriptions: Vec::new(),
    });
    let ranked = rig.ranker.rank("ship release production", &skills).await;
    assert_eq!(ranked, bm25("ship release production", &skills).await);
    assert!(rig.mock.requests().is_empty());
}

#[tokio::test]
async fn the_skill_flag_switches_the_ranker_off_without_touching_tool_routing() {
    let off = runtime("features: {skill_selection: false}\n");
    let (tx, _rx) = mpsc::channel(1);
    assert!(for_point(RoutingPoint::SKILLS, &off, tx.clone()).is_none());
    assert_eq!(RoutingPoint::TOOLS.choice, "tool_routing");
    assert_eq!(RoutingPoint::SKILLS.choice, "skill_selection");
}
