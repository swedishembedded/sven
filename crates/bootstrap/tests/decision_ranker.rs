// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in tool retrieval for
// large agent tool sets then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Spec: a decision model re-ranks `tools search` only when BM25 cannot tell
//! its top candidates apart; in every other case the result is BM25's, exactly.

use std::sync::Arc;
use std::time::Duration;

use sven_bootstrap::decision_ranker::{DecisionRanker, OPTIONS_PER_REQUEST};
use sven_decision::config::DecisionConfig;
use sven_decision::{
    Answer, DecisionError, DecisionResult, DecisionRuntime, MockDecisionProvider, QuestionKind,
};
use sven_hsm::ToolCapability;
use sven_tool_api::events::ToolEvent;
use sven_tool_registry::{Bm25Ranker, ToolDescriptor, ToolRanker};
use tokio::sync::mpsc;

fn tool(name: &str, summary: &str, capability: ToolCapability) -> ToolDescriptor {
    ToolDescriptor {
        name: name.into(),
        summary: summary.into(),
        capability,
        mcp: false,
    }
}

/// Four tools BM25 cannot tell apart for "change document": same words, so
/// equal scores, ordered by name.
fn tied_tools() -> Vec<ToolDescriptor> {
    ["alpha", "bravo", "charlie", "delta"]
        .map(|n| {
            tool(
                &format!("{n}_doc"),
                "Change a document.",
                ToolCapability::WriteFile,
            )
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

fn rig(extra: &str) -> Rig {
    let mock = Arc::new(MockDecisionProvider::new("mock/model"));
    let runtime = runtime(extra);
    let (tx, notices) = mpsc::channel(8);
    let ranker = DecisionRanker::new(mock.clone(), runtime.clone(), Some(tx));
    Rig {
        ranker,
        mock,
        runtime,
        notices,
    }
}

/// An answer to the question `id` that puts `winner` first with 0.9.
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

/// The scripted reply to the merged call of both option orders.
fn script_both(rig: &Rig, first_winner: &str, second_winner: &str, options: &[&str]) {
    let mut ascending: Vec<&str> = options.to_vec();
    ascending.sort_unstable();
    let descending: Vec<&str> = ascending.iter().rev().copied().collect();
    let answers = [
        picks("0:tool", first_winner, &ascending),
        picks("1:tool", second_winner, &descending),
    ];
    rig.mock.push_result(DecisionResult::for_answers(
        "mock/model",
        answers
            .iter()
            .map(|(id, kind, answer)| (id.as_str(), *kind, answer.clone()))
            .collect(),
    ));
}

const NAMES: [&str; 4] = ["alpha_doc", "bravo_doc", "charlie_doc", "delta_doc"];

async fn bm25(query: &str, tools: &[ToolDescriptor]) -> Vec<sven_tool_registry::RankedTool> {
    Bm25Ranker.rank(query, tools).await
}

fn names(ranked: &[sven_tool_registry::RankedTool]) -> Vec<&str> {
    ranked.iter().map(|r| r.name.as_str()).collect()
}

#[tokio::test]
async fn a_clear_bm25_winner_is_returned_without_asking_anyone() {
    let rig = rig("");
    let mut tools = tied_tools();
    tools.push(tool(
        "grep",
        "Search file contents for a regular expression.",
        ToolCapability::ReadFile,
    ));
    let ranked = rig
        .ranker
        .rank("search file contents regular expression", &tools)
        .await;
    assert_eq!(
        ranked,
        bm25("search file contents regular expression", &tools).await
    );
    assert!(rig.mock.requests().is_empty());
}

#[tokio::test]
async fn the_model_can_promote_a_lower_ranked_correct_tool() {
    let rig = rig("");
    let tools = tied_tools();
    let baseline = bm25("change document", &tools).await;
    assert_eq!(names(&baseline), NAMES, "BM25 alone puts alpha first");
    script_both(&rig, "charlie_doc", "charlie_doc", &NAMES);

    let ranked = rig.ranker.rank("change document", &tools).await;

    assert_eq!(
        names(&ranked),
        ["charlie_doc", "alpha_doc", "bravo_doc", "delta_doc"]
    );
    let mut a: Vec<&str> = names(&ranked);
    let mut b: Vec<&str> = names(&baseline);
    a.sort_unstable();
    b.sort_unstable();
    assert_eq!(a, b, "a re-rank reorders; it never removes a tool");
    assert_eq!(
        rig.mock.requests().len(),
        1,
        "both option orders travel in one call"
    );
    assert_eq!(rig.mock.requests()[0].questions.len(), 2);
}

#[tokio::test]
async fn an_abstention_returns_bm25_exactly() {
    let rig = rig("");
    let tools = tied_tools();
    let flat: Vec<(&str, f32)> = NAMES.iter().map(|n| (*n, 0.25)).collect();
    let answer = Answer::choice(&flat).unwrap();
    rig.mock.push_result(DecisionResult::for_answers(
        "mock/model",
        vec![
            ("0:tool", QuestionKind::Choice, answer.clone()),
            ("1:tool", QuestionKind::Choice, answer),
        ],
    ));
    let ranked = rig.ranker.rank("change document", &tools).await;
    assert_eq!(ranked, bm25("change document", &tools).await);
    let trace = rig.runtime.telemetry().drain().traces;
    assert_eq!(trace.len(), 1);
    assert_eq!(trace[0].verdict, "abstain");
    assert_eq!(trace[0].abstain_reason.as_deref(), Some("low_confidence"));
}

#[tokio::test]
async fn a_winner_that_depends_on_the_order_of_the_options_is_not_trusted() {
    let rig = rig("");
    let tools = tied_tools();
    script_both(&rig, "bravo_doc", "delta_doc", &NAMES);
    let ranked = rig.ranker.rank("change document", &tools).await;
    assert_eq!(ranked, bm25("change document", &tools).await);
    let trace = rig.runtime.telemetry().drain().traces;
    assert_eq!(trace[0].abstain_reason.as_deref(), Some("order_sensitive"));
}

#[tokio::test]
async fn a_provider_failure_returns_bm25_exactly_and_is_reported_once() {
    let mut rig = rig("");
    let tools = tied_tools();
    for _ in 0..2 {
        rig.mock
            .push_error(DecisionError::Unreachable("refused".into()));
    }
    let expected = bm25("change document", &tools).await;
    assert_eq!(rig.ranker.rank("change document", &tools).await, expected);
    assert_eq!(rig.ranker.rank("change document", &tools).await, expected);

    let ToolEvent::Notice(text) = rig.notices.try_recv().expect("one notice") else {
        panic!("a notice")
    };
    assert!(text.contains("Decisions unavailable"), "{text}");
    assert!(
        rig.notices.try_recv().is_err(),
        "once per session, not per search"
    );

    let drained = rig.runtime.telemetry().drain();
    assert_eq!(drained.traces.len(), 2);
    assert_eq!(
        drained.traces[0].abstain_reason.as_deref(),
        Some("provider_failed:unreachable")
    );
    assert_eq!(drained.inference.unwrap().calls, 2);
}

#[tokio::test]
async fn in_shadow_mode_the_decision_is_traced_and_never_applied() {
    let rig = rig("policy: {mode: shadow}\n");
    let tools = tied_tools();
    script_both(&rig, "charlie_doc", "charlie_doc", &NAMES);
    let ranked = rig.ranker.rank("change document", &tools).await;
    assert_eq!(ranked, bm25("change document", &tools).await);
    let trace = rig.runtime.telemetry().drain().traces;
    assert_eq!(trace[0].verdict, "accept");
    assert_eq!(trace[0].selected.as_deref(), Some("charlie_doc"));
    assert!(!trace[0].applied);
}

#[tokio::test]
async fn an_applied_decision_is_traced_with_its_inputs() {
    let rig = rig("");
    let tools = tied_tools();
    script_both(&rig, "charlie_doc", "charlie_doc", &NAMES);
    rig.ranker.rank("change document", &tools).await;
    let drained = rig.runtime.telemetry().drain();
    assert!(drained.traces[0].applied);
    assert_eq!(drained.traces[0].policy, "tool_routing");
    assert_eq!(drained.traces[0].inputs_digest.len(), 64);
    let inference = drained.inference.unwrap();
    assert_eq!((inference.calls, inference.batch_size), (1, Some(2)));
    assert_eq!(inference.model_id.as_deref(), Some("mock/model"));
}

#[tokio::test]
async fn a_request_never_carries_more_options_than_the_cap() {
    let rig = rig("");
    let many: Vec<ToolDescriptor> = (0..100)
        .map(|i| {
            tool(
                &format!("tool_{i:03}"),
                "Change a document.",
                ToolCapability::WriteFile,
            )
        })
        .collect();
    // Whatever the mock answers, the question it was asked is what is checked.
    let _ = rig.ranker.rank("change document", &many).await;
    for request in rig.mock.requests() {
        for question in &request.questions {
            let sven_decision::Question::Choice(c) = question else {
                panic!("choices only")
            };
            assert!(
                c.options.len() <= OPTIONS_PER_REQUEST,
                "{}",
                c.options.len()
            );
        }
    }
    assert!(!rig.mock.requests().is_empty());
}

#[tokio::test]
async fn a_pool_spanning_categories_is_narrowed_by_category_before_the_shortlist() {
    let rig = rig("");
    let mut tools: Vec<ToolDescriptor> = (0..12)
        .map(|i| {
            tool(
                &format!("read_{i:02}"),
                "Change a document.",
                ToolCapability::ReadFile,
            )
        })
        .collect();
    tools.extend((0..12).map(|i| {
        tool(
            &format!("write_{i:02}"),
            "Change a document.",
            ToolCapability::WriteFile,
        )
    }));

    // First call: which category; second: which tool within it.
    let categories = ["read", "write"];
    let category_answers = [
        picks("0:tool", "write", &categories),
        picks("1:tool", "write", &["write", "read"]),
    ];
    rig.mock.push_result(DecisionResult::for_answers(
        "mock/model",
        category_answers
            .iter()
            .map(|(i, k, a)| (i.as_str(), *k, a.clone()))
            .collect(),
    ));
    let within: Vec<String> = (0..8).map(|i| format!("write_{i:02}")).collect();
    let within_refs: Vec<&str> = within.iter().map(String::as_str).collect();
    let mut rev = within_refs.clone();
    rev.reverse();
    let tool_answers = [
        picks("0:tool", "write_03", &within_refs),
        picks("1:tool", "write_03", &rev),
    ];
    rig.mock.push_result(DecisionResult::for_answers(
        "mock/model",
        tool_answers
            .iter()
            .map(|(i, k, a)| (i.as_str(), *k, a.clone()))
            .collect(),
    ));

    let ranked = rig.ranker.rank("change document", &tools).await;
    assert_eq!(ranked[0].name, "write_03");
    assert_eq!(ranked.len(), bm25("change document", &tools).await.len());
    let requests = rig.mock.requests();
    assert_eq!(requests.len(), 2);
    let sven_decision::Question::Choice(second) = &requests[1].questions[0] else {
        panic!("a choice")
    };
    assert!(
        second.options.iter().all(|o| o.id.starts_with("write_")),
        "only the chosen category"
    );
}

#[tokio::test]
async fn the_state_the_model_reads_fits_the_input_budget() {
    let rig = rig("policy: {max_decision_input_tokens: 64}\n");
    let tools = tied_tools();
    let long_query = format!("change document {}", "word ".repeat(2_000));
    let _ = rig.ranker.rank(&long_query, &tools).await;
    for request in rig.mock.requests() {
        assert!(request.state.len() < 64 * 4, "{}", request.state.len());
    }
    assert!(Duration::from_millis(500) >= rig.runtime.timeout());
}
