// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that ask the person instead of
// guessing for its clients. If your team needs expertise in agent dialogue
// design then you can procure our services by sending an email to
// info@swedishembedded.com.
//! Whether the reactive agent's final answer closes with a question for the
//! person and the choices it offers, so the machine can ask it as a
//! question (see `reactive_agent`'s post-check). Pure text reading; it
//! invents nothing.

/// Bounds on how many alternatives make a sensible multiple-choice question.
/// Fewer is not a choice; more is a list the user should read as prose.
const MIN_ALTERNATIVES: usize = 2;
/// Upper bound, see [`MIN_ALTERNATIVES`].
const MAX_ALTERNATIVES: usize = 6;

/// Openers that mark a question as addressed to the *user* rather than
/// rhetorical. Matched case-insensitively against the question sentence.
const USER_DIRECTED_OPENERS: &[&str] = &[
    "which",
    "should i",
    "shall i",
    "do you",
    "would you",
    "could you",
    "can you",
];

/// Extract a clarification question from a final answer, or `None` when the
/// answer is resolved.
///
/// Both halves must come from the model: a question it addressed to the user,
/// and between [`MIN_ALTERNATIVES`] and [`MAX_ALTERNATIVES`] alternatives it
/// enumerated itself. Requiring both is what keeps a resolved answer - including
/// one that happens to contain a rhetorical question, or a bullet list of work
/// done - untouched.
pub(super) fn clarification_question(text: &str) -> Option<(String, Vec<String>)> {
    // Only what the model enumerated *after* asking counts. A list is the
    // answer set to a question when it follows one; a list that precedes a
    // question is the far more ordinary shape "here is what I did, shall I go
    // on", where the items answer nothing and the question is a yes/no with no
    // options at all. The two are indistinguishable by content, so the
    // conservative reading is the only honest one: this check sits on every
    // turn of the default mode, and its whole licence is that it invents
    // nothing.
    let (prompt, after_question) = user_directed_question(text)?;
    let options = enumerated_alternatives(&text[after_question..]);
    if !(MIN_ALTERNATIVES..=MAX_ALTERNATIVES).contains(&options.len()) {
        return None;
    }
    Some((prompt, options))
}

/// The first question sentence that is addressed to the user, and the byte
/// offset just past it.
fn user_directed_question(text: &str) -> Option<(String, usize)> {
    let mut start = 0;
    for (i, ch) in text.char_indices() {
        match ch {
            '?' => {
                let end = i + ch.len_utf8();
                let sentence = text[start..end].trim();
                if is_user_directed(sentence) {
                    return Some((sentence.to_string(), end));
                }
                start = end;
            }
            '.' | '!' | '\n' => start = i + ch.len_utf8(),
            _ => {}
        }
    }
    None
}

/// Whether a question sentence asks *the user* something.
fn is_user_directed(sentence: &str) -> bool {
    let s = sentence
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .to_ascii_lowercase();
    USER_DIRECTED_OPENERS.iter().any(|o| s.starts_with(o))
        || s.contains(" you ")
        || s.contains(" you?")
        || s.contains(" your ")
}

/// The alternatives the model enumerated as a `-`/`*`/`N.` list, in order and
/// deduplicated.
///
/// Called with only the text that follows the question, so a list of work
/// already done - which precedes its closing question - is never mistaken for
/// a set of answers to it.
fn enumerated_alternatives(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let item = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .map(str::trim)
            .map(str::to_string)
            .or_else(|| numbered_item(line));
        if let Some(item) = item {
            if !item.is_empty() && !out.contains(&item) {
                out.push(item);
            }
        }
    }
    out
}

/// The body of a `1. ` / `1) ` list item, if the line is one.
fn numbered_item(line: &str) -> Option<String> {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let rest = &line[digits..];
    let rest = rest
        .strip_prefix(". ")
        .or_else(|| rest.strip_prefix(") "))?;
    Some(rest.trim().to_string())
}
