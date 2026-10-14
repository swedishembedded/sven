// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping delegated agent work
// cheap and non-duplicative for its clients. If your team needs expertise in
// multi-agent context handoff then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The decision point behind `task`'s near-match deduplication.
//!
//! The deterministic rules of `task_tool::subgoals` settle exact and
//! contained subgoals. When two read-only goals over contained scopes share
//! most of their words but are worded differently, a decision model may say
//! whether they ask for the same thing. It only advises, and only toward
//! launching less: the question is asked in both option orders, the policy's
//! thresholds are high, and an abstention, a disagreement between the orders or
//! a failure means the subgoal launches as it would have without the model.
//! A writing subgoal never reaches this point, and the answer cannot widen
//! the scope that was already required to be contained.
//!
//! Gated by `decision.features.delegation`.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use sven_decision::config::Feature;
use sven_decision::policy::{
    agree, estimate_input_tokens, evaluate, evaluate_error, AllowedShape, Cost, DecisionPolicy,
    FallbackAction, Impact, PolicyDecl, Selected, Thresholds, Verdict,
};
use sven_decision::telemetry::{trace_record, CallRecord};
use sven_decision::{
    decide_batched, Choice, ChoiceOption, DecisionProvider, DecisionRequest, DecisionRuntime,
    Question, QuestionKind,
};
use sven_tool_api::events::ToolEvent;
use tokio::sync::mpsc;

use crate::task_tool::NearMatch;

/// The trace label of the decision point.
const LABEL: &str = "delegation.same_goal";
const SAME: &str = "same";
const DIFFERENT: &str = "different";

/// The decision point's policy, declared here and nowhere else: a wrong
/// "same" costs the parent a result for a question it did not ask, so the
/// thresholds are high and abstention (launching) is the default.
fn policy(budget: u32) -> DecisionPolicy {
    DecisionPolicy::new(PolicyDecl {
        label: LABEL.into(),
        shape: AllowedShape {
            kind: QuestionKind::Choice,
            max_options: 2,
        },
        max_input_tokens: budget,
        false_positive: Cost::High,
        false_negative: Cost::Low,
        thresholds: Thresholds {
            min_confidence: 0.9,
            min_margin: 0.7,
        },
        impact: Impact::Routine,
        fallback: FallbackAction::Legacy,
    })
    .expect("the delegation policy is valid")
}

/// Asks a decision model whether two near-identical goals are the same.
pub(crate) struct DecisionNearMatch {
    provider: Arc<dyn DecisionProvider>,
    runtime: Arc<DecisionRuntime>,
    notices: Option<mpsc::Sender<ToolEvent>>,
    policy: DecisionPolicy,
    budget: u32,
}

impl DecisionNearMatch {
    pub(crate) fn new(
        provider: Arc<dyn DecisionProvider>,
        runtime: Arc<DecisionRuntime>,
        notices: Option<mpsc::Sender<ToolEvent>>,
    ) -> Self {
        let budget = runtime.config().input_budget_tokens(None);
        Self {
            provider,
            runtime,
            notices,
            policy: policy(budget),
            budget,
        }
    }

    fn request(&self, state: &str, options: [&str; 2]) -> DecisionRequest {
        DecisionRequest {
            state: state.to_string(),
            questions: vec![Question::Choice(Choice {
                id: "same".into(),
                instruction: "Do the two goals ask for the same result?".into(),
                options: options
                    .into_iter()
                    .map(|id| ChoiceOption {
                        id: id.into(),
                        description: None,
                    })
                    .collect(),
            })],
            deadline: self.runtime.timeout(),
        }
    }
}

#[async_trait]
impl NearMatch for DecisionNearMatch {
    async fn same_goal(&self, new: &str, registered: &str) -> Option<bool> {
        let state = format!("Goal A: {new}\nGoal B: {registered}");
        if estimate_input_tokens(&state) > self.budget {
            return None;
        }
        let requests = [
            self.request(&state, [DIFFERENT, SAME]),
            self.request(&state, [SAME, DIFFERENT]),
        ];
        let started = Instant::now();
        let batched = decide_batched(&*self.provider, &requests).await;
        let model_id = batched
            .results
            .iter()
            .find_map(|r| r.as_ref().ok().map(|r| r.model.id.clone()));
        self.runtime.telemetry().record_call(&CallRecord {
            wall: started.elapsed(),
            model_id: model_id.clone(),
            batch_size: batched.batch_sizes.iter().sum(),
            cached: batched
                .results
                .iter()
                .all(|r| r.as_ref().is_ok_and(|r| r.cached)),
        });
        let verdicts: Vec<Verdict> = requests
            .iter()
            .zip(&batched.results)
            .map(|(request, result)| match result {
                Ok(result) => evaluate(&self.policy, &request.questions[0], result),
                Err(error) => {
                    if let (Some(text), Some(notices)) =
                        (self.runtime.diagnose(error), &self.notices)
                    {
                        let _ = notices.try_send(ToolEvent::Notice(text));
                    }
                    evaluate_error(error)
                }
            })
            .collect();
        let verdict = agree(&verdicts[0], &verdicts[1]);
        let applied = !self.runtime.shadow() && matches!(verdict, Verdict::Accept(_));
        self.runtime.telemetry().record_trace(trace_record(
            LABEL,
            &requests[0],
            &verdict,
            model_id.as_deref(),
            applied,
        ));
        match verdict {
            Verdict::Accept(accepted) if applied => match accepted.selected {
                Selected::Candidate { id, .. } => Some(id == SAME),
                Selected::Proposition { .. } => None,
            },
            _ => None,
        }
    }
}

