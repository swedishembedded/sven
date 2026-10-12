// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The typed AST of a harness document.
//!
//! [`HarnessSpec`] is what a YAML document deserializes into before any
//! validation runs. Every field the compiler needs is typed; every primitive
//! is a variant of [`StepSpec`]; nothing is left as an opaque map for the
//! interpreter to re-parse. Typed specifications exist for all twelve
//! primitives from day one (ADR 0004) even where the runtime adapters land
//! in later releases — the language is defined whole, and rejected early,
//! rather than growing silently.

use serde::{Deserialize, Serialize};

/// The only harness `api_version` this compiler accepts. Bumping it is a
/// breaking change to the language, handled the way `sven_vocab::verify`
/// handles forward compatibility: explicit, documented, and never silently
/// reinterpreted.
pub const SUPPORTED_API_VERSION: &str = "1";

/// A step identifier: the map key of a step. Unique by construction (map
/// keys), non-empty, and referenced by name from other steps.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct StepId(pub String);

impl StepId {
    /// The identifier's text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for StepId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A capability name a harness declares it needs. The names are the
/// `ToolCapability` buckets of the kernel's permission model, carried here
/// as data so this crate stays dependency-free; the runtime crate maps them
/// onto the real enum and rejects unknown names there.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct CapabilityName(pub String);

impl CapabilityName {
    /// All capability names the language knows.
    #[must_use]
    pub fn known() -> &'static [&'static str] {
        &[
            "ReadFile",
            "WriteFile",
            "DeleteFile",
            "ExecuteShell",
            "NetworkAccess",
            "GitOperation",
            "RunVerifier",
            "ControlDevice",
            "SpawnChild",
        ]
    }

    /// The name's text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for CapabilityName {
    type Error = String;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        if Self::known().contains(&text.as_str()) {
            Ok(Self(text))
        } else {
            Err(format!(
                "unknown capability `{text}` (known: {})",
                Self::known().join(", ")
            ))
        }
    }
}

impl std::fmt::Display for CapabilityName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The type of an input or output value. Serialized as its lowercase name
/// (`string`, `number`, `boolean`, `json`) — the form harness documents
/// write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IOType {
    /// A UTF-8 string.
    String,
    /// A JSON number.
    Number,
    /// A JSON boolean.
    Boolean,
    /// Any JSON value (object, array, scalar).
    Json,
}

impl IOType {
    /// `true` when `value` fits this type.
    #[must_use]
    pub fn matches(&self, value: &serde_json::Value) -> bool {
        match self {
            Self::String => value.is_string(),
            Self::Number => value.is_number(),
            Self::Boolean => value.is_boolean(),
            Self::Json => true,
        }
    }
}

/// A declared input or output: a name and its type.
///
/// Accepts both the shorthand form (`result: string`) and the explicit form
/// (`result: {type: string, description: ...}`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputValue {
    /// The value's type.
    pub ty: IOType,
    /// Human documentation for the field; carried into the compiled plan.
    pub description: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum InputValueRepr {
    /// `result: string`
    Shorthand(IOType),
    /// `result: {type: string, ...}`
    Full {
        #[serde(rename = "type")]
        ty: IOType,
        #[serde(default)]
        description: Option<String>,
    },
}

impl<'de> serde::Deserialize<'de> for InputValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let repr = InputValueRepr::deserialize(deserializer)?;
        Ok(Self::from(repr))
    }
}

impl From<InputValueRepr> for InputValue {
    fn from(repr: InputValueRepr) -> Self {
        match repr {
            InputValueRepr::Shorthand(ty) => Self {
                ty,
                description: None,
            },
            InputValueRepr::Full { ty, description } => Self { ty, description },
        }
    }
}

impl Serialize for InputValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.description.is_none() {
            return self.ty.serialize(serializer);
        }
        let mut map = serde_json::Map::new();
        map.insert(
            "type".into(),
            serde_json::to_value(&self.ty).map_err(serde::ser::Error::custom)?,
        );
        map.insert(
            "description".into(),
            serde_json::Value::String(self.description.clone().unwrap_or_default()),
        );
        serde_json::Value::Object(map).serialize(serializer)
    }
}

