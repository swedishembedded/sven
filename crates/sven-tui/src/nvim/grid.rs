// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Neovim grid and highlight-attribute data structures.
//!
//! These are pure data types with no async code, no RPC, and no process
//! management — they can be created and tested entirely in isolation.

use std::collections::HashMap;

use ratatui::style::{Color, Modifier, Style};
use rmpv::Value;

// ── Cell ─────────────────────────────────────────────────────────────────────

/// A single cell in the Neovim grid.
#[derive(Debug, Clone)]
pub struct Cell {
    pub text: String,
    pub attr_id: u64,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            text: " ".to_string(),
            attr_id: 0,
        }
    }
}

// ── Grid ─────────────────────────────────────────────────────────────────────

/// 2-D grid representing the Neovim screen.
#[derive(Debug, Clone)]
pub struct Grid {
    pub width: usize,
    pub height: usize,
    pub cells: Vec<Vec<Cell>>,
}

impl Grid {
    pub fn new(width: usize, height: usize) -> Self {
        let cells = vec![vec![Cell::default(); width]; height];
        Self {
            width,
            height,
            cells,
        }
    }

    pub fn resize(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.cells = vec![vec![Cell::default(); width]; height];
    }

    pub fn clear(&mut self) {
        for row in &mut self.cells {
            for cell in row {
                *cell = Cell::default();
            }
        }
    }

    pub fn set_cell(&mut self, row: usize, col: usize, cell: Cell) {
        if row < self.height && col < self.width {
            self.cells[row][col] = cell;
        }
    }

    /// Apply a `grid_scroll` redraw event.
    ///
    /// The region `[top, bot) × [left, right)` is scrolled by `rows` rows:
    /// - `rows > 0`: content moves **up** — lines `[top, top+rows)` are
    ///   discarded; lines `[top+rows, bot)` shift to `[top, bot-rows)`;
    ///   the now-empty rows at the bottom are cleared.
    /// - `rows < 0`: content moves **down** — lines `[bot+rows, bot)` are
    ///   discarded; lines `[top, bot+rows)` shift to `[top+|rows|, bot)`;
    ///   the now-empty rows at the top are cleared.
    ///
    /// Neovim will subsequently send `grid_line` events to fill the cleared
    /// (invalidated) rows with new content.
    pub fn scroll(&mut self, top: usize, bot: usize, left: usize, right: usize, rows: i64) {
        if rows == 0 {
            return;
        }
        let right = right.min(self.width);
        let bot = bot.min(self.height);

        if rows > 0 {
            let count = rows as usize;
            for r in top..bot.saturating_sub(count) {
                for c in left..right {
                    let src = self.cells[r + count][c].clone();
                    self.cells[r][c] = src;
                }
            }
            for r in bot.saturating_sub(count)..bot {
                for c in left..right {
                    self.cells[r][c] = Cell::default();
                }
            }
        } else {
            let count = (-rows) as usize;
            for r in (top + count..bot).rev() {
                for c in left..right {
                    let src = self.cells[r - count][c].clone();
                    self.cells[r][c] = src;
                }
            }
            let clear_end = (top + count).min(bot);
            for r in top..clear_end {
                for c in left..right {
                    self.cells[r][c] = Cell::default();
                }
            }
        }
    }
}

// ── HlAttr ───────────────────────────────────────────────────────────────────

/// Highlight attributes received via `hl_attr_define` Neovim redraw events.
#[derive(Debug, Clone, Default)]
pub struct HlAttr {
    pub foreground: Option<Color>,
    pub background: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
}

