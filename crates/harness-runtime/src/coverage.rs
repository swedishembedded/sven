// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Work items that know which files they cover.
//!
//! A `for_each` with `covers` records, when an item's checks have passed, a
//! digest of the workspace files the item covers. A completed item is trusted
//! only for those contents: when a run is resumed, the digests are taken
//! again at the next item boundary, and an item whose files changed since (or
//! appeared, or vanished) is pending again and runs again. Conservative on
//! purpose: any difference re-pends, and nothing the agent says counts.
//! Digests come from the harness-only `digest_files` tool.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in telling which finished work is
//! still valid after the code under it changed then you can procure our
//! services by sending an email to info@swedishembedded.com.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sven_harness_spec::{PlanIteration, PlanPrimitive, PlanStep, Template, DIGEST_TOOL};
use sven_hsm::{Context, Effect, Reaction, ToolCapability};

use crate::command_gate::INTERRUPTED_OPERATION;
use crate::iteration::ItemStatus;
use crate::machine::HarnessMachine;
use crate::run::{HarnessRunState, Inflight, TraceRecord, Waiting};
use crate::state::HarnessState;

/// Why the loop is digesting files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DigestPurpose {
    /// The item whose body just ended is being recorded.
    Complete,
    /// The completed items are being checked against what they recorded.
    Revalidate,
}

/// A digest call in flight, kept in the loop's state so that it can be asked
/// again after an interruption and shown to whoever must approve it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DigestOp {
    /// Why the digests are wanted.
    pub purpose: DigestPurpose,
    /// The items asked about, by position.
    pub items: Vec<usize>,
    /// The tool's arguments.
    pub args: Value,
}

/// The `covers` of a `for_each` step, empty for any other.
pub(crate) fn covers_of(step: &PlanStep) -> &[Template] {
    match &step.primitive {
        PlanPrimitive::Iterate {
            iteration: PlanIteration::ForEach { covers, .. },
            ..
        } => covers,
        _ => &[],
    }
}

impl HarnessMachine {
    /// Asks for the digests of the files the items at `items` cover.
    pub(crate) fn request_digest(
        &mut self,
        run: &mut HarnessRunState,
        step: &PlanStep,
        purpose: DigestPurpose,
        items: Vec<usize>,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        run.counts.tool_calls += 1;
        if run.exceeds_tool_budget(self.run.plan()) {
            return self.fail_now(
                run,
                "tool_call_budget",
                "the run exceeded its tool-call budget",
                ctx,
            );
        }
        let covers = covers_of(step).to_vec();
        let mut groups = Map::new();
        let Some(state) = run.iterations.get(&step.id) else {
            return self.fail_now(run, "internal", "iteration state vanished", ctx);
        };
        let wanted: Vec<(String, Value)> = items
            .iter()
            .filter_map(|&i| state.items.get(i).map(|item| (i, item)))
            .map(|(i, item)| (item.id.clone(), item_facts(i, item)))
            .collect();
        for (id, facts) in wanted {
            let saved = swap_item_facts(run, &facts);
            let paths: Vec<Value> = covers
                .iter()
                .map(|template| match run.resolve_template(template) {
                    Value::String(text) => json!(text),
                    other => json!(other.to_string()),
                })
                .collect();
            restore_item_facts(run, saved);
            groups.insert(id, Value::Array(paths));
        }
        let args = json!({ "groups": groups });
        let call_id = self.call_id_for(step);
        let effect = Effect::CallTool {
            call_id,
            name: DIGEST_TOOL.to_string(),
            capability: ToolCapability::ReadFile,
            args: args.clone(),
        };
        if let Some(state) = run.iterations.get_mut(&step.id) {
            state.digesting = Some(DigestOp {
                purpose,
                items,
                args,
            });
        }
        self.cache_effect(effect.clone());
        self.park(run, step, Waiting::Tool);
        run.inflight = Some(Inflight {
            step: step.id.clone(),
            call_id: Some(call_id),
        });
        run.store(ctx);
        Reaction::Transition {
            target: HarnessState::WaitingForTool,
            effects: vec![effect],
            rationale: format!("loop `{}` digests the files its items cover", step.id),
        }
    }

