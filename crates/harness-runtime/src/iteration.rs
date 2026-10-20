// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `iterate` step: repetition with durable per-item progress.
//!
//! An `iterate` step is visited again every time its body finishes. With
//! `for_each` the first visit reads the collection once, from a run input or
//! through the harness-only snapshot tool, and **freezes** it in the run
//! state: items, their identities and their content digests. Nothing the
//! workspace holds afterwards can add, drop or reorder an item, so an agent
//! that edits the backlog file mid-run cannot skip work. Each later visit marks
//! the item whose body just finished as completed (its checks passed, or the
//! run would have failed before reaching the loop step) and starts the next
//! one. Because the state lives in the run state, a checkpoint carries it and a
//! resumed run continues at the item it was on without repeating a completed
//! one. With `until` the visits are only counted against the bound.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sven_harness_spec::{IterateSource, PlanIteration, PlanPrimitive, PlanStep, SNAPSHOT_TOOL};
use sven_hsm::{Context, Effect, Reaction, ToolCapability};

use crate::command_gate::INTERRUPTED_OPERATION;
use crate::machine::HarnessMachine;
use crate::run::{HarnessRunState, Inflight, TraceRecord, Waiting};
use crate::state::HarnessState;

/// Where an item is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    /// Not started.
    Pending,
    /// Its body is running (or was, when the run was suspended).
    Active,
    /// Its body finished with every check passed.
    Completed,
}

/// One frozen item of a `for_each`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkItem {
    /// The item's identity, from the collection's `id` field.
    pub id: String,
    /// The item as it was when the collection was frozen.
    pub value: Value,
    /// SHA-256 of the item's canonical JSON.
    pub input_digest: String,
    /// Where the item is.
    pub status: ItemStatus,
    /// How many times its body has been started.
    pub attempts: u32,
    /// For a completed item, the digest of the plan whose checks it passed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_under: Option<String>,
}

/// An `iterate` step's progress, in the run state under the step's id.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IterationState {
    /// The frozen items of a `for_each`; empty for an `until`.
    #[serde(default)]
    pub items: Vec<WorkItem>,
    /// Index of the next item to start.
    #[serde(default)]
    pub next: usize,
    /// Index of the item whose body is running.
    #[serde(default)]
    pub active: Option<usize>,
    /// For an `until`, how many times the body has been entered.
    #[serde(default)]
    pub entries: u64,
    /// SHA-256 of the collection as it was read, for a file source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_digest: Option<String>,
}

impl IterationState {
    fn completed(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.status == ItemStatus::Completed)
            .count()
    }

    fn summary(&self) -> Value {
        json!({
            "total": self.items.len(),
            "completed": self.completed(),
            "pending": self.items.len() - self.completed(),
        })
    }
}

/// SHA-256 of a value's canonical JSON (object keys are sorted).
fn digest_of(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    format!("{:x}", Sha256::digest(bytes))
}

/// The text an item is named by: a string or a number.
fn id_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// Freezes `collection` into items.
fn freeze(
    collection: &Value,
    id_field: Option<&str>,
    max_items: u64,
) -> Result<Vec<WorkItem>, (&'static str, String)> {
    let Value::Array(elements) = collection else {
        return Err((
            "iterate_source_not_array",
            "the collection a for_each reads is not an array".to_string(),
        ));
    };
    if elements.len() as u64 > max_items {
        return Err((
            "iterate_too_many_items",
            format!(
                "the collection has {} items, more than max_items ({max_items})",
                elements.len()
            ),
        ));
    }
    let mut seen = BTreeSet::new();
    let mut items = Vec::with_capacity(elements.len());
    for (index, element) in elements.iter().enumerate() {
        let named = match id_field {
            Some(field) => element.get(field).and_then(id_text),
            None => id_text(element),
        };
        let Some(id) = named else {
            return Err((
                "iterate_missing_id",
                format!(
                    "item {index} has no usable id ({})",
                    id_field.map_or_else(
                        || "items must be strings or numbers".to_string(),
                        |f| format!("field `{f}` must be a non-empty string or a number")
                    )
                ),
            ));
        };
        if !seen.insert(id.clone()) {
            return Err((
                "iterate_duplicate_id",
                format!("item {index} repeats the id `{id}`"),
            ));
        }
        items.push(WorkItem {
            id,
            value: element.clone(),
            input_digest: digest_of(element),
            status: ItemStatus::Pending,
            attempts: 0,
            verified_under: None,
        });
    }
    Ok(items)
}

