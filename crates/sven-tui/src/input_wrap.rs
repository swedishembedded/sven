// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Wrapping and cursor-positioning logic for the multiline input box.
//!
//! [`wrap_content`] converts a raw string and byte-index cursor into a list of
//! visual lines and the `(row, col)` position of the cursor in those lines.
//! It respects both explicit newlines and soft-wrap at a given display-column
//! limit, and handles multi-byte UTF-8 and wide (CJK / emoji) characters
//! correctly via `unicode_width`.

use unicode_width::UnicodeWidthChar;

/// Output of [`wrap_content`].
#[derive(Debug, PartialEq, Eq)]
pub struct WrapState {
    /// Visual lines produced by splitting on `\n` and soft-wrapping at `width`.
    /// Always contains at least one element (possibly `""`).
    pub lines: Vec<String>,
    /// Index into `lines` that contains the cursor position.
    pub cursor_row: usize,
    /// Display-column offset within `lines[cursor_row]` of the cursor.
    pub cursor_col: usize,
}

/// Wrap `content` into visual lines of at most `width` display columns and
/// compute where `cursor_byte` (a UTF-8 byte offset into `content`) falls in
/// the resulting line grid.
///
/// Rules
/// -----
/// * `'\n'` always starts a new visual line.
/// * A character that *would* push the column count past `width` is
///   soft-wrapped: it starts a new visual line at column 0.
/// * Wide characters (e.g. CJK ideographs) count as 2 columns.
/// * Zero-width characters (e.g. combining marks) count as 0 columns.
/// * `cursor_byte` is clamped to `content.len()` if out of range.
/// * When `width == 0` soft-wrapping is disabled (infinite line width).
///
/// The returned `cursor_row` / `cursor_col` values are suitable for passing
/// directly to `frame.set_cursor_position`.
pub fn wrap_content(content: &str, width: usize, cursor_byte: usize) -> WrapState {
    let cursor_byte = cursor_byte.min(content.len());

    let mut lines: Vec<String> = Vec::new();
    let mut cur_line = String::new();
    let mut cur_col: usize = 0; // display columns on the current line
    let mut cur_byte: usize = 0;
    let mut c_row: usize = 0;
    let mut c_col: usize = 0;
    let mut cursor_placed = false;

    for ch in content.chars() {
        let ch_bytes = ch.len_utf8();
        let ch_width = if ch == '\n' {
            0
        } else {
            UnicodeWidthChar::width(ch).unwrap_or(1)
        };

        // Soft-wrap: if adding this non-newline char would exceed `width`,
        // flush the current line and start a new one first.
        let soft_wrap =
            width > 0 && ch != '\n' && ch_width > 0 && cur_col + ch_width > width;
        if soft_wrap {
            lines.push(std::mem::take(&mut cur_line));
            cur_col = 0;
        }

        // The cursor sits *before* the character at `cursor_byte`.
        if !cursor_placed && cur_byte == cursor_byte {
            cursor_placed = true;
            c_row = lines.len();
            c_col = cur_col;
        }

        if ch == '\n' {
            lines.push(std::mem::take(&mut cur_line));
            cur_col = 0;
        } else {
            cur_line.push(ch);
            cur_col += ch_width;
        }

        cur_byte += ch_bytes;
    }

    // Cursor at the very end of the string.
    if !cursor_placed && cur_byte == cursor_byte {
        c_row = lines.len();
        c_col = cur_col;
        // If the final line is exactly full the cursor logically starts the
        // next visual line (consistent with how most terminals render it).
        if width > 0 && c_col >= width {
            c_row += 1;
            c_col = 0;
        }
    }

    // Push whatever remains on the current line.
    lines.push(cur_line);

    // Ensure cursor_row is a valid index (can happen if cursor is on a newly
    // started line after a full-width line or a trailing newline).
    while c_row >= lines.len() {
        lines.push(String::new());
    }

    WrapState { lines, cursor_row: c_row, cursor_col: c_col }
}

