// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Guard expression evaluation.
//!
//! [`GuardExpr`] is a small, total, side-effect-free expression language that
//! edges use to select transitions. It evaluates against a [`GuardCtx`] which
//! holds read-only references to the context facts, the optional parsed decision,
//! the triggering event (as JSON), retry counters, and the optional loop-state
//! JSON value.
//!
//! # Design contract
//!
//! * **No I/O** — guards are pure functions of the `GuardCtx`.
//! * **No arithmetic** beyond comparisons — anything more complex is a native fn.
//! * **Missing paths resolve to `null`** — comparisons to `null` are legal.
//! * **Total** — evaluation never panics; unexpected inputs return `false` or
//!   `Err` gracefully.

use std::collections::HashMap;

use serde_json::Value;
use sven_hsm::event::{Event, EventKind, InternalEvent};

use crate::model::{CmpOp, EventPattern, GuardExpr, GuardRoot, PathKey};

// ─── GuardCtx ────────────────────────────────────────────────────────────────

/// Read-only evaluation context for a guard expression.
pub struct GuardCtx<'a> {
    /// `ctx.facts` — the machine's accumulated domain knowledge.
    pub facts: &'a serde_json::Map<String, Value>,
    /// `ctx.retry_counters`.
    pub retry: &'a HashMap<String, u32>,
    /// Optional parsed LLM decision (only present in loop nodes after
    /// `LlmTurnComplete` produces a schema-validated response).
    pub decision: Option<&'a Value>,
    /// The triggering event, pre-serialised to JSON for path access.
    pub event: &'a Value,
    /// Optional loop-state JSON value (from `ctx.facts["lc_state"]`).
    pub loop_state: Option<&'a Value>,
}


// ─── GuardError ──────────────────────────────────────────────────────────────

/// An error from guard evaluation.
#[derive(Debug, thiserror::Error)]
pub enum GuardError {
    #[error("type error in guard: {0}")]
    TypeError(String),
}

// ─── Evaluation ──────────────────────────────────────────────────────────────