/// The typed harness specification a YAML document deserializes into.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarnessSpec {
    /// The language version. Only [`SUPPORTED_API_VERSION`] compiles.
    #[serde(rename = "api_version")]
    pub api_version: String,
    /// The harness's name, carried into the plan for tracing.
    pub name: String,
    /// Descriptions of the values the harness takes at start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Named JSON Schemas the document declares, by name. A `predict`
    /// step's `schema` names one of these; the schema travels in the plan,
    /// so the run constrains its model turns without any external source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schemas: Option<std::collections::BTreeMap<String, serde_json::Value>>,
    /// Typed inputs, by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<std::collections::BTreeMap<String, InputValue>>,
    /// Typed outputs, by name. A `return` step's value must match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outputs: Option<std::collections::BTreeMap<String, InputValue>>,
    /// Capabilities the harness needs. A primitive that exercises a
    /// capability the harness does not declare fails to compile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<CapabilityName>>,
    /// Run limits. Required when any step can run away (model, tool,
    /// verifier, retry, parallel, delegate primitives).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<LimitsSpec>,
    /// The entry step: where execution starts.
    pub entry: String,
    /// The steps, by id.
    pub steps: std::collections::BTreeMap<String, StepSpec>,
}

/// Run limits. Every field is an upper bound the runtime enforces; the
/// compiler requires the block whenever the step set can run away.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitsSpec {
    /// The most steps one run may advance through (internal events
    /// included). Bounds every cycle that pure primitives could spin in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_steps: Option<u64>,
    /// The most model turns one run may make.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_model_turns: Option<u64>,
    /// The most tool calls one run may make.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_calls: Option<u64>,
    /// The deepest delegation chain (parent harness = 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_delegate_depth: Option<u64>,
    /// The most concurrent branches one `parallel` step may start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_parallel: Option<u64>,
}

/// One step of a harness: exactly one primitive.
///
/// The primitive's `then` names the next step; terminal primitives
/// (`return`, `fail`) have no successor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    rename_all = "snake_case",
    try_from = "serde_json::Value",
    into = "serde_json::Value"
)]
pub enum StepSpec {
    /// One constrained model turn: the model answers a JSON-schema-shaped
    /// question (when a schema is named) and the answer is stored.
    Predict(Box<PredictStepSpec>),
    /// A model turn with a tool loop: the model may call tools until it
    /// answers.
    Agent(Box<AgentStepSpec>),
    /// One tool call.
    Tool(Box<ToolStepSpec>),
    /// One declarative verification of run artifacts.
    Verify(Box<VerifyStepSpec>),
    /// Store a value into run facts, then move on.
    Assign(Box<AssignStepSpec>),
    /// Choose between two successors on a condition.
    Branch(Box<BranchStepSpec>),
    /// Run a body step with a bounded attempt count.
    Retry(Box<RetryStepSpec>),
    /// Run several child steps concurrently and join.
    Parallel(Box<ParallelStepSpec>),
    /// Start a child run: another harness or a registered machine.
    Delegate(Box<DelegateStepSpec>),
    /// Run a sandboxed transform over a value.
    Transform(Box<TransformStepSpec>),
    /// Finish the harness successfully with a value.
    Return(Box<ReturnStepSpec>),
    /// Finish the harness with an explicit failure.
    Fail(Box<FailStepSpec>),
}

/// `predict`: one constrained model turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PredictStepSpec {
    /// What to ask. A template over run data.
    pub prompt: String,
    /// Optional name of a JSON Schema declared in the document's top-level
    /// `schemas` map. The named schema travels in the plan and constrains
    /// the turn's response format; the answer must parse as JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Where to store the answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_as: Option<String>,
    /// The next step.
    pub then: String,
}

/// `agent`: a model turn with a tool loop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentStepSpec {
    /// The instruction for the agent.
    pub prompt: String,
    /// The tools the agent may call in this step. Empty/omitted = every tool
    /// the harness's capabilities admit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    /// Where to store the agent's final text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_as: Option<String>,
    /// The next step.
    pub then: String,
}

/// `tool`: one tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolStepSpec {
    /// The tool's registered name.
    pub name: String,
    /// The call's arguments. Values may be templates over run data.
    #[serde(default)]
    pub args: serde_json::Value,
    /// Where to store the tool's result value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_as: Option<String>,
    /// The next step.
    pub then: String,
}