/// Given an already-computed `WrapState`, return the byte offset in `content`
/// that corresponds to visual `(target_row, target_col)`.
///
/// If `target_col` exceeds the length of the target visual line the cursor is
/// placed at the end of that line (clamped).  This matches the behaviour of
/// most editors when moving vertically across lines of different lengths.
pub fn byte_offset_at_row_col(content: &str, width: usize, target_row: usize, target_col: usize) -> usize {
    // Re-run the wrap loop, stopping when we enter the target row and have
    // consumed `target_col` display columns (or the line ends).
    let mut lines: Vec<String> = Vec::new();
    let mut cur_line = String::new();
    let mut cur_col: usize = 0;
    let mut cur_byte: usize = 0;
    // byte-start of each visual line
    let mut line_start_bytes: Vec<usize> = vec![0];

    for ch in content.chars() {
        let ch_bytes = ch.len_utf8();
        let ch_width = if ch == '\n' {
            0
        } else {
            UnicodeWidthChar::width(ch).unwrap_or(1)
        };
        let soft_wrap = width > 0 && ch != '\n' && ch_width > 0 && cur_col + ch_width > width;
        if soft_wrap {
            lines.push(std::mem::take(&mut cur_line));
            cur_col = 0;
            line_start_bytes.push(cur_byte);
        }
        if ch == '\n' {
            lines.push(std::mem::take(&mut cur_line));
            cur_col = 0;
            line_start_bytes.push(cur_byte + ch_bytes);
        } else {
            cur_line.push(ch);
            cur_col += ch_width;
        }
        cur_byte += ch_bytes;
    }
    lines.push(cur_line);

    // Clamp target_row to valid range.
    let target_row = target_row.min(lines.len().saturating_sub(1));
    let line_start = line_start_bytes[target_row];
    let line_text = &lines[target_row];

    // Walk the target line until we reach target_col display columns.
    let mut col = 0usize;
    let mut byte_off = line_start;
    for ch in line_text.chars() {
        if col >= target_col {
            break;
        }
        col += UnicodeWidthChar::width(ch).unwrap_or(1);
        byte_off += ch.len_utf8();
    }
    byte_off.min(content.len())
}

