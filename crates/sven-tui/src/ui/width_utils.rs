// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Width-aware text utilities for terminal column calculations.
//!
//! All functions use standard (non-CJK) unicode display width.  East Asian
//! Ambiguous characters (such as ⚙ U+2699, ⬡ U+2B21, … U+2026) are treated
//! as width 1, matching the behaviour of most Western terminals.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Display width of a string in terminal columns (non-CJK / standard width).
#[inline]
pub fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Display width of a single character in terminal columns (non-CJK / standard width).
#[inline]
pub fn char_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// The display width of the ellipsis character `…` (U+2026).
///
/// In standard (non-CJK) mode, U+2026 is East Asian Ambiguous and treated as
/// width 1, matching most Western terminal emulators.
const ELLIPSIS_WIDTH: usize = 1;
const ELLIPSIS: &str = "…";

/// Truncate a string to fit within `max_cols` display columns.
///
/// If the string is truncated, an ellipsis (`…`) is appended so that the
/// result still fits within `max_cols` columns.  If `max_cols` is 0, returns
/// an empty string.
pub fn truncate_to_width(s: &str, max_cols: usize) -> String {
    if max_cols == 0 {
        return String::new();
    }
    let w = display_width(s);
    if w <= max_cols {
        return s.to_string();
    }
    if max_cols <= ELLIPSIS_WIDTH {
        // Not enough space for ellipsis — return empty or just the ellipsis.
        return if max_cols == ELLIPSIS_WIDTH {
            ELLIPSIS.to_string()
        } else {
            String::new()
        };
    }
    // Reserve ELLIPSIS_WIDTH columns for "…".
    let target = max_cols - ELLIPSIS_WIDTH;
    let truncated = truncate_to_width_exact(s, target);
    format!("{truncated}{ELLIPSIS}")
}

/// Truncate a string to fit within `max_cols` display columns without appending
/// an ellipsis.  The result is always `<= max_cols` columns wide.
pub fn truncate_to_width_exact(s: &str, max_cols: usize) -> String {
    if max_cols == 0 {
        return String::new();
    }
    let mut col = 0usize;
    let mut end = 0usize;
    for c in s.chars() {
        let cw = char_width(c);
        if col + cw > max_cols {
            break;
        }
        col += cw;
        end += c.len_utf8();
    }
    s[..end].to_string()
}

/// Pad `s` on the right with spaces (or truncate without ellipsis) to reach
/// exactly `cols` display columns.
pub fn fit_to_width(s: &str, cols: usize) -> String {
    let w = display_width(s);
    if w >= cols {
        truncate_to_width_exact(s, cols)
    } else {
        format!("{s}{}", " ".repeat(cols - w))
    }
}

/// Given a string and a display-column position, return the byte offset of the
/// character whose display column range contains `col`.
///
/// Used to convert mouse-reported column positions (display space) back to byte
/// indices for text selection.  If `col` is past the end of the string the
/// returned offset points one byte past the last character.
pub fn col_to_byte_offset(s: &str, col: usize) -> usize {
    let mut current_col = 0usize;
    for (byte_idx, c) in s.char_indices() {
        if current_col >= col {
            return byte_idx;
        }
        current_col += char_width(c);
    }
    s.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_width_ascii() {
        assert_eq!(display_width("hello"), 5);
    }

    #[test]
    fn test_display_width_gear() {
        // ⚙ is U+2699 GEAR — East Asian Ambiguous, treated as 1 in non-CJK mode.
        let w = display_width("⚙");
        assert_eq!(w, 1, "gear width should be 1 in non-CJK mode, got {w}");
    }

    #[test]
    fn test_truncate_to_width_short() {
        assert_eq!(truncate_to_width("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_to_width_exact_boundary() {
        assert_eq!(truncate_to_width("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_to_width_over() {
        let result = truncate_to_width("hello world", 8);
        assert_eq!(display_width(&result), 8, "result: '{result}'");
        assert!(
            result.ends_with('…'),
            "should end with ellipsis: '{result}'"
        );
    }

    #[test]
    fn test_truncate_exact_no_ellipsis() {
        let result = truncate_to_width_exact("hello world", 5);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_fit_to_width_pad() {
        let result = fit_to_width("hi", 5);
        assert_eq!(result, "hi   ");
        assert_eq!(display_width(&result), 5);
    }

    #[test]
    fn test_fit_to_width_truncate() {
        let result = fit_to_width("hello world", 5);
        assert_eq!(display_width(&result), 5);
    }

    #[test]
    fn test_col_to_byte_offset() {
        let s = "hello";
        assert_eq!(col_to_byte_offset(s, 0), 0);
        assert_eq!(col_to_byte_offset(s, 3), 3);
        assert_eq!(col_to_byte_offset(s, 10), 5);
    }
}
