// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use std::io::Write;

/// Write clean output to stdout - suitable for piping to the next agent.
pub fn write_stdout(text: &str) {
    print!("{text}");
    let _ = std::io::stdout().flush();
}

/// Write a final newline to stdout if the text didn't already end with one.
pub fn finalise_stdout(text: &str) {
    if !text.ends_with('\n') {
        println!();
    }
}

/// Write a diagnostic / error message to stderr (never pollutes stdout pipeline).
pub fn write_stderr(msg: &str) {
    eprintln!("{msg}");
}

/// Write a structured progress line to stderr.
///
/// Lines are prefixed with `[sven:...]` so CI systems can scrape them with
/// simple pattern matching without interfering with stdout conversation output.
pub fn write_progress(msg: &str) {
    eprintln!("{msg}");
}

/// Format a `[sven:tokens]` diagnostic line from a `TokenUsage` event.
///
/// Shared by both the CI runner and the conversation handler to ensure
/// identical diagnostic output format.
#[allow(clippy::too_many_arguments)]
pub fn format_token_usage_line(
    input: u32,
    output: u32,
    cache_read: u32,
    cache_write: u32,
    cache_read_total: u32,
    cache_write_total: u32,
    max_tokens: usize,
    max_output_tokens: usize,
) -> String {
    let total_ctx = input + cache_read + cache_write;
    let input_budget = if max_output_tokens > 0 {
        max_tokens.saturating_sub(max_output_tokens)
    } else {
        max_tokens
    };
    let ctx_pct = if input_budget > 0 {
        ((total_ctx as u64 * 100) / input_budget as u64).min(100) as u32
    } else {
        0
    };
    let ctx_cache = if total_ctx > 0 {
        cache_read * 100 / total_ctx
    } else {
        0
    };
    // Always show cache_read/cache_write, even at 0 - "no cache activity" is
    // itself useful signal (e.g. a provider/config that doesn't cache at
    // all), and was previously indistinguishable from "not computed".
    let mut line = format!(
        "[sven:tokens] input={input} output={output} cache_read={cache_read} cache_write={cache_write}"
    );
    if input_budget > 0 {
        line.push_str(&format!(" ctx_pct={ctx_pct} ctx_cache={ctx_cache}"));
    }
    line.push_str(&format!(
        " cache_read_total={cache_read_total} cache_write_total={cache_write_total}"
    ));
    line
}

/// Build the verbose-only ` output=…` snippet for a `[sven:tool:result]` line.
///
/// Returns an empty string at default verbosity (`trace == 0`) or for empty
/// output, so the snippet appears only at `-v`. Long output is truncated to a
/// fixed character limit with a `…[+N chars]` suffix so a large tool payload
/// never floods stderr.
///
/// Shared by `RuntimeRunner` and `CiRunner`'s event handlers — they used to
/// carry byte-identical copies of this (one a named function, one inlined).
pub fn tool_output_snippet(trace: u8, output: &str) -> String {
    const TOOL_OUTPUT_SNIPPET_LIMIT: usize = 1500;
    if trace < 1 || output.is_empty() {
        return String::new();
    }
    let preview: String = output.chars().take(TOOL_OUTPUT_SNIPPET_LIMIT).collect();
    let total = output.chars().count();
    if total > TOOL_OUTPUT_SNIPPET_LIMIT {
        format!(
            " output={:?}...[+{} chars]",
            preview,
            total - TOOL_OUTPUT_SNIPPET_LIMIT
        )
    } else {
        format!(" output={output:?}")
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalise_adds_newline_when_missing() {
        let needs_newline = !("hello".ends_with('\n'));
        let already_newline = "hello\n".ends_with('\n');
        assert!(needs_newline, "text without newline should trigger newline");
        assert!(
            already_newline,
            "text with newline should not trigger extra newline"
        );
    }

    #[test]
    fn finalise_stdout_does_not_panic_on_empty_string() {
        finalise_stdout("");
    }

    #[test]
    fn finalise_stdout_does_not_panic_with_trailing_newline() {
        finalise_stdout("already done\n");
    }

    #[test]
    fn write_stderr_does_not_panic_on_empty_message() {
        write_stderr("");
    }

    #[test]
    fn write_stdout_does_not_panic_on_empty_string() {
        write_stdout("");
    }

    #[test]
    fn write_progress_does_not_panic() {
        write_progress("[sven:step:start] 1/3 label=\"Analyse codebase\"");
    }

    // ── format_token_usage_line ────────────────────────────────────────────

    #[test]
    fn cache_read_and_write_always_shown_even_at_zero() {
        // Previously gated behind `> 0`, so "no cache activity" was
        // indistinguishable from "not computed" - always show both.
        let line = format_token_usage_line(10, 5, 0, 0, 0, 0, 0, 0);
        assert!(line.contains("cache_read=0"), "{line}");
        assert!(line.contains("cache_write=0"), "{line}");
    }

    #[test]
    fn cache_read_and_write_shown_when_nonzero() {
        let line = format_token_usage_line(10, 5, 3, 7, 0, 0, 0, 0);
        assert!(line.contains("cache_read=3"), "{line}");
        assert!(line.contains("cache_write=7"), "{line}");
    }

    #[test]
    fn cache_totals_always_shown() {
        let line = format_token_usage_line(10, 5, 3, 7, 103, 207, 0, 0);
        assert!(line.contains("cache_read_total=103"), "{line}");
        assert!(line.contains("cache_write_total=207"), "{line}");
    }

    #[test]
    fn ctx_pct_hidden_when_budget_unknown() {
        let line = format_token_usage_line(10, 5, 0, 0, 0, 0, 0, 0);
        assert!(!line.contains("ctx_pct"), "{line}");
    }

    #[test]
    fn ctx_pct_shown_when_budget_known() {
        let line = format_token_usage_line(10, 5, 0, 0, 0, 0, 2048, 512);
        assert!(line.contains("ctx_pct"), "{line}");
    }

    // ── tool_output_snippet ────────────────────────────────────────────────

    #[test]
    fn tool_output_snippet_empty_at_default_verbosity() {
        assert_eq!(tool_output_snippet(0, "some output"), "");
    }

    #[test]
    fn tool_output_snippet_truncates_long_output() {
        let long = "x".repeat(4000);
        let snippet = tool_output_snippet(1, &long);
        assert!(snippet.contains("...[+2500 chars]"), "{snippet}");
    }
}
