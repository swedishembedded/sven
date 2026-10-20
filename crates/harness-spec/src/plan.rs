// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The compiled harness plan.
//!
//! [`HarnessPlan`] is what a validated [`HarnessSpec`](crate::HarnessSpec)
//! compiles into: a canonical, deterministic form of the program plus its
//! content digest. The digest is the run-time pin — a resumed run rejects a
//! plan whose digest no longer matches what it was started with — so it is
//! computed over a *canonical serialization* (map keys sorted, whitespace
//! normalized), which makes it invariant to YAML key order and formatting
//! and sensitive to any change in program meaning.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::expr::Template;
use crate::source_map::SourceRef;
use crate::spec::{HarnessSpec, StepDecoding, StepSpec};

/// The compiled-plan format version, stamped into the digest input so plans
/// from different compiler generations can never collide.
pub const PLAN_FORMAT_VERSION: &str = "harness-plan/1";

/// The format stamp of plans compiled from `api_version: "2"` documents.
/// Distinct from [`PLAN_FORMAT_VERSION`] so that a v2 plan's digest can never
/// equal a v1 plan's, and a checkpoint of one is never resumed as the other.
pub const WORKFLOW_PLAN_FORMAT_VERSION: &str = "harness-plan/2";

/// The format stamp for plans compiled from `api_version`.
fn format_for(api_version: &str) -> &'static str {
    if api_version == crate::spec::WORKFLOW_API_VERSION {
        WORKFLOW_PLAN_FORMAT_VERSION
    } else {
        PLAN_FORMAT_VERSION
    }
}

/// A compiled, canonical harness program.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarnessPlan {
    /// The harness's name.
    pub name: String,
    /// The `api_version` the plan was compiled from.
    pub api_version: String,
    /// The entry step id.
    pub entry: String,
    /// Declared inputs, by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<std::collections::BTreeMap<String, crate::spec::InputValue>>,
    /// Declared outputs, by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outputs: Option<std::collections::BTreeMap<String, crate::spec::InputValue>>,
    /// Named JSON Schemas, by name. A `predict` step's `schema` names one;
    /// the runtime sends it as the turn's response format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schemas: Option<std::collections::BTreeMap<String, serde_json::Value>>,
    /// Declared capabilities, sorted, deduplicated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Declared limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<PlanLimits>,
    /// The tool profile a version 2 workflow declared (`none`, `coding`):
    /// what the host must provide for the plan's agent steps. Absent in
    /// version 1 plans.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// For a version 2 plan, where each generated step came from in the
    /// authored document. Empty in version 1 plans.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub source_map: std::collections::BTreeMap<String, SourceRef>,
    /// The steps, in canonical (sorted) order.
    pub steps: std::collections::BTreeMap<String, PlanStep>,
    /// Lowercase hex SHA-256 of the canonical form.
    pub digest: String,
}

/// The plan-level limits, canonicalized (defaults applied).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanLimits {
    /// Most steps per run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_steps: Option<u64>,
    /// Most model turns per run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_model_turns: Option<u64>,
    /// Most tool calls per run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_calls: Option<u64>,
    /// The run input that holds the tool-call limit, when it comes from one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_calls_input: Option<String>,
    /// Deepest delegation chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_delegate_depth: Option<u64>,
    /// Most concurrent branches per `parallel`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_parallel: Option<u64>,
    /// Most interpreter ticks per helper invocation. Unset means the
    /// runtime's default budget applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_helper_fuel: Option<u64>,
    /// Most linear memory per helper instance, in 64 KiB pages. Unset means
    /// the runtime's default budget applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_helper_pages: Option<u32>,
}

/// The compiled form of an `iterate` step's repetition, bounds applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanIteration {
    /// Once per item of a collection frozen on the first visit.
    ForEach {
        /// Where the collection is read from.
        source: crate::spec::IterateSource,
        /// The item field that names an item, if the items are objects.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// The most items taken.
        max_items: u64,
    },
    /// Until another step ends the loop, at most `max_iterations` times.
    Until {
        /// The most body entries.
        max_iterations: u64,
        /// The run input that lowers the bound for a run.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_iterations_input: Option<String>,
    },
}

/// A compiled step: the primitive plus its resolved successor edges.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PlanStep {
    /// The step's id.
    pub id: String,
    /// The primitive, canonicalized.
    pub primitive: PlanPrimitive,
}