impl HlAttr {
    /// Convert to a ratatui `Style`.
    pub fn to_style(&self) -> Style {
        let mut style = Style::default();
        if let Some(fg) = self.foreground {
            style = style.fg(fg);
        }
        if let Some(bg) = self.background {
            style = style.bg(bg);
        }
        if self.bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if self.italic {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if self.underline {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        if self.reverse {
            style = style.add_modifier(Modifier::REVERSED);
        }
        style
    }

    /// Parse highlight attributes from the `rgb_attrs` map in a
    /// `hl_attr_define` event.  Accessible to the handler module.
    pub(crate) fn from_map(map: &HashMap<String, Value>) -> Self {
        let mut attr = HlAttr::default();

        if let Some(Value::Integer(fg)) = map.get("foreground") {
            if let Ok(v) = u32::try_from(fg.as_u64().unwrap_or(0)) {
                let r = ((v >> 16) & 0xFF) as u8;
                let g = ((v >> 8) & 0xFF) as u8;
                let b = (v & 0xFF) as u8;
                attr.foreground = Some(Color::Rgb(r, g, b));
            }
        }
        if let Some(Value::Integer(bg)) = map.get("background") {
            if let Ok(v) = u32::try_from(bg.as_u64().unwrap_or(0)) {
                let r = ((v >> 16) & 0xFF) as u8;
                let g = ((v >> 8) & 0xFF) as u8;
                let b = (v & 0xFF) as u8;
                attr.background = Some(Color::Rgb(r, g, b));
            }
        }
        if let Some(Value::Boolean(true)) = map.get("bold") {
            attr.bold = true;
        }
        if let Some(Value::Boolean(true)) = map.get("italic") {
            attr.italic = true;
        }
        if let Some(Value::Boolean(true)) = map.get("underline") {
            attr.underline = true;
        }
        if let Some(Value::Boolean(true)) = map.get("reverse") {
            attr.reverse = true;
        }

        attr
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Modifier};
    use rmpv::Value;

    use super::*;

    #[test]
    fn grid_new_allocates_correct_dimensions() {
        let g = Grid::new(80, 24);
        assert_eq!(g.width, 80, "grid width field");
        assert_eq!(g.height, 24, "grid height field");
        assert_eq!(g.cells.len(), 24, "row count == height");
        assert_eq!(g.cells[0].len(), 80, "column count == width");
    }

    #[test]
    fn grid_cells_initialised_to_space_with_attr_zero() {
        let g = Grid::new(10, 5);
        for &(r, c) in &[(0, 0), (0, 9), (4, 0), (4, 9)] {
            assert_eq!(g.cells[r][c].text, " ", "cell ({r},{c}) text");
            assert_eq!(g.cells[r][c].attr_id, 0, "cell ({r},{c}) attr_id");
        }
    }

    #[test]
    fn grid_set_cell_writes_text_and_attr_at_target() {
        let mut g = Grid::new(10, 5);
        g.set_cell(
            2,
            3,
            Cell {
                text: "A".into(),
                attr_id: 7,
            },
        );
        assert_eq!(g.cells[2][3].text, "A", "text written");
        assert_eq!(g.cells[2][3].attr_id, 7, "attr_id written");
        assert_eq!(g.cells[2][4].text, " ", "adjacent cell unchanged");
    }

    #[test]
    fn grid_set_cell_ignores_out_of_bounds_without_panic() {
        let mut g = Grid::new(10, 5);
        g.set_cell(
            100,
            0,
            Cell {
                text: "X".into(),
                attr_id: 1,
            },
        );
        g.set_cell(
            0,
            100,
            Cell {
                text: "X".into(),
                attr_id: 1,
            },
        );
        g.set_cell(
            5,
            0,
            Cell {
                text: "X".into(),
                attr_id: 1,
            },
        );
        assert_eq!(g.cells[0][0].text, " ");
    }

    #[test]
    fn grid_clear_resets_every_cell_to_default() {
        let mut g = Grid::new(5, 3);
        g.set_cell(
            0,
            0,
            Cell {
                text: "X".into(),
                attr_id: 9,
            },
        );
        g.set_cell(
            2,
            4,
            Cell {
                text: "Y".into(),
                attr_id: 1,
            },
        );
        g.clear();
        assert_eq!(g.cells[0][0].text, " ", "top-left reset");
        assert_eq!(g.cells[0][0].attr_id, 0, "top-left attr reset");
        assert_eq!(g.cells[2][4].text, " ", "bottom-right reset");
    }

    #[test]
    fn grid_resize_updates_dimensions_and_discards_old_content() {
        let mut g = Grid::new(80, 24);
        g.set_cell(
            0,
            0,
            Cell {
                text: "X".into(),
                attr_id: 1,
            },
        );
        g.resize(40, 10);
        assert_eq!(g.width, 40);
        assert_eq!(g.height, 10);
        assert_eq!(g.cells[0][0].text, " ", "content cleared after resize");
    }

    #[test]
    fn hlattr_default_maps_to_plain_ratatui_style() {
        assert_eq!(HlAttr::default().to_style(), Style::default());
    }

    #[test]
    fn hlattr_foreground_integer_decoded_as_rgb_color() {
        let mut map = std::collections::HashMap::new();
        map.insert("foreground".into(), Value::Integer(0xFF0000u32.into()));
        let attr = HlAttr::from_map(&map);
        assert_eq!(attr.foreground, Some(Color::Rgb(0xFF, 0x00, 0x00)));
        assert_eq!(attr.to_style().fg, Some(Color::Rgb(0xFF, 0x00, 0x00)));
    }

    #[test]
    fn hlattr_background_integer_decoded_as_rgb_color() {
        let mut map = std::collections::HashMap::new();
        map.insert("background".into(), Value::Integer(0x0000FFu32.into()));
        let attr = HlAttr::from_map(&map);
        assert_eq!(attr.background, Some(Color::Rgb(0x00, 0x00, 0xFF)));
    }

    #[test]
    fn hlattr_green_channel_isolated_correctly() {
        let mut map = std::collections::HashMap::new();
        map.insert("foreground".into(), Value::Integer(0x00FF00u32.into()));
        let attr = HlAttr::from_map(&map);
        assert_eq!(attr.foreground, Some(Color::Rgb(0x00, 0xFF, 0x00)));
    }

    #[test]
    fn hlattr_bold_true_sets_bold_modifier() {
        let mut map = std::collections::HashMap::new();
        map.insert("bold".into(), Value::Boolean(true));
        let attr = HlAttr::from_map(&map);
        let style = attr.to_style();
        assert!(attr.bold);
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn hlattr_italic_true_sets_italic_modifier() {
        let mut map = std::collections::HashMap::new();
        map.insert("italic".into(), Value::Boolean(true));
        let attr = HlAttr::from_map(&map);
        let style = attr.to_style();
        assert!(attr.italic);
        assert!(style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn hlattr_bold_false_does_not_set_bold_modifier() {
        let mut map = std::collections::HashMap::new();
        map.insert("bold".into(), Value::Boolean(false));
        let attr = HlAttr::from_map(&map);
        assert!(!attr.bold);
        assert!(!attr.to_style().add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn hlattr_combined_colour_and_bold_applied_together() {
        let mut map = std::collections::HashMap::new();
        map.insert("foreground".into(), Value::Integer(0xFF0000u32.into()));
        map.insert("bold".into(), Value::Boolean(true));
        let style = HlAttr::from_map(&map).to_style();
        assert_eq!(style.fg, Some(Color::Rgb(0xFF, 0x00, 0x00)));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn grid_scroll_up_positive_rows_shifts_content_and_clears_bottom() {
        let mut g = Grid::new(4, 5);
        for r in 0..5usize {
            let ch = (b'A' + r as u8) as char;
            g.set_cell(
                r,
                0,
                Cell {
                    text: ch.to_string(),
                    attr_id: 0,
                },
            );
        }
        g.scroll(0, 5, 0, 4, 2);
        assert_eq!(g.cells[0][0].text, "C", "row 0 ← former row 2");
        assert_eq!(g.cells[1][0].text, "D", "row 1 ← former row 3");
        assert_eq!(g.cells[2][0].text, "E", "row 2 ← former row 4");
        assert_eq!(g.cells[3][0].text, " ", "row 3 cleared");
        assert_eq!(g.cells[4][0].text, " ", "row 4 cleared");
    }

    #[test]
    fn grid_scroll_down_negative_rows_shifts_content_and_clears_top() {
        let mut g = Grid::new(4, 5);
        for r in 0..5usize {
            let ch = (b'A' + r as u8) as char;
            g.set_cell(
                r,
                0,
                Cell {
                    text: ch.to_string(),
                    attr_id: 0,
                },
            );
        }
        g.scroll(0, 5, 0, 4, -2);
        assert_eq!(g.cells[0][0].text, " ", "row 0 cleared");
        assert_eq!(g.cells[1][0].text, " ", "row 1 cleared");
        assert_eq!(g.cells[2][0].text, "A", "row 2 ← former row 0");
        assert_eq!(g.cells[3][0].text, "B", "row 3 ← former row 1");
        assert_eq!(g.cells[4][0].text, "C", "row 4 ← former row 2");
    }

    #[test]
    fn grid_scroll_up_sub_region_does_not_touch_rows_outside_region() {
        let mut g = Grid::new(4, 6);
        for r in 0..6usize {
            let ch = (b'A' + r as u8) as char;
            g.set_cell(
                r,
                0,
                Cell {
                    text: ch.to_string(),
                    attr_id: 0,
                },
            );
        }
        g.scroll(1, 4, 0, 4, 1);
        assert_eq!(g.cells[0][0].text, "A", "row 0 outside region, unchanged");
        assert_eq!(g.cells[1][0].text, "C", "row 1 ← former row 2 (C)");
        assert_eq!(g.cells[2][0].text, "D", "row 2 ← former row 3 (D)");
        assert_eq!(g.cells[3][0].text, " ", "row 3 cleared");
        assert_eq!(g.cells[4][0].text, "E", "row 4 outside region, unchanged");
        assert_eq!(g.cells[5][0].text, "F", "row 5 outside region, unchanged");
    }

    #[test]
    fn grid_scroll_zero_rows_is_a_noop() {
        let mut g = Grid::new(4, 3);
        g.set_cell(
            0,
            0,
            Cell {
                text: "X".into(),
                attr_id: 0,
            },
        );
        g.scroll(0, 3, 0, 4, 0);
        assert_eq!(g.cells[0][0].text, "X");
    }

    #[test]
    fn grid_scroll_simulates_fold_close_content_moves_up() {
        let mut g = Grid::new(20, 6);
        let content = ["---", "", "**You:** hi", "", "**Agent:**", "response"];
        for (r, text) in content.iter().enumerate() {
            for (c, ch) in text.chars().enumerate() {
                g.set_cell(
                    r,
                    c,
                    Cell {
                        text: ch.to_string(),
                        attr_id: 0,
                    },
                );
            }
        }
        // Closing the fold collapses rows 1-3 into one fold line.
        // Neovim emits grid_scroll(top=1, bot=6, left=0, right=20, rows=2).
        g.scroll(1, 6, 0, 20, 2);
        // Row 1 should now contain what was at row 3 (empty), and rows 4-5 shift up.
        assert_eq!(g.cells[1][0].text, " ", "row 1: was row 3 (blank)");
        assert_eq!(g.cells[2][0].text, "*", "row 2: was row 4 (**Agent:**)");
        assert_eq!(g.cells[3][0].text, "r", "row 3: was row 5 (response)");
    }
}
