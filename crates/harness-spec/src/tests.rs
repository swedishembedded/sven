// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Unit tests for the expression and template layer.

use crate::expr::{is_truthy, Expr, Path, Template};

fn resolver(path: &Path) -> Option<serde_json::Value> {
    match path.as_text().as_str() {
        "facts.flipped" => Some(serde_json::Value::Bool(true)),
        "inputs.count" => Some(serde_json::json!(3)),
        "inputs.name" => Some(serde_json::Value::String("sven".into())),
        _ => None,
    }
}

#[test]
fn templates_parse_and_render_utf8_correctly() {
    let t = Template::parse("hej {{ inputs.name }} — {{facts.flipped}}!").expect("parses");
    assert_eq!(t.paths().len(), 2);
    assert_eq!(
        t.render(&resolver),
        "hej sven — true!",
        "multi-byte UTF-8 around holes must survive"
    );
}

#[test]
fn an_unclosed_hole_is_a_parse_error() {
    assert!(Template::parse("open {{ facts.x").is_err());
}

#[test]
fn a_single_hole_carries_its_path() {
    let t = Template::parse("{{ inputs.count }}").expect("parses");
    assert!(t.is_single_hole());
    assert_eq!(
        t.single_hole().map(Path::as_text).as_deref(),
        Some("inputs.count")
    );
    // A composite template does not carry types.
    assert!(!Template::parse("x{{ inputs.count }}")
        .expect("parses")
        .is_single_hole());
}

#[test]
fn paths_reject_empty_segments() {
    assert!(Path::parse("inputs..x").is_err());
    assert!(Path::parse("").is_err());
    assert!(Path::parse("facts.ok").is_ok());
}

#[test]
fn truthiness_follows_the_documented_rule() {
    assert!(!is_truthy(&serde_json::Value::Null));
    assert!(!is_truthy(&serde_json::json!(false)));
    assert!(!is_truthy(&serde_json::json!("")));
    assert!(!is_truthy(&serde_json::json!([])));
    assert!(!is_truthy(&serde_json::json!({})));
    assert!(is_truthy(&serde_json::json!(0)), "zero is data, not false");
    assert!(is_truthy(&serde_json::json!("x")));
    assert!(is_truthy(&serde_json::json!(true)));
}

#[test]
fn expressions_parse_from_document_forms() {
    let resolve = |p: &_| resolver(p);
    // Plain path, truthy.
    let e = Expr::parse_value(&serde_json::json!("facts.flipped")).unwrap();
    assert!(e.eval(&resolve));
    // Template form.
    let e = Expr::parse_value(&serde_json::json!("{{ facts.flipped }}")).unwrap();
    assert!(e.eval(&resolve));
    // Negation.
    let e = Expr::parse_value(&serde_json::json!("!inputs.missing")).unwrap();
    assert!(e.eval(&resolve));
    // Comparison.
    let e = Expr::parse_value(&serde_json::json!({"eq": "inputs.count", "value": 3})).unwrap();
    assert!(e.eval(&resolve));
    let e = Expr::parse_value(&serde_json::json!({"ne": "inputs.count", "value": 3})).unwrap();
    assert!(!e.eval(&resolve));
    // Combinators.
    let e = Expr::parse_value(&serde_json::json!({
        "all": ["facts.flipped", {"eq": "inputs.count", "value": 3}]
    }))
    .unwrap();
    assert!(e.eval(&resolve));
    // Unknown form rejected.
    assert!(Expr::parse_value(&serde_json::json!({"teleport": 1})).is_err());
}

#[test]
fn expressions_round_trip_through_json_for_the_plan() {
    let e = Expr::parse_value(&serde_json::json!({"any": ["facts.flipped", "!inputs.missing"]}))
        .unwrap();
    let back = serde_json::to_value(&e).unwrap();
    let again = Expr::parse_value(&back).unwrap();
    assert_eq!(e, again, "plan serialization must be lossless");
}
