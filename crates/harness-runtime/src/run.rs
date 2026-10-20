// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The harness run's durable data plane.
//!
//! Everything a run knows lives in one JSON fact under
//! [`RUN_STATE_FACT`]: the current step id, every step's stored result,
//! the harness's own facts, the run inputs, and the small bookkeeping
//! records (what I/O is in flight, the active retry frame, the agent
//! loop's pending calls, the limit counters). Keeping it all in the
//! [`Context`](sven_hsm::Context) is what makes a run snapshot- and
//! restore-able like any other kernel machine — the machine struct itself
//! holds no run state.
//!
//! Path resolution follows the harness language's grammar: `steps.<id>.value`
//! is a step's stored result, `steps.<id>.<more>` indexes into it,
//! `facts.<name>` and `inputs.<name>` read the run's own namespaces.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sven_harness_spec::{Expr, HarnessBundle, HarnessPlan, Helper, Path, Template};
use sven_hsm::{Context, MachineId, ToolCallId};

use crate::approval::{HeldCall, HeldGate};
use uuid::Uuid;

use crate::error::HarnessRunError;

/// The context fact under which a harness run's whole state is serialized.
pub const RUN_STATE_FACT: &str = "harness.run";

/// The context fact under which a run's terminal outcome is reported: a
/// `return`'s resolved value, a `fail`'s code and message, or the cancelled
/// marker. The machine writes what it also keeps for its own `outcome()`
/// access into the context at the same dispatch, so a parent harvesting a
/// child's report — a caller that only ever holds the context — reads the
/// same value.
pub const OUTCOME_FACT: &str = "harness.outcome";

/// Derives the machine id of a child run a parent spawns.
///
/// Deterministic per (parent plan, step, branch, emission ordinal): a
/// restore or an event replay re-derives the same id, so a recorded child
/// completion still correlates after reconstruction, and repeated
/// delegation of one step still names its children apart. The ordinal is
/// the parent's `counts.children` counter at the emission — a pure function
/// of the run state a replay reconstructs.
#[must_use]
pub fn child_machine_id(plan: &str, step: &str, branch: Option<&str>, ordinal: u64) -> MachineId {
    let name = format!(
        "harness/child/{plan}/{step}/{}/{ordinal}",
        branch.unwrap_or("-")
    );
    MachineId::from_uuid(Uuid::new_v5(&Uuid::NAMESPACE_URL, name.as_bytes()))
}

/// Derives the tool call id of a harness step's call.
///
/// Deterministic per (harness, label): event-sourced replay and checkpoint
/// restore reproduce the same id, so a recorded `ToolSucceeded` still
/// correlates after a resume. A plain step's label is its step id; an attempt
/// of a retry body appends `-<n>` (`probe-1`, `probe-2`, ... — 1-based, the
/// first attempt included), which is what makes each attempt its own
/// correlatable call.
#[must_use]
pub fn step_call_id(harness: &str, label: &str) -> ToolCallId {
    let name = format!("harness/{harness}/{label}");
    ToolCallId::from_uuid(Uuid::new_v5(&Uuid::NAMESPACE_URL, name.as_bytes()))
}

/// A compiled harness plan plus the inputs the run was started with.
///
/// Inputs beyond the plan's declared set are kept (a harness may read
/// undeclared inputs opportunistically); declared-but-missing inputs resolve
/// to nothing, which templates render visibly rather than inventing values
/// for.
#[derive(Debug, Clone)]
pub struct HarnessRunData {
    plan: HarnessPlan,
    inputs: Value,
    /// The bundle's helpers, by name. A run built from a bare document has
    /// none; its `transform` steps fail with `helper_missing` when reached.
    helpers: BTreeMap<String, Helper>,
    /// The bundle's digest, when the run was built from one: a checkpoint
    /// pins it, and a resume that offers a different bundle is refused —
    /// a changed helper is a changed program.
    bundle_digest: Option<String>,
    /// Where this run sits in a delegation chain (a top-level run is 1).
    delegate_depth: u64,
}

impl HarnessRunData {
    /// Parses, validates and compiles `yaml`, storing `inputs` as the run's
    /// input namespace.
    ///
    /// # Errors
    ///
    /// Every parse, validation and compile failure of the harness language —
    /// a harness must fail offline, before anything runs.
    pub fn new(yaml: &str, inputs: Value) -> Result<Self, HarnessRunError> {
        Ok(Self {
            plan: sven_harness_spec::compile_str(yaml)?,
            inputs,
            helpers: BTreeMap::new(),
            bundle_digest: None,
            delegate_depth: 1,
        })
    }

