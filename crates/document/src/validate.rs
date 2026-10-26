// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Is an OCR answer for one page acceptable?
//!
//! Deterministic checks on the text a model returned. A failed check makes the
//! pipeline retry the page at a higher resolution; what is left after the
//! retry budget is recorded as a warning on the page.

use std::collections::HashSet;

/// What is wrong with an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Problem {
    /// The model hit its output limit mid-page.
    Cutoff,
    /// Nothing came back for a page that has content.
    Empty,
    /// The model fell into a loop.
    Repetition,
    /// Control tokens of the model leaked into the text.
    SpecialTokens,
    /// Much of the page's native text is missing from the answer.
    LowCoverage,
    /// Numbers and codes of the page's text layer are not in the answer: a
    /// misread digit in a register value or a table cell.
    NumbersDiffer,
}

impl Problem {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Cutoff => "output hit the token limit",
            Self::Empty => "empty output",
            Self::Repetition => "degenerate repetition",
            Self::SpecialTokens => "model control tokens in the output",
            Self::LowCoverage => "output misses much of the native text",
            Self::NumbersDiffer => "numbers differ from the text layer",
        }
    }
}

/// End-of-turn markers some servers leave at the end of an answer.
const END_MARKERS: &[&str] = &[
    "<|im_end|>",
    "<｜end▁of▁sentence｜>",
    "<|endoftext|>",
    "</s>",
];

/// Native words that must reappear in the answer when the page has a good
/// text layer.
const MIN_COVERAGE: f32 = 0.5;

/// Share of the text layer's numbers and codes the answer must contain.
const MIN_NUMBER_RECALL: f32 = 0.8;
/// Fewest numbers a text layer needs for the comparison to mean anything.
const MIN_NUMBERS: usize = 15;

/// Drop trailing end-of-turn markers and surrounding whitespace.
pub fn clean(answer: &str) -> String {
    let mut text = answer.trim();
    loop {
        let before = text.len();
        for marker in END_MARKERS {
            text = text.strip_suffix(marker).unwrap_or(text).trim_end();
        }
        if text.len() == before {
            return text.to_string();
        }
    }
}

/// Check a cleaned answer. `native` is the page's text layer when it is good
/// enough to compare against.
pub fn check(
    text: &str,
    cutoff: bool,
    native: Option<&str>,
    numbers_from: Option<&str>,
) -> Vec<Problem> {
    let mut problems = Vec::new();
    if cutoff {
        problems.push(Problem::Cutoff);
    }
    if text.is_empty() {
        problems.push(Problem::Empty);
        return problems;
    }
    if has_special_tokens(text) {
        problems.push(Problem::SpecialTokens);
    }
    if repeats(text) {
        problems.push(Problem::Repetition);
    }
    if let Some(native) = native {
        if coverage(native, text).is_some_and(|c| c < MIN_COVERAGE) {
            problems.push(Problem::LowCoverage);
        }
    }
    if let Some(native) = numbers_from {
        if number_recall(native, text).is_some_and(|r| r < MIN_NUMBER_RECALL) {
            problems.push(Problem::NumbersDiffer);
        }
    }
    problems
}

fn has_special_tokens(text: &str) -> bool {
    for (open, close) in [("<|", "|>"), ("<｜", "｜>")] {
        let mut rest = text;
        while let Some(at) = rest.find(open) {
            let after = &rest[at + open.len()..];
            if after.chars().take(40).collect::<String>().contains(close) {
                return true;
            }
            rest = after;
        }
    }
    false
}

/// A line repeated many times in a row, or a short period repeating through
/// the tail.
fn repeats(text: &str) -> bool {
    let mut run = 1;
    let mut previous = "";
    for line in text.lines().map(str::trim) {
        let structural = line.chars().all(|c| "|-: ".contains(c));
        if line.chars().count() >= 8 && !structural && line == previous {
            run += 1;
            if run >= 8 {
                return true;
            }
        } else {
            run = 1;
        }
        previous = line;
    }
    let chars: Vec<char> = text.chars().collect();
    (3..=120).any(|period| {
        let need = period * 6;
        chars.len() >= need && {
            let tail = &chars[chars.len() - need..];
            tail.chunks(period).all(|c| c == &tail[..period])
                && tail[..period].iter().any(|c| c.is_alphanumeric())
        }
    })
}

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(str::to_lowercase)
}

/// Share of the native text's distinct words that the answer contains;
/// `None` when the native text has too few to judge.
pub fn coverage(native: &str, answer: &str) -> Option<f32> {
    let wanted: HashSet<String> = words(native).collect();
    if wanted.len() < 20 {
        return None;
    }
    let have: HashSet<String> = words(answer).collect();
    Some(wanted.iter().filter(|w| have.contains(*w)).count() as f32 / wanted.len() as f32)
}

/// Tokens with a digit and at least three characters: register values, bit
/// patterns, part numbers, years, amounts.
fn number_tokens(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 3 && t.chars().any(|c| c.is_ascii_digit()))
        .map(str::to_lowercase)
        .collect()
}

/// Share of the text layer's numbers found in the answer; `None` when the
/// layer has too few to judge by.
pub fn number_recall(native: &str, answer: &str) -> Option<f32> {
    let wanted = number_tokens(native);
    if wanted.len() < MIN_NUMBERS {
        return None;
    }
    let have = number_tokens(answer);
    Some(wanted.iter().filter(|t| have.contains(*t)).count() as f32 / wanted.len() as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaked_grounding_tokens_and_end_markers() {
        assert_eq!(clean("hello <|im_end|> "), "hello");
        let grounded = "<|ref|>title<|/ref|><|det|>[[1,2,3,4]]<|/det|>";
        assert_eq!(
            check(grounded, false, None, None),
            vec![Problem::SpecialTokens]
        );
        assert!(check("a < b and c |> d", false, None, None).is_empty());
    }

    #[test]
    fn loops_are_found_but_table_rules_are_not() {
        let looped = format!("intro\n{}", "same line again\n".repeat(10));
        assert_eq!(check(&looped, false, None, None), vec![Problem::Repetition]);
        let tail = format!("start {}", "and so on ".repeat(12));
        assert_eq!(check(&tail, false, None, None), vec![Problem::Repetition]);
        let table = "| a | b |\n|---|---|\n".to_string() + &"|---|---|\n".repeat(10);
        assert!(check(&table, false, None, None).is_empty());
    }

    #[test]
    fn cutoff_empty_and_coverage() {
        assert_eq!(check("text", true, None, None), vec![Problem::Cutoff]);
        assert_eq!(check("", false, None, None), vec![Problem::Empty]);
        let native: String = (0..40).map(|i| format!("word{i}x ")).collect();
        assert_eq!(
            check(
                "completely different content here",
                false,
                Some(&native),
                None
            ),
            vec![Problem::LowCoverage]
        );
        assert!(check(&native, false, Some(&native), None).is_empty());
    }

    #[test]
    fn misread_digits_are_caught_by_comparison_with_the_text_layer() {
        let native: String = (0..30)
            .map(|i| format!("0x{:04X} ", 0x4002_0000u32 + i))
            .collect();
        let misread = native.replace("0x4002", "0xD002");
        assert_eq!(
            check(&misread, false, None, Some(&native)),
            vec![Problem::NumbersDiffer]
        );
        assert!(check(&native, false, None, Some(&native)).is_empty());
        // A layer with few numbers proves nothing.
        assert!(check("anything", false, None, Some("page 3 of 9")).is_empty());
    }
}
