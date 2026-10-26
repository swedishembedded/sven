// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The step specification: what lowering a workflow produces, and what the
//! plan compiler works on.
//!
//! [`HarnessSpec`] is a map of steps, each one of the closed set of
//! primitives of [`StepSpec`] (`predict`, `agent`, `verify`, `assign`,
//! `branch`, `delegate`, `gate`, `iterate`, `return`, `fail`). It is not a
//! document format: a harness document is a workflow
//! ([`crate::workflow`]), and nothing but the lowering writes this. Every
//! field the compiler needs is typed; nothing is left as an opaque map for
//! the interpreter to re-parse.

use serde::{Deserialize, Serialize};

use crate::source_map::Lowering;

pub use crate::decoding::StepDecoding;
pub use crate::iterate::{
    ForEachSpec, IterateSource, IterateStepSpec, JsonSource, Reconcile, UntilSpec,
};

/// The only harness `api_version` this compiler accepts: the declarative
/// workflow language (ADR 0005). Bumping it is a breaking change to the
/// language, explicit, documented and never silently reinterpreted. (Version 1,
/// the step-map language, was removed; ADR 0006.)
pub const API_VERSION: &str = "2";

pub use crate::names::{
    DIGEST_TOOL, GATE_TOOL, ITEM_FAILED_FACT, MAX_COVERS, RESERVED_TOOLS, SNAPSHOT_TOOL,
};

/// How many items a `for_each` takes when its definition names no bound.
pub const DEFAULT_MAX_ITEMS: u64 = 500;

/// The most items a `for_each` may be bounded to.
pub const MAX_ITEMS_LIMIT: u64 = 10_000;

/// The most iterations an `until` may be bounded to.
pub const MAX_ITERATIONS_LIMIT: u64 = 1_000;

/// The longest a command gate may run, in seconds.
pub const MAX_GATE_TIMEOUT_SECS: u64 = 3600;

/// How long a command gate may run when its definition gives no timeout.
pub const DEFAULT_GATE_TIMEOUT_SECS: u64 = 600;

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
#[serde(untagged, deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct HarnessSpec {
    /// The language version. Only [`API_VERSION`] compiles.
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
    /// Run variables, by name.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub variables: std::collections::BTreeMap<String, crate::vars::VariableDecl>,
    /// What lowering a workflow recorded about this spec. Never
    /// read from or written to a document.
    #[serde(skip)]
    pub lowering: Option<Lowering>,
}

/// Run limits. Every field is an upper bound the runtime enforces; the
/// compiler requires the block whenever the step set can run away.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// Names a run input holding the tool-call limit, which then replaces
    /// `max_tool_calls` for the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_calls_input: Option<String>,
    /// The deepest delegation chain (parent harness = 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_delegate_depth: Option<u64>,
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
    /// One declarative verification of run artifacts.
    Verify(Box<VerifyStepSpec>),
    /// Store a value into run facts, then move on.
    Assign(Box<AssignStepSpec>),
    /// Choose between two successors on a condition.
    Branch(Box<BranchStepSpec>),
    /// Start a child run: another harness of the package.
    Delegate(Box<DelegateStepSpec>),
    /// Finish the harness successfully with a value.
    Return(Box<ReturnStepSpec>),
    /// Finish the harness with an explicit failure.
    Fail(Box<FailStepSpec>),
    /// Run a command and let its exit status decide.
    Gate(Box<GateStepSpec>),
    /// Repeat a body over a frozen collection, or until a condition holds,
    /// within a bound.
    Iterate(Box<IterateStepSpec>),
}