/// `verify`: one declarative verification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifyStepSpec {
    /// The verifier spec, as typed data (the same vocabulary the kernel's
    /// `Effect::Verify` carries).
    pub spec: serde_json::Value,
    /// Where to store the verdict (`passed` / `failed` / `needs_human` /
    /// `unknown`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_as: Option<String>,
    /// The next step.
    pub then: String,
}

/// `assign`: store values into run facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssignStepSpec {
    /// Values to store, by fact name. Values may be templates or literals.
    #[serde(rename = "set")]
    pub set: std::collections::BTreeMap<String, serde_json::Value>,
    /// The next step.
    pub then: String,
}

/// `branch`: choose between two successors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BranchStepSpec {
    /// The condition. A path string, `!path`, or an object form.
    pub on: serde_json::Value,
    /// The step to go to when the condition holds.
    #[serde(rename = "when_true")]
    pub when_true: String,
    /// The step to go to when it does not.
    #[serde(rename = "when_false")]
    pub when_false: String,
}

/// `retry`: run a body with bounded attempts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetryStepSpec {
    /// The most attempts (>= 1). This is what makes the cycle bounded.
    pub attempts: u64,
    /// The step to (re)run.
    pub body: String,
    /// The step to go to after the body succeeds.
    pub then: String,
    /// The step to go to when every attempt is spent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_exhausted: Option<String>,
}

/// `parallel`: run children concurrently and join.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParallelStepSpec {
    /// The steps to run concurrently. Each is entered once; the step waits
    /// at a join until every branch reaches a terminal or rejoins.
    pub branches: Vec<String>,
    /// The step to go to once every branch has finished.
    pub join: String,
}

/// `delegate`: start a child run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DelegateStepSpec {
    /// What to run: a harness name in the same bundle, or a registered
    /// machine mode.
    pub target: String,
    /// Whether `target` names a harness in the bundle or a machine mode.
    /// Defaults to `harness`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_kind: Option<DelegateTarget>,
    /// The child's input value (templates allowed).
    #[serde(default)]
    pub input: serde_json::Value,
    /// Where to store the child's result value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_as: Option<String>,
    /// The next step.
    pub then: String,
}

/// What a `delegate` target names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DelegateTarget {
    /// Another harness in the same bundle.
    #[default]
    Harness,
    /// A registered native machine mode.
    Machine,
}

/// `transform`: run a sandboxed helper over a value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransformStepSpec {
    /// The transform helper's name in the bundle.
    pub helper: String,
    /// The value to transform. A template or literal.
    #[serde(default)]
    pub input: serde_json::Value,
    /// Where to store the transformed value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_as: Option<String>,
    /// The next step.
    pub then: String,
}

/// `return`: finish successfully.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReturnStepSpec {
    /// The harness's output value. Must satisfy the declared `outputs`
    /// contract.
    #[serde(default)]
    pub value: serde_json::Value,
}

/// `fail`: finish with a failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailStepSpec {
    /// A machine-readable failure code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// A human-readable message (template allowed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// The primitive key for each variant, and the conversion between a
/// single-key YAML map and [`StepSpec`]. serde_yaml's externally-tagged
/// enum deserialization demands YAML tags (`!return`), which no harness
/// document should have to write; routing through a `serde_json::Value`
/// (untagged) gives the natural `return: {...}` form.
impl StepSpec {
    /// The YAML key each primitive is written under.
    #[must_use]
    pub fn key(&self) -> &'static str {
        match self {
            Self::Predict(_) => "predict",
            Self::Agent(_) => "agent",
            Self::Tool(_) => "tool",
            Self::Verify(_) => "verify",
            Self::Assign(_) => "assign",
            Self::Branch(_) => "branch",
            Self::Retry(_) => "retry",
            Self::Parallel(_) => "parallel",
            Self::Delegate(_) => "delegate",
            Self::Transform(_) => "transform",
            Self::Return(_) => "return",
            Self::Fail(_) => "fail",
        }
    }
}

impl TryFrom<serde_json::Value> for StepSpec {
    type Error = String;

