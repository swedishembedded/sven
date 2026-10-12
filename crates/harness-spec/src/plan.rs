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
use crate::spec::{HarnessSpec, StepSpec};

/// The compiled-plan format version, stamped into the digest input so plans
/// from different compiler generations can never collide.
pub const PLAN_FORMAT_VERSION: &str = "harness-plan/1";

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
        let envelope = serde_json::json!({
            "format": PLAN_FORMAT_VERSION,
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
    HarnessPlan::canonical_digest(&value)
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
            then: p.then.clone(),
        },
        StepSpec::Agent(a) => PlanPrimitive::Agent {
            prompt: Template::parse(&a.prompt).unwrap_or_else(|_| Template::empty()),
            tools: a.tools.clone(),
            store_as: a.store_as.clone(),
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
    };
    PlanStep {
        id: id.to_string(),
        primitive,
    }
}