    /// The compiled plan this run executes.
    #[must_use]
    pub fn plan(&self) -> &HarnessPlan {
        &self.plan
    }

    /// The run's input namespace.
    #[must_use]
    pub fn inputs(&self) -> &Value {
        &self.inputs
    }

    /// Builds a run from a compiled plan and the run's inputs — how a
    /// child spawner starts a registered harness without re-parsing it.
    #[must_use]
    pub fn from_parts(plan: HarnessPlan, inputs: Value) -> Self {
        Self {
            plan,
            inputs,
            helpers: BTreeMap::new(),
            bundle_digest: None,
            delegate_depth: 1,
        }
    }

    /// Builds a run from a bundle: the document is compiled against the
    /// helper set (a `transform` step naming a helper the bundle does not
    /// carry, or a helper no step references, is refused here, offline),
    /// and the helpers travel with the run.
    ///
    /// # Errors
    ///
    /// Every parse, validation and compile failure of the harness language,
    /// plus the bundle's own used↔declared helper checks.
    pub fn from_bundle(bundle: &HarnessBundle, inputs: Value) -> Result<Self, HarnessRunError> {
        let compiled = bundle.compile()?;
        Ok(Self {
            plan: compiled.plan,
            inputs,
            helpers: bundle.helpers.clone(),
            bundle_digest: Some(compiled.digest),
            delegate_depth: 1,
        })
    }

    /// Attaches helpers to a run built from parts — how a child spawner
    /// hands a registered harness's helpers to its child.
    #[must_use]
    pub fn with_helpers(mut self, helpers: BTreeMap<String, Helper>) -> Self {
        self.helpers = helpers;
        self
    }

    /// Starts the run at `depth` of its delegation chain: how a child
    /// spawner tells a delegated child how many ancestors it has, so the
    /// plan's `max_delegate_depth` bounds the whole chain.
    #[must_use]
    pub fn with_delegate_depth(mut self, depth: u64) -> Self {
        self.delegate_depth = depth;
        self
    }

    /// This run's depth in its delegation chain (a top-level run is 1).
    #[must_use]
    pub fn delegate_depth(&self) -> u64 {
        self.delegate_depth
    }

    /// The helper named `name`, if the run carries one.
    #[must_use]
    pub fn helper(&self, name: &str) -> Option<&Helper> {
        self.helpers.get(name)
    }

    /// The bundle this run was built from, as its digest; `None` for a run
    /// built from a bare document.
    #[must_use]
    pub fn bundle_digest(&self) -> Option<&str> {
        self.bundle_digest.as_deref()
    }

    /// Pins the run to a bundle digest — how a bundle resume attaches the
    /// pin its checkpoint carries.
    #[must_use]
    pub fn pinned_to_bundle(mut self, digest: String) -> Self {
        self.bundle_digest = Some(digest);
        self
    }

    /// Re-enters this run's plan at `step`: how a parallel fan-out starts
    /// a child at its branch.
    ///
    /// # Errors
    ///
    /// `step` is not one of the plan's steps.
    pub fn with_entry(mut self, step: &str) -> Result<Self, HarnessRunError> {
        if !self.plan.steps.contains_key(step) {
            return Err(HarnessRunError::UnknownEntry(step.to_string()));
        }
        self.plan.entry = step.to_string();
        Ok(self)
    }
}

/// One executed step's stored result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StepRecord {
    /// The value the step stored. Every side-effecting step stores under
    /// `value`; templates address it as `steps.<id>.value`.
    pub value: Value,
}

/// What a waiting state is waiting on. Derived when the side effect is
/// emitted; guards the completion events (a late or duplicate result for a
/// call nobody is waiting on is ignored) and the tick rule (a run with I/O
/// in flight does not advance its pure chain).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Waiting {
    /// A model turn (`predict` or `agent`).
    Model,
    /// One tool call's result.
    Tool,
    /// A verifier verdict.
    Verifier,
    /// A delegated child run.
    Child,
    /// A parallel step's branches.
    Join,
}

