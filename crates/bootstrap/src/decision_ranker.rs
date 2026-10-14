// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in tool retrieval for
// large agent tool sets then you can procure our services by sending an
// email to info@swedishembedded.com.
//! A [`ToolRanker`] that lets a decision model break BM25's ties.
//!
//! The same ranker serves two decision points, each with its own label and
//! feature flag ([`RoutingPoint`]): `tool_routing` re-ranks `tools search`
//! and `skill_selection` re-ranks `skill search`.
//!
//! BM25 stays the candidate generator and the fallback. The model is asked
//! only when BM25's two best candidates are within [`AMBIGUITY`] of each
//! other, and never about more than [`OPTIONS_PER_REQUEST`] options at once:
//!
//! 1. the best [`POOL`] BM25 matches are the pool;
//! 2. when the pool is larger than one request holds and spans several
//!    categories, the model first picks a category and the pool narrows to it;
//! 3. the best [`OPTIONS_PER_REQUEST`] of the pool are the shortlist, and the
//!    model picks the tool that fits.
//!
//! Each question is asked with the options in two orders (name ascending and
//! descending) in one batched call, and a winner that changes with the order
//! is not trusted. An accepted winner moves to the front; everything else
//! keeps BM25's order, so a re-rank only reorders and never removes a tool.
//! Anything else (no ambiguity, an abstention, a provider failure, shadow
//! mode) returns BM25's ranking exactly.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use sven_decision::config::Feature;
use sven_decision::policy::{
    agree, estimate_input_tokens, evaluate, evaluate_error, AbstainReason, AllowedShape, Cost,
    DecisionPolicy, FallbackAction, Impact, PolicyDecl, Selected, Thresholds, Verdict,
};
use sven_decision::telemetry::{trace_record, CallRecord};
use sven_decision::{
    decide_batched, Choice, ChoiceOption, DecisionError, DecisionProvider, DecisionRequest,
    DecisionRuntime, Question, QuestionKind,
};
use sven_tool_api::events::ToolEvent;
use sven_tool_registry::{Bm25Ranker, RankedTool, ToolDescriptor, ToolRanker};
use tokio::sync::mpsc;

/// The most options one question carries. Far under what a decision model
/// holds, and enough to cover the plausible tools of a query.
pub const OPTIONS_PER_REQUEST: usize = 8;
/// How many BM25 matches the model's two levels draw on.
pub const POOL: usize = 32;
/// BM25's top two are ambiguous when the second is within this fraction of
/// the first.
pub const AMBIGUITY: f64 = 0.10;
/// The least tokens of the query worth asking about.
const MIN_QUERY_TOKENS: u32 = 16;

/// A decision point the ranker serves: the labels its two questions are
/// traced under and the flag that switches it on.
#[derive(Clone, Copy, Debug)]
pub struct RoutingPoint {
    /// The question that picks one of the shortlisted candidates.
    pub choice: &'static str,
    /// The question that picks a category when the pool spans several.
    pub category: &'static str,
    /// The `decision.features.*` flag that gates the point.
    pub feature: Feature,
}

impl RoutingPoint {
    /// Which tool `tools search` should put first.
    pub const TOOLS: Self = Self {
        choice: "tool_routing",
        category: "tool_routing.category",
        feature: Feature::ToolRouting,
    };
    /// Which skill `skill search` should put first.
    pub const SKILLS: Self = Self {
        choice: "skill_selection",
        category: "skill_selection.category",
        feature: Feature::SkillSelection,
    };
}

/// What asking one question came to.
enum Asked {
    /// The model picked this option and the policy accepted it.
    Chose(String),
    /// The policy declined the answer.
    Declined,
    /// The provider failed.
    Failed,
}

/// Re-ranks ambiguous `tools search` results with a decision model.
pub struct DecisionRanker {
    bm25: Bm25Ranker,
    provider: Arc<dyn DecisionProvider>,
    runtime: Arc<DecisionRuntime>,
    notices: Option<mpsc::Sender<ToolEvent>>,
    tool_policy: DecisionPolicy,
    category_policy: DecisionPolicy,
    /// The input limit the model last reported; 0 until it has.
    model_limit: AtomicU32,
}

