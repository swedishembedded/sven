// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Deriving a short, display-safe session title from free-form text.

/// Sanitizes an LLM-generated chat title. The model may return code blocks,
/// multi-line output, or other unsuitable content. Returns a short, display-safe title.
pub fn sanitize_llm_title(raw: &str) -> String {
    let s = raw.trim();
    // Take first line only - avoid code blocks or multi-line output.
    let first_line = s.lines().next().unwrap_or(s).trim();
    // Strip markdown code block markers (```lang or ```).
    let stripped = first_line
        .strip_prefix("```")
        .map(|t| t.trim_start_matches(char::is_alphanumeric).trim())
        .unwrap_or(first_line);
    let stripped = stripped.strip_suffix("```").unwrap_or(stripped).trim();
    // Take first sentence or up to 60 chars.
    let end = stripped
        .char_indices()
        .find(|(_, c)| matches!(*c, '.' | '!' | '?' | '\n'))
        .map(|(i, _)| i + 1)
        .unwrap_or(stripped.len());
    let out: String = stripped.chars().take(end.min(60)).collect();
    let out = out.trim().trim_matches('"').trim();
    if out.is_empty()
        || out
            .chars()
            .all(|c| c.is_ascii_punctuation() || c.is_whitespace())
    {
        "Chat".to_string()
    } else {
        out.to_string()
    }
}

/// Derives a human-readable title (capitalised, up to ~80 chars) from a
/// free-form text string - used as a session's default title before the
/// model's own (via [`sanitize_llm_title`]) is available.
pub fn make_title(text: &str) -> String {
    // Take up to the first sentence (stop at '.', '!', '?') or 80 chars.
    let trimmed = text.trim();
    let sentence_end = trimmed
        .char_indices()
        .find(|(_, c)| matches!(*c, '.' | '!' | '?'))
        .map(|(i, _)| i + 1)
        .unwrap_or(trimmed.len());
    let raw: String = trimmed.chars().take(sentence_end.min(80)).collect();
    let raw = raw.trim_end_matches(['.', '!', '?']).trim();
    if raw.is_empty() {
        return "Conversation".to_string();
    }
    // Capitalise first character.
    let mut chars = raw.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_title_takes_first_sentence() {
        assert_eq!(make_title("fix the bug. then ship it"), "Fix the bug");
    }

    #[test]
    fn make_title_empty_falls_back() {
        assert_eq!(make_title("   "), "Conversation");
    }

    #[test]
    fn sanitize_llm_title_passes_through_plain_text() {
        assert_eq!(
            sanitize_llm_title("Fix the flaky test"),
            "Fix the flaky test"
        );
    }

    #[test]
    fn sanitize_llm_title_empty_falls_back() {
        assert_eq!(sanitize_llm_title("..."), "Chat");
    }
}