impl HarnessMachine {
    /// `true` when `step` is an `iterate` step.
    pub(super) fn is_iterate_step(&self, step: &str) -> bool {
        matches!(
            self.plan().step(step).map(|s| &s.primitive),
            Some(PlanPrimitive::Iterate { .. })
        )
    }

    /// Visits an `iterate` step.
    pub(super) fn run_iterate(
        &mut self,
        run: &mut HarnessRunState,
        step: &PlanStep,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        let PlanPrimitive::Iterate {
            iteration,
            body,
            on_exhausted,
            ..
        } = &step.primitive
        else {
            return self.fail_now(
                run,
                "internal",
                "iterate dispatch reached another step",
                ctx,
            );
        };
        match iteration {
            PlanIteration::Until {
                max_iterations,
                max_iterations_input,
            } => {
                // The run may lower the bound, never raise it.
                let bound = max_iterations_input
                    .as_deref()
                    .and_then(|name| run.inputs.get(name))
                    .and_then(Value::as_u64)
                    .map_or(*max_iterations, |n| n.min(*max_iterations));
                let state = run.iterations.entry(step.id.clone()).or_default();
                if state.entries >= bound {
                    let target = on_exhausted.clone();
                    return self.walk_to(run, &target, ctx);
                }
                state.entries += 1;
                let pass = state.entries;
                run.facts.insert("iteration".to_string(), json!(pass));
                run.trace.push(TraceRecord::ItemStarted {
                    step: step.id.clone(),
                    item: pass.to_string(),
                    index: pass - 1,
                    attempt: 1,
                });
                let target = body.clone();
                self.walk_to(run, &target, ctx)
            }
            PlanIteration::ForEach { source, .. } => {
                if run.iterations.contains_key(&step.id) {
                    return self.iterate_next(run, step, ctx);
                }
                match source {
                    IterateSource::Input(name) => {
                        let collection = run.inputs.get(name).cloned().unwrap_or(Value::Null);
                        self.iterate_freeze(run, step, &collection, None, ctx)
                    }
                    IterateSource::Json(json) => self.request_snapshot(run, step, json, ctx),
                }
            }
        }
    }

