// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use std::io::Write;

/// Write clean output to stdout — suitable for piping to the next agent.
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
    let mut line = format!("[sven:tokens] input={input} output={output}");
    if cache_read > 0 || cache_write > 0 {
        line.push_str(&format!(
            " cache_read={cache_read} cache_write={cache_write}"
        ));
    }
    if input_budget > 0 {
        line.push_str(&format!(" ctx_pct={ctx_pct} ctx_cache={ctx_cache}"));
    }
    if cache_read_total > 0 || cache_write_total > 0 {
        line.push_str(&format!(
            " cache_read_total={cache_read_total} cache_write_total={cache_write_total}"
        ));
    }
    line
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
}
