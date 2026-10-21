// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The variable tools an agent step is answered by the machine itself:
//! `harness_vars_list`, `harness_vars_get` and `harness_vars_set`.
//!
//! A call never reaches an executor. The machine checks it against what the
//! step was granted and the variable's declaration, stores an accepted value
//! in the run's facts (so it is checkpointed and replayed like everything
//! else) and hands the answer to the next turn, which appends it to the
//! conversation like any tool result.
//!
//! Swedish Embedded AB implements programmable, data-defined agent harnesses
//! for its clients. If your team needs expertise in declarative agent
//! workflow languages then you can procure our services by sending an email
//! to info@swedishembedded.com.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use sven_harness_spec::vars::{admit, var_fact, var_set_fact, VarAccess};
use sven_harness_spec::HarnessPlan;

use crate::run::HarnessRunState;

/// Whether `name` is one of the tools the machine answers for a step that
/// was granted variables.
pub(crate) fn is_var_tool(name: &str) -> bool {
    sven_harness_spec::vars::VAR_TOOLS.contains(&name)
}

/// Answers one variable-tool call, storing a value it accepts.
pub(crate) fn answer(
    plan: &HarnessPlan,
    run: &mut HarnessRunState,
    grants: &BTreeMap<String, VarAccess>,
    tool: &str,
    args: &Value,
) -> String {
    let name = args.get("name").and_then(Value::as_str);
    match tool {
        "harness_vars_list" => list(plan, run, grants),
        "harness_vars_get" => match name {
            None => "error: give the variable's `name`".to_string(),
            Some(name) if !grants.contains_key(name) => not_visible(name, grants),
            Some(name) => {
                if run
                    .facts
                    .get(&var_set_fact(name))
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    let value = run
                        .facts
                        .get(&var_fact(name))
                        .cloned()
                        .unwrap_or(Value::Null);
                    json!({ "name": name, "value": value }).to_string()
                } else {
                    format!("`{name}` is not set yet")
                }
            }
        },
        "harness_vars_set" => {
            let Some(name) = name else {
                return "error: give the variable's `name` and `value`".to_string();
            };
            match grants.get(name) {
                None => return not_visible(name, grants),
                Some(VarAccess::Read) => {
                    return format!("error: `{name}` is read-only for this task; it was not set")
                }
                Some(VarAccess::Write) => {}
            }
            let Some(decl) = plan.variables.get(name) else {
                return format!("error: `{name}` is not declared");
            };
            let Some(value) = args.get("value").cloned() else {
                return format!("error: give the `value` to set `{name}` to");
            };
            match admit(decl, value) {
                Ok(value) => {
                    run.facts.insert(var_fact(name), value);
                    run.facts.insert(var_set_fact(name), Value::Bool(true));
                    format!("ok: `{name}` is set")
                }
                Err(why) => format!("error: `{name}` was not set: {why}"),
            }
        }
        other => format!("error: unknown variable tool `{other}`"),
    }
}

fn not_visible(name: &str, grants: &BTreeMap<String, VarAccess>) -> String {
    format!(
        "error: this task cannot see a variable `{name}` (it can see: {})",
        if grants.is_empty() {
            "none".to_string()
        } else {
            grants.keys().cloned().collect::<Vec<_>>().join(", ")
        }
    )
}

fn list(plan: &HarnessPlan, run: &HarnessRunState, grants: &BTreeMap<String, VarAccess>) -> String {
    let rows: Vec<Value> = grants
        .iter()
        .map(|(name, access)| {
            let decl = plan.variables.get(name);
            let set = run
                .facts
                .get(&var_set_fact(name))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            json!({
                "name": name,
                "type": decl.map(|d| serde_json::to_value(&d.ty).unwrap_or(Value::Null)),
                "description": decl.and_then(|d| d.description.clone()),
                "schema": decl.and_then(|d| d.schema.clone()),
                "access": match access { VarAccess::Read => "read", VarAccess::Write => "write" },
                "set": set,
            })
        })
        .collect();
    Value::Array(rows).to_string()
}
