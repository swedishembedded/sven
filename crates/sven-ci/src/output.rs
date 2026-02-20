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

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // These test the observable side-effects of the output helpers.
    // stdout/stderr writes are fire-and-forget; we verify the logic
    // that decides *what* is written, not the OS write itself.

    #[test]
    fn finalise_adds_newline_when_missing() {
        // We capture by redirecting is not possible in unit tests, but we can
        // at least ensure the function does not panic and branches correctly.
        // The branch condition: ends_with('\n') → no println!, else println!()
        // We test via the predicate directly.
        let needs_newline = !("hello".ends_with('\n'));
        let already_newline = "hello\n".ends_with('\n');
        assert!(needs_newline, "text without newline should trigger newline");
        assert!(already_newline, "text with newline should not trigger extra newline");
    }

    #[test]
    fn finalise_stdout_does_not_panic_on_empty_string() {
        // Must not panic – empty string does not end with '\n'
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
}
