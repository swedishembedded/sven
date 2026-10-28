// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent tool output
// bounded and recoverable for its clients. If your team needs expertise in
// token-efficient command output for LLM agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! What the model sees of a finished command: a bounded slice of its output
//! and a short footer.
//!
//! The command's whole output lives in an output buffer; this module only
//! decides which of its lines to show and words the footer that says what was
//! left out and how to get it. The result fits [`Limits`] including the
//! footer, so the later global truncation (`OutputCategory::Bounded`) has
//! nothing to cut and the footer, with the buffer handle and the exit code,
//! always arrives intact.

use std::time::Duration;

/// Lines shown unless the call names another count.
pub(super) const DEFAULT_VISIBLE_LINES: usize = 80;
/// Most lines a call may ask to see.
pub(super) const MAX_VISIBLE_LINES: usize = 1000;
/// Visible bytes, footer included, unless a call asks for more lines.
pub(super) const DEFAULT_VISIBLE_BYTES: usize = 8 * 1024;
/// Most visible bytes, footer included, whatever a call asks for.
pub(super) const MAX_VISIBLE_BYTES: usize = 64 * 1024;
/// A line longer than this is cut; the rest stays in the buffer.
pub(super) const MAX_LINE_BYTES: usize = 2048;

/// Which lines of a long output are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OutputMode {
    /// A quarter from the start, the rest from the end: the start names what
    /// ran, the end holds the errors and the summary.
    Auto,
    Head,
    Tail,
    HeadTail,
}

impl OutputMode {
    pub(super) const NAMES: &'static str = "auto, head, tail, head_tail";

    pub(super) fn parse(name: &str) -> Option<Self> {
        match name {
            "auto" => Some(Self::Auto),
            "head" => Some(Self::Head),
            "tail" => Some(Self::Tail),
            "head_tail" => Some(Self::HeadTail),
            _ => None,
        }
    }

    /// How many of `visible` lines come from the start and from the end.
    fn split(self, visible: usize) -> (usize, usize) {
        match self {
            Self::Auto => (visible / 4, visible - visible / 4),
            Self::Head => (visible, 0),
            Self::Tail => (0, visible),
            Self::HeadTail => (visible / 2, visible - visible / 2),
        }
    }
}

/// How much of the output may be shown.
#[derive(Debug, Clone, Copy)]
pub(super) struct Limits {
    pub(super) lines: usize,
    /// Bytes for the whole result, footer included.
    pub(super) bytes: usize,
    pub(super) mode: OutputMode,
}

/// What the output buffer holds, as far as rendering needs it.
pub(super) struct Captured {
    /// The first lines the buffer retained (at least [`Limits::lines`] of
    /// them when it has that many).
    pub(super) head: Vec<String>,
    /// The last lines the command wrote, as far as they were kept.
    pub(super) tail: Vec<String>,
    pub(super) observed_lines: usize,
    pub(super) retained_lines: usize,
    pub(super) observed_bytes: usize,
    pub(super) retained_bytes: usize,
}

/// How the footer words the end of the command and the way to the buffer.
pub(super) struct Footer<'a> {
    /// After `Exit: `, for example `0` or `timed out`.
    pub(super) exit: String,
    pub(super) duration: Duration,
    /// An extra line for the model, such as how to retry after a timeout.
    pub(super) note: Option<String>,
    /// The buffer handle and how to read it; `None` when the model has no way
    /// to read a buffer.
    pub(super) buffer: Option<BufferAccess<'a>>,
}

pub(super) struct BufferAccess<'a> {
    pub(super) handle: &'a str,
}

/// Render `captured` under `limits` with the footer.
pub(super) fn render(captured: &Captured, limits: Limits, footer: &Footer<'_>) -> String {
    let total = captured.observed_lines;
    let visible = limits.lines.clamp(1, MAX_VISIBLE_LINES);
    let (want_head, want_tail) = if total <= visible {
        (total, 0)
    } else {
        limits.mode.split(visible)
    };
    let mut head: Vec<String> = captured.head.iter().take(want_head).cloned().collect();
    let mut tail: Vec<String> =
        captured.tail[captured.tail.len().saturating_sub(want_tail)..].to_vec();
    let mut cut_lines = 0;
    for line in head.iter_mut().chain(tail.iter_mut()) {
        if cut_line(line) {
            cut_lines += 1;
        }
    }

    // Drop lines from the longer side until body and footer fit the budget.
    // Sizes are tracked as running sums: a 1,000-line result must not be
    // re-assembled once per dropped line.
    let mut body_bytes: usize = head.iter().chain(tail.iter()).map(|l| l.len() + 1).sum();
    loop {
        let shown = head.len() + tail.len();
        let text = footer_text(captured, footer, shown, head.len(), cut_lines);
        let marker = if shown < total { MARKER_RESERVE } else { 0 };
        if body_bytes + marker + text.len() + HEADING_RESERVE <= limits.bytes || shown == 0 {
            break;
        }
        let dropped = if head.len() >= tail.len() {
            head.pop()
        } else {
            Some(tail.remove(0))
        };
        body_bytes -= dropped.map_or(0, |l| l.len() + 1);
    }

    let shown = head.len() + tail.len();
    let truncated =
        shown < total || cut_lines > 0 || captured.observed_bytes > captured.retained_bytes;
    let text = footer_text(captured, footer, shown, head.len(), cut_lines);
    let mut out = String::with_capacity(body_bytes + MARKER_RESERVE + text.len() + 2);
    for line in &head {
        out.push_str(line);
        out.push('\n');
    }
    if shown < total {
        out.push_str(&format!(
            "[... {} lines omitted ...]\n",
            group(total - shown)
        ));
    }
    for line in &tail {
        out.push_str(line);
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n');
    }
    if truncated {
        out.push_str("[output truncated]\n");
    }
    out.push_str(&text);
    out
}

