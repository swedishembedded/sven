// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Lowering of a task's `corrections` block.
//!
//! After the agent's turn (and the task's own checks) the block judges the
//! result with a command gate, then, while the judgement calls for it and the
//! corrections last: a supervisor, a model-only author in a conversation of
//! its own, reads the findings and writes a message; a second command gate
//! decides whether the message may be sent, the author rewriting it when it
//! is refused; the accepted message is delivered as a user turn into the
//! agent's own conversation with a fresh tool allowance; and the result is
//! judged again. When the corrections or the rewrites are spent the loop ends
//! and the run goes on: running out is not a failure.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use serde_json::{json, Value};

use crate::error::HarnessError;
use crate::lower_checks::{gate_step, lower_checks, Builder, Phase, RepairPlan, Resolved};
use crate::lower_context::{Context, LoopVars};
use crate::rewrite::{output_fact, rewrite_condition, rewrite_text, Scope};
use crate::source_map::SourceRef;
use crate::steps::{
    AgentStepSpec, BranchStepSpec, IterateStepSpec, PredictStepSpec, StepDecoding, StepSpec,
    UntilSpec,
};
use crate::workflow::{decoding_of, Corrections, GateDef, TaskSpec};

/// The default ask to an author whose message was refused.
const DEFAULT_REFUSAL: &str = "That message cannot be sent: {{ failure.stdout }}\n\
Write it again without the problem.";

/// What the block needs to know about the task it belongs to.
pub(super) struct Turn<'a> {
    pub cx: &'a Context<'a>,
    pub task: &'a TaskSpec,
    pub vars: LoopVars,
    /// The tasks (and authors) whose output exists by now.
    pub visible: &'a [String],
    pub tools: Option<Vec<String>>,
    pub agent_system: Option<String>,
    /// The task's conversation, which the correction turns continue.
    pub conversation: String,
    /// What the correction turns may do with the task's variables.
    pub grants: std::collections::BTreeMap<String, crate::vars::VarAccess>,
    /// The model role that answers the agent.
    pub role: Option<String>,
    pub decoding: StepDecoding,
    pub stop: String,
    pub pool: String,
    /// Where the run goes when the corrections are over.
    pub exit: &'a str,
}

fn origin(task: &str, role: &str) -> SourceRef {
    SourceRef {
        task: Some(task.to_string()),
        role: role.to_string(),
        check: None,
        attempt: None,
    }
}

fn command_gate<'a>(
    cx: &'a Context<'_>,
    name: &str,
) -> Result<&'a crate::workflow::CommandGate, HarnessError> {
    match cx.w.gates.get(name) {
        Some(GateDef::Command(c)) => Ok(c),
        _ => Err(HarnessError::Invalid(format!(
            "internal: `{name}` is not a command gate"
        ))),
    }
}