/// The compiled form of each primitive — resolved references, canonical
/// templates. Same variants as [`StepSpec`](crate::StepSpec); this is the
/// plan-side twin, so the compiler's output is explicit about what was
/// validated and resolved rather than re-borrowing the spec.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PlanPrimitive {
    /// Compiled `predict`.
    Predict {
        /// The prompt template, parsed.
        prompt: Template,
        /// Optional schema name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        schema: Option<String>,
        /// Where to store the answer.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        store_as: Option<String>,
        /// The conversation the turn is in (version 2 plans only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conversation: Option<String>,
        /// The system message the conversation starts with, parsed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system: Option<Template>,
        /// Decoding settings (version 2 plans only).
        #[serde(default, skip_serializing_if = "StepDecoding::is_empty")]
        decoding: StepDecoding,
        /// The model role that answers the step's turns (version 2 plans only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<String>,
        /// What the prompt is, for the record (version 2 plans only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<String>,
        /// Successor.
        then: String,
    },
    /// Compiled `agent`.
    Agent {
        /// The instruction template, parsed.
        prompt: Template,
        /// Tool allowlist for the step.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tools: Option<Vec<String>>,
        /// Where to store the final text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        store_as: Option<String>,
        /// The system message the shared conversation starts with, parsed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system: Option<Template>,
        /// Decoding settings (version 2 plans only).
        #[serde(default, skip_serializing_if = "StepDecoding::is_empty")]
        decoding: StepDecoding,
        /// The model role that answers the step's turns (version 2 plans only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<String>,
        /// What the prompt is, for the record (version 2 plans only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<String>,
        /// Where the loop's stop status is recorded (version 2 plans only).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop_as: Option<String>,
        /// The fact holding the calls the step may still make.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pool: Option<String>,
        /// Sets the pool when the step starts, parsed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowance: Option<Template>,
        /// The repeated answers that end the loop.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_repeated_results: Option<u32>,
        /// Successor.
        then: String,
    },
    /// Compiled `tool`.
    Tool {
        /// The tool's name.
        name: String,
        /// The arguments (templates inside).
        args: serde_json::Value,
        /// Where to store the result.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        store_as: Option<String>,
        /// Successor.
        then: String,
    },
    /// Compiled `verify`.
    Verify {
        /// The verifier spec as typed JSON.
        spec: serde_json::Value,
        /// Where to store the verdict.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        store_as: Option<String>,
        /// Successor.
        then: String,
    },
    /// Compiled `assign`.
    Assign {
        /// Values to store.
        set: std::collections::BTreeMap<String, serde_json::Value>,
        /// Successor.
        then: String,
    },
    /// Compiled `branch`.
    Branch {
        /// The condition, parsed.
        on: crate::expr::Expr,
        /// Successor when true.
        when_true: String,
        /// Successor when false.
        when_false: String,
    },
    /// Compiled `retry`.
    Retry {
        /// Attempt bound (>= 1, validated).
        attempts: u64,
        /// The body step.
        body: String,
        /// Successor after the body succeeds.
        then: String,
        /// Successor when attempts are spent.
        on_exhausted: String,
    },
    /// Compiled `parallel`.
    Parallel {
        /// The branch steps.
        branches: Vec<String>,
        /// The join step.
        join: String,
    },
    /// Compiled `delegate`.
    Delegate {
        /// The target (harness name or machine mode).
        target: String,
        /// What the target names.
        #[serde(default)]
        as_kind: crate::spec::DelegateTarget,
        /// The child input.
        input: serde_json::Value,
        /// Where to store the result.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        store_as: Option<String>,
        /// Fail the step when the child reports `ok: false`.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        propagate_failure: bool,
        /// Successor.
        then: String,
    },
    /// Compiled `transform`.
    Transform {
        /// The helper's name.
        helper: String,
        /// The input value.
        input: serde_json::Value,
        /// Where to store the result.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        store_as: Option<String>,
        /// Successor.
        then: String,
    },
    /// Compiled `return`.
    Return {
        /// The output value.
        value: serde_json::Value,
    },
    /// Compiled `fail`.
    Fail {
        /// The failure code.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        /// The failure message template, parsed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<Template>,
    },
    /// Compiled `iterate` (plans of `api_version: "2"` only).
    Iterate {
        /// What repeats and how it is bounded.
        iteration: PlanIteration,
        /// First step of the body.
        body: String,
        /// Successor once the loop is finished.
        then: String,
        /// Successor when an `until` loop is exhausted; empty otherwise.
        on_exhausted: String,
        /// Where the summary is stored.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        store_as: Option<String>,
    },
    /// Compiled `gate` (plans of `api_version: "2"` only).
    Gate {
        /// The argv, one template per element.
        command: Vec<Template>,
        /// The most seconds the command may run.
        timeout_secs: u64,
        /// The working directory, relative to the workspace root.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// Where to store the typed gate result.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        store_as: Option<String>,
        /// What the command reads on standard input, parsed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdin: Option<Template>,
        /// Successor when the gate passed.
        then: String,
        /// Successor when it did not.
        on_fail: String,
    },
}

