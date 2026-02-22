//! `NvimHandler`: the nvim-rs `Handler` implementation that receives Neovim
//! redraw and custom RPC notifications and writes them into the shared grid.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use nvim_rs::{compat::tokio::Compat, Handler, Neovim};
use rmpv::Value;
use tokio::{process::ChildStdin, sync::{Mutex, Notify}};
use tracing::debug;

use super::grid::{Cell, Grid, HlAttr};

/// Handles Neovim RPC notifications (redraw events).  All shared state is
/// behind `Arc<Mutex<…>>` so it can safely be read from the TUI render loop.
#[derive(Clone)]
pub struct NvimHandler {
    pub(super) grid: Arc<Mutex<Grid>>,
    pub(super) hl_attrs: Arc<Mutex<HashMap<u64, HlAttr>>>,
    pub(super) cursor_pos: Arc<Mutex<(u16, u16)>>,
    /// Fired after each `flush` event so the TUI can re-render immediately.
    pub(super) flush_notify: Arc<Notify>,
    /// Fired when Neovim sends `sven_submit` (e.g. `:w`).
    pub(super) submit_notify: Arc<Notify>,
    /// Fired when Neovim sends `sven_quit` (e.g. `:q` / `:qa`).
    pub(super) quit_notify: Arc<Notify>,
}

impl NvimHandler {
    pub fn new(
        grid: Arc<Mutex<Grid>>,
        hl_attrs: Arc<Mutex<HashMap<u64, HlAttr>>>,
        cursor_pos: Arc<Mutex<(u16, u16)>>,
        flush_notify: Arc<Notify>,
        submit_notify: Arc<Notify>,
        quit_notify: Arc<Notify>,
    ) -> Self {
        Self { grid, hl_attrs, cursor_pos, flush_notify, submit_notify, quit_notify }
    }

    async fn handle_redraw_event(&self, event_name: &str, args: &[Value]) {
        match event_name {
            "grid_resize"      => self.handle_grid_resize(args).await,
            "grid_clear"       => self.handle_grid_clear(args).await,
            "grid_line"        => self.handle_grid_line(args).await,
            "grid_scroll"      => self.handle_grid_scroll(args).await,
            "grid_cursor_goto" => self.handle_grid_cursor_goto(args).await,
            "hl_attr_define"   => self.handle_hl_attr_define(args).await,
            "flush" => {
                debug!("Redraw flush — notifying TUI");
                self.flush_notify.notify_one();
            }
            _ => debug!("Unhandled redraw event: {}", event_name),
        }
    }

    pub(super) async fn handle_grid_resize(&self, args: &[Value]) {
        for params_val in args {
            if let Value::Array(params) = params_val {
                if params.len() >= 3 {
                    if let (
                        Some(Value::Integer(_grid)),
                        Some(Value::Integer(width)),
                        Some(Value::Integer(height)),
                    ) = (params.first(), params.get(1), params.get(2))
                    {
                        let width  = width.as_u64().unwrap_or(80)  as usize;
                        let height = height.as_u64().unwrap_or(24) as usize;
                        debug!("Grid resize: {}x{}", width, height);
                        let mut grid = self.grid.lock().await;
                        grid.resize(width, height);
                    }
                }
            }
        }
    }

    pub(super) async fn handle_grid_clear(&self, _args: &[Value]) {
        debug!("Grid clear");
        let mut grid = self.grid.lock().await;
        grid.clear();
    }

    /// Handle `grid_scroll(grid, top, bot, left, right, rows, cols)`.
    pub(super) async fn handle_grid_scroll(&self, args: &[Value]) {
        for params in args {
            if let Value::Array(p) = params {
                if p.len() >= 6 {
                    let top   = p.get(1).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let bot   = p.get(2).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let left  = p.get(3).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let right = p.get(4).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let rows  = p.get(5).and_then(|v| v.as_i64()).unwrap_or(0);
                    debug!("grid_scroll top={top} bot={bot} rows={rows}");
                    let mut grid = self.grid.lock().await;
                    grid.scroll(top, bot, left, right, rows);
                }
            }
        }
    }

