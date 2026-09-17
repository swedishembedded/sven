// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Variable binding: `ask_user` results are nameable (e.g. "code") so a
//! later step can reference the value ("Ask the user for the code" ->
//! "enter the code"). Stored as a plain JSON fact on the [`Context`] so it
//! replays exactly like every other piece of machine state.

use std::collections::BTreeMap;

use sven_hsm::context::Context;

const VARS_FACT: &str = "ui_test_vars";

/// Loads the current variable bindings, empty if none have been set yet.
#[must_use]
pub fn load(ctx: &Context) -> BTreeMap<String, String> {
    ctx.fact(VARS_FACT)
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

fn store(ctx: &mut Context, vars: &BTreeMap<String, String>) {
    ctx.set_fact(
        VARS_FACT,
        serde_json::to_value(vars).expect("BTreeMap<String,String> always serializes"),
    );
}

/// Binds `value` under `name`, overwriting any prior binding of that name.
///
/// `name` is normalized (trimmed, lower-cased) so "Code", "code", and
/// " code " all address the same slot - two independent LLM calls (the
/// step that asks and the step that references) must agree on a slug
/// without depending on exact casing surviving both.
pub fn bind(ctx: &mut Context, name: &str, value: &str) {
    let mut vars = load(ctx);
    vars.insert(normalize_name(name), value.to_string());
    store(ctx, &vars);
}

/// Resolves a previously bound variable, or `None` if it was never asked.
#[must_use]
pub fn resolve(ctx: &Context, name: &str) -> Option<String> {
    load(ctx).get(&normalize_name(name)).cloned()
}

fn normalize_name(name: &str) -> String {
    name.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bound_value_resolves_back() {
        let mut ctx = Context::new();
        bind(&mut ctx, "code", "123456");
        assert_eq!(resolve(&ctx, "code"), Some("123456".to_string()));
    }

    #[test]
    fn an_unbound_name_resolves_to_none() {
        let ctx = Context::new();
        assert_eq!(resolve(&ctx, "code"), None);
    }

    #[test]
    fn binding_is_case_and_whitespace_insensitive() {
        let mut ctx = Context::new();
        bind(&mut ctx, "  Code  ", "123456");
        assert_eq!(resolve(&ctx, "code"), Some("123456".to_string()));
        assert_eq!(resolve(&ctx, "CODE"), Some("123456".to_string()));
    }

    #[test]
    fn rebinding_the_same_name_overwrites_the_previous_value() {
        let mut ctx = Context::new();
        bind(&mut ctx, "code", "111111");
        bind(&mut ctx, "code", "222222");
        assert_eq!(resolve(&ctx, "code"), Some("222222".to_string()));
    }

    #[test]
    fn multiple_bindings_coexist() {
        let mut ctx = Context::new();
        bind(&mut ctx, "code", "123456");
        bind(&mut ctx, "phone", "0701234567");
        assert_eq!(resolve(&ctx, "code"), Some("123456".to_string()));
        assert_eq!(resolve(&ctx, "phone"), Some("0701234567".to_string()));
    }

    #[test]
    fn bindings_survive_a_load_round_trip_via_the_fact() {
        let mut ctx = Context::new();
        bind(&mut ctx, "code", "123456");
        let loaded = load(&ctx);
        assert_eq!(loaded.get("code"), Some(&"123456".to_string()));
    }
}