/// Room kept for the `[... N lines omitted ...]` line.
const MARKER_RESERVE: usize = 40;

/// Room kept for the blank line and the `[output truncated]` heading.
const HEADING_RESERVE: usize = 24;

/// Cut `line` to [`MAX_LINE_BYTES`] on a character boundary; whether it was cut.
fn cut_line(line: &mut String) -> bool {
    if line.len() <= MAX_LINE_BYTES {
        return false;
    }
    let omitted = line.len() - line.floor_char_boundary(MAX_LINE_BYTES);
    line.truncate(line.floor_char_boundary(MAX_LINE_BYTES));
    line.push_str(&format!("[... +{} bytes]", group(omitted)));
    true
}

/// The footer lines (without the `[output truncated]` heading, which the
/// caller adds when it applies) for a result showing `shown` lines.
fn footer_text(
    captured: &Captured,
    footer: &Footer<'_>,
    shown: usize,
    head_shown: usize,
    cut_lines: usize,
) -> String {
    let total = captured.observed_lines;
    let discarded = captured.observed_bytes > captured.retained_bytes;
    let truncated = shown < total || cut_lines > 0 || discarded;
    let mut out = String::new();
    if truncated {
        out.push_str(&format!(
            "Lines: {} total, {} shown, {} omitted",
            group(total),
            group(shown),
            group(total - shown)
        ));
        if cut_lines > 0 {
            out.push_str(&format!(
                " ({} cut at {} bytes)",
                group(cut_lines),
                group(MAX_LINE_BYTES)
            ));
        }
        out.push_str(" | Bytes: ");
        if discarded {
            out.push_str(&format!(
                "{} observed, {} retained",
                group(captured.observed_bytes),
                group(captured.retained_bytes)
            ));
        } else {
            out.push_str(&format!("{} captured", group(captured.observed_bytes)));
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "Exit: {} | Duration: {:.1}s",
        footer.exit,
        footer.duration.as_secs_f32()
    ));
    if let (true, Some(buffer)) = (truncated, &footer.buffer) {
        out.push_str(&format!(" | Buffer: {}\n", buffer.handle));
        out.push_str(&advice(buffer.handle, captured, shown, head_shown));
    }
    if let Some(note) = &footer.note {
        out.push('\n');
        out.push_str(note);
    }
    out
}

/// The line telling the model how to reach what was left out.
fn advice(handle: &str, captured: &Captured, shown: usize, head_shown: usize) -> String {
    let grep = format!("task(action=\"grep\", handle=\"{handle}\", pattern=\"error|FAILED\")");
    // The first omitted block, in the lines the buffer kept, in a span small
    // enough to be worth reading in one call.
    let tail_shown = shown - head_shown;
    let first = head_shown + 1;
    let last = captured
        .observed_lines
        .saturating_sub(tail_shown)
        .min(captured.retained_lines)
        .min(first + 199);
    if shown < captured.observed_lines && first <= last {
        format!(
            "Use {grep} or task(action=\"read\", handle=\"{handle}\", start_line={first}, end_line={last})."
        )
    } else {
        format!("Use {grep}.")
    }
}

