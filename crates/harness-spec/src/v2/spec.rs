// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The typed AST of a version 2 workflow document.
//!
//! Every object rejects unknown keys. Fields whose shape depends on a key
//! (a gate is a command or an artifact, a check is a gate name, an artifact
//! or a metric) are decoded from raw values with errors that say what the
//! author may write, instead of serde's "did not match any variant".
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use crate::spec::{CapabilityName, IOType, InputValue, IterateSource, Reconcile, StepDecoding};

/// A whole workflow document, as written.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowSpec {
    /// The workflow's name.
    pub name: String,
    /// Human documentation.
    pub description: Option<String>,
    /// Typed per-run inputs.
    pub inputs: Option<BTreeMap<String, InputValue>>,
    /// Typed outputs and the values they take.
    pub outputs: Option<BTreeMap<String, OutputDecl>>,
    /// The agent every task runs as.
    pub agent: AgentSpec,
    /// Limits and execution settings.
    pub execution: ExecutionSpec,
    /// The exact capability grant, when the author states one.
    pub capabilities: Option<Vec<CapabilityName>>,
    /// Named trusted checks.
    pub gates: BTreeMap<String, GateDef>,
    /// The tasks, in order.
    pub workflow: Vec<TaskSpec>,
    /// The run's success condition.
    pub success: SuccessSpec,
    /// What a failed check does to the run.
    pub on_failure: OnFailure,
    /// The documents this one uses, by the alias a `use` task names: the path
    /// as written, relative to this document's directory.
    pub imports: BTreeMap<String, String>,
    /// Run variables, by name: typed values agents read and set through the
    /// harness's variable tools and command gates capture.
    pub variables: BTreeMap<String, VariableSpec>,
    /// What each import offers, resolved by the package before lowering. Never
    /// read from a document.
    pub resolved: BTreeMap<String, Imported>,
}

/// The interface of an imported workflow: what a `use` task may pass and
/// read, and what the child needs in order to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    /// The imported document's name: the harness a `use` task delegates to.
    pub name: String,
    /// The inputs the child takes; a `use` passes exactly these.
    pub inputs: BTreeMap<String, InputValue>,
    /// The outputs the child returns; later templates may read their fields.
    pub outputs: BTreeMap<String, InputValue>,
    /// The capabilities the child holds. The parent must hold them too.
    pub capabilities: Vec<String>,
}

/// What a failed check or an exhausted loop does to the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnFailure {
    /// End the run as failed, with the failure's code and message.
    #[default]
    Fail,
    /// End the run by returning its outputs with `status: failed` and the
    /// failure, so what it produced (and its trajectory) is the result of an
    /// attempt that did not succeed.
    Return,
}

/// One declared output and the value it takes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputDecl {
    /// The output's type.
    #[serde(rename = "type")]
    pub ty: IOType,
    /// The value; strings are templates over inputs and task outputs.
    pub value: Value,
    /// Human documentation.
    #[serde(default)]
    pub description: Option<String>,
}

/// The tool profile the host must provide for agent tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    /// Whatever tools the host gives the session.
    #[default]
    None,
    /// File inspection, editing, search and shell.
    Coding,
}

impl Profile {
    /// The profile's name as written and as carried in the plan.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Coding => "coding",
        }
    }

    /// The capabilities an agent with this profile exercises.
    #[must_use]
    pub fn capabilities(self) -> &'static [&'static str] {
        match self {
            Self::None => &[],
            Self::Coding => &["ReadFile", "WriteFile", "DeleteFile", "ExecuteShell"],
        }
    }
}

/// A setting in a workflow: a literal, or `"{{ inputs.<name> }}"`.
pub type Setting = Option<Value>;

/// `sampling:` of an agent or a task.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sampling {
    /// Softmax temperature, within 0 to 2.
    #[serde(default)]
    pub temperature: Setting,
    /// Nucleus-sampling mass, above 0 and at most 1.
    #[serde(default)]
    pub top_p: Setting,
    /// Number of highest-probability tokens kept, at least 1.
    #[serde(default)]
    pub top_k: Setting,
    /// Seed. Never defaulted: a run is only seeded when the document says so.
    #[serde(default)]
    pub seed: Setting,
}