/// The agent step's in-flight model↔tool loop.
///
/// Mirrors the machines' `LoopState` in miniature: a harness agent step has
/// a fixed tool allowlist, no all-tools fallback, and no schema demand —
/// the plan's agent primitive is exactly this loop and nothing else.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentLoop {
    /// The agent step's id.
    pub step: String,
    /// The thread the loop runs in (`harness.<plan name>`).
    pub thread: String,
    /// The step's tool allowlist.
    pub tools: Vec<String>,
    /// Calls proposed by the last turn, not yet settled.
    pub pending: Vec<ToolCallId>,
    /// Model turns spent in this loop so far.
    pub rounds: u64,
    /// `true` when the step declared no tool list: the session's agent-mode
    /// tools are open to it and `tools` is empty. `false` when `tools` is
    /// an explicit allowlist, which is also enforced on proposed calls.
    #[serde(default)]
    pub open: bool,
    /// Proposed calls refused before they ran (call id to reason), drained
    /// into the next turn so the model reads why they did not run.
    #[serde(default)]
    pub refused: BTreeMap<String, String>,
    /// The calls the last turn proposed and the step admitted (call id to
    /// call): what a call the permission gate holds for approval is run as
    /// once it is approved.
    #[serde(default)]
    pub calls: BTreeMap<String, HeldCall>,
    /// The model role that answers the loop's turns.
    #[serde(default)]
    pub role: Option<String>,
    /// Where the loop records how it ended.
    #[serde(default)]
    pub stop_as: Option<String>,
    /// The fact holding the calls the step may still make.
    #[serde(default)]
    pub pool: Option<String>,
    /// The repeated answers that end the loop.
    #[serde(default)]
    pub max_repeats: Option<u32>,
    /// How many times each (call, answer) pair has been seen this loop.
    #[serde(default)]
    pub repeats: BTreeMap<String, u32>,
    /// Why the loop ends once the calls in flight settle (`tool_cap`,
    /// `repetition`).
    #[serde(default)]
    pub ending: Option<String>,
    /// The decoding settings of the step's turns.
    #[serde(default)]
    pub decoding: sven_vocab::TurnDecoding,
}

/// The active retry frame: which retry step is in play, its body, and how
/// many attempts are left. One frame at a time — a run's control flow is a
/// linear walk, so at most one retry can be in play; entering a different
/// retry step replaces the frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetryFrame {
    /// The retry step's id.
    pub step: String,
    /// The body step's id.
    pub body: String,
    /// Total attempts the step declared.
    pub attempts: u64,
    /// Attempts left (starts at `attempts`, decremented on every loop-back).
    pub left: u64,
}

/// The delegated child run currently in flight: the machine id the
/// `InstantiateSubmachine` effect carried (the correlation key for its
/// `SubmachineCompleted`) and the delegate step it belongs to. One child at
/// a time — the walk is linear, and `parallel` joins keep their own record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChildRun {
    /// The child's machine instance id, as the effect emitted it.
    pub machine: String,
    /// The delegate step that started it.
    pub step: String,
}

/// A parallel step's open fan-out: one child per branch (in declared
/// order) and the results collected so far. The join settles only when
/// every slot is filled, and the aggregate is always the declared order —
/// whatever order the children complete in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParallelJoin {
    /// The parallel step whose fan-out is in flight.
    pub step: String,
    /// (branch step id, machine uuid) per branch, in declared order.
    pub machines: Vec<(String, String)>,
    /// Each branch's result; `None` until the matching child completes.
    pub results: Vec<Option<Value>>,
}

impl RetryFrame {
    /// The 1-based number of the attempt now running.
    #[must_use]
    pub fn attempt(&self) -> u64 {
        self.attempts - self.left + 1
    }
}

/// The side effect currently in flight, matched against completion events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Inflight {
    /// The step that emitted it.
    pub step: String,
    /// The call id of the emitted `CallTool`, when the effect was one.
    pub call_id: Option<ToolCallId>,
}

/// Declared-limit counters, incremented as the run spends them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    /// Steps processed (every primitive execution).
    pub steps: u64,
    /// Model turns emitted.
    pub model_turns: u64,
    /// Tool calls emitted.
    pub tool_calls: u64,
    /// Child runs spawned (delegation and fan-out branches).
    #[serde(default)]
    pub children: u64,
}

