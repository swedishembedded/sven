// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in tool-definition
// budgets for agents then you can procure our services by sending an email
// to info@swedishembedded.com.
//! One-line tool summaries.
//!
//! A tool whose full definition is not in the request is known to the model by
//! one line. [`crate::Tool::summary`] states it; [`one_line_summary`] derives
//! one from free text for tools that come from elsewhere (an MCP server's
//! description) and for tools that predate the method.

/// The longest a [`crate::Tool::summary`] may be, in characters.
pub const SUMMARY_MAX_CHARS: usize = 100;

/// The first sentence of `text` on one line, cut to at most `max_chars`
/// characters (a cut ends in `...`).
///
/// A sentence ends at the first `.`, `!` or `?` followed by whitespace, or at
/// the first blank line; runs of whitespace collapse to one space. The cut
/// never splits a character.
#[must_use]
pub fn one_line_summary(text: &str, max_chars: usize) -> String {
    let paragraph = text.trim().split("\n\n").next().unwrap_or_default();
    let collapsed = paragraph.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut sentence = collapsed.as_str();
    let chars: Vec<(usize, char)> = collapsed.char_indices().collect();
    for window in chars.windows(2) {
        let [(at, c), (_, next)] = [window[0], window[1]];
        if matches!(c, '.' | '!' | '?') && next == ' ' {
            sentence = &collapsed[..at + c.len_utf8()];
            break;
        }
    }
    if sentence.chars().count() <= max_chars {
        return sentence.to_string();
    }
    let keep = max_chars.saturating_sub(3);
    let cut: String = sentence.chars().take(keep).collect();
    format!("{}...", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_sentence_is_the_summary() {
        assert_eq!(
            one_line_summary("Fetch a page.\nReturns text. More.", 100),
            "Fetch a page."
        );
    }

    #[test]
    fn an_over_long_sentence_is_cut_on_a_character_boundary() {
        let text = "é".repeat(150);
        let s = one_line_summary(&text, 100);
        assert_eq!(s.chars().count(), 100);
        assert!(s.ends_with("..."));
    }

    #[test]
    fn a_dotted_token_is_not_a_sentence_end() {
        assert_eq!(
            one_line_summary("Reads v1.2 of the config file", 100),
            "Reads v1.2 of the config file"
        );
    }

    #[test]
    fn nothing_yields_nothing() {
        assert_eq!(one_line_summary("  \n ", 100), "");
    }
}
