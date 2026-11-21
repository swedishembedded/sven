// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The run budget's contract, on a provider that reports what it was asked.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use sven_model::{CompletionRequest, ModelProvider, ResponseEvent, ResponseStream};

use super::*;

/// Answers every request with `output` completion tokens (no usage when
/// `None`) and remembers the output limit each request carried.
struct Counting {
    output: Option<u32>,
    limits: Mutex<Vec<(Option<u32>, bool)>>,
}

impl Counting {
    fn new(output: Option<u32>) -> Arc<Self> {
        Arc::new(Self {
            output,
            limits: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl ModelProvider for Counting {
    fn name(&self) -> &str {
        "counting"
    }
    fn model_name(&self) -> &str {
        "m"
    }
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        self.limits
            .lock()
            .unwrap()
            .push((req.max_output_tokens_override, req.output_limit_is_room));
        let mut events = vec![Ok(ResponseEvent::TextDelta("ok".into()))];
        if let Some(output_tokens) = self.output {
            events.push(Ok(ResponseEvent::Usage {
                input_tokens: 100,
                output_tokens,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: None,
            }));
        }
        events.push(Ok(ResponseEvent::Done));
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

async fn complete(provider: &BudgetedProvider, limit: Option<u32>) -> anyhow::Result<()> {
    let req = CompletionRequest {
        max_output_tokens_override: limit,
        ..CompletionRequest::default()
    };
    let _: Vec<_> = provider.complete(req).await?.collect().await;
    Ok(())
}

fn budgeted(inner: &Arc<Counting>, limits: RunBudgetLimits) -> (BudgetedProvider, Arc<RunBudget>) {
    let budget = RunBudget::new(limits);
    let inner: Arc<dyn ModelProvider> = inner.clone();
    (BudgetedProvider::new(inner, Arc::clone(&budget)), budget)
}

#[tokio::test]
async fn the_turn_after_the_last_allowed_one_is_refused_with_the_typed_error() {
    let inner = Counting::new(Some(5));
    let (provider, budget) = budgeted(
        &inner,
        RunBudgetLimits {
            max_turns: Some(2),
            ..RunBudgetLimits::default()
        },
    );
    complete(&provider, None).await.unwrap();
    complete(&provider, None).await.unwrap();
    assert!(
        budget.exhausted().is_none(),
        "two of two turns is not exhaustion"
    );

    let refused = complete(&provider, None).await.unwrap_err();
    let refused = refused.downcast_ref::<BudgetExhausted>().expect("typed");
    assert_eq!(refused.bound, Bound::Turns);
    assert_eq!(refused.to_string(), "run budget exhausted: 2 of 2 turns");
    assert_eq!(budget.exhausted().map(|e| e.bound), Some(Bound::Turns));
    assert_eq!(
        inner.limits.lock().unwrap().len(),
        2,
        "the third never left"
    );
}

#[tokio::test]
async fn each_request_is_held_to_the_tokens_left_and_the_run_ends_when_none_are() {
    let inner = Counting::new(Some(60));
    let (provider, budget) = budgeted(
        &inner,
        RunBudgetLimits {
            max_completion_tokens: Some(100),
            max_turns: Some(40),
        },
    );
    complete(&provider, Some(4096)).await.unwrap();
    complete(&provider, Some(4096)).await.unwrap();
    // Two answers of 60 against a cap of 100: the second was held to 40 and
    // the server did not honour it, so the run is past its cap.
    assert_eq!(
        *inner.limits.lock().unwrap(),
        vec![(Some(100), false), (Some(40), false)]
    );
    let refused = complete(&provider, Some(10)).await.unwrap_err();
    assert_eq!(
        refused.to_string(),
        "run budget exhausted: 120 of 100 completion tokens, 2 of 40 turns"
    );
    assert_eq!(
        budget.spent(),
        Spent {
            completion_tokens: 120,
            turns: 2
        }
    );
}

#[tokio::test]
async fn a_tighter_limit_than_the_tokens_left_is_kept_as_sent() {
    let inner = Counting::new(Some(1));
    let (provider, _) = budgeted(
        &inner,
        RunBudgetLimits {
            max_completion_tokens: Some(1000),
            ..RunBudgetLimits::default()
        },
    );
    let req = CompletionRequest {
        max_output_tokens_override: Some(300),
        output_limit_is_room: true,
        ..CompletionRequest::default()
    };
    let _: Vec<_> = provider.complete(req).await.unwrap().collect().await;
    assert_eq!(*inner.limits.lock().unwrap(), vec![(Some(300), true)]);
}

#[tokio::test]
async fn a_completion_without_usage_is_charged_all_it_was_allowed() {
    let inner = Counting::new(None);
    let (provider, budget) = budgeted(
        &inner,
        RunBudgetLimits {
            max_completion_tokens: Some(500),
            ..RunBudgetLimits::default()
        },
    );
    complete(&provider, Some(200)).await.unwrap();
    assert_eq!(budget.spent().completion_tokens, 200);
    assert_eq!(budget.unmetered(), 1);
}

#[tokio::test]
async fn a_completion_streaming_holds_its_allowance_from_the_next() {
    let inner = Counting::new(Some(10));
    let (provider, budget) = budgeted(
        &inner,
        RunBudgetLimits {
            max_completion_tokens: Some(100),
            ..RunBudgetLimits::default()
        },
    );
    let open = provider
        .complete(CompletionRequest::default())
        .await
        .unwrap();
    assert_eq!(budget.remaining().max_completion_tokens, Some(0));
    assert!(complete(&provider, None).await.is_err(), "nothing is left");
    drop(open);
    // The open stream never reported its usage: charged its whole allowance.
    assert_eq!(budget.spent().completion_tokens, 100);
}

#[test]
fn a_childs_spending_is_charged_to_the_run() {
    let budget = RunBudget::new(RunBudgetLimits {
        max_completion_tokens: Some(100),
        max_turns: Some(3),
    });
    budget.charge(Spent {
        completion_tokens: 30,
        turns: 3,
    });
    assert_eq!(
        budget.remaining(),
        RunBudgetLimits {
            max_completion_tokens: Some(70),
            max_turns: Some(0)
        }
    );
    assert_eq!(budget.admit(None).unwrap_err().bound, Bound::Turns);
}
