// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Effect template rendering — [`EffectTmpl`] → `Vec<Effect>` + context mutations.
//!
//! This module lives in `sven-core` (not `sven-graph`) because it needs access
//! to `loop_core::init_loop`, `build_turn_effect`, and `sven_llm::TurnRequest`
//! to build `Effect::CallLlm` values correctly.
//!
//! # Context mutations
//!
//! Some templates (`StartLoop`, `SetFact`) mutate `Context` but produce no
//! kernel `Effect`. They are rendered to an empty `Effect` slice and the
//! mutation is applied as a side effect of this function.

use serde_json::Value;
use sven_graph::{
    model::{EffectTmpl, ToolsSpec},
    native::{NativeArgs, NativeOutcome, NativeRegistry},
    template::TemplateCtx,
};
use sven_hsm::{
    context::Context,
    effect::Effect,
    event::Event,
    ids::{MachineId, TimerId, ToolCallId},
};

use crate::machines::loop_core::{build_turn_effect, init_loop};

/// Render a slice of [`EffectTmpl`]s into concrete [`Effect`]s, applying any
/// context mutations (e.g. `StartLoop`, `SetFact`) as side effects.
///
/// Templates that produce no kernel effect (context mutations) contribute
/// nothing to the returned `Vec`; their mutations are applied before continuing.
pub fn render_effects(
    templates: &[EffectTmpl],
    ctx: &mut Context,
    event: &Event,
    tmpl_ctx: &TemplateCtx<'_>,
    native: &NativeRegistry,
) -> Vec<Effect> {
    let mut effects = Vec::with_capacity(templates.len());
    for tmpl in templates {
        match tmpl {
            EffectTmpl::CallLlm {
                thread,
                prompt_tmpl,
                tools,
                max_rounds,
                schema,
                model,
            } => {
                let prompt = tmpl_ctx
                    .render_or(prompt_tmpl, "")
                    .unwrap_or_default();
                let (tool_names, mode) = expand_tools(tools);
                let eff = build_turn_effect(
                    thread,
                    &tool_names,
                    &mode,
                    model.as_deref(),
                    Some(&prompt),
                    None,
                    *max_rounds,
                    schema.as_ref().map(|s| s.schema.clone()),
                    schema.as_ref().map(|s| s.name.as_str()),
                );
                effects.push(eff);
            }

            EffectTmpl::CallTool { name, capability, args_tmpl } => {
                let args_str = tmpl_ctx.render_or(args_tmpl, "null").unwrap_or_default();
                let args: Value = serde_json::from_str(&args_str).unwrap_or(Value::Null);
                effects.push(Effect::CallTool {
                    call_id: ToolCallId::new(),
                    name: name.clone(),
                    capability: *capability,
                    args,
                });
            }

            EffectTmpl::AskUser { prompt_tmpl } => {
                let prompt = tmpl_ctx.render_or(prompt_tmpl, "").unwrap_or_default();
                effects.push(Effect::AskUser { prompt });
            }

            EffectTmpl::Approve { capability, description_tmpl } => {
                let desc = tmpl_ctx.render_or(description_tmpl, "").unwrap_or_default();
                effects.push(Effect::RequestHumanApproval {
                    approval_id: sven_hsm::ids::ApprovalId::new(),
                    capability: *capability,
                    description: desc,
                });
            }

            EffectTmpl::Checkpoint { label } => {
                effects.push(Effect::CreateCheckpoint { label: label.clone() });
            }

            EffectTmpl::Rollback { label } => {
                effects.push(Effect::RollbackToCheckpoint { label: label.clone() });
            }

            EffectTmpl::Emit { name, payload_tmpl } => {
                let payload_str = tmpl_ctx.render_or(payload_tmpl, "null").unwrap_or_default();
                let payload: Value = serde_json::from_str(&payload_str).unwrap_or(Value::Null);
                effects.push(Effect::EmitInternal { name: name.clone(), payload });
            }

            EffectTmpl::Spawn { graph_name, descriptor_tmpl } => {
                let desc_str = tmpl_ctx.render_or(descriptor_tmpl, "{}").unwrap_or_default();
                let mut descriptor: Value = serde_json::from_str(&desc_str).unwrap_or(Value::Null);
                // Inject the graph name so GraphChildSpawner knows what to build.
                if let Value::Object(ref mut m) = descriptor {
                    m.insert("graph".into(), Value::String(graph_name.clone()));
                }
                effects.push(Effect::InstantiateSubmachine {
                    machine: MachineId::new(),
                    descriptor,
                });
            }

            EffectTmpl::SpawnEach { path, graph_name, descriptor_tmpl } => {
                use sven_graph::guard::GuardCtx;
                let guard_ctx = GuardCtx {
                    facts: tmpl_ctx.facts,
                    retry: &std::collections::HashMap::new(), // can't access retry through TemplateCtx
                    decision: tmpl_ctx.decision,
                    event: tmpl_ctx.event,
                    loop_state: tmpl_ctx.loop_state,
                };
                let array = sven_graph::guard::resolve_path_expr(path, &guard_ctx);
                if let Value::Array(items) = array {
                    for item in items {
                        // Bind `$item` — a placeholder that template rendering
                        // would resolve, but for now we inject into facts temporarily.
                        // (A proper implementation uses a dedicated binding scope;
                        //  this is a simplified version for Phase 1.)
                        let mut temp_facts = tmpl_ctx.facts.clone();
                        temp_facts.insert("_item".into(), item.clone());
                        let temp_tmpl_ctx = TemplateCtx {
                            facts: &temp_facts,
                            decision: tmpl_ctx.decision,
                            event: tmpl_ctx.event,
                            loop_state: tmpl_ctx.loop_state,
                            constants: tmpl_ctx.constants,
                        };
                        let desc_str = temp_tmpl_ctx
                            .render_or(descriptor_tmpl, "{}")
                            .unwrap_or_default();
                        let mut descriptor: Value =
                            serde_json::from_str(&desc_str).unwrap_or(Value::Null);
                        if let Value::Object(ref mut m) = descriptor {
                            m.insert("graph".into(), Value::String(graph_name.clone()));
                        }
                        effects.push(Effect::InstantiateSubmachine {
                            machine: MachineId::new(),
                            descriptor,
                        });
                    }
                }
            }

            EffectTmpl::Timer { timer_id_tmpl, duration } => {
                let _id_str = tmpl_ctx.render_or(timer_id_tmpl, "timer").unwrap_or_default();
                effects.push(Effect::ScheduleTimeout {
                    timer_id: TimerId::new(),
                    duration: *duration,
                });
            }

            EffectTmpl::CancelTimer { .. } => {
                effects.push(Effect::CancelTimeout { timer_id: TimerId::new() });
            }

            EffectTmpl::StartLoop { thread, tools, max_rounds, .. } => {
                // Context mutation only — no kernel Effect.
                let (tool_names, mode) = expand_tools(&Some(tools.clone()));
                init_loop(ctx, thread, &tool_names, &mode, *max_rounds);
            }

            EffectTmpl::SetFact { key, value_tmpl } => {
                // Context mutation only — no kernel Effect.
                let val_str = tmpl_ctx.render_or(value_tmpl, "null").unwrap_or_default();
                let val: Value = if val_str.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_str(&val_str).unwrap_or(Value::String(val_str))
                };
                ctx.set_fact(key.clone(), val);
            }

            EffectTmpl::Native { fn_name, params } => {
                let args = NativeArgs { params: params.clone() };
                match native.call(fn_name, ctx, event, &args) {
                    Ok(NativeOutcome::Handled(efs)) => effects.extend(efs),
                    Ok(_) => {} // Goto/Bubble/Ignore from an effect are no-ops here
                    Err(e) => {
                        tracing::warn!("native effect '{fn_name}' error: {e}");
                    }
                }
            }
        }
    }
    effects
}