/// `thinking:` of an agent or a task.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Thinking {
    /// Reasoning on or off, on servers that have the switch.
    #[serde(default)]
    pub enabled: Setting,
    /// The most thinking tokens before a turn is cut off as a runaway.
    #[serde(default)]
    pub budget: Setting,
}

/// `turn:` of an agent or a task: bounds on one model response.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnBounds {
    /// The most output tokens one response may ask for.
    #[serde(default)]
    pub max_output_tokens: Setting,
    /// Seconds to wait for the server's first response.
    #[serde(default)]
    pub first_response_timeout: Setting,
    /// The most wall-clock seconds one turn may take, whole. Default: none.
    #[serde(default)]
    pub deadline: Setting,
}

/// The step-level form of a `sampling`, `thinking` and `turn` block.
#[must_use]
pub fn decoding_of(
    sampling: &Option<Sampling>,
    thinking: &Option<Thinking>,
    bounds: &Option<TurnBounds>,
) -> StepDecoding {
    let (s, t, b) = (
        sampling.clone().unwrap_or_default(),
        thinking.clone().unwrap_or_default(),
        bounds.clone().unwrap_or_default(),
    );
    StepDecoding {
        temperature: s.temperature,
        top_p: s.top_p,
        top_k: s.top_k,
        seed: s.seed,
        thinking: t.enabled,
        thinking_budget: t.budget,
        max_output_tokens: b.max_output_tokens,
        first_response_timeout: b.first_response_timeout,
        deadline: b.deadline,
    }
}

/// `agent.system`: one text, or variants chosen by a condition over inputs.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum AgentSystem {
    /// The system message, a template over inputs.
    Text(String),
    /// Entries tried in order; the last has no `when`.
    Variants(Vec<SystemVariant>),
}

/// One entry of a list-valued `agent.system`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemVariant {
    /// A condition over inputs; absent on the entry used when none holds.
    #[serde(default)]
    pub when: Option<Value>,
    /// The system message, a template over inputs.
    pub text: String,
}

/// The `agent:` block.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSpec {
    /// The tool profile.
    #[serde(default)]
    pub profile: Profile,
    /// Standing instructions prepended to every task prompt.
    #[serde(default)]
    pub instructions: Option<String>,
    /// An explicit tool allowlist for every task.
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    /// The system message of the agent's conversation (a template over
    /// inputs), delivered as the model's system message and recorded in the
    /// trajectory as its system step; or a list of variants of which the
    /// first whose condition holds is chosen when the run starts.
    #[serde(default)]
    pub system: Option<AgentSystem>,
    /// The model role every task is answered by unless it names its own: a
    /// name the host maps to a provider. No endpoint or key ever appears in a
    /// document.
    #[serde(default)]
    pub model: Option<String>,
    /// Sampling for every task; a task's own settings replace it.
    #[serde(default)]
    pub sampling: Option<Sampling>,
    /// Thinking for every task; a task's own settings replace it.
    #[serde(default)]
    pub thinking: Option<Thinking>,
    /// Bounds on each response, for every task.
    #[serde(default)]
    pub turn: Option<TurnBounds>,
}

/// The `execution:` block.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionSpec {
    /// The workspace kind. Only `repository` exists.
    #[serde(default)]
    pub workspace: Option<Workspace>,
    /// Must be true when given: every run is resumable.
    #[serde(default)]
    pub resume: Option<bool>,
    /// Run limits.
    #[serde(default)]
    pub limits: Option<ExecLimits>,
    /// Where the run's private directory lives: an absolute path, a template
    /// over inputs only. Absent, the host chooses.
    #[serde(default)]
    pub scratchpad: Option<String>,
}

/// Where the workflow's commands and files live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Workspace {
    /// The host-provided workspace root.
    Repository,
}