/// One structured record of what a run did: the execution trace a
/// checkpoint carries and an inspector reads. Records belong to the run
/// that wrote them — its own run state — and name cross-run operations
/// (child machine ids, tool call ids) explicitly, so nested children stay
/// associated with the parent step that started them, and failures point at
/// the step and operation they came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum TraceRecord {
    /// A step began executing; the attempt it is on, 1-based (the body of
    /// a `retry` re-enters for a second attempt, everything else is 1).
    StepStarted {
        /// The step that began.
        step: String,
        /// The step's primitive, in words (`predict`, `tool`, ...).
        primitive: String,
        /// The attempt this execution is, 1-based.
        attempt: u64,
    },
    /// A step's result was stored; the id of the tool call that produced
    /// it, when the step called a tool.
    StepCompleted {
        /// The step that completed.
        step: String,
        /// The operation's call id, when the step called a tool.
        #[serde(skip_serializing_if = "Option::is_none")]
        call_id: Option<ToolCallId>,
    },
    /// A `branch` step chose its successor.
    BranchSelected {
        /// The branch step.
        step: String,
        /// The condition's result.
        condition: bool,
        /// The step it selected.
        selected: String,
    },
    /// A `verify` step settled its verdict.
    Verified {
        /// The verifier's kind, in words (`file_exists`, ...).
        verifier: String,
        /// Whether the verdict passed.
        ok: bool,
    },
    /// An `iterate` step froze its collection.
    IterationStarted {
        /// The iterate step.
        step: String,
        /// How many items the collection holds.
        total: u64,
    },
    /// An `iterate` step started the body for an item (or a pass of an
    /// `until`, named by its number).
    ItemStarted {
        /// The iterate step.
        step: String,
        /// The item's id.
        item: String,
        /// The item's index in the frozen collection, from 0.
        index: u64,
        /// How many times this item's body has been started, from 1.
        attempt: u64,
    },
    /// An item's body finished with every check passed.
    ItemCompleted {
        /// The iterate step.
        step: String,
        /// The item's id.
        item: String,
        /// The item's index in the frozen collection, from 0.
        index: u64,
    },
    /// A resumed `for_each` with `reconcile: refresh` read its collection
    /// again and merged it by item id.
    ItemsReconciled {
        /// The iterate step.
        step: String,
        /// Items that stayed as they were.
        kept: u64,
        /// Completed items whose content changed and are pending again.
        invalidated: u64,
        /// Items the refreshed collection added.
        added: u64,
        /// Items the refreshed collection no longer holds.
        removed: u64,
    },
    /// A `gate` step judged its command's report.
    GateEvaluated {
        /// The gate step.
        step: String,
        /// Whether the gate passed.
        passed: bool,
        /// `passed`, `failed` (non-zero exit), `timeout` or `exec_error`.
        kind: String,
        /// The command's exit status, when it had one.
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i64>,
    },
    /// A retry frame looped back; the attempt it is about to make.
    RetryAttempt {
        /// The body step re-entered.
        step: String,
        /// The 1-based attempt about to run.
        attempt: u64,
        /// Attempts left after this one.
        #[serde(default)]
        remaining: u64,
    },
    /// A delegate step started a child run.
    ChildStarted {
        /// The delegate step that started it.
        step: String,
        /// The child's machine id.
        child: String,
    },
    /// A child run of a delegate or parallel step completed.
    ChildCompleted {
        /// The parent step whose child completed.
        step: String,
        /// The child's machine id.
        child: String,
        /// Whether the child reported success.
        ok: bool,
    },
    /// A parallel step fanned out, in declared order.
    FanOut {
        /// The parallel step.
        step: String,
        /// The branch steps, in declared order.
        branches: Vec<String>,
    },
    /// A parallel step's join settled.
    Joined {
        /// The parallel step.
        step: String,
        /// How many results the join settled.
        results: usize,
    },
    /// The run failed.
    Failed {
        /// The step that was in flight.
        step: String,
        /// The failure code.
        code: String,
        /// The failure message.
        message: String,
        /// The in-flight operation's id (a tool call's or a child run's),
        /// when the failure belongs to one.
        #[serde(skip_serializing_if = "Option::is_none")]
        operation: Option<String>,
    },
    /// The run reached its terminal outcome.
    Outcome {
        /// The resolved outcome value.
        outcome: Value,
    },
}