/// Expand a `ToolsSpec` into `(tool_names, all_tools_mode)` for `build_turn_effect`.
fn expand_tools(spec: &Option<ToolsSpec>) -> (Vec<String>, String) {
    match spec {
        None | Some(ToolsSpec::AllMode(_)) => {
            let mode = match spec {
                Some(ToolsSpec::AllMode(m)) => m.clone(),
                _ => "agent".to_string(),
            };
            (vec![], mode)
        }
        Some(ToolsSpec::Named(names)) => (names.clone(), String::new()),
    }
}

// ── TemplateCtx extension ────────────────────────────────────────────────────

/// Extension trait to give TemplateCtx a convenience method.
trait RenderExt {
    fn render_or(&self, tmpl: &str, default: &str) -> Result<String, String>;
}

impl<'a> RenderExt for TemplateCtx<'a> {
    fn render_or(&self, tmpl: &str, _default: &str) -> Result<String, String> {
        sven_graph::template::render(tmpl, self).map_err(|e| e.to_string())
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;
    use sven_graph::{model::EffectTmpl, native::NativeRegistry, template::TemplateCtx};
    use sven_hsm::{context::Context, event::Event};

    use super::render_effects;

    fn empty_ctx() -> Context {
        Context::new()
    }

    fn empty_tmpl_ctx<'a>(
        facts: &'a serde_json::Map<String, serde_json::Value>,
        constants: &'a HashMap<String, String>,
        event: &'a serde_json::Value,
    ) -> TemplateCtx<'a> {
        TemplateCtx {
            facts,
            decision: None,
            event,
            loop_state: None,
            constants,
        }
    }

    #[test]
    fn set_fact_mutates_context() {
        let mut ctx = empty_ctx();
        let native = NativeRegistry::new();
        let facts = ctx.facts.clone();
        let constants = HashMap::new();
        let event_val = json!({});
        let tmpl_ctx = empty_tmpl_ctx(&facts, &constants, &event_val);
        render_effects(
            &[EffectTmpl::SetFact {
                key: "answer".into(),
                value_tmpl: "42".into(),
            }],
            &mut ctx,
            &Event::UserCancelled,
            &tmpl_ctx,
            &native,
        );
        assert_eq!(ctx.fact("answer"), Some(&json!(42)));
    }

    #[test]
    fn ask_user_renders_prompt() {
        let mut ctx = empty_ctx();
        let native = NativeRegistry::new();
        let mut facts = ctx.facts.clone();
        facts.insert("name".into(), json!("Alice"));
        let constants = HashMap::new();
        let event_val = json!({});
        let tmpl_ctx = empty_tmpl_ctx(&facts, &constants, &event_val);
        let effects = render_effects(
            &[EffectTmpl::AskUser {
                prompt_tmpl: "Hello {{ fact.name }}!".into(),
            }],
            &mut ctx,
            &Event::UserCancelled,
            &tmpl_ctx,
            &native,
        );
        assert_eq!(effects.len(), 1);
        if let sven_hsm::effect::Effect::AskUser { prompt } = &effects[0] {
            assert_eq!(prompt, "Hello Alice!");
        } else {
            panic!("expected AskUser effect");
        }
    }
}