impl HarnessPlan {
    /// The step for `id`, if it exists.
    #[must_use]
    pub fn step(&self, id: &str) -> Option<&PlanStep> {
        self.steps.get(id)
    }

    /// Computes the canonical digest of a plan-shaped value: canonical JSON
    /// (sorted keys — `serde_json` with the `preserve_order` feature off is
    /// a `BTreeMap` under the hood) over a tagged envelope.
    #[must_use]
    pub fn canonical_digest(value: &serde_json::Value) -> String {
        Self::digest_in_format(PLAN_FORMAT_VERSION, value)
    }

    fn digest_in_format(format: &str, value: &serde_json::Value) -> String {
        let envelope = serde_json::json!({
            "format": format,
            "plan": value,
        });
        let mut hasher = Sha256::new();
        // serde_json::to_string on a Value with sorted maps is canonical:
        // no whitespace, keys in sorted order, numbers in serde_json's
        // canonical form.
        hasher.update(serde_json::to_string(&envelope).unwrap_or_default());
        format!("{:x}", hasher.finalize())
    }
}

/// Compiles a validated spec into a plan. Validation has already run; this
/// only canonicalizes and digests.
#[must_use]
pub fn compile(spec: &HarnessSpec) -> HarnessPlan {
    let steps: std::collections::BTreeMap<String, PlanStep> = spec
        .steps
        .iter()
        .map(|(id, step)| (id.clone(), compile_step(id, step)))
        .collect();
    let capabilities: Vec<String> = spec
        .capabilities
        .as_ref()
        .map(|caps| {
            let mut names: Vec<String> = caps.iter().map(|c| c.0.clone()).collect();
            names.sort();
            names.dedup();
            names
        })
        .unwrap_or_default();
    let limits = spec.limits.as_ref().map(|l| PlanLimits {
        max_steps: l.max_steps,
        max_model_turns: l.max_model_turns,
        max_tool_calls: l.max_tool_calls,
        max_tool_calls_input: l.max_tool_calls_input.clone(),
        max_delegate_depth: l.max_delegate_depth,
        max_parallel: l.max_parallel,
        max_helper_fuel: l.max_helper_fuel,
        max_helper_pages: l.max_helper_pages,
    });

    let mut plan = HarnessPlan {
        name: spec.name.clone(),
        api_version: spec.api_version.clone(),
        entry: spec.entry.clone(),
        inputs: spec.inputs.clone(),
        outputs: spec.outputs.clone(),
        schemas: spec.schemas.clone(),
        capabilities,
        limits,
        profile: spec.lowering.as_ref().map(|l| l.profile.clone()),
        source_map: spec
            .lowering
            .as_ref()
            .map(|l| l.source_map.clone())
            .unwrap_or_default(),
        steps,
        // Placeholder, replaced below.
        digest: String::new(),
    };
    plan.digest = canonical_digest_of_plan(&plan);
    plan
}

/// The digest of a plan: over its canonical serialization without the
/// (self-referential) digest field itself.
#[must_use]
pub fn canonical_digest_of_plan(plan: &HarnessPlan) -> String {
    // Serialize the plan with the digest field dropped.
    let mut value = serde_json::to_value(plan).unwrap_or(serde_json::Value::Null);
    if let serde_json::Value::Object(map) = &mut value {
        map.remove("digest");
    }
    HarnessPlan::digest_in_format(format_for(&plan.api_version), &value)
}