/// Adjust `scroll_offset` so that `cursor_row` is inside the visible window.
///
/// * If the cursor is above the window, the window scrolls up.
/// * If the cursor is below the window, the window scrolls down just enough
///   to make the cursor the last visible row.
/// * `visible_height == 0` is a no-op.
pub fn adjust_scroll(cursor_row: usize, visible_height: usize, scroll_offset: &mut usize) {
    if visible_height == 0 {
        return;
    }
    if cursor_row < *scroll_offset {
        *scroll_offset = cursor_row;
    } else if cursor_row >= *scroll_offset + visible_height {
        *scroll_offset = cursor_row + 1 - visible_height;
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Empty / trivial ───────────────────────────────────────────────────────

    #[test]
    fn empty_string_one_empty_line_cursor_at_origin() {
        let s = wrap_content("", 10, 0);
        assert_eq!(s.lines, vec!["".to_string()]);
        assert_eq!(s.cursor_row, 0);
        assert_eq!(s.cursor_col, 0);
    }

    #[test]
    fn single_char_cursor_at_start() {
        let s = wrap_content("x", 10, 0);
        assert_eq!(s.cursor_row, 0);
        assert_eq!(s.cursor_col, 0);
    }

    #[test]
    fn single_char_cursor_at_end() {
        let s = wrap_content("x", 10, 1);
        assert_eq!(s.cursor_row, 0);
        assert_eq!(s.cursor_col, 1);
    }

    // ── ASCII soft-wrapping ───────────────────────────────────────────────────

    #[test]
    fn no_wrap_when_content_fits() {
        let s = wrap_content("hello", 10, 5);
        assert_eq!(s.lines, vec!["hello".to_string()]);
        assert_eq!(s.cursor_row, 0);
        assert_eq!(s.cursor_col, 5);
    }

    #[test]
    fn soft_wrap_splits_at_width() {
        // width=3: "abc" fills the line; "de" starts on the next
        let s = wrap_content("abcde", 3, 0);
        assert_eq!(s.lines, vec!["abc".to_string(), "de".to_string()]);
    }

    #[test]
    fn cursor_before_first_char_of_wrapped_line() {
        // "abcde", width=3 → ["abc", "de"]
        // cursor at byte 3 (before 'd') → row 1, col 0
        let s = wrap_content("abcde", 3, 3);
        assert_eq!(s.cursor_row, 1);
        assert_eq!(s.cursor_col, 0);
    }

    #[test]
    fn cursor_within_first_wrapped_segment() {
        // cursor at byte 2 (before 'c') → row 0, col 2
        let s = wrap_content("abcde", 3, 2);
        assert_eq!(s.cursor_row, 0);
        assert_eq!(s.cursor_col, 2);
    }

    #[test]
    fn cursor_within_second_wrapped_segment() {
        // cursor at byte 4 (before 'e') → row 1, col 1
        let s = wrap_content("abcde", 3, 4);
        assert_eq!(s.cursor_row, 1);
        assert_eq!(s.cursor_col, 1);
    }

    #[test]
    fn cursor_at_end_of_exactly_full_line_wraps_to_next_row() {
        // "abc" exactly fills width=3; cursor at end (byte 3) → row 1, col 0
        let s = wrap_content("abc", 3, 3);
        assert_eq!(s.cursor_row, 1);
        assert_eq!(s.cursor_col, 0);
    }

    #[test]
    fn cursor_at_end_of_short_line_stays_on_same_row() {
        // "ab" is shorter than width=3; cursor at end (byte 2) → row 0, col 2
        let s = wrap_content("ab", 3, 2);
        assert_eq!(s.cursor_row, 0);
        assert_eq!(s.cursor_col, 2);
    }

    // ── Newline handling ──────────────────────────────────────────────────────

    #[test]
    fn explicit_newline_splits_lines() {
        let s = wrap_content("a\nb", 10, 0);
        assert_eq!(s.lines, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn cursor_before_newline_stays_on_first_row() {
        // "a\nb", cursor at byte 1 (before '\n') → row 0, col 1
        let s = wrap_content("a\nb", 10, 1);
        assert_eq!(s.cursor_row, 0);
        assert_eq!(s.cursor_col, 1);
    }

    #[test]
    fn cursor_after_newline_is_on_second_row() {
        // "a\nb", cursor at byte 2 (after '\n', before 'b') → row 1, col 0
        let s = wrap_content("a\nb", 10, 2);
        assert_eq!(s.cursor_row, 1);
        assert_eq!(s.cursor_col, 0);
    }

    #[test]
    fn trailing_newline_creates_empty_last_line() {
        let s = wrap_content("a\n", 10, 2);
        assert_eq!(s.lines, vec!["a".to_string(), "".to_string()]);
        // Cursor at byte 2 (end, after '\n') → row 1, col 0
        assert_eq!(s.cursor_row, 1);
        assert_eq!(s.cursor_col, 0);
    }

    #[test]
    fn three_lines_separated_by_newlines() {
        let s = wrap_content("a\nb\nc", 10, 0);
        assert_eq!(s.lines, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
        // cursor at 'b' → byte 2 → row 1, col 0
        let s2 = wrap_content("a\nb\nc", 10, 2);
        assert_eq!(s2.cursor_row, 1);
        assert_eq!(s2.cursor_col, 0);
    }

    // ── Soft-wrap + newline combined ──────────────────────────────────────────

    #[test]
    fn newline_resets_column_counter_for_following_text() {
        // "ab\ncde" in width=3: "ab" on line 0; after '\n' the col counter
        // resets so "cde" (exactly 3 cols) fits entirely on line 1 —
        // no soft wrap because it doesn't *exceed* the width limit.
        // "ab\ncdef" (width=3): "cde"=3 fits, then 'f' overflows → ["ab","cde","f"].
        let s = wrap_content("ab\ncdef", 3, 0);
        assert_eq!(s.lines, vec!["ab".to_string(), "cde".to_string(), "f".to_string()]);
    }

    #[test]
    fn newline_followed_by_exactly_fitting_text_no_extra_wrap() {
        // "ab\ncde" in width=3: "cde" is exactly 3 cols, fits without wrapping.
        let s = wrap_content("ab\ncde", 3, 0);
        assert_eq!(s.lines, vec!["ab".to_string(), "cde".to_string()]);
    }

    #[test]
    fn cursor_after_newline_before_soft_wrapped_content() {
        // "ab\ncdef", width=3 → ["ab","cde","f"]
        // cursor at byte 3 (after '\n', before 'c') → row 1, col 0
        let s = wrap_content("ab\ncdef", 3, 3);
        assert_eq!(s.cursor_row, 1);
        assert_eq!(s.cursor_col, 0);
    }

    #[test]
    fn cursor_on_third_wrapped_line() {
        // "ab\ncdef", width=3 → ["ab","cde","f"]
        // cursor at byte 6 (before 'f') → row 2, col 0
        let s = wrap_content("ab\ncdef", 3, 6);
        assert_eq!(s.cursor_row, 2);
        assert_eq!(s.cursor_col, 0);
    }

    // ── Unicode / wide characters ─────────────────────────────────────────────

    #[test]
    fn wide_chars_counted_as_two_columns() {
        // '中' is 2 display columns; width=4 fits 2 ideographs, then wraps
        let s = wrap_content("中中中", 4, 0);
        assert_eq!(s.lines, vec!["中中".to_string(), "中".to_string()]);
    }

    #[test]
    fn cursor_after_wide_char_at_correct_column() {
        // '中' (3 bytes, 2 cols), cursor at byte 3 → row 0, col 2
        let s = wrap_content("中X", 10, 3);
        assert_eq!(s.cursor_row, 0);
        assert_eq!(s.cursor_col, 2);
    }

    #[test]
    fn multibyte_ascii_equivalent_cursor() {
        // ASCII 'é' (2 bytes in UTF-8), cursor at end (byte 2) → col 1
        let s = wrap_content("é", 10, "é".len());
        assert_eq!(s.cursor_row, 0);
        assert_eq!(s.cursor_col, 1);
    }

    // ── Zero-width disables soft-wrap ─────────────────────────────────────────

    #[test]
    fn zero_width_never_soft_wraps() {
        let s = wrap_content("a very long string indeed", 0, 0);
        assert_eq!(s.lines.len(), 1);
    }

    // ── Out-of-range cursor clamping ──────────────────────────────────────────

    #[test]
    fn cursor_beyond_end_is_clamped() {
        let s = wrap_content("abc", 10, 999);
        // After clamping to len=3, cursor is at the end
        assert_eq!(s.cursor_row, 0);
        assert_eq!(s.cursor_col, 3);
    }

    // ── adjust_scroll ─────────────────────────────────────────────────────────

    #[test]
    fn adjust_scroll_noop_when_cursor_in_visible_window() {
        let mut off = 2;
        adjust_scroll(3, 5, &mut off); // cursor row 3 is inside rows 2..7
        assert_eq!(off, 2);
    }

    #[test]
    fn adjust_scroll_scrolls_up_when_cursor_above_window() {
        let mut off = 5;
        adjust_scroll(2, 3, &mut off); // cursor at row 2, window 5..8 → scroll to 2
        assert_eq!(off, 2);
    }

    #[test]
    fn adjust_scroll_scrolls_down_when_cursor_below_window() {
        let mut off = 0;
        adjust_scroll(5, 3, &mut off); // cursor at row 5, window 0..3 → scroll to 3
        assert_eq!(off, 3);
    }

    #[test]
    fn adjust_scroll_cursor_exactly_at_bottom_of_window_is_noop() {
        let mut off = 0;
        adjust_scroll(2, 3, &mut off); // cursor at row 2, window 0..3 (last visible row)
        assert_eq!(off, 0);
    }

    #[test]
    fn adjust_scroll_zero_visible_height_is_noop() {
        let mut off = 0;
        adjust_scroll(100, 0, &mut off);
        assert_eq!(off, 0);
    }

    #[test]
    fn adjust_scroll_already_at_top_stays_zero() {
        let mut off = 0;
        adjust_scroll(0, 3, &mut off);
        assert_eq!(off, 0);
    }
}
