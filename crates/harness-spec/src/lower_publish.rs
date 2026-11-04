// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Lowering a task's `publish:`: one publish step between the task and what
//! follows it, which hands the host the task's files once its checks passed.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use crate::error::HarnessError;
use crate::lower::origin;
use crate::lower_checks::Builder;
use crate::lower_context::{Context, LoopVars};
use crate::rewrite::rewrite_text;
use crate::steps::{PublishStepSpec, StepSpec};
use crate::workflow::TaskSpec;

/// Adds `task`'s publish step, which goes on to `next`, and answers its id;
/// none for a task that publishes nothing.
pub(super) fn lower(
    cx: &Context<'_>,
    b: &mut Builder,
    task: &TaskSpec,
    through: &[String],
    vars: LoopVars,
    next: &str,
) -> Result<Option<String>, HarnessError> {
    if task.publish.is_empty() {
        return Ok(None);
    }
    let id = format!("{}__publish", task.name);
    let scope = cx.scope(format!("task `{}`: publish", task.name), through, vars);
    let files = task
        .publish
        .iter()
        .map(|file| rewrite_text(file.trim(), &scope))
        .collect::<Result<Vec<_>, _>>()?;
    b.add(
        &id,
        StepSpec::Publish(Box::new(PublishStepSpec {
            task: task.name.clone(),
            files,
            then: next.to_string(),
        })),
        origin(Some(&task.name), "publish"),
    );
    Ok(Some(id))
}