    /// After a resume has settled the list of items: checks the completed
    /// ones against the files they covered, or goes on.
    pub(crate) fn iterate_revalidate(
        &mut self,
        run: &mut HarnessRunState,
        step: &PlanStep,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        let completed: Vec<usize> = run
            .iterations
            .get(&step.id)
            .map(|state| {
                state
                    .items
                    .iter()
                    .enumerate()
                    .filter(|(_, i)| {
                        i.status == ItemStatus::Completed && i.covered_digest.is_some()
                    })
                    .map(|(index, _)| index)
                    .collect()
            })
            .unwrap_or_default();
        if completed.is_empty() {
            return self.iterate_continue(run, step, ctx);
        }
        self.request_digest(run, step, DigestPurpose::Revalidate, completed, ctx)
    }

    /// The digest tool answered, or failed.
    pub(crate) fn digest_settled(
        &mut self,
        run: &mut HarnessRunState,
        step: &PlanStep,
        report: Result<&Value, &str>,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        run.awaiting = None;
        run.inflight = None;
        let Some(op) = run
            .iterations
            .get(&step.id)
            .and_then(|state| state.digesting.clone())
        else {
            return self.fail_now(run, "internal", "digest result without a request", ctx);
        };
        // A call whose answer never arrived proved nothing: ask again.
        if report == Err(INTERRUPTED_OPERATION) {
            run.counts.tool_calls = run.counts.tool_calls.saturating_sub(1);
            return self.request_digest(run, step, op.purpose, op.items, ctx);
        }
        let answer: Option<Value> = match report {
            Ok(Value::String(text)) => serde_json::from_str(text).ok(),
            Ok(other) => Some(other.clone()),
            Err(error) => {
                return self.fail_now(
                    run,
                    "iterate_digest_failed",
                    &format!("cannot digest the files of loop `{}`: {error}", step.id),
                    ctx,
                );
            }
        };
        let Some(answer) = answer.filter(Value::is_object) else {
            return self.fail_now(
                run,
                "iterate_digest_failed",
                &format!(
                    "the digest tool's answer for loop `{}` is not an object",
                    step.id
                ),
                ctx,
            );
        };
        let Some(state) = run.iterations.get_mut(&step.id) else {
            return self.fail_now(run, "internal", "iteration state vanished", ctx);
        };
        state.digesting = None;
        match op.purpose {
            DigestPurpose::Complete => {
                for &index in &op.items {
                    let digest = state
                        .items
                        .get(index)
                        .and_then(|item| answer.get(&item.id))
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    if let Some(item) = state.items.get_mut(index) {
                        item.covered_digest = digest;
                    }
                }
                self.iterate_next(run, step, ctx)
            }
            DigestPurpose::Revalidate => {
                let mut changed = Vec::new();
                for &index in &op.items {
                    let Some(item) = state.items.get_mut(index) else {
                        continue;
                    };
                    let now = answer.get(&item.id).and_then(Value::as_str);
                    if now != item.covered_digest.as_deref() {
                        item.status = ItemStatus::Pending;
                        item.covered_digest = None;
                        item.verified_under = None;
                        changed.push(item.id.clone());
                    }
                }
                if !changed.is_empty() {
                    state.next = 0;
                    run.trace.push(TraceRecord::ItemsInvalidated {
                        step: step.id.clone(),
                        items: changed,
                    });
                }
                self.iterate_continue(run, step, ctx)
            }
        }
    }
}

/// The run facts an item's templates read.
fn item_facts(index: usize, item: &crate::iteration::WorkItem) -> Value {
    json!({ "item": item.value, "item_id": item.id, "item_index": index })
}

/// Puts an item's facts in place for resolving its templates; returns what
/// they replaced.
fn swap_item_facts(run: &mut HarnessRunState, facts: &Value) -> Vec<(String, Option<Value>)> {
    ["item", "item_id", "item_index"]
        .iter()
        .map(|key| {
            let previous = run.facts.get(*key).cloned();
            if let Some(value) = facts.get(*key) {
                run.facts.insert((*key).to_string(), value.clone());
            }
            ((*key).to_string(), previous)
        })
        .collect()
}

fn restore_item_facts(run: &mut HarnessRunState, saved: Vec<(String, Option<Value>)>) {
    for (key, value) in saved {
        match value {
            Some(value) => run.facts.insert(key, value),
            None => run.facts.remove(&key),
        };
    }
}