/// `execution.limits`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecLimits {
    /// The most step executions.
    #[serde(default)]
    pub steps: Option<u64>,
    /// The most model turns.
    #[serde(default)]
    pub model_turns: Option<u64>,
    /// The most tool calls: a whole number, or a single `{{ inputs.<name> }}`
    /// the host computes per run.
    #[serde(default)]
    pub tool_calls: Option<Value>,
    /// The deepest delegation chain.
    #[serde(default)]
    pub delegate_depth: Option<u64>,
}

/// A named gate.
#[derive(Debug, Clone, PartialEq)]
pub enum GateDef {
    /// A command whose exit status decides.
    Command(CommandGate),
    /// A file that must exist.
    Artifact(ArtifactCheck),
}

/// A command gate.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandGate {
    /// Human documentation.
    #[serde(default)]
    pub description: Option<String>,
    /// The argv; templates over inputs. Exactly one of `command` and `run`.
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// A script, or named scripts run in order; the first failure stops the
    /// gate. Templates, like an argv.
    #[serde(default)]
    pub run: Option<super::gate_steps::Script>,
    /// The shell a script runs under (default `bash`).
    #[serde(default)]
    pub shell: Option<super::gate_steps::Shell>,
    /// Variables set for the command or script: name to value, a template.
    /// Data should reach a script this way, not by splicing it into the text.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Seconds; the default applies when absent.
    #[serde(default)]
    pub timeout: Option<u64>,
    /// Working directory inside the workspace.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Runs in the directory a run input names instead: a string input the
    /// host sets to an absolute directory outside the workspace, such as a
    /// pinned checkout the agent cannot write. Exclusive with `cwd`.
    #[serde(default)]
    pub cwd_input: Option<String>,
    /// Text the command reads on standard input (a template over inputs and
    /// earlier outputs), for what is too large for an argument.
    #[serde(default)]
    pub stdin: Option<String>,
    /// How much of the end of each output stream the result keeps, in bytes
    /// (default 16384, at most 1048576): a check whose findings are long, a
    /// diff for a supervisor, needs more.
    #[serde(default)]
    pub output_bytes: Option<u64>,
    /// Numbers the command leaves in JSON files, by name.
    #[serde(default)]
    pub metrics: BTreeMap<String, MetricDecl>,
}

/// Where a number the gate's command leaves can be read.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricDecl {
    /// Workspace-relative JSON file.
    pub file: String,
    /// RFC 6901 pointer into it.
    pub pointer: String,
    /// Always `number`.
    #[serde(rename = "type")]
    pub ty: MetricType,
}

/// The only metric type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricType {
    /// A JSON number.
    Number,
}

/// A file that must exist, optionally of a minimum size or exact content.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactCheck {
    /// Human documentation.
    #[serde(default)]
    pub description: Option<String>,
    /// Workspace-relative path.
    pub artifact: String,
    /// Minimum size in bytes.
    #[serde(default)]
    pub min_bytes: Option<u64>,
    /// Exact content, as a SHA-256 in hex.
    #[serde(default)]
    pub sha256: Option<String>,
}

/// A comparison against a number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    /// Equal.
    Eq,
    /// Not equal.
    Ne,
    /// Greater than.
    Gt,
    /// Greater than or equal.
    Gte,
    /// Less than.
    Lt,
    /// Less than or equal.
    Lte,
}

impl Cmp {
    /// The key it is written under, and the operator name the verifier
    /// vocabulary uses for it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::Ne => "ne",
            Self::Gt => "gt",
            Self::Gte => "gte",
            Self::Lt => "lt",
            Self::Lte => "lte",
        }
    }
}

/// A threshold: one comparison and the number it compares against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Threshold {
    /// The comparison.
    pub cmp: Cmp,
    /// The number.
    pub value: f64,
}

/// The six comparison keys, as written; exactly one may be set.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThresholdKeys {
    eq: Option<f64>,
    ne: Option<f64>,
    gt: Option<f64>,
    gte: Option<f64>,
    lt: Option<f64>,
    lte: Option<f64>,
}