/// The whole run state, serialized under [`RUN_STATE_FACT`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HarnessRunState {
    /// The step the walk is at (or just completed, while its side effect is
    /// in flight).
    pub current: String,
    /// Results stored so far, by step id.
    pub steps: BTreeMap<String, StepRecord>,
    /// The harness's own `facts` namespace.
    pub facts: BTreeMap<String, Value>,
    /// The run inputs (a copy of what the run was started with).
    pub inputs: Value,
    /// What I/O is in flight, if any.
    pub awaiting: Option<Waiting>,
    /// The in-flight side effect's identity.
    pub inflight: Option<Inflight>,
    /// The active agent loop, when an agent step's loop is running.
    pub agent: Option<AgentLoop>,
    /// The active retry frame, when a retry step is in play.
    pub retry: Option<RetryFrame>,
    /// Set when a resume interrupted work in flight: the next item boundary of
    /// a `reconcile: refresh` loop reads its collection again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reconcile_pending: bool,
    /// The progress of each `iterate` step that has begun, by step id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub iterations: BTreeMap<String, crate::iteration::IterationState>,
    /// The delegated child in flight, when a delegate step is waiting.
    pub child: Option<ChildRun>,
    /// The parallel step's open fan-out, while the join waits.
    pub join: Option<ParallelJoin>,
    /// This run's delegation depth (a top-level run is at 1; a delegated
    /// child starts at its parent's depth + 1).
    pub delegate_depth: u64,
    /// The compiled plan's content digest, recorded when the run starts.
    /// A checkpoint carries it, and resuming refuses a document whose
    /// digest no longer matches — an edited harness cannot silently replace
    /// the version a run was checkpointed under.
    pub plan_digest: Option<String>,
    /// The bundle's content digest, when the run was built from one. A
    /// checkpoint pins it alongside the plan digest: a helper's bytes are
    /// part of the program's meaning, so a resume that offers a different
    /// bundle is refused even when the plan itself is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_digest: Option<String>,
    /// Calls the permission gate holds for a person, oldest first: the
    /// first is being asked about, the rest wait their turn.
    #[serde(default)]
    pub approvals: Vec<HeldGate>,
    /// The structured execution trace: what the run did, in order.
    pub trace: Vec<TraceRecord>,
    /// Limit counters.
    pub counts: Counts,
}

impl HarnessRunState {
    /// Loads the run state from a context; a fresh state when none is there
    /// yet.
    #[must_use]
    pub fn load(ctx: &Context) -> Self {
        ctx.fact(RUN_STATE_FACT)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Persists the run state into the context.
    pub fn store(&self, ctx: &mut Context) {
        if let Ok(v) = serde_json::to_value(self) {
            ctx.set_fact(RUN_STATE_FACT, v);
        }
    }

    /// Seeds a fresh run: clears all bookkeeping, stores the inputs, and
    /// points the walk at the plan's entry step.
    pub fn start(&mut self, plan: &HarnessPlan, inputs: &Value, bundle_digest: Option<&str>) {
        *self = Self {
            current: plan.entry.clone(),
            inputs: inputs.clone(),
            plan_digest: Some(plan.digest.clone()),
            bundle_digest: bundle_digest.map(str::to_string),
            // A top-level run is the whole delegation chain's depth 1;
            // delegated children reseed with their inherited depth.
            delegate_depth: 1,
            ..Self::default()
        };
    }

    /// Stores a step's result under `steps.<id>.value` and mirrors it into
    /// `facts.<store_as>` when the step declared a `store_as`.
    pub fn store_step_value(&mut self, step: &str, value: Value, store_as: Option<&str>) {
        self.steps.insert(
            step.to_string(),
            StepRecord {
                value: value.clone(),
            },
        );
        if let Some(name) = store_as {
            self.facts.insert(name.to_string(), value);
        }
    }

    /// Resolves a path against the run's namespaces.
    #[must_use]
    pub fn resolve_path(&self, path: &Path) -> Option<Value> {
        let segments = path.segments();
        let (head, rest) = (segments.first()?, &segments[1..]);
        match head.as_str() {
            // `steps.<id>` is the step's stored value, and so is the
            // canonical `steps.<id>.value`; `steps.<id>.<more>` indexes the
            // stored value (object keys, array indices).
            "steps" => {
                let id = rest.first()?;
                let value = &self.steps.get(id)?.value;
                match rest.get(1).map(String::as_str) {
                    None | Some("value") => descend(value, &rest[2.min(rest.len())..]),
                    _ => descend(value, &rest[1..]),
                }
            }
            // `facts`/`inputs` descend their JSON values.
            "facts" => descend(self.facts.get(rest.first()?)?, &rest[1..]),
            "inputs" => {
                if rest.is_empty() {
                    Some(self.inputs.clone())
                } else {
                    descend(self.inputs.get(rest.first()?)?, &rest[1..])
                }
            }
            // `env` resolves to nothing in v1 — a harness has no
            // environment namespace yet, and an absent value renders
            // visibly in any template rather than fabricating one.
            _ => None,
        }
    }

    /// The resolver every template and expression in this run evaluates
    /// against.
    pub fn resolver(&self) -> impl Fn(&Path) -> Option<Value> + '_ {
        move |path| self.resolve_path(path)
    }

