// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in tool retrieval for
// large agent tool sets then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Turning text into the terms that are compared: the one place that decides
//! what counts as the same word.

/// Words that say nothing about which tool is wanted.
const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "any", "are", "as", "at", "be", "by", "can", "do", "for", "from", "how", "i",
    "in", "into", "is", "it", "its", "me", "my", "need", "of", "on", "or", "should", "that", "the",
    "then", "this", "to", "tool", "use", "want", "when", "which", "with", "you", "your",
];

/// The terms of `text`, in order of appearance: lowercased runs of letters and
/// digits (so `read_file`, `read-file` and `readFile` all give `read` and
/// `file`) with plural and verb endings removed, and common words dropped.
pub fn terms(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut previous_lower = false;
    for c in text.chars() {
        if !c.is_alphanumeric() {
            flush(&mut word, &mut words);
            previous_lower = false;
            continue;
        }
        // `readFile` is two words.
        if c.is_uppercase() && previous_lower {
            flush(&mut word, &mut words);
        }
        previous_lower = c.is_lowercase() || c.is_ascii_digit();
        word.extend(c.to_lowercase());
    }
    flush(&mut word, &mut words);
    words
}

fn flush(word: &mut String, words: &mut Vec<String>) {
    if word.is_empty() {
        return;
    }
    let token = std::mem::take(word);
    if !STOP_WORDS.contains(&token.as_str()) {
        words.push(undouble(stem(token)));
    }
}

/// Removes a plural or verb ending: enough to make `searches`, `searching`
/// and `search` one term, and no more.
fn stem(token: String) -> String {
    // `min_stem` keeps short words whole: `string` is not `str` + `ing`.
    let strip = |suffix: &str, replacement: &str, min_stem: usize| -> Option<String> {
        token
            .strip_suffix(suffix)
            .filter(|rest| rest.chars().count() >= min_stem)
            .map(|rest| format!("{rest}{replacement}"))
    };
    strip("ies", "y", 3)
        .or_else(|| strip("sses", "ss", 2))
        .or_else(|| strip("ing", "", 4))
        .or_else(|| strip("ed", "", 3))
        .or_else(|| strip("es", "", 3).filter(|s| s.ends_with(['s', 'x', 'z', 'h'])))
        .or_else(|| {
            let keeps_s = token.ends_with("ss") || token.ends_with("us");
            (!keeps_s).then(|| strip("s", "", 3)).flatten()
        })
        .unwrap_or(token)
}

/// Reduces a doubled final consonant, so `cancel` and `cancelling` (stem
/// `cancell`) meet. Applied to both sides, so only the equivalence matters,
/// not that the result is a word.
fn undouble(mut stem: String) -> String {
    if stem.chars().count() > 3 {
        let mut last = stem.chars().rev();
        if let (Some(a), Some(b)) = (last.next(), last.next()) {
            if a == b && a.is_alphabetic() && !"aeiou".contains(a) {
                stem.pop();
            }
        }
    }
    stem
}

/// The parts of `query` that may be a tool's name written out: the whole
/// query, and each stretch inside quotes or backticks, as sorted distinct
/// terms.
pub fn named_spans(query: &str) -> Vec<Vec<String>> {
    let mut spans = vec![query];
    for quote in ['"', '\'', '`'] {
        spans.extend(query.split(quote).skip(1).step_by(2));
    }
    let mut named: Vec<Vec<String>> = spans
        .into_iter()
        .map(|span| {
            let mut found = terms(span);
            found.sort();
            found.dedup();
            found
        })
        .filter(|found| !found.is_empty())
        .collect();
    named.dedup();
    named
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_styles_and_endings_give_the_same_terms() {
        assert_eq!(terms("read_file"), ["read", "file"]);
        assert_eq!(terms("read-file"), ["read", "file"]);
        assert_eq!(terms("readFile"), ["read", "file"]);
        assert_eq!(terms("Searches files"), terms("search file"));
        assert_eq!(terms("searching"), terms("search"));
    }

    #[test]
    fn short_words_and_double_s_are_not_mangled() {
        assert_eq!(terms("class"), terms("classes"));
        assert_eq!(terms("bus"), ["bus"]);
        assert_eq!(terms("is"), Vec::<String>::new());
    }

    #[test]
    fn quoted_stretches_are_candidate_names() {
        let spans = named_spans("load the \"read_file\" tool, or `grep`");
        assert!(spans.contains(&vec!["file".to_string(), "read".to_string()]));
        assert!(spans.contains(&vec!["grep".to_string()]));
        assert_eq!(named_spans("read_file").len(), 1);
        assert!(named_spans("the of").is_empty());
    }

    #[test]
    fn a_doubled_final_consonant_is_reduced() {
        let family = ["cancel", "cancelling", "cancelled", "cancels"];
        let folded: Vec<_> = family.iter().map(|w| terms(w)).collect();
        assert!(folded.windows(2).all(|w| w[0] == w[1]), "{folded:?}");
        assert_eq!(terms("class"), terms("classes"));
        assert_eq!(terms("bus"), ["bus"]);
    }
}