impl ThresholdKeys {
    /// The single comparison written, or an error saying what is wrong.
    ///
    /// # Errors
    ///
    /// No comparison, several, or a number that is not finite.
    pub fn threshold(&self) -> Result<Threshold, String> {
        let all = [
            (Cmp::Eq, self.eq),
            (Cmp::Ne, self.ne),
            (Cmp::Gt, self.gt),
            (Cmp::Gte, self.gte),
            (Cmp::Lt, self.lt),
            (Cmp::Lte, self.lte),
        ];
        let mut set = all.iter().filter_map(|(cmp, v)| v.map(|v| (*cmp, v)));
        let (Some((cmp, value)), None) = (set.next(), set.next()) else {
            return Err("write exactly one of eq, ne, gt, gte, lt, lte".to_string());
        };
        if !value.is_finite() {
            return Err("the number must be finite".to_string());
        }
        Ok(Threshold { cmp, value })
    }
}

/// A `metric: gate.name` check with its threshold.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricCheck {
    /// `<gate>.<metric>`.
    pub metric: String,
    /// The comparison the metric must satisfy.
    pub threshold: Threshold,
}

/// One entry of a task's `verify:` list.
#[derive(Debug, Clone, PartialEq)]
pub enum Check {
    /// A named gate that must pass.
    Gate(String),
    /// A named gate whose result is recorded and never repaired or failed on
    /// (`{gate: name, required: false}`): the run goes on whatever it says.
    Soft(String),
    /// An inline file check.
    Artifact(ArtifactCheck),
    /// A metric threshold.
    Metric(MetricCheck),
}

/// One extra user turn of a task.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FollowUp {
    /// A condition over inputs and task outputs; the turn is skipped when
    /// it does not hold.
    #[serde(default)]
    pub when: Option<Value>,
    /// The prompt.
    pub task: String,
    /// How many times the turn is sent, each only while its condition still
    /// holds (the condition is evaluated again after every turn). At least 1
    /// and at most 8; default 1.
    #[serde(default)]
    pub max: Option<Value>,
    /// With `max` read from an input, the most the plan is unrolled to.
    #[serde(default)]
    pub at_most: Option<u64>,
    /// Sets the task's tool-call allowance for this turn (a whole number or a
    /// single `{{ inputs.<name> }}`); without it the turn draws on what the
    /// task's allowance still holds.
    #[serde(default)]
    pub allowance: Option<Value>,
}

/// `repair:`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repair {
    /// How many repair turns a failed check earns: a whole number, or a single
    /// `{{ inputs.<name> }}` (a number input) with `at_most`.
    pub attempts: Value,
    /// With a count read from an input, the most the plan is unrolled to; the
    /// run uses as many as the input says, up to this.
    #[serde(default)]
    pub at_most: Option<u64>,
    /// A prompt template over `failure.*`; a default applies when absent.
    #[serde(default)]
    pub prompt: Option<String>,
}

/// `for_each:` of a task: the body runs once per item of a collection that is
/// frozen when the loop begins.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForEach {
    /// Where the collection comes from.
    pub source: IterateSource,
    /// The item field that names an item (items that are scalars name
    /// themselves). Templates read the item as `{{ item.<field> }}`.
    #[serde(default)]
    pub id: Option<String>,
    /// The most items; a larger collection fails the run.
    #[serde(default)]
    pub max_items: Option<u64>,
    /// What a resumed run does with a collection that changed on disk:
    /// `frozen` (default) or `refresh`, which merges by item id at the next
    /// item boundary and makes a completed item whose content changed pending
    /// again. Only a file source can change.
    #[serde(default)]
    pub reconcile: Reconcile,
    /// Workspace files an item covers (paths, templates over `item`). When
    /// the item completes their content digest is recorded; when a run is
    /// resumed, an item whose files have changed since is pending again.
    #[serde(default)]
    pub covers: Vec<String>,
    /// What happens to an item whose checks fail for good: without it the run
    /// fails at that item; with it the item is recorded `failed`, these
    /// checks run (a rollback, say) and the loop goes on with the next item.
    #[serde(default)]
    pub on_item_failure: Option<ItemFailure>,
}