/// Compiles one step. Every parse here is guaranteed to succeed: validation
/// has already accepted the document, so a `Template`/`Expr` parse failure
/// would mean the validator and the compiler disagree — the fallbacks keep
/// that from ever becoming a panic, and the plan degrades visibly (an empty
/// template renders as text, an always-false branch) rather than inventing
/// control flow.
fn compile_step(id: &str, step: &StepSpec) -> PlanStep {
    let primitive = match step {
        StepSpec::Predict(p) => PlanPrimitive::Predict {
            prompt: Template::parse(&p.prompt).unwrap_or_else(|_| Template::empty()),
            schema: p.schema.clone(),
            store_as: p.store_as.clone(),
            conversation: p.conversation.clone(),
            system: p.system.as_deref().and_then(|t| Template::parse(t).ok()),
            decoding: p.decoding.clone(),
            origin: p.origin.clone(),
            role: p.role.clone(),
            then: p.then.clone(),
        },
        StepSpec::Agent(a) => PlanPrimitive::Agent {
            prompt: Template::parse(&a.prompt).unwrap_or_else(|_| Template::empty()),
            tools: a.tools.clone(),
            store_as: a.store_as.clone(),
            system: a.system.as_deref().and_then(|t| Template::parse(t).ok()),
            decoding: a.decoding.clone(),
            origin: a.origin.clone(),
            role: a.role.clone(),
            stop_as: a.stop_as.clone(),
            pool: a.pool.clone(),
            allowance: a.allowance.as_deref().and_then(|t| Template::parse(t).ok()),
            max_repeated_results: a.max_repeated_results,
            then: a.then.clone(),
        },
        StepSpec::Tool(t) => PlanPrimitive::Tool {
            name: t.name.clone(),
            args: t.args.clone(),
            store_as: t.store_as.clone(),
            then: t.then.clone(),
        },
        StepSpec::Verify(v) => PlanPrimitive::Verify {
            spec: v.spec.clone(),
            store_as: v.store_as.clone(),
            then: v.then.clone(),
        },
        StepSpec::Assign(a) => PlanPrimitive::Assign {
            set: a.set.clone(),
            then: a.then.clone(),
        },
        StepSpec::Branch(b) => PlanPrimitive::Branch {
            on: crate::expr::Expr::parse_value(&b.on).unwrap_or(crate::expr::Expr::truthy_false()),
            when_true: b.when_true.clone(),
            when_false: b.when_false.clone(),
        },
        StepSpec::Retry(r) => PlanPrimitive::Retry {
            attempts: r.attempts,
            body: r.body.clone(),
            then: r.then.clone(),
            // Validation has ensured this exists.
            on_exhausted: r.on_exhausted.clone().unwrap_or_default(),
        },
        StepSpec::Parallel(p) => PlanPrimitive::Parallel {
            branches: p.branches.clone(),
            join: p.join.clone(),
        },
        StepSpec::Delegate(d) => PlanPrimitive::Delegate {
            target: d.target.clone(),
            as_kind: d.as_kind.unwrap_or_default(),
            input: d.input.clone(),
            store_as: d.store_as.clone(),
            propagate_failure: d.propagate_failure,
            then: d.then.clone(),
        },
        StepSpec::Transform(t) => PlanPrimitive::Transform {
            helper: t.helper.clone(),
            input: t.input.clone(),
            store_as: t.store_as.clone(),
            then: t.then.clone(),
        },
        StepSpec::Return(r) => PlanPrimitive::Return {
            value: r.value.clone(),
        },
        StepSpec::Fail(f) => PlanPrimitive::Fail {
            code: f.code.clone(),
            message: f.message.as_deref().and_then(|m| Template::parse(m).ok()),
        },
        StepSpec::Iterate(i) => PlanPrimitive::Iterate {
            // Validation has ensured exactly one of the two is present.
            iteration: match (&i.for_each, &i.until) {
                (Some(f), _) => PlanIteration::ForEach {
                    source: f.source.clone(),
                    id: f.id.clone(),
                    max_items: f.max_items,
                },
                (_, Some(u)) => PlanIteration::Until {
                    max_iterations: u.max_iterations,
                    max_iterations_input: u.max_iterations_input.clone(),
                },
                _ => PlanIteration::Until {
                    max_iterations: 0,
                    max_iterations_input: None,
                },
            },
            body: i.body.clone(),
            then: i.then.clone(),
            on_exhausted: i.on_exhausted.clone().unwrap_or_default(),
            store_as: i.store_as.clone(),
        },
        StepSpec::Gate(g) => PlanPrimitive::Gate {
            command: g
                .command
                .iter()
                .map(|arg| Template::parse(arg).unwrap_or_else(|_| Template::empty()))
                .collect(),
            timeout_secs: g.timeout,
            cwd: g.cwd.clone(),
            stdin: g.stdin.as_deref().and_then(|t| Template::parse(t).ok()),
            store_as: g.store_as.clone(),
            then: g.then.clone(),
            on_fail: g.on_fail.clone(),
        },
    };
    PlanStep {
        id: id.to_string(),
        primitive,
    }
}
