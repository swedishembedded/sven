// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Character error rate between a reference text and a hypothesis, used to
//! score conversions against ground truth.

/// Edit distance between the whitespace-normalised texts divided by the
/// reference length. `None` when the reference is empty.
pub fn character_error_rate(reference: &str, hypothesis: &str) -> Option<f64> {
    let normalise = |s: &str| {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .collect::<Vec<_>>()
    };
    let (r, h) = (normalise(reference), normalise(hypothesis));
    if r.is_empty() {
        return None;
    }
    Some(levenshtein(&r, &h) as f64 / r.len() as f64)
}

fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(ca != cb))
                .min(above + 1)
                .min(row[j] + 1);
            diagonal = above;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_is_ignored_and_errors_are_counted() {
        assert_eq!(character_error_rate("a  b\nc", "a b c"), Some(0.0));
        assert_eq!(character_error_rate("abcd", "abxd"), Some(0.25));
        assert_eq!(character_error_rate("", "x"), None);
    }
}
