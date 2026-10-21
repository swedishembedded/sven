// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `agent.system` as a list of `{when, text}` variants: the first whose
//! condition holds is the system message of the agent's conversation.
//!
//! The conditions read inputs, which are known when the run starts, so the
//! choice is made once, before the first task: a chain of `branch` steps, each
//! leading to an `assign` that stores its text under [`FACT`]. Every agent
//! turn then reads `{{ facts.agent_system }}`, the same way a plain string
//! would be read.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in running one workflow under
//! several standing instructions then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::collections::BTreeMap;

use serde_json::json;

use crate::error::HarnessError;
use crate::lower_checks::Builder;
use crate::lower_context::{Context, LoopVars};
use crate::rewrite::{rewrite_condition, rewrite_text};
use crate::source_map::SourceRef;
use crate::steps::{AssignStepSpec, BranchStepSpec, StepSpec};
use crate::workflow::{AgentSystem, SystemVariant};

/// The fact the chosen variant's text is stored under.
const FACT: &str = "agent_system";

fn origin() -> SourceRef {
    SourceRef {
        task: None,
        role: "agent_system".to_string(),
        check: None,
        attempt: None,
    }
}

/// The template an agent step's system message is: the text itself, or the
/// fact the chosen variant is stored in.
pub(super) fn template(cx: &Context<'_>, vars: LoopVars) -> Result<Option<String>, HarnessError> {
    Ok(match &cx.w.agent.system {
        None => None,
        Some(AgentSystem::Text(text)) => Some(rewrite_text(
            text,
            &cx.scope("agent.system".into(), &[], vars),
        )?),
        Some(AgentSystem::Variants(_)) => Some(format!("{{{{ facts.{FACT} }}}}")),
    })
}

/// Puts the choice of variant in front of `entry`; returns the new entry.
/// Without variants the entry is unchanged.
pub(super) fn lower_variants(
    cx: &Context<'_>,
    b: &mut Builder,
    entry: String,
) -> Result<String, HarnessError> {
    let Some(AgentSystem::Variants(variants)) = &cx.w.agent.system else {
        return Ok(entry);
    };
    let invalid = |why: String| HarnessError::Invalid(format!("agent.system: {why}"));
    let Some((last, others)) = variants.split_last() else {
        return Err(invalid("needs at least one entry".into()));
    };
    if last.when.is_some() {
        return Err(invalid(
            "the last `agent.system` entry must have no `when`: it is the text used when none \
             of the others holds"
                .into(),
        ));
    }
    if let Some(at) = others.iter().position(|v| v.when.is_none()) {
        return Err(invalid(format!(
            "entry {} has no `when`, so the entries after it are unreachable",
            at + 1
        )));
    }
    let step_id = |i: usize, what: &str| format!("agent_system__{what}{i}");
    let mut next = entry;
    let assign = |b: &mut Builder, i: usize, v: &SystemVariant, then: &str| {
        let scope = cx.scope(format!("agent.system[{i}]: text"), &[], LoopVars::default());
        let text = rewrite_text(&v.text, &scope)?;
        let id = step_id(i, "set");
        b.add(
            &id,
            StepSpec::Assign(Box::new(AssignStepSpec {
                set: BTreeMap::from([(FACT.to_string(), json!(text))]),
                then: then.to_string(),
            })),
            origin(),
        );
        Ok::<_, HarnessError>(id)
    };
    let mut chosen = assign(b, variants.len() - 1, last, &next)?;
    for (i, v) in others.iter().enumerate().rev() {
        let set = assign(b, i, v, &next)?;
        let scope = cx.scope(format!("agent.system[{i}]: when"), &[], LoopVars::default());
        let id = step_id(i, "when");
        b.add(
            &id,
            StepSpec::Branch(Box::new(BranchStepSpec {
                on: rewrite_condition(v.when.as_ref().expect("checked above"), &scope, "")?,
                when_true: set,
                when_false: chosen,
            })),
            origin(),
        );
        chosen = id;
    }
    next = chosen;
    Ok(next)
}