fn policy(label: &str, budget: u32) -> DecisionPolicy {
    DecisionPolicy::new(PolicyDecl {
        label: label.into(),
        shape: AllowedShape {
            kind: QuestionKind::Choice,
            max_options: OPTIONS_PER_REQUEST,
        },
        max_input_tokens: budget,
        false_positive: Cost::Low,
        false_negative: Cost::Low,
        thresholds: Thresholds {
            min_confidence: 0.55,
            min_margin: 0.15,
        },
        impact: Impact::Routine,
        fallback: FallbackAction::Legacy,
    })
    .expect("the tool routing policies are valid")
}

impl DecisionRanker {
    /// A ranker that asks `provider`, accounts to `runtime`, and reports the
    /// session's one diagnostic on `notices`.
    #[must_use]
    pub fn new(
        provider: Arc<dyn DecisionProvider>,
        runtime: Arc<DecisionRuntime>,
        notices: Option<mpsc::Sender<ToolEvent>>,
    ) -> Self {
        Self::for_point(RoutingPoint::TOOLS, provider, runtime, notices)
    }

    /// A ranker for the decision point `point`.
    #[must_use]
    pub fn for_point(
        point: RoutingPoint,
        provider: Arc<dyn DecisionProvider>,
        runtime: Arc<DecisionRuntime>,
        notices: Option<mpsc::Sender<ToolEvent>>,
    ) -> Self {
        let budget = runtime.config().input_budget_tokens(None);
        Self {
            bm25: Bm25Ranker,
            provider,
            tool_policy: policy(point.choice, budget),
            category_policy: policy(point.category, budget),
            runtime,
            notices,
            model_limit: AtomicU32::new(0),
        }
    }

    fn budget(&self) -> u32 {
        let reported = self.model_limit.load(Ordering::Relaxed);
        self.runtime
            .config()
            .input_budget_tokens((reported > 0).then_some(reported))
    }

    async fn rerank(
        &self,
        query: &str,
        descriptors: &HashMap<&str, &ToolDescriptor>,
        ranked: &[RankedTool],
    ) -> Option<String> {
        let mut pool: Vec<&ToolDescriptor> = ranked
            .iter()
            .take(POOL)
            .filter_map(|r| descriptors.get(r.name.as_str()).copied())
            .collect();

        let mut categories: Vec<&str> = Vec::new();
        for tool in &pool {
            if !categories.contains(&tool.category()) {
                categories.push(tool.category());
            }
        }
        if pool.len() > OPTIONS_PER_REQUEST && categories.len() > 1 {
            let options = categories
                .iter()
                .map(|category| {
                    let members: Vec<&str> = pool
                        .iter()
                        .filter(|t| t.category() == *category)
                        .map(|t| t.name.as_str())
                        .collect();
                    (
                        (*category).to_string(),
                        format!(
                            "{} tools, such as {}",
                            members.len(),
                            members[..members.len().min(2)].join(", ")
                        ),
                    )
                })
                .collect();
            match self.ask(query, &self.category_policy, options).await {
                Asked::Chose(category) => pool.retain(|t| t.category() == category),
                Asked::Declined => {}
                Asked::Failed => return None,
            }
        }

        let options = pool
            .iter()
            .take(OPTIONS_PER_REQUEST)
            .map(|t| (t.name.clone(), t.summary.clone()))
            .collect();
        match self.ask(query, &self.tool_policy, options).await {
            Asked::Chose(name) => Some(name),
            Asked::Declined | Asked::Failed => None,
        }
    }