/// Evaluate a [`GuardExpr`] against a [`GuardCtx`].
///
/// Returns `true` or `false`; returns `Err` only for structural type errors
/// (e.g. `len` applied to a non-array). Callers that want a boolean should call
/// `eval(expr, ctx).unwrap_or(false)`.
pub fn eval(expr: &GuardExpr, ctx: &GuardCtx<'_>) -> Result<bool, GuardError> {
    match expr {
        GuardExpr::Lit(v) => Ok(value_is_truthy(v)),
        GuardExpr::Path { root, keys } => {
            let v = resolve_path(*root, keys, ctx);
            Ok(value_is_truthy(&v))
        }
        GuardExpr::Len(inner) => {
            let v = eval_to_value(inner, ctx);
            let n = match &v {
                Value::Array(a) => a.len(),
                Value::Object(o) => o.len(),
                Value::String(s) => s.len(),
                Value::Null => 0,
                _ => {
                    return Err(GuardError::TypeError(format!(
                        "len() requires array/object/string, got {v}"
                    )))
                }
            };
            Ok(n > 0)
        }
        GuardExpr::Not(inner) => Ok(!eval(inner, ctx)?),
        GuardExpr::And(exprs) => {
            for e in exprs {
                if !eval(e, ctx)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        GuardExpr::Or(exprs) => {
            for e in exprs {
                if eval(e, ctx)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        GuardExpr::Cmp { op, lhs, rhs } => {
            let l = eval_to_value(lhs, ctx);
            let r = eval_to_value(rhs, ctx);
            Ok(compare_values(*op, &l, &r))
        }
    }
}

/// Evaluate a guard expression to a JSON value (for `Cmp` sub-expressions).
fn eval_to_value(expr: &GuardExpr, ctx: &GuardCtx<'_>) -> Value {
    match expr {
        GuardExpr::Lit(v) => v.clone(),
        GuardExpr::Path { root, keys } => resolve_path(*root, keys, ctx),
        GuardExpr::Len(inner) => {
            let v = eval_to_value(inner, ctx);
            let n = match &v {
                Value::Array(a) => a.len(),
                Value::Object(o) => o.len(),
                Value::String(s) => s.len(),
                _ => 0,
            };
            Value::Number(serde_json::Number::from(n))
        }
        // Boolean sub-expressions coerce to bool JSON.
        other => Value::Bool(eval(other, ctx).unwrap_or(false)),
    }
}

/// Resolve a root + path to a JSON value. Missing paths resolve to `null`.
fn resolve_path(root: GuardRoot, keys: &[PathKey], ctx: &GuardCtx<'_>) -> Value {
    match root {
        GuardRoot::Fact => {
            // First key is the fact name; subsequent keys navigate the JSON tree.
            let Some(PathKey::Field(fact_name)) = keys.first() else {
                return Value::Null;
            };
            let base = ctx.facts.get(fact_name).cloned().unwrap_or(Value::Null);
            navigate(&base, &keys[1..])
        }
        GuardRoot::Decision => {
            let base = ctx.decision.cloned().unwrap_or(Value::Null);
            navigate(&base, keys)
        }
        GuardRoot::Event => navigate(ctx.event, keys),
        GuardRoot::Retry => {
            let Some(PathKey::Field(key)) = keys.first() else {
                return Value::Null;
            };
            let n = ctx.retry.get(key.as_str()).copied().unwrap_or(0);
            if keys.len() == 1 {
                Value::Number(serde_json::Number::from(n))
            } else {
                Value::Null // retry values are scalars, no deeper navigation
            }
        }
        GuardRoot::Loop => {
            let base = ctx.loop_state.cloned().unwrap_or(Value::Null);
            navigate(&base, keys)
        }
    }
}

/// Navigate a JSON value via a slice of path keys.
fn navigate(v: &Value, keys: &[PathKey]) -> Value {
    if keys.is_empty() {
        return v.clone();
    }
    match (&keys[0], v) {
        (PathKey::Field(f), Value::Object(m)) => {
            navigate(m.get(f.as_str()).unwrap_or(&Value::Null), &keys[1..])
        }
        (PathKey::Index(i), Value::Array(a)) => {
            navigate(a.get(*i).unwrap_or(&Value::Null), &keys[1..])
        }
        _ => Value::Null,
    }
}

/// JSON "truthiness": `false`, `null`, `0`, `""`, `[]`, `{}` → false; else true.
fn value_is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Compare two JSON values according to `op`.
///
/// String comparison is lexicographic.  Numeric comparison uses f64 coercion.
/// Cross-type comparisons (`Eq`/`Ne`) use JSON equality; ordered comparisons
/// return `false` for non-comparable types.
fn compare_values(op: CmpOp, l: &Value, r: &Value) -> bool {
    // Numeric coercion helper.
    let as_f64 = |v: &Value| -> Option<f64> {
        match v {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.parse::<f64>().ok(),
            _ => None,
        }
    };

    match op {
        CmpOp::Eq => l == r,
        CmpOp::Ne => l != r,
        CmpOp::Gt | CmpOp::Ge | CmpOp::Lt | CmpOp::Le => {
            // Try numeric first; fall back to string lexicographic.
            if let (Some(lf), Some(rf)) = (as_f64(l), as_f64(r)) {
                match op {
                    CmpOp::Gt => lf > rf,
                    CmpOp::Ge => lf >= rf,
                    CmpOp::Lt => lf < rf,
                    CmpOp::Le => lf <= rf,
                    _ => unreachable!(),
                }
            } else if let (Value::String(ls), Value::String(rs)) = (l, r) {
                match op {
                    CmpOp::Gt => ls > rs,
                    CmpOp::Ge => ls >= rs,
                    CmpOp::Lt => ls < rs,
                    CmpOp::Le => ls <= rs,
                    _ => unreachable!(),
                }
            } else {
                false
            }
        }
    }
}

// ─── EventPattern matching ────────────────────────────────────────────────────

/// Evaluate a [`GuardExpr`] to a JSON [`Value`] (for use in effect renderers).
///
/// This is the same logic as `eval_to_value` (private), exposed as a public
/// helper so `sven-core`'s effect renderer can resolve `SpawnEach` array paths.
pub fn resolve_path_expr(expr: &GuardExpr, ctx: &GuardCtx<'_>) -> Value {
    eval_to_value(expr, ctx)
}

/// Return `true` if `event` matches `pattern`.
///
/// Note: [`EventPattern::Final`] and [`EventPattern::Failed`] match only the
/// raw event kinds; the loop runtime decides whether a `LlmTurnComplete` is
/// "final" by returning `FinalAnswer` from `on_llm_turn_complete`. The
/// `GraphMachine` only evaluates `Final`-pattern edges after the loop runtime
/// has classified the turn as `FinalAnswer`.
#[must_use]
pub fn matches_pattern(pattern: &EventPattern, event: &Event) -> bool {
    match pattern {
        EventPattern::Any => true,
        EventPattern::Kind(k) => event.kind() == *k,
        EventPattern::Final => matches!(event.kind(), EventKind::LlmTurnComplete),
        EventPattern::Failed => matches!(event.kind(), EventKind::LlmFailed),
        EventPattern::SubmachineCompleted => {
            matches!(event, Event::Internal(InternalEvent::SubmachineCompleted { .. }))
        }
        EventPattern::Custom(name) => {
            matches!(event, Event::Internal(InternalEvent::Custom { name: n, .. }) if n == name)
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::model::{CmpOp, GuardExpr, GuardRoot, PathKey};

    #[test]
    fn lit_true() {
        let facts = serde_json::Map::new();
        let retry = HashMap::new();
        let event = json!({});
        let ctx = GuardCtx {
            facts: &facts,
            retry: &retry,
            decision: None,
            event: &event,
            loop_state: None,
        };
        assert!(eval(&GuardExpr::Lit(json!(true)), &ctx).unwrap());
        assert!(!eval(&GuardExpr::Lit(json!(false)), &ctx).unwrap());
        assert!(!eval(&GuardExpr::Lit(json!(null)), &ctx).unwrap());
        assert!(eval(&GuardExpr::Lit(json!(1)), &ctx).unwrap());
        assert!(!eval(&GuardExpr::Lit(json!(0)), &ctx).unwrap());
    }

    #[test]
    fn fact_path() {
        let mut facts = serde_json::Map::new();
        facts.insert("x".into(), json!(42));
        let retry = HashMap::new();
        let event = json!({});
        let ctx = GuardCtx {
            facts: &facts,
            retry: &retry,
            decision: None,
            event: &event,
            loop_state: None,
        };
        let expr = GuardExpr::Path {
            root: GuardRoot::Fact,
            keys: vec![PathKey::Field("x".into())],
        };
        assert!(eval(&expr, &ctx).unwrap());
    }

    #[test]
    fn decision_status_eq() {
        let decision = json!({ "status": "proceed" });
        let facts = serde_json::Map::new();
        let retry = HashMap::new();
        let event = json!({});
        let ctx = GuardCtx {
            facts: &facts,
            retry: &retry,
            decision: Some(&decision),
            event: &event,
            loop_state: None,
        };
        let expr = GuardExpr::Cmp {
            op: CmpOp::Eq,
            lhs: Box::new(GuardExpr::Path {
                root: GuardRoot::Decision,
                keys: vec![PathKey::Field("status".into())],
            }),
            rhs: Box::new(GuardExpr::Lit(json!("proceed"))),
        };
        assert!(eval(&expr, &ctx).unwrap());

        let expr_ne = GuardExpr::Cmp {
            op: CmpOp::Ne,
            lhs: Box::new(GuardExpr::Path {
                root: GuardRoot::Decision,
                keys: vec![PathKey::Field("status".into())],
            }),
            rhs: Box::new(GuardExpr::Lit(json!("failed"))),
        };
        assert!(eval(&expr_ne, &ctx).unwrap());
    }

    #[test]
    fn retry_counter() {
        let facts = serde_json::Map::new();
        let mut retry = HashMap::new();
        retry.insert("recovery".into(), 4u32);
        let event = json!({});
        let ctx = GuardCtx {
            facts: &facts,
            retry: &retry,
            decision: None,
            event: &event,
            loop_state: None,
        };
        let expr = GuardExpr::Cmp {
            op: CmpOp::Gt,
            lhs: Box::new(GuardExpr::Path {
                root: GuardRoot::Retry,
                keys: vec![PathKey::Field("recovery".into())],
            }),
            rhs: Box::new(GuardExpr::Lit(json!(3))),
        };
        assert!(eval(&expr, &ctx).unwrap());
    }

    #[test]
    fn and_short_circuits() {
        let facts = serde_json::Map::new();
        let retry = HashMap::new();
        let event = json!({});
        let ctx = GuardCtx {
            facts: &facts,
            retry: &retry,
            decision: None,
            event: &event,
            loop_state: None,
        };
        let expr = GuardExpr::And(vec![
            GuardExpr::Lit(json!(false)),
            GuardExpr::Lit(json!(true)),
        ]);
        assert!(!eval(&expr, &ctx).unwrap());
    }

    #[test]
    fn not() {
        let facts = serde_json::Map::new();
        let retry = HashMap::new();
        let event = json!({});
        let ctx = GuardCtx {
            facts: &facts,
            retry: &retry,
            decision: None,
            event: &event,
            loop_state: None,
        };
        assert!(eval(&GuardExpr::Not(Box::new(GuardExpr::Lit(json!(false)))), &ctx).unwrap());
        assert!(!eval(&GuardExpr::Not(Box::new(GuardExpr::Lit(json!(true)))), &ctx).unwrap());
    }

    #[test]
    fn pattern_matching() {
        use sven_hsm::event::EventKind;
        let msg = Event::UserMessage { text: "hi".into() };
        assert!(matches_pattern(&EventPattern::Any, &msg));
        assert!(matches_pattern(&EventPattern::Kind(EventKind::UserMessage), &msg));
        assert!(!matches_pattern(&EventPattern::Kind(EventKind::UserCancelled), &msg));
        assert!(matches_pattern(&EventPattern::Final, &Event::LlmTurnComplete {
            thread: "t".into(), text: "x".into(), tool_calls: vec![]
        }));
        assert!(!matches_pattern(&EventPattern::Failed, &msg));
    }
}
