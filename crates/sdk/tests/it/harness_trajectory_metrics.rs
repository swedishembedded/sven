// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A harness trajectory's final metrics are the sums of its conversations'
//! steps: prompt tokens with the cached part included, cached tokens and
//! cost, as ATIF defines them and as an agent's trajectory already reports.
//!
//! Swedish Embedded AB implements cost-accountable agent workflows for its
//! clients. If your team needs expertise in measuring what agent runs cost
//! then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use sven_model::{CompletionRequest, ModelProvider, ResponseEvent, ResponseStream};
use sven_sdk::{atif, Engine, RunOptions};

const WORKFLOW: &str = r#"
api_version: "2"
name: priced
execution: { limits: { model_turns: 4 } }
workflow:
  - name: plan
    model_only: true
    task: Plan it.
  - name: write
    model_only: true
    task: Write it.
"#;

/// Answers every request with 30 uncached and 70 cached prompt tokens, 5
/// generated tokens and a cost of 0.25 USD.
struct Priced;

#[async_trait]
impl ModelProvider for Priced {
    fn name(&self) -> &str {
        "priced"
    }
    fn model_name(&self) -> &str {
        "m"
    }
    async fn complete(&self, _req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        let events: Vec<anyhow::Result<ResponseEvent>> = vec![
            Ok(ResponseEvent::TextDelta("ok".into())),
            Ok(ResponseEvent::Usage {
                input_tokens: 30,
                output_tokens: 5,
                cache_read_tokens: 70,
                cache_write_tokens: 0,
                cost_usd: Some(0.25),
            }),
            Ok(ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

#[tokio::test]
async fn the_final_metrics_are_the_sums_of_every_conversations_steps() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::builder()
        .model_provider(Arc::new(Priced) as Arc<dyn ModelProvider>)
        .project_root(dir.path())
        .build()
        .expect("an engine builds");
    let harness = engine.load_harness_from_str(WORKFLOW).expect("loads");
    let mut run = harness.start(json!({})).expect("starts");
    let outcome = tokio::time::timeout(Duration::from_secs(20), run.execute(RunOptions::new()))
        .await
        .expect("the run ends")
        .expect("an outcome");
    assert!(
        matches!(&outcome.result, Some(Ok(_))),
        "{:?}",
        outcome.result
    );
    assert_eq!(
        outcome.usage.input_tokens,
        Some(60),
        "uncached only, as documented"
    );
    assert_eq!(outcome.usage.cached_tokens, Some(140));
    assert!(outcome
        .usage
        .cost_usd
        .is_some_and(|c| (c - 0.5).abs() < 1e-9));

    let trajectory = run.trajectory().expect("an executed run has one");
    let steps: Vec<&atif::StepMetrics> = trajectory
        .steps
        .iter()
        .chain(
            trajectory
                .subagent_trajectories
                .iter()
                .flatten()
                .flat_map(|t| t.steps.iter()),
        )
        .filter_map(|s| s.metrics.as_ref())
        .collect();
    assert_eq!(steps.len(), 2, "one priced request per task");
    let total = trajectory.final_metrics.as_ref().expect("final metrics");
    assert_eq!(total.total_prompt_tokens, Some(200), "uncached plus cached");
    assert_eq!(
        total.total_prompt_tokens,
        Some(steps.iter().filter_map(|m| m.prompt_tokens).sum())
    );
    assert_eq!(total.total_cached_tokens, Some(140));
    assert_eq!(total.total_completion_tokens, Some(10));
    assert!(
        total.total_cost_usd.is_some_and(|c| (c - 0.5).abs() < 1e-9),
        "{total:?}"
    );
}
