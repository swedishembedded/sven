// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//! Deterministic evidence that a learner's turn taught a concept: a worked
//! numeric example whose arithmetic is right, stated next to a phrase of the
//! concept. Reciting phrases, or stating a relation with wrong numbers, is not
//! evidence, so a learner cannot raise a simulated student's mastery by
//! keyword stuffing.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// The arithmetic relation a worked example of a concept must satisfy among
/// three numbers of one sentence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    /// `a + b = c`: series resistance, voltage drops around a loop, currents at a node.
    Sum,
    /// `a * b / (a + b) = c`: two resistors in parallel.
    Parallel,
    /// `a / b = c` with `b` not 1: Ohm's law solved for a quantity.
    Quotient,
}

/// How many new worked examples one learner turn can credit to one concept.
pub const MAX_NEW_PER_TURN: usize = 2;

/// Relative tolerance of a stated result.
const TOLERANCE: f64 = 0.005;

/// Every number in `text`, in order (`.` or `,` as the decimal mark).
fn numbers(text: &str) -> Vec<f64> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_digit()
                    || (matches!(chars[i], '.' | ',')
                        && chars.get(i + 1).is_some_and(char::is_ascii_digit)))
            {
                i += 1;
            }
            let raw: String = chars[start..i].iter().collect();
            if let Ok(value) = raw.replace(',', ".").parse::<f64>() {
                out.push(value);
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Sentences: split at `;`, newlines, and `.`, `!`, `?` followed by whitespace.
fn sentences(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let ends = matches!(c, ';' | '\n')
            || (matches!(c, '.' | '!' | '?')
                && chars.peek().is_none_or(|(_, n)| n.is_whitespace()));
        if ends {
            parts.push(&text[start..at]);
            start = at + c.len_utf8();
        }
    }
    parts.push(&text[start..]);
    parts
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= TOLERANCE * b.abs() + 1e-9
}

/// The identities of the examples in `sentence` that satisfy `evidence`.
fn relations(evidence: Evidence, sentence: &str) -> BTreeSet<String> {
    let n: Vec<f64> = numbers(sentence).into_iter().filter(|v| *v > 0.0).collect();
    let mut found = BTreeSet::new();
    for i in 0..n.len() {
        for j in 0..n.len() {
            if i == j {
                continue;
            }
            for k in 0..n.len() {
                if k == i || k == j {
                    continue;
                }
                let (a, b, c) = (n[i], n[j], n[k]);
                let hit = match evidence {
                    Evidence::Sum => i < j && close(a + b, c),
                    Evidence::Parallel => i < j && close(a * b / (a + b), c),
                    Evidence::Quotient => (b - 1.0).abs() > f64::EPSILON && close(a / b, c),
                };
                if hit {
                    // One example per set of three numbers, however it is read.
                    let mut triple = [a, b, c];
                    triple.sort_by(f64::total_cmp);
                    found.insert(format!(
                        "{:.4}:{:.4}:{:.4}",
                        triple[0], triple[1], triple[2]
                    ));
                }
            }
        }
    }
    found
}

/// The worked examples in `turn` not in `seen` yet, at most
/// [`MAX_NEW_PER_TURN`], from sentences that also contain one of `phrases`
/// (lower case). They are added to `seen`.
pub fn new_examples(
    evidence: Evidence,
    phrases: &[String],
    turn: &str,
    seen: &mut BTreeSet<String>,
) -> usize {
    let mut fresh = BTreeSet::new();
    for sentence in sentences(turn) {
        let lowered = sentence.to_lowercase();
        if phrases.iter().any(|p| lowered.contains(p)) {
            fresh.extend(
                relations(evidence, sentence)
                    .into_iter()
                    .filter(|r| !seen.contains(r)),
            );
        }
    }
    let taken: Vec<String> = fresh.into_iter().take(MAX_NEW_PER_TURN).collect();
    let count = taken.len();
    seen.extend(taken);
    count
}