    fn request_snapshot(
        &mut self,
        run: &mut HarnessRunState,
        step: &PlanStep,
        source: &sven_harness_spec::JsonSource,
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
        let call_id = self.call_id_for(step, run);
        let effect = Effect::CallTool {
            call_id,
            name: SNAPSHOT_TOOL.to_string(),
            capability: ToolCapability::ReadFile,
            args: snapshot_args(source),
        };
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
            rationale: format!("iterate step `{}` reads its collection", step.id),
        }
    }

    /// The snapshot tool answered, or failed.
    pub(super) fn snapshot_settled(
        &mut self,
        run: &mut HarnessRunState,
        step_id: &str,
        report: Result<&Value, &str>,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        let Some(step) = self.run.plan().step(step_id).cloned() else {
            return self.fail_now(run, "unknown_step", "iterate step vanished", ctx);
        };
        run.awaiting = None;
        run.inflight = None;
        // A read whose answer never arrived is simply read again.
        if report == Err(INTERRUPTED_OPERATION) {
            run.counts.tool_calls = run.counts.tool_calls.saturating_sub(1);
            if let PlanPrimitive::Iterate {
                iteration:
                    PlanIteration::ForEach {
                        source: IterateSource::Json(json),
                        ..
                    },
                ..
            } = &step.primitive
            {
                return self.request_snapshot(run, &step, json, ctx);
            }
        }
        let parsed = match report {
            Ok(Value::String(text)) => serde_json::from_str::<Value>(text).ok(),
            Ok(other) => Some(other.clone()),
            Err(error) => {
                return self.fail_now(
                    run,
                    "iterate_source_unreadable",
                    &format!("cannot read the collection of loop `{step_id}`: {error}"),
                    ctx,
                );
            }
        };
        let Some((collection, digest)) = parsed
            .as_ref()
            .and_then(|v| Some((v.get("value")?, v.get("digest")?.as_str()?.to_string())))
        else {
            return self.fail_now(
                run,
                "iterate_source_malformed",
                &format!(
                    "the snapshot tool's answer for loop `{step_id}` is not {{digest, value}}"
                ),
                ctx,
            );
        };
        self.iterate_freeze(run, &step, collection, Some(digest), ctx)
    }

    fn iterate_freeze(
        &mut self,
        run: &mut HarnessRunState,
        step: &PlanStep,
        collection: &Value,
        source_digest: Option<String>,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        let PlanPrimitive::Iterate {
            iteration: PlanIteration::ForEach { id, max_items, .. },
            ..
        } = &step.primitive
        else {
            return self.fail_now(run, "internal", "freeze reached a non-for_each step", ctx);
        };
        let items = match freeze(collection, id.as_deref(), *max_items) {
            Ok(items) => items,
            Err((code, message)) => {
                return self.fail_now(run, code, &format!("loop `{}`: {message}", step.id), ctx)
            }
        };
        run.trace.push(TraceRecord::IterationStarted {
            step: step.id.clone(),
            total: items.len() as u64,
        });
        run.iterations.insert(
            step.id.clone(),
            IterationState {
                items,
                source_digest,
                ..IterationState::default()
            },
        );
        self.iterate_next(run, step, ctx)
    }

    /// Completes the item that was running, then starts the next or leaves
    /// the loop.
    fn iterate_next(
        &mut self,
        run: &mut HarnessRunState,
        step: &PlanStep,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        let PlanPrimitive::Iterate {
            body,
            then,
            store_as,
            ..
        } = &step.primitive
        else {
            return self.fail_now(
                run,
                "internal",
                "iterate dispatch reached another step",
                ctx,
            );
        };
        let digest = self.run.plan().digest.clone();
        let Some(state) = run.iterations.get_mut(&step.id) else {
            return self.fail_now(run, "internal", "iteration state vanished", ctx);
        };
        if let Some(index) = state.active.take() {
            let item = &mut state.items[index];
            item.status = ItemStatus::Completed;
            item.verified_under = Some(digest);
            run.trace.push(TraceRecord::ItemCompleted {
                step: step.id.clone(),
                item: item.id.clone(),
                index: index as u64,
            });
        }
        while state
            .items
            .get(state.next)
            .is_some_and(|i| i.status == ItemStatus::Completed)
        {
            state.next += 1;
        }
        let started = state.items.get(state.next).is_some().then(|| {
            let index = state.next;
            state.next += 1;
            state.active = Some(index);
            let item = &mut state.items[index];
            item.status = ItemStatus::Active;
            item.attempts += 1;
            (index, item.id.clone(), item.value.clone(), item.attempts)
        });
        let summary = state.summary();
        if let Some(name) = store_as {
            run.facts.insert(name.clone(), summary);
        }
        match started {
            Some((index, id, value, attempt)) => {
                run.facts.insert("item".to_string(), value);
                run.facts.insert("item_id".to_string(), json!(id));
                run.facts.insert("item_index".to_string(), json!(index));
                run.trace.push(TraceRecord::ItemStarted {
                    step: step.id.clone(),
                    item: id,
                    index: index as u64,
                    attempt: u64::from(attempt),
                });
                let target = body.clone();
                self.walk_to(run, &target, ctx)
            }
            None => {
                let target = then.clone();
                self.walk_to(run, &target, ctx)
            }
        }
    }
}

/// The snapshot tool's arguments for a file source.
pub(super) fn snapshot_args(source: &sven_harness_spec::JsonSource) -> Value {
    json!({ "path": source.file, "pointer": source.pointer })
}
