// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents whose every step a person can follow
// for its clients. If your team needs expertise in observable, explainable
// agent runs then you can procure our services by sending an email to
// info@swedishembedded.com.

//! A tool call's intent: a few words the model gives each call, saying what
//! it is for ("List the files in the project root"), shown to the person as
//! the call's label wherever its steps are listed.
//!
//! Every tool offered to a model carries the optional argument [`INTENT`]
//! (the registry adds it to each schema that does not already have a
//! parameter of that name) and the registry takes it off again before the
//! tool runs, so no tool sees it. It is the model's wording, not the call:
//! two calls that differ only in their intent are the same call to anything
//! that compares calls ([`without_intent`]).

use serde_json::Value;

/// The argument every tool call may carry its intent in.
pub const INTENT: &str = "intent";

/// What a model is told the argument is: short, to keep its cost low.
pub const INTENT_DESCRIPTION: &str = "Short description of intent";

/// The most characters of an intent kept; a longer one is cut.
pub const MAX_INTENT_CHARS: usize = 120;

/// The intent `args` carry: trimmed, on one line, at most
/// [`MAX_INTENT_CHARS`] characters; `None` when there is none.
#[must_use]
pub fn intent_of(args: &Value) -> Option<String> {
    let text = args.get(INTENT)?.as_str()?;
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.is_empty() {
        return None;
    }
    Some(one_line.chars().take(MAX_INTENT_CHARS).collect())
}

/// `args` without the intent: what a call is, whatever it was said to be for.
#[must_use]
pub fn without_intent(args: &Value) -> Value {
    match args {
        Value::Object(map) if map.contains_key(INTENT) => {
            let mut map = map.clone();
            map.remove(INTENT);
            Value::Object(map)
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Spec: an intent is read as one trimmed line, cut to the limit, and a
    /// blank or non-text one is none.
    #[test]
    fn an_intent_is_one_short_line_or_none() {
        assert_eq!(
            intent_of(&json!({"intent": "  List the\n files  "})).as_deref(),
            Some("List the files")
        );
        assert_eq!(
            intent_of(&json!({"intent": "x".repeat(500)})).map(|s| s.chars().count()),
            Some(MAX_INTENT_CHARS)
        );
        assert_eq!(intent_of(&json!({"intent": "   "})), None);
        assert_eq!(intent_of(&json!({"intent": 3})), None);
        assert_eq!(intent_of(&json!({"path": "a"})), None);
        assert_eq!(intent_of(&json!("not an object")), None);
    }

    /// Spec: taking the intent off leaves every other argument as it was.
    #[test]
    fn without_the_intent_the_call_is_what_it_does() {
        assert_eq!(
            without_intent(&json!({"intent": "look", "path": "a"})),
            json!({"path": "a"})
        );
        assert_eq!(without_intent(&json!({"path": "a"})), json!({"path": "a"}));
        assert_eq!(without_intent(&json!([1, 2])), json!([1, 2]));
    }
}