    /// Renders a template: a single-hole template carries the referenced
    /// value's type through; any other renders to a string, with an
    /// unresolvable hole rendered visibly rather than silently emptied.
    #[must_use]
    pub fn resolve_template(&self, template: &Template) -> Value {
        if let Some(path) = template.single_hole() {
            return self.resolve_path(path).unwrap_or(Value::Null);
        }
        Value::String(template.render(&self.resolver()))
    }

    /// Resolves every template string inside a plan-carried JSON value
    /// (tool args, assign sets, delegate inputs, return values).
    #[must_use]
    pub fn resolve_value(&self, value: &Value) -> Value {
        match value {
            Value::String(text) => match Template::parse(text) {
                Ok(template) => self.resolve_template(&template),
                Err(_) => value.clone(),
            },
            Value::Array(items) => {
                Value::Array(items.iter().map(|v| self.resolve_value(v)).collect())
            }
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), self.resolve_value(v)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    /// Evaluates a branch condition against the run's namespaces.
    #[must_use]
    pub fn eval_expr(&self, expr: &Expr) -> bool {
        expr.eval(&self.resolver())
    }
}

/// Indexes `value` by the remaining path segments (object keys, array
/// indices).
fn descend(value: &Value, segments: &[String]) -> Option<Value> {
    let mut current = value.clone();
    for segment in segments {
        current = index_into(&current, segment)?;
    }
    Some(current)
}

fn index_into(value: &Value, segment: &str) -> Option<Value> {
    match value {
        Value::Object(map) => map.get(segment).cloned(),
        Value::Array(items) => segment
            .parse::<usize>()
            .ok()
            .and_then(|i| items.get(i).cloned()),
        _ => None,
    }
}

/// The step budget a harness without a declared `max_steps` runs under.
///
/// Validation already requires limits on every harness whose steps can run
/// away; this only bounds a pure chain against a compiler/retry-accounting
/// bug, where an honest run can never get near it.
const FALLBACK_STEP_BUDGET: u64 = 4096;

impl HarnessRunState {
    /// The effective step budget: the declared `max_steps` or the fallback
    /// runaway guard.
    #[must_use]
    pub fn step_budget(&self, plan: &HarnessPlan) -> u64 {
        plan.limits
            .as_ref()
            .and_then(|l| l.max_steps)
            .unwrap_or(FALLBACK_STEP_BUDGET)
    }

    /// The declared model-turn budget, if any.
    #[must_use]
    pub fn model_turn_budget(&self, plan: &HarnessPlan) -> Option<u64> {
        plan.limits.as_ref().and_then(|l| l.max_model_turns)
    }

    /// The declared tool-call budget, if any.
    #[must_use]
    pub fn tool_call_budget(&self, plan: &HarnessPlan) -> Option<u64> {
        let limits = plan.limits.as_ref()?;
        // A limit the run was started with replaces the document's number.
        limits
            .max_tool_calls_input
            .as_deref()
            .and_then(|name| self.inputs.get(name))
            .and_then(Value::as_u64)
            .or(limits.max_tool_calls)
    }

    /// `true` when the run has spent more model turns than its budget
    /// allows (a run without a declared budget is unbounded).
    #[must_use]
    pub fn exceeds_model_budget(&self, plan: &HarnessPlan) -> bool {
        self.model_turn_budget(plan)
            .is_some_and(|budget| self.counts.model_turns > budget)
    }

    /// `true` when the run has spent more tool calls than its budget
    /// allows (a run without a declared budget is unbounded).
    #[must_use]
    pub fn exceeds_tool_budget(&self, plan: &HarnessPlan) -> bool {
        self.tool_call_budget(plan)
            .is_some_and(|budget| self.counts.tool_calls > budget)
    }
}
