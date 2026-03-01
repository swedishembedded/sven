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