/// `1234567` as `1,234,567`.
fn group(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(n: usize) -> Vec<String> {
        (1..=n).map(|i| format!("line {i}")).collect()
    }

    fn captured(lines: &[String]) -> Captured {
        let n = lines.len();
        let bytes = lines.iter().map(|l| l.len() + 1).sum();
        Captured {
            head: lines.iter().take(MAX_VISIBLE_LINES).cloned().collect(),
            tail: lines[n.saturating_sub(MAX_VISIBLE_LINES)..].to_vec(),
            observed_lines: n,
            retained_lines: n,
            observed_bytes: bytes,
            retained_bytes: bytes,
        }
    }

    fn limits(mode: OutputMode) -> Limits {
        Limits {
            lines: DEFAULT_VISIBLE_LINES,
            bytes: DEFAULT_VISIBLE_BYTES,
            mode,
        }
    }

    fn footer(buffer: bool) -> Footer<'static> {
        Footer {
            exit: "0".into(),
            duration: Duration::from_millis(18_400),
            note: None,
            buffer: buffer.then_some(BufferAccess { handle: "buf_0042" }),
        }
    }

    #[test]
    fn short_output_is_the_output_and_one_exit_line() {
        let out = render(
            &captured(&numbered(3)),
            limits(OutputMode::Auto),
            &footer(true),
        );
        assert_eq!(out, "line 1\nline 2\nline 3\n\nExit: 0 | Duration: 18.4s");
    }

    #[test]
    fn no_output_is_only_the_exit_line() {
        let out = render(&captured(&[]), limits(OutputMode::Auto), &footer(true));
        assert_eq!(out, "Exit: 0 | Duration: 18.4s");
    }

    #[test]
    fn auto_shows_a_quarter_head_and_the_rest_tail_and_counts_exactly() {
        let out = render(
            &captured(&numbered(8_420)),
            limits(OutputMode::Auto),
            &footer(true),
        );
        assert!(out.starts_with("line 1\n") && out.contains("line 20\n"));
        assert!(!out.contains("line 21\n") && !out.contains("line 8360\n"));
        assert!(out.contains("line 8361\n") && out.contains("line 8420\n"));
        assert!(out.contains("[... 8,340 lines omitted ...]"), "{out}");
        assert!(
            out.contains(
                "[output truncated]\nLines: 8,420 total, 80 shown, 8,340 omitted | Bytes: "
            ),
            "{out}"
        );
        assert!(out.contains("Exit: 0 | Duration: 18.4s | Buffer: buf_0042\n"));
        assert!(
            out.ends_with(
                "task(action=\"read\", handle=\"buf_0042\", start_line=21, end_line=220)."
            ),
            "{out}"
        );
    }

    #[test]
    fn head_tail_and_single_sided_modes() {
        let c = captured(&numbered(500));
        let head = render(&c, limits(OutputMode::Head), &footer(true));
        assert!(head.contains("line 80\n") && !head.contains("line 81\n"));
        let tail = render(&c, limits(OutputMode::Tail), &footer(true));
        assert!(tail.contains("line 421\n") && !tail.contains("line 420\n"));
        let both = render(&c, limits(OutputMode::HeadTail), &footer(true));
        assert!(both.contains("line 40\n") && !both.contains("line 41\n"));
        assert!(both.contains("line 461\n") && !both.contains("line 460\n"));
    }

    #[test]
    fn the_byte_budget_covers_the_footer_and_keeps_both_ends() {
        let wide: Vec<String> = (0..1000)
            .map(|i| format!("{i:04} {}", "w".repeat(200)))
            .collect();
        let out = render(&captured(&wide), limits(OutputMode::Auto), &footer(true));
        assert!(out.len() <= DEFAULT_VISIBLE_BYTES, "{} bytes", out.len());
        assert!(out.starts_with("0000 ") && out.contains("0999 "));
        assert!(out.contains("Buffer: buf_0042"), "footer survives: {out}");
    }

    #[test]
    fn a_long_line_is_cut_on_a_character_boundary() {
        for unit in ["é", "€", "😀", "a"] {
            let line = unit.repeat(5_000);
            let out = render(&captured(&[line]), limits(OutputMode::Auto), &footer(true));
            assert!(out.len() < MAX_LINE_BYTES + 400, "{} bytes", out.len());
            assert!(out.contains("[... +"), "{out}");
            assert!(out.contains("(1 cut at 2,048 bytes)"), "{out}");
        }
    }

    #[test]
    fn discarded_bytes_are_reported_as_observed_against_retained() {
        let mut c = captured(&numbered(10));
        c.observed_lines = 5_000;
        c.observed_bytes = 90_000;
        c.retained_bytes = 60;
        let out = render(&c, limits(OutputMode::Head), &footer(true));
        assert!(out.contains("Bytes: 90,000 observed, 60 retained"), "{out}");
    }

    #[test]
    fn a_reader_less_footer_names_no_buffer() {
        let out = render(
            &captured(&numbered(500)),
            limits(OutputMode::Auto),
            &footer(false),
        );
        assert!(out.contains("Exit: 0 | Duration: 18.4s"));
        assert!(!out.contains("buf_0042") && !out.contains("Use "), "{out}");
    }

    #[test]
    fn digits_are_grouped() {
        assert_eq!(group(0), "0");
        assert_eq!(group(999), "999");
        assert_eq!(group(1_000), "1,000");
        assert_eq!(group(1_234_567), "1,234,567");
    }
}
