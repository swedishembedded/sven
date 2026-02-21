//! Grid-to-ratatui rendering: converts a `Grid` snapshot to a `Vec<Line>`.

use std::collections::HashMap;

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::grid::{Grid, HlAttr};

/// Render a grid snapshot to ratatui `Line`s, merging adjacent cells that
/// share the same highlight attribute into a single `Span`.
///
/// Exported as `pub(crate)` so unit tests can drive it without a live bridge.
pub(crate) fn render_grid_to_lines(
    grid: &Grid,
    attrs: &HashMap<u64, HlAttr>,
    scroll: usize,
    visible_height: usize,
) -> Vec<Line<'static>> {
    let start_row = scroll;
    let end_row   = (start_row + visible_height).min(grid.height);
    let mut lines = Vec::new();

    for row in start_row..end_row {
        if row >= grid.cells.len() {
            break;
        }
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut current_text    = String::new();
        let mut current_attr_id = 0u64;
        let mut current_style   = Style::default();

        for cell in &grid.cells[row] {
            if cell.attr_id != current_attr_id && !current_text.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut current_text), current_style));
            }
            if cell.attr_id != current_attr_id {
                current_attr_id = cell.attr_id;
                current_style   = attrs
                    .get(&current_attr_id)
                    .map(|a| a.to_style())
                    .unwrap_or_default();
            }
            current_text.push_str(&cell.text);
        }
        if !current_text.is_empty() {
            spans.push(Span::styled(current_text, current_style));
        }
        lines.push(Line::from(spans));
    }
    lines
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ratatui::style::{Color, Style};

    use super::*;
    use crate::nvim::grid::{Cell, HlAttr};

    #[test]
    fn render_produces_one_line_per_visible_row() {
        let g     = Grid::new(10, 5);
        let lines = render_grid_to_lines(&g, &HashMap::new(), 0, 3);
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn render_scroll_zero_maps_row_zero_to_first_output_line() {
        let mut g = Grid::new(10, 5);
        g.set_cell(0, 0, Cell { text: "A".into(), attr_id: 0 });
        g.set_cell(1, 0, Cell { text: "B".into(), attr_id: 0 });
        let lines = render_grid_to_lines(&g, &HashMap::new(), 0, 2);
        let row0: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        let row1: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(row0.contains('A'), "first output line comes from grid row 0");
        assert!(row1.contains('B'), "second output line comes from grid row 1");
    }

    #[test]
    fn render_scroll_offset_skips_leading_rows() {
        let mut g = Grid::new(10, 5);
        g.set_cell(0, 0, Cell { text: "A".into(), attr_id: 0 });
        g.set_cell(2, 0, Cell { text: "C".into(), attr_id: 0 });
        let lines = render_grid_to_lines(&g, &HashMap::new(), 2, 1);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains('C'),  "row 2 content visible after scroll");
        assert!(!text.contains('A'), "row 0 content absent after scroll");
    }

    #[test]
    fn render_visible_height_limits_number_of_output_lines() {
        let g = Grid::new(10, 10);
        let lines = render_grid_to_lines(&g, &HashMap::new(), 0, 4);
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn render_scroll_plus_height_clamps_to_grid_height() {
        let g     = Grid::new(10, 5);
        let lines = render_grid_to_lines(&g, &HashMap::new(), 3, 10); // would go past row 5
        assert_eq!(lines.len(), 2, "only rows 3 and 4 exist");
    }

    #[test]
    fn render_merges_adjacent_cells_with_same_attr_into_one_span() {
        let mut g = Grid::new(4, 1);
        for c in 0..4 {
            g.set_cell(0, c, Cell { text: "X".into(), attr_id: 1 });
        }
        let lines = render_grid_to_lines(&g, &HashMap::new(), 0, 1);
        let span_count = lines[0].spans.len();
        assert_eq!(span_count, 1, "four cells with the same attr_id must merge into one span");
    }

    #[test]
    fn render_splits_cells_with_different_attrs_into_separate_spans() {
        let mut g = Grid::new(3, 1);
        g.set_cell(0, 0, Cell { text: "A".into(), attr_id: 1 });
        g.set_cell(0, 1, Cell { text: "B".into(), attr_id: 2 });
        g.set_cell(0, 2, Cell { text: "C".into(), attr_id: 1 });
        let lines = render_grid_to_lines(&g, &HashMap::new(), 0, 1);
        let span_count = lines[0].spans.len();
        assert_eq!(span_count, 3, "A(1) B(2) C(1) must produce three spans");
    }

    #[test]
    fn render_applies_foreground_colour_from_hl_attr() {
        let mut g = Grid::new(3, 1);
        for c in 0..3 {
            g.set_cell(0, c, Cell { text: "x".into(), attr_id: 5 });
        }
        let mut attrs = HashMap::new();
        attrs.insert(5u64, HlAttr {
            foreground: Some(Color::Rgb(255, 0, 0)),
            ..HlAttr::default()
        });
        let lines = render_grid_to_lines(&g, &attrs, 0, 1);
        let span_style = lines[0].spans[0].style;
        assert_eq!(span_style.fg, Some(Color::Rgb(255, 0, 0)), "foreground colour applied");
    }

    #[test]
    fn render_unknown_attr_id_defaults_to_plain_style() {
        let mut g = Grid::new(1, 1);
        g.set_cell(0, 0, Cell { text: "?".into(), attr_id: 999 });
        let lines = render_grid_to_lines(&g, &HashMap::new(), 0, 1);
        assert_eq!(lines[0].spans[0].style, Style::default());
    }

    #[test]
    fn render_empty_grid_returns_empty_lines() {
        let g = Grid::new(10, 0);
        let lines = render_grid_to_lines(&g, &HashMap::new(), 0, 5);
        assert!(lines.is_empty());
    }
}