/// `predict`: one constrained model turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// The conversation the turn is in (a template, resolved when the step
    /// starts): its own thread, named by this, instead of the run's shared
    /// one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    /// The system message the conversation starts with (a template), when
    /// this turn starts it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Decoding settings for the turn.
    #[serde(default, skip_serializing_if = "StepDecoding::is_empty")]
    pub decoding: StepDecoding,
    /// The model role that answers the step's turns (a name the host maps
    /// to a provider).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// What the prompt is, for the record: `task`, `follow_up`, `repair` or
    /// `correction`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// With a schema: the repair turns an answer that is truncated or does
    /// not parse earns, each in the same conversation. Absent means
    /// [`DEFAULT_ANSWER_REPAIRS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_attempts: Option<u32>,
    /// A number input that lowers `repair_attempts` at run time (the count
    /// read from an input, as `repair.attempts` allows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_limit_input: Option<String>,
    /// Where the run continues when the repairs are spent and the answer is
    /// still unusable: a loop's item-failure path. Absent, the run fails with
    /// `invalid_answer` or `output_truncated`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_unusable_answer: Option<String>,
    /// The next step.
    pub then: String,
}

/// The repair turns a schema'd answer earns when its task says nothing.
pub const DEFAULT_ANSWER_REPAIRS: u32 = 2;

/// `agent`: a model turn with a tool loop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentStepSpec {
    /// The instruction for the agent.
    pub prompt: String,
    /// The tools the agent may call in this step. Omitted = the session's
    /// agent-mode tools, gated by the declared capabilities; an explicit
    /// list is also a name allowlist (a proposed call outside it is
    /// refused before it runs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    /// Where to store the agent's final text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_as: Option<String>,
    /// The conversation the step's turns are in (a template, resolved when
    /// the step starts): its own thread, named by this, instead of the run's
    /// shared one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    /// The variables the step's agent may read or set.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub vars: std::collections::BTreeMap<String, crate::vars::VarAccess>,
    /// The system message the step's conversation starts with (a template),
    /// when this step starts it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Decoding settings for the step's turns.
    #[serde(default, skip_serializing_if = "StepDecoding::is_empty")]
    pub decoding: StepDecoding,
    /// The model role that answers the step's turns (a name the host maps
    /// to a provider).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// What the prompt is, for the record: `task`, `follow_up`, `repair` or
    /// `correction`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Where to record how the step's loop ended (`concluded`,
    /// `stopped_without_conclusion`, `output_truncated`, `tool_cap`, `repetition`, `watchdog`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_as: Option<String>,
    /// The fact holding the tool calls the step may still make.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    /// Sets the pool to this (a template resolving to a whole number) when
    /// the step starts; without it the step draws on what the pool still
    /// holds. A call beyond the pool is refused and ends the loop with the
    /// stop `tool_cap`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowance: Option<String>,
    /// Ends the loop with the stop `repetition` when the same call has been
    /// answered the same way this many times.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_repeated_results: Option<u32>,
    /// The next step.
    pub then: String,
}

/// `verify`: one declarative verification.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct AssignStepSpec {
    /// Values to store, by fact name. Values may be templates or literals.
    #[serde(rename = "set")]
    pub set: std::collections::BTreeMap<String, serde_json::Value>,
    /// The next step.
    pub then: String,
}

/// `branch`: choose between two successors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

/// `delegate`: start a child run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegateStepSpec {
    /// The harness to run: its name in the same package.
    pub target: String,
    /// The child's input value (templates allowed).
    #[serde(default)]
    pub input: serde_json::Value,
    /// Where to store the child's result value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_as: Option<String>,
    /// The next step.
    pub then: String,
}

/// `return`: finish successfully.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReturnStepSpec {
    /// The harness's output value. Must satisfy the declared `outputs`
    /// contract.
    #[serde(default)]
    pub value: serde_json::Value,
}

/// `fail`: finish with a failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailStepSpec {
    /// A machine-readable failure code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// A human-readable message (template allowed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `gate`: run a command; its exit status decides.