    /// Asks which of `options` (id, description, best BM25 match first) fits
    /// `query`, in two orders, and judges the answers against `policy`.
    async fn ask(
        &self,
        query: &str,
        policy: &DecisionPolicy,
        mut options: Vec<(String, String)>,
    ) -> Asked {
        let instruction = "Which option fits the task best?";
        let budget = self.budget();
        // Drop the weakest BM25 matches until the options leave room for the task.
        let cost = |options: &[(String, String)]| -> u32 {
            estimate_input_tokens(instruction)
                + options
                    .iter()
                    .map(|(id, d)| estimate_input_tokens(id) + estimate_input_tokens(d))
                    .sum::<u32>()
        };
        while options.len() > 2 && cost(&options) + MIN_QUERY_TOKENS > budget {
            options.pop();
        }
        let room = budget.saturating_sub(cost(&options));
        if options.len() < 2 || room < MIN_QUERY_TOKENS {
            self.trace_declined(
                policy,
                query,
                AbstainReason::InputTooLarge {
                    tokens: cost(&options) + MIN_QUERY_TOKENS,
                    budget,
                },
            );
            return Asked::Declined;
        }
        // `room` is tokens; the estimator charges one per three characters.
        let task: String = query
            .chars()
            .take((room as usize).saturating_mul(3).saturating_sub(8))
            .collect();
        let state = format!("Task: {task}");

        let ask = |options: Vec<(String, String)>| DecisionRequest {
            state: state.clone(),
            questions: vec![Question::Choice(Choice {
                id: "tool".into(),
                instruction: instruction.into(),
                options: options
                    .into_iter()
                    .map(|(id, description)| ChoiceOption {
                        id,
                        description: Some(description),
                    })
                    .collect(),
            })],
            deadline: self.runtime.timeout(),
        };
        options.sort();
        let ascending = ask(options.clone());
        options.reverse();
        let descending = ask(options);
        let requests = [ascending, descending];

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

        let mut verdicts = Vec::with_capacity(2);
        for (request, result) in requests.iter().zip(&batched.results) {
            verdicts.push(match result {
                Ok(result) => {
                    if let Some(limit) = result.max_input_tokens {
                        self.model_limit.store(limit, Ordering::Relaxed);
                    }
                    evaluate(policy, &request.questions[0], result)
                }
                Err(error) => {
                    self.report(error);
                    evaluate_error(error)
                }
            });
        }
        let failed = batched.results.iter().any(Result::is_err);
        let verdict = agree(&verdicts[0], &verdicts[1]);
        let applied = !self.runtime.shadow() && matches!(verdict, Verdict::Accept(_));
        self.runtime.telemetry().record_trace(trace_record(
            policy.label(),
            &requests[0],
            &verdict,
            model_id.as_deref(),
            applied,
        ));
        match verdict {
            Verdict::Accept(accepted) => match accepted.selected {
                Selected::Candidate { id, .. } => Asked::Chose(id),
                Selected::Proposition { .. } => Asked::Declined,
            },
            Verdict::Abstain(_) if failed => Asked::Failed,
            Verdict::Abstain(_) => Asked::Declined,
        }
    }

    fn trace_declined(&self, policy: &DecisionPolicy, query: &str, reason: AbstainReason) {
        let request = DecisionRequest {
            state: query.to_string(),
            questions: Vec::new(),
            deadline: self.runtime.timeout(),
        };
        self.runtime.telemetry().record_trace(trace_record(
            policy.label(),
            &request,
            &Verdict::Abstain(reason),
            None,
            false,
        ));
    }

    /// Says once per session that decisions are unavailable.
    fn report(&self, error: &DecisionError) {
        let Some(text) = self.runtime.diagnose(error) else {
            return;
        };
        if let Some(notices) = &self.notices {
            if notices.try_send(ToolEvent::Notice(text)).is_err() {
                tracing::debug!("the decision notice could not be delivered");
            }
        }
    }
}