    pub(super) async fn handle_grid_line(&self, args: &[Value]) {
        for params_val in args {
            if let Value::Array(params) = params_val {
                if params.len() >= 4 {
                    let row       = params.get(1).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let col_start = params.get(2).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    if let Some(Value::Array(cells)) = params.get(3) {
                        let mut grid = self.grid.lock().await;
                        let mut col = col_start;
                        let mut current_attr = 0u64;
                        for cell_data in cells {
                            if let Value::Array(cell_parts) = cell_data {
                                if let Some(Value::String(text)) = cell_parts.first() {
                                    let text_str = text.as_str().unwrap_or(" ");
                                    if let Some(Value::Integer(hl_id)) = cell_parts.get(1) {
                                        current_attr = hl_id.as_u64().unwrap_or(0);
                                    }
                                    let repeat = if let Some(Value::Integer(r)) = cell_parts.get(2) {
                                        r.as_u64().unwrap_or(1) as usize
                                    } else { 1 };
                                    for _ in 0..repeat {
                                        if col < grid.width {
                                            grid.set_cell(row, col, Cell {
                                                text: text_str.to_string(),
                                                attr_id: current_attr,
                                            });
                                            col += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    pub(super) async fn handle_grid_cursor_goto(&self, args: &[Value]) {
        for params_val in args {
            if let Value::Array(params) = params_val {
                if params.len() >= 3 {
                    if let (
                        Some(Value::Integer(_grid)),
                        Some(Value::Integer(row)),
                        Some(Value::Integer(col)),
                    ) = (params.first(), params.get(1), params.get(2))
                    {
                        let row = row.as_u64().unwrap_or(0) as u16;
                        let col = col.as_u64().unwrap_or(0) as u16;
                        let mut cursor = self.cursor_pos.lock().await;
                        *cursor = (row, col);
                    }
                }
            }
        }
    }

    pub(super) async fn handle_hl_attr_define(&self, args: &[Value]) {
        for params_val in args {
            if let Value::Array(params) = params_val {
                if params.len() >= 4 {
                    if let (
                        Some(Value::Integer(id)),
                        Some(Value::Map(rgb_attrs)),
                        _,
                        _,
                    ) = (params.first(), params.get(1), params.get(2), params.get(3))
                    {
                        let attr_id = id.as_u64().unwrap_or(0);
                        let mut attr_map = HashMap::new();
                        for (key, value) in rgb_attrs {
                            if let Value::String(key_str) = key {
                                if let Some(key) = key_str.as_str() {
                                    attr_map.insert(key.to_string(), value.clone());
                                }
                            }
                        }
                        let hl_attr = HlAttr::from_map(&attr_map);
                        let mut attrs = self.hl_attrs.lock().await;
                        attrs.insert(attr_id, hl_attr);
                    }
                }
            }
        }
    }
}

#[async_trait]
impl Handler for NvimHandler {
    type Writer = Compat<ChildStdin>;

    async fn handle_request(
        &self,
        _name: String,
        _args: Vec<Value>,
        _neovim: Neovim<Self::Writer>,
    ) -> Result<Value, Value> {
        Ok(Value::from("ok"))
    }

    async fn handle_notify(
        &self,
        name: String,
        args: Vec<Value>,
        _neovim: Neovim<Self::Writer>,
    ) {
        if name == "redraw" {
            for event_batch in args {
                if let Value::Array(events) = event_batch {
                    if let Some(Value::String(event_name)) = events.first() {
                        if let Some(event_name_str) = event_name.as_str() {
                            self.handle_redraw_event(event_name_str, &events[1..]).await;
                        }
                    }
                }
            }
        } else if name == "sven_submit" {
            debug!("Received sven_submit notification from Neovim");
            self.submit_notify.notify_one();
        } else if name == "sven_quit" {
            debug!("Received sven_quit notification from Neovim");
            self.quit_notify.notify_one();
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use ratatui::style::Color;
    use rmpv::Value;
    use tokio::sync::{Mutex, Notify};

    use super::*;

    fn make_handler() -> (
        NvimHandler,
        Arc<Mutex<Grid>>,
        Arc<Mutex<HashMap<u64, HlAttr>>>,
        Arc<Mutex<(u16, u16)>>,
    ) {
        let grid          = Arc::new(Mutex::new(Grid::new(80, 24)));
        let hl_attrs      = Arc::new(Mutex::new(HashMap::new()));
        let cursor        = Arc::new(Mutex::new((0u16, 0u16)));
        let flush_notify  = Arc::new(Notify::new());
        let submit_notify = Arc::new(Notify::new());
        let quit_notify   = Arc::new(Notify::new());
        let handler = NvimHandler::new(
            grid.clone(), hl_attrs.clone(), cursor.clone(),
            flush_notify, submit_notify, quit_notify,
        );
        (handler, grid, hl_attrs, cursor)
    }

    fn grid_line_event(row: u64, col: u64, cells: &[(&str, u64, u64)]) -> Value {
        let cell_values: Vec<Value> = cells.iter().map(|(text, hl, repeat)| {
            Value::Array(vec![
                Value::String((*text).into()),
                Value::Integer((*hl).into()),
                Value::Integer((*repeat).into()),
            ])
        }).collect();
        Value::Array(vec![
            Value::Integer(1.into()),
            Value::Integer(row.into()),
            Value::Integer(col.into()),
            Value::Array(cell_values),
        ])
    }

    #[tokio::test]
    async fn handler_grid_line_writes_chars_at_specified_row_and_col() {
        let (handler, grid, _, _) = make_handler();
        let event = grid_line_event(3, 5, &[("H", 0, 1), ("i", 0, 1)]);
        handler.handle_grid_line(&[event]).await;
        let g = grid.lock().await;
        assert_eq!(g.cells[3][5].text, "H", "row=3 col=5");
        assert_eq!(g.cells[3][6].text, "i", "row=3 col=6");
        assert_eq!(g.cells[3][4].text, " ", "col before start unchanged");
        assert_eq!(g.cells[3][7].text, " ", "col after end unchanged");
    }

    #[tokio::test]
    async fn handler_grid_line_repeat_field_fills_multiple_consecutive_cells() {
        let (handler, grid, _, _) = make_handler();
        let event = grid_line_event(0, 0, &[("X", 0, 4)]);
        handler.handle_grid_line(&[event]).await;
        let g = grid.lock().await;
        for i in 0..4 { assert_eq!(g.cells[0][i].text, "X", "col {i}"); }
        assert_eq!(g.cells[0][4].text, " ", "col 4 must be untouched");
    }

    #[tokio::test]
    async fn handler_grid_line_attr_id_stored_with_each_cell() {
        let (handler, grid, _, _) = make_handler();
        let event = grid_line_event(0, 0, &[("A", 7, 1)]);
        handler.handle_grid_line(&[event]).await;
        let g = grid.lock().await;
        assert_eq!(g.cells[0][0].attr_id, 7);
    }

    #[tokio::test]
    async fn handler_grid_line_multi_row_batch_writes_all_rows() {
        let (handler, grid, _, _) = make_handler();
        let row0 = grid_line_event(0, 0, &[("A", 0, 1)]);
        let row1 = grid_line_event(1, 0, &[("B", 0, 1)]);
        let row2 = grid_line_event(2, 0, &[("C", 0, 1)]);
        handler.handle_grid_line(&[row0, row1, row2]).await;
        let g = grid.lock().await;
        assert_eq!(g.cells[0][0].text, "A", "row 0");
        assert_eq!(g.cells[1][0].text, "B", "row 1");
        assert_eq!(g.cells[2][0].text, "C", "row 2");
    }

    #[tokio::test]
    async fn handler_grid_cursor_goto_stores_row_and_col() {
        let (handler, _, _, cursor) = make_handler();
        let params = Value::Array(vec![
            Value::Integer(1.into()),
            Value::Integer(7.into()),
            Value::Integer(12.into()),
        ]);
        handler.handle_grid_cursor_goto(&[params]).await;
        let pos = cursor.lock().await;
        assert_eq!(pos.0, 7,  "row stored");
        assert_eq!(pos.1, 12, "col stored");
    }

    #[tokio::test]
    async fn handler_grid_resize_updates_grid_dimensions() {
        let (handler, grid, _, _) = make_handler();
        let params = Value::Array(vec![
            Value::Integer(1.into()),
            Value::Integer(120.into()),
            Value::Integer(40.into()),
        ]);
        handler.handle_grid_resize(&[params]).await;
        let g = grid.lock().await;
        assert_eq!(g.width,  120);
        assert_eq!(g.height, 40);
    }

    #[tokio::test]
    async fn handler_grid_clear_resets_previously_written_cells() {
        let (handler, grid, _, _) = make_handler();
        { let mut g = grid.lock().await; g.set_cell(0, 0, Cell { text: "Q".into(), attr_id: 3 }); }
        handler.handle_grid_clear(&[]).await;
        let g = grid.lock().await;
        assert_eq!(g.cells[0][0].text,    " ");
        assert_eq!(g.cells[0][0].attr_id, 0);
    }

    #[tokio::test]
    async fn handler_grid_scroll_delegates_scroll_to_grid() {
        let (handler, grid, _, _) = make_handler();
        {
            let mut g = grid.lock().await;
            g.set_cell(0, 0, Cell { text: "A".into(), attr_id: 0 });
            g.set_cell(2, 0, Cell { text: "B".into(), attr_id: 0 });
        }
        let params = Value::Array(vec![
            Value::Integer(1.into()), Value::Integer(0.into()), Value::Integer(3.into()),
            Value::Integer(0.into()), Value::Integer(80.into()), Value::Integer(2.into()),
            Value::Integer(0.into()),
        ]);
        handler.handle_grid_scroll(&[params]).await;
        let g = grid.lock().await;
        assert_eq!(g.cells[0][0].text, "B", "B moved from row 2 to row 0");
        assert_eq!(g.cells[1][0].text, " ", "row 1 cleared");
        assert_eq!(g.cells[2][0].text, " ", "row 2 cleared");
    }

    #[tokio::test]
    async fn handler_hl_attr_define_stores_colour_and_modifiers() {
        let (handler, _, hl_attrs, _) = make_handler();
        let rgb_map = Value::Map(vec![
            (Value::String("foreground".into()), Value::Integer(0xFF0000u32.into())),
            (Value::String("bold".into()),       Value::Boolean(true)),
        ]);
        let params = Value::Array(vec![
            Value::Integer(42.into()), rgb_map, Value::Map(vec![]), Value::Array(vec![]),
        ]);
        handler.handle_hl_attr_define(&[params]).await;
        let attrs = hl_attrs.lock().await;
        let attr  = attrs.get(&42).expect("attr 42 must be stored");
        assert_eq!(attr.foreground, Some(Color::Rgb(255, 0, 0)));
        assert!(attr.bold);
    }
}