/// The advisor a session uses: present when decisions are on and
/// `decision.features.delegation` allows it.
pub(crate) fn for_session(
    runtime: &Arc<DecisionRuntime>,
    notices: &mpsc::Sender<ToolEvent>,
) -> Option<Arc<dyn NearMatch>> {
    if !runtime.feature_enabled(Feature::Delegation) {
        return None;
    }
    let provider: Arc<dyn DecisionProvider> = runtime.provider()?.clone();
    Some(Arc::new(DecisionNearMatch::new(
        provider,
        Arc::clone(runtime),
        Some(notices.clone()),
    )))
}

#[cfg(test)]
mod tests {
    use sven_decision::config::DecisionConfig;
    use sven_decision::{Answer, DecisionError, DecisionResult, MockDecisionProvider};

    use super::*;

    fn rig() -> (
        DecisionNearMatch,
        Arc<MockDecisionProvider>,
        Arc<DecisionRuntime>,
    ) {
        let config: DecisionConfig = serde_yaml::from_str("enabled: true\n").unwrap();
        let runtime = Arc::new(DecisionRuntime::new(config).unwrap());
        let mock = Arc::new(MockDecisionProvider::new("mock/model"));
        (
            DecisionNearMatch::new(mock.clone(), runtime.clone(), None),
            mock,
            runtime,
        )
    }

    fn answer(id: &str, same: f32, order: [&str; 2]) -> (String, QuestionKind, Answer) {
        let p = |o: &str| (o.to_string(), if o == SAME { same } else { 1.0 - same });
        let pairs: Vec<(String, f32)> = order.iter().map(|o| p(o)).collect();
        let refs: Vec<(&str, f32)> = pairs.iter().map(|(o, p)| (o.as_str(), *p)).collect();
        (
            id.into(),
            QuestionKind::Choice,
            Answer::choice(&refs).unwrap(),
        )
    }

    fn script(mock: &MockDecisionProvider, first: f32, second: f32) {
        let a = answer("0:same", first, [DIFFERENT, SAME]);
        let b = answer("1:same", second, [SAME, DIFFERENT]);
        mock.push_result(DecisionResult::for_answers(
            "mock/model",
            vec![(a.0.as_str(), a.1, a.2), (b.0.as_str(), b.1, b.2)],
        ));
    }

    #[tokio::test]
    async fn a_confident_answer_in_both_orders_says_same() {
        let (advisor, mock, _) = rig();
        script(&mock, 0.97, 0.97);
        assert_eq!(
            advisor.same_goal("find callers", "find all callers").await,
            Some(true)
        );
        assert_eq!(mock.requests().len(), 1, "both orders travel in one call");
    }

    #[tokio::test]
    async fn a_confident_different_is_a_no() {
        let (advisor, mock, _) = rig();
        script(&mock, 0.02, 0.02);
        assert_eq!(advisor.same_goal("a", "b").await, Some(false));
    }

    #[tokio::test]
    async fn anything_short_of_a_confident_answer_abstains() {
        let (advisor, mock, runtime) = rig();
        script(&mock, 0.6, 0.6);
        assert_eq!(
            advisor.same_goal("a", "b").await,
            None,
            "below the thresholds"
        );
        script(&mock, 0.97, 0.02);
        assert_eq!(
            advisor.same_goal("a", "b").await,
            None,
            "the answer follows the order"
        );
        for _ in 0..2 {
            mock.push_error(DecisionError::Unreachable("refused".into()));
        }
        assert_eq!(
            advisor.same_goal("a", "b").await,
            None,
            "a failure launches"
        );
        assert_eq!(runtime.telemetry().drain().traces.len(), 3);
    }

    #[tokio::test]
    async fn shadow_mode_records_the_verdict_and_never_acts_on_it() {
        let config: DecisionConfig =
            serde_yaml::from_str("enabled: true\npolicy: {mode: shadow}\n").unwrap();
        let runtime = Arc::new(DecisionRuntime::new(config).unwrap());
        let mock = Arc::new(MockDecisionProvider::new("mock/model"));
        let advisor = DecisionNearMatch::new(mock.clone(), runtime, None);
        script(&mock, 0.97, 0.97);
        assert_eq!(advisor.same_goal("a", "b").await, None);
    }

    #[tokio::test]
    async fn the_flag_switches_the_point_off() {
        let config: DecisionConfig =
            serde_yaml::from_str("enabled: true\nfeatures: {delegation: false}\n").unwrap();
        let runtime = Arc::new(DecisionRuntime::new(config).unwrap());
        let (tx, _rx) = mpsc::channel(1);
        assert!(for_session(&runtime, &tx).is_none());
    }
}