#[async_trait]
impl ToolRanker for DecisionRanker {
    async fn rank(&self, query: &str, candidates: &[ToolDescriptor]) -> Vec<RankedTool> {
        let mut ranked = self.bm25.rank(query, candidates).await;
        let ambiguous = match ranked.as_slice() {
            [first, second, ..] => first.score - second.score <= AMBIGUITY * first.score,
            _ => false,
        };
        if !ambiguous {
            return ranked;
        }
        let descriptors: HashMap<&str, &ToolDescriptor> =
            candidates.iter().map(|t| (t.name.as_str(), t)).collect();
        let winner = self.rerank(query, &descriptors, &ranked).await;
        match winner {
            Some(winner) if !self.runtime.shadow() => {
                if let Some(at) = ranked.iter().position(|r| r.name == winner) {
                    let top_score = ranked[0].score;
                    let mut promoted = ranked.remove(at);
                    promoted.score = top_score;
                    ranked.insert(0, promoted);
                }
                ranked
            }
            _ => ranked,
        }
    }
}

/// The session's decision runtime. Off contacts nothing; `required` refuses
/// to start without a suitable model, before anything that depends on one is
/// assembled.
pub async fn runtime_for(config: &crate::Config) -> anyhow::Result<Arc<DecisionRuntime>> {
    let runtime = Arc::new(
        DecisionRuntime::new(config.decision.clone())
            .map_err(|e| anyhow::anyhow!("invalid decision configuration: {e}"))?,
    );
    runtime.require_ready().await.map_err(anyhow::Error::msg)?;
    Ok(runtime)
}

/// The ranker a session searches with: the decision ranker when decisions are
/// on and `decision.features.tool_routing` allows it, otherwise `None` and
/// BM25 alone.
#[must_use]
pub fn for_session(
    runtime: &Arc<DecisionRuntime>,
    notices: mpsc::Sender<ToolEvent>,
) -> Option<Arc<dyn ToolRanker>> {
    for_point(RoutingPoint::TOOLS, runtime, notices)
}

/// The ranker for `point`: the decision ranker when decisions are on and the
/// point's flag allows it, otherwise `None` and BM25 alone.
#[must_use]
pub fn for_point(
    point: RoutingPoint,
    runtime: &Arc<DecisionRuntime>,
    notices: mpsc::Sender<ToolEvent>,
) -> Option<Arc<dyn ToolRanker>> {
    if !runtime.feature_enabled(point.feature) {
        return None;
    }
    let provider: Arc<dyn DecisionProvider> = runtime.provider()?.clone();
    Some(Arc::new(DecisionRanker::for_point(
        point,
        provider,
        Arc::clone(runtime),
        Some(notices),
    )))
}

/// The rankers a session's search tools use: the discovery tool's, present
/// when tool definitions are deferred, and the `skill` tool's.
#[must_use]
pub(crate) fn session_rankers(
    config: &crate::Config,
    runtime: &sven_turn::AgentRuntimeContext,
    mode: &Arc<tokio::sync::Mutex<sven_vocab::AgentMode>>,
    decision: &Arc<DecisionRuntime>,
    notices: mpsc::Sender<ToolEvent>,
) -> (
    Option<crate::registry::DeferredDisclosure>,
    Option<Arc<dyn ToolRanker>>,
) {
    let deferred = (config.tools.disclosure.mode == sven_tool_registry::Disclosure::Deferred
        && !runtime.no_tools)
        .then(|| crate::registry::DeferredDisclosure {
            mode: Arc::clone(mode),
            ranker: for_session(decision, notices.clone()),
        });
    (deferred, for_point(RoutingPoint::SKILLS, decision, notices))
}

#[cfg(test)]
mod notice_tests {
    use sven_hsm::ObservationSink;
    use sven_tool_api::events::ToolEvent;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn a_tool_notice_reaches_every_surface_as_a_warning() {
        let obs = ObservationSink::new(8);
        let mut seen = obs.subscribe();
        let (tx, rx) = mpsc::channel(4);
        crate::runtime_builder::spawn_tool_event_forwarder(rx, obs);
        tx.send(ToolEvent::Notice("decisions unavailable".into()))
            .await
            .unwrap();
        match seen.recv().await.unwrap() {
            sven_vocab::SessionEvent::Warning(text) => assert_eq!(text, "decisions unavailable"),
            other => panic!("expected a warning, got {other:?}"),
        }
    }
}