/// `for_each.on_item_failure`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemFailure {
    /// Gates that run, in order, after an item failed; if one of them fails
    /// the run fails with `rollback_failed`.
    #[serde(default)]
    pub verify: Vec<String>,
}

/// When a task hands the host a durable checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointPolicy {
    /// After every completed item of a `for_each` task.
    EachItem,
}

/// `until:` of a task: the body repeats until a metric holds.
#[derive(Debug, Clone, PartialEq)]
pub struct Until {
    /// `<gate>.<metric>`, measured by a fresh run of the gate's command
    /// after every pass of the body.
    pub metric: String,
    /// What the metric must satisfy.
    pub threshold: Threshold,
    /// The most passes of the body.
    pub max_iterations: u64,
}

/// `corrections:` of a task: a supervisor reads how the task's result was
/// judged and writes the message the agent is sent next.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Corrections {
    /// The most corrections (1 to 16).
    pub max: u64,
    /// Lowers the most corrections for a run: a single `{{ inputs.<name> }}`.
    #[serde(default)]
    pub count: Option<Value>,
    /// The command gate that judges the task's result. It runs before the
    /// first correction and again after every correction, and its latest
    /// result is what `while` reads.
    pub check: String,
    /// The condition that earns a correction, over `gates.<check>.*` and the
    /// task outputs. Default: the check did not pass.
    #[serde(default, rename = "while")]
    pub while_: Option<Value>,
    /// The model that writes each correction.
    pub author: Author,
    /// The command gate that decides whether the author's message may be
    /// sent; it reads the message on standard input.
    pub accept: String,
    /// How many times a refused message is rewritten (0 to 8); when they are
    /// spent no correction is sent and the loop ends. Default 1.
    #[serde(default)]
    pub retries: Option<u64>,
    /// The prompt that asks the author to rewrite a refused message; a
    /// template over `failure.*`.
    #[serde(default)]
    pub refusal: Option<String>,
    /// Sets the task's tool-call allowance for each correction.
    #[serde(default)]
    pub allowance: Option<Value>,
}

/// The model-only author of a correction.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Author {
    /// Names the author's conversation and its output
    /// (`tasks.<name>.output`).
    pub name: String,
    /// The model role that answers the author.
    #[serde(default)]
    pub model: Option<String>,
    /// The system message of its conversation.
    #[serde(default)]
    pub system: Option<String>,
    /// What to ask, with the check's findings in it.
    pub task: String,
    /// Sampling for the author's turns.
    #[serde(default)]
    pub sampling: Option<Sampling>,
    /// Thinking for the author's turns.
    #[serde(default)]
    pub thinking: Option<Thinking>,
    /// Bounds on each response of the author.
    #[serde(default)]
    pub turn: Option<TurnBounds>,
}