    fn try_from(value: serde_json::Value) -> Result<Self, Self::Error> {
        let serde_json::Value::Object(map) = value else {
            return Err(format!(
                "a step must be a map with one primitive key, got {value}"
            ));
        };
        if map.len() != 1 {
            return Err(format!(
                "a step must hold exactly one primitive key, got {} keys: {}",
                map.len(),
                map.keys().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        let (key, body) = map.into_iter().next().expect("len checked == 1");
        let text =
            serde_json::to_string(&body).map_err(|e| format!("unreadable step body: {e}"))?;
        match key.as_str() {
            "predict" => serde_json::from_str(&text)
                .map(|p| Self::Predict(Box::new(p)))
                .map_err(|e| format!("bad `predict` step: {e}")),
            "agent" => serde_json::from_str(&text)
                .map(|a| Self::Agent(Box::new(a)))
                .map_err(|e| format!("bad `agent` step: {e}")),
            "tool" => serde_json::from_str(&text)
                .map(|t| Self::Tool(Box::new(t)))
                .map_err(|e| format!("bad `tool` step: {e}")),
            "verify" => serde_json::from_str(&text)
                .map(|v| Self::Verify(Box::new(v)))
                .map_err(|e| format!("bad `verify` step: {e}")),
            "assign" => serde_json::from_str(&text)
                .map(|a| Self::Assign(Box::new(a)))
                .map_err(|e| format!("bad `assign` step: {e}")),
            "branch" => serde_json::from_str(&text)
                .map(|b| Self::Branch(Box::new(b)))
                .map_err(|e| format!("bad `branch` step: {e}")),
            "retry" => serde_json::from_str(&text)
                .map(|r| Self::Retry(Box::new(r)))
                .map_err(|e| format!("bad `retry` step: {e}")),
            "parallel" => serde_json::from_str(&text)
                .map(|p| Self::Parallel(Box::new(p)))
                .map_err(|e| format!("bad `parallel` step: {e}")),
            "delegate" => serde_json::from_str(&text)
                .map(|d| Self::Delegate(Box::new(d)))
                .map_err(|e| format!("bad `delegate` step: {e}")),
            "transform" => serde_json::from_str(&text)
                .map(|t| Self::Transform(Box::new(t)))
                .map_err(|e| format!("bad `transform` step: {e}")),
            "return" => serde_json::from_str(&text)
                .map(|r| Self::Return(Box::new(r)))
                .map_err(|e| format!("bad `return` step: {e}")),
            "fail" => serde_json::from_str(&text)
                .map(|f| Self::Fail(Box::new(f)))
                .map_err(|e| format!("bad `fail` step: {e}")),
            other => Err(format!(
                "unknown step primitive `{other}` (known: predict, agent, tool, verify, \
                 assign, branch, retry, parallel, delegate, transform, return, fail)"
            )),
        }
    }
}

impl From<StepSpec> for serde_json::Value {
    fn from(step: StepSpec) -> Self {
        let (key, body): (&'static str, serde_json::Value) = match &step {
            StepSpec::Predict(p) => ("predict", serde_json::to_value(p).unwrap_or_default()),
            StepSpec::Agent(a) => ("agent", serde_json::to_value(a).unwrap_or_default()),
            StepSpec::Tool(t) => ("tool", serde_json::to_value(t).unwrap_or_default()),
            StepSpec::Verify(v) => ("verify", serde_json::to_value(v).unwrap_or_default()),
            StepSpec::Assign(a) => ("assign", serde_json::to_value(a).unwrap_or_default()),
            StepSpec::Branch(b) => ("branch", serde_json::to_value(b).unwrap_or_default()),
            StepSpec::Retry(r) => ("retry", serde_json::to_value(r).unwrap_or_default()),
            StepSpec::Parallel(p) => ("parallel", serde_json::to_value(p).unwrap_or_default()),
            StepSpec::Delegate(d) => ("delegate", serde_json::to_value(d).unwrap_or_default()),
            StepSpec::Transform(t) => ("transform", serde_json::to_value(t).unwrap_or_default()),
            StepSpec::Return(r) => ("return", serde_json::to_value(r).unwrap_or_default()),
            StepSpec::Fail(f) => ("fail", serde_json::to_value(f).unwrap_or_default()),
        };
        serde_json::json!({ key: body })
    }
}