/// Lowers the block; returns the id of the step that starts it.
pub(super) fn lower_corrections(
    b: &mut Builder,
    c: &Corrections,
    t: &Turn<'_>,
) -> Result<String, HarnessError> {
    let name = &t.task.name;
    let place = format!("task `{name}`: corrections");
    let scope =
        |what: &str| -> Scope<'_> { t.cx.scope(format!("{place}: {what}"), t.visible, t.vars) };
    let (first, iter, when, author, deliver, recheck) = (
        format!("{name}__corr_first"),
        format!("{name}__corr_iter"),
        format!("{name}__corr_when"),
        format!("{name}__corr_author"),
        format!("{name}__corr_deliver"),
        format!("{name}__corr_recheck"),
    );
    let at = |role: &str| origin(name, role);
    let check_run =
        t.cx.command_run(&c.check, command_gate(t.cx, &c.check)?, t.vars, t.visible)?;
    // The result is judged before the first correction and after each one;
    // what it says is read from the gate's latest result, not acted on here.
    b.add(
        &first,
        gate_step(&check_run, &iter, &iter),
        at("correction"),
    );
    b.add(
        &recheck,
        gate_step(&check_run, &iter, &iter),
        at("correction"),
    );

    let count_input = c
        .count
        .as_ref()
        .and_then(Value::as_str)
        .and_then(|text| crate::expr::Template::parse(text).ok())
        .and_then(|tpl| tpl.single_hole().map(|h| h.segments()[1..].join(".")));
    b.add(
        &iter,
        StepSpec::Iterate(Box::new(IterateStepSpec {
            for_each: None,
            until: Some(UntilSpec {
                max_iterations: c.max,
                max_iterations_input: count_input,
            }),
            body: when.clone(),
            then: t.exit.to_string(),
            on_exhausted: Some(t.exit.to_string()),
            store_as: None,
        })),
        at("correction"),
    );

    let condition = c
        .while_
        .clone()
        .unwrap_or_else(|| json!({ "not": format!("gates.{}.passed", c.check) }));
    b.add(
        &when,
        StepSpec::Branch(Box::new(BranchStepSpec {
            on: rewrite_condition(&condition, &scope("while"), name)?,
            when_true: author.clone(),
            when_false: t.exit.to_string(),
        })),
        at("correction"),
    );

    // The accept gate reads the author's message; a refusal sends the author
    // back to rewrite it, and when the rewrites are spent the loop ends.
    let author_fact = output_fact(&c.author.name);
    let accept = Resolved {
        label: c.accept.clone(),
        required: true,
        kind: crate::lower_checks::Kind::Command(t.cx.command_run(
            &c.accept,
            command_gate(t.cx, &c.accept)?,
            t.vars,
            t.visible,
        )?),
    };
    let author_decoding = decoding_of(&c.author.sampling, &c.author.thinking, &c.author.turn);
    let author_system = c
        .author
        .system
        .as_deref()
        .map(|text| rewrite_text(text, &scope("author.system")))
        .transpose()?;
    let phase = Phase {
        prefix: format!("{name}__accept"),
        task: Some(name.clone()),
        fail_code: "gate_failed",
        what: place.clone(),
        place: place.clone(),
        repair: Some(RepairPlan {
            attempts: u32::try_from(c.retries.unwrap_or(1)).unwrap_or(u32::MAX),
            limit_input: None,
            prompt: c
                .refusal
                .clone()
                .unwrap_or_else(|| DEFAULT_REFUSAL.to_string()),
            tools: None,
            fact: author_fact.clone(),
            inputs: t.cx.inputs.as_ref(),
            tasks: t.visible,
            item: t.vars.item,
            iteration: t.vars.iteration,
            decoding: author_decoding.clone(),
            model_only: Some(None),
            role: c.author.model.clone(),
            stop: None,
            pool: None,
            max_repeated_results: None,
            conversation: Some(c.author.name.clone()),
            system: None,
            structured: &t.cx.structured,
            gates: &t.cx.command_gates,
            vars: std::collections::BTreeMap::new(),
            var_names: &t.cx.var_names,
        }),
        loop_back: Some(t.exit.to_string()),
        item_failure: None,
    };
    let accept_entry = lower_checks(b, &phase, &[accept], &deliver)?;

    b.add(
        &author,
        StepSpec::Predict(Box::new(PredictStepSpec {
            prompt: rewrite_text(&c.author.task, &scope("author.task"))?,
            schema: None,
            store_as: Some(author_fact.clone()),
            conversation: Some(c.author.name.clone()),
            system: author_system,
            origin: Some("task".to_string()),
            repair_attempts: None,
            repair_limit_input: None,
            on_unusable_answer: None,
            role: c.author.model.clone(),
            decoding: author_decoding,
            then: accept_entry,
        })),
        at("correction"),
    );

    let allowance = c
        .allowance
        .as_ref()
        .map(|v| match v {
            Value::String(text) => rewrite_text(text, &scope("allowance")),
            other => Ok(other.to_string()),
        })
        .transpose()?;
    b.add(
        &deliver,
        StepSpec::Agent(Box::new(AgentStepSpec {
            prompt: format!("{{{{ facts.{author_fact} }}}}"),
            tools: t.tools.clone(),
            store_as: Some(output_fact(name)),
            conversation: Some(t.conversation.clone()),
            vars: t.grants.clone(),
            system: t.agent_system.clone(),
            origin: Some("correction".to_string()),
            role: t.role.clone(),
            stop_as: Some(t.stop.clone()),
            pool: Some(t.pool.clone()),
            allowance,
            max_repeated_results: t.task.max_repeated_results,
            decoding: t.decoding.clone(),
            attach: Vec::new(),
            then: recheck,
        })),
        at("correction"),
    );
    Ok(first)
}