/// One task.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskSpec {
    /// The task's identity: stable across reordering and reformatting.
    pub name: String,
    /// Human documentation.
    pub description: Option<String>,
    /// The prompt.
    pub task: String,
    /// This task's tool allowlist.
    pub tools: Option<Vec<String>>,
    /// Run the task only when this holds.
    pub when: Option<Value>,
    /// Extra user turns.
    pub follow_ups: Vec<FollowUp>,
    /// Files the task must produce.
    pub outputs: Vec<String>,
    /// Checks that must pass, in order.
    pub verify: Vec<Check>,
    /// Repair policy.
    pub repair: Option<Repair>,
    /// Run the task once per item of a frozen collection.
    pub for_each: Option<ForEach>,
    /// Repeat the task until a metric holds.
    pub until: Option<Until>,
    /// `each_item` (for a `for_each` task): the host is handed a checkpoint
    /// every time an item completes.
    pub checkpoint: Option<CheckpointPolicy>,
    /// A single model turn with no tools, for a planner or a supervisor, in
    /// a conversation of its own.
    pub model_only: bool,
    /// For a `model_only` task, the system message of its conversation (a
    /// template over inputs and earlier outputs).
    pub system: Option<String>,
    /// The model role that answers this task.
    pub model: Option<String>,
    /// For a `model_only` task, the JSON Schema its answer must satisfy; the
    /// parsed answer is the task's output.
    pub schema: Option<Value>,
    /// Sampling for this task's turns.
    pub sampling: Option<Sampling>,
    /// Thinking for this task's turns.
    pub thinking: Option<Thinking>,
    /// Bounds on each response of this task.
    pub turn: Option<TurnBounds>,
    /// The tool calls the task's first turn may make (a whole number or a
    /// single `{{ inputs.<name> }}`); follow-ups and repairs draw on what is
    /// left. A call beyond it is refused and the turn ends with the stop
    /// `tool_cap`.
    pub allowance: Option<Value>,
    /// Ends a turn with the stop `repetition` when the same call has been
    /// answered the same way this many times.
    pub max_repeated_results: Option<u32>,
    /// A supervisor that writes the messages the agent is sent after its
    /// result was judged.
    pub corrections: Option<Corrections>,
    /// Runs an imported workflow as this task (`use: <alias>`) instead of an
    /// agent turn; its outputs are `tasks.<name>.output.<field>`.
    pub use_import: Option<String>,
    /// The values passed to the imported workflow's inputs.
    pub with: BTreeMap<String, Value>,
    /// The conversation the task's turns are in. Absent, the task has a
    /// conversation of its own (one per item inside a `for_each`, one per
    /// pass inside an `until`); tasks that name the same conversation share
    /// it. A template over inputs and the loop variables.
    pub conversation: Option<String>,
    /// The variables the task may see, and what it may do with each.
    pub vars: BTreeMap<String, VarUse>,
    /// `do:` - the tasks a loop runs for each item or pass, in order, in
    /// place of a single prompt. Each has its own prompt, tools,
    /// conversation, checks and repairs.
    pub body: Vec<TaskSpec>,
}

impl WorkflowSpec {
    /// Every task of the document: each top-level task followed by the tasks
    /// of its `do:` body.
    #[must_use]
    pub fn all_tasks(&self) -> Vec<&TaskSpec> {
        self.workflow
            .iter()
            .flat_map(|t| std::iter::once(t).chain(t.body.iter()))
            .collect()
    }

    /// The tasks that run, in the order they first run: a loop with a `do:`
    /// body stands for the tasks of its body.
    #[must_use]
    pub fn runnable(&self) -> Vec<&TaskSpec> {
        self.workflow
            .iter()
            .flat_map(|t| {
                if t.body.is_empty() {
                    vec![t]
                } else {
                    t.body.iter().collect()
                }
            })
            .collect()
    }

    /// The loop whose `do:` body holds the task named `name`.
    #[must_use]
    pub fn owner_of(&self, name: &str) -> Option<&TaskSpec> {
        self.workflow
            .iter()
            .find(|t| t.body.iter().any(|b| b.name == name))
    }

    /// The task a task's loop belongs to: its owner for a task of a `do:`
    /// body, the task itself otherwise.
    #[must_use]
    pub fn loop_of<'a>(&'a self, task: &'a TaskSpec) -> &'a TaskSpec {
        self.owner_of(&task.name).unwrap_or(task)
    }
}

/// `success:`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuccessSpec {
    /// Gates that must pass after the last task.
    #[serde(default)]
    pub gates: Vec<String>,
    /// Metric thresholds, by `<gate>.<metric>`.
    #[serde(default)]
    pub metrics: BTreeMap<String, ThresholdKeys>,
    /// Work-item conditions.
    #[serde(default)]
    pub work_items: Option<WorkItemsSpec>,
}

/// `success.work_items:`
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkItemsSpec {
    /// Every item of every `for_each` task must have completed.
    #[serde(default)]
    pub all_completed: bool,
    /// Every item must have completed or failed (been rolled back by
    /// `on_item_failure`): the loops ran to the end.
    #[serde(default)]
    pub all_settled: bool,
}

pub use super::vars::{VarUse, VariableSpec};