///
/// The command is trusted workflow configuration: it is part of the compiled
/// plan and the agent cannot redefine it. It runs without a shell. The
/// run's machine, not the model and not the command's output, decides whether
/// the gate passed: no error, no timeout, exit status 0.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateStepSpec {
    /// The commands, in order; the first to fail ends the gate. The timeout
    /// bounds them together.
    pub steps: Vec<crate::gate_command::GateCommand>,
    /// Variables set for every command: name to a template over run data.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,
    /// The most seconds the command may run.
    #[serde(default = "default_gate_timeout")]
    pub timeout: u64,
    /// The working directory, relative to the workspace root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Names a run input holding an absolute directory outside the workspace
    /// to run in. The host sets inputs; the agent cannot write there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd_input: Option<String>,
    /// How much of the end of each output stream the result keeps, in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<u64>,
    /// Text the command reads on its standard input (a template), for what
    /// is too big for an argument: a model's answer, a diff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    /// Where to store the typed gate result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_as: Option<String>,
    /// The variable a passing gate's standard output is stored in. Version 2
    /// only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<String>,
    /// The step to go to when the gate passed.
    pub then: String,
    /// The step to go to when it did not (non-zero exit, timeout, or the
    /// command could not be run).
    pub on_fail: String,
}

fn default_gate_timeout() -> u64 {
    DEFAULT_GATE_TIMEOUT_SECS
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
            Self::Verify(_) => "verify",
            Self::Assign(_) => "assign",
            Self::Branch(_) => "branch",
            Self::Delegate(_) => "delegate",
            Self::Return(_) => "return",
            Self::Fail(_) => "fail",
            Self::Gate(_) => "gate",
            Self::Iterate(_) => "iterate",
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
            "verify" => serde_json::from_str(&text)
                .map(|v| Self::Verify(Box::new(v)))
                .map_err(|e| format!("bad `verify` step: {e}")),
            "assign" => serde_json::from_str(&text)
                .map(|a| Self::Assign(Box::new(a)))
                .map_err(|e| format!("bad `assign` step: {e}")),
            "branch" => serde_json::from_str(&text)
                .map(|b| Self::Branch(Box::new(b)))
                .map_err(|e| format!("bad `branch` step: {e}")),
            "delegate" => serde_json::from_str(&text)
                .map(|d| Self::Delegate(Box::new(d)))
                .map_err(|e| format!("bad `delegate` step: {e}")),
            "return" => serde_json::from_str(&text)
                .map(|r| Self::Return(Box::new(r)))
                .map_err(|e| format!("bad `return` step: {e}")),
            "fail" => serde_json::from_str(&text)
                .map(|f| Self::Fail(Box::new(f)))
                .map_err(|e| format!("bad `fail` step: {e}")),
            "gate" => serde_json::from_str(&text)
                .map(|g| Self::Gate(Box::new(g)))
                .map_err(|e| format!("bad `gate` step: {e}")),
            "iterate" => serde_json::from_str(&text)
                .map(|i| Self::Iterate(Box::new(i)))
                .map_err(|e| format!("bad `iterate` step: {e}")),
            other => Err(format!("unknown step primitive `{other}`")),
        }
    }
}

impl From<StepSpec> for serde_json::Value {
    fn from(step: StepSpec) -> Self {
        let (key, body): (&'static str, serde_json::Value) = match &step {
            StepSpec::Predict(p) => ("predict", serde_json::to_value(p).unwrap_or_default()),
            StepSpec::Agent(a) => ("agent", serde_json::to_value(a).unwrap_or_default()),
            StepSpec::Verify(v) => ("verify", serde_json::to_value(v).unwrap_or_default()),
            StepSpec::Assign(a) => ("assign", serde_json::to_value(a).unwrap_or_default()),
            StepSpec::Branch(b) => ("branch", serde_json::to_value(b).unwrap_or_default()),
            StepSpec::Delegate(d) => ("delegate", serde_json::to_value(d).unwrap_or_default()),
            StepSpec::Return(r) => ("return", serde_json::to_value(r).unwrap_or_default()),
            StepSpec::Fail(f) => ("fail", serde_json::to_value(f).unwrap_or_default()),
            StepSpec::Gate(g) => ("gate", serde_json::to_value(g).unwrap_or_default()),
            StepSpec::Iterate(i) => ("iterate", serde_json::to_value(i).unwrap_or_default()),
        };
        serde_json::json!({ key: body })
    }
}
