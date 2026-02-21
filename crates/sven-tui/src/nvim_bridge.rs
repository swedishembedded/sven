use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use nvim_rs::{
    compat::tokio::Compat,
    create::tokio as create,
    exttypes::Buffer,
    Handler, Neovim,
    uioptions::UiAttachOptions,
};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use rmpv::Value;
use tokio::{
    process::{Child, ChildStdin, Command},
    sync::Mutex,
};
use tracing::debug;

/// A single cell in the Neovim grid
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

/// 2D grid representing the Neovim screen
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
        let bot   = bot.min(self.height);

        if rows > 0 {
            let count = rows as usize;
            // Shift rows upward: source [top+count, bot) → dest [top, bot-count)
            for r in top..bot.saturating_sub(count) {
                for c in left..right {
                    let src = self.cells[r + count][c].clone();
                    self.cells[r][c] = src;
                }
            }
            // Clear the now-invalid rows at the bottom of the region
            for r in bot.saturating_sub(count)..bot {
                for c in left..right {
                    self.cells[r][c] = Cell::default();
                }
            }
        } else {
            let count = (-rows) as usize;
            // Shift rows downward: source [top, bot-count) → dest [top+count, bot)
            // Iterate in reverse so we don't overwrite source rows before reading them
            for r in (top + count..bot).rev() {
                for c in left..right {
                    let src = self.cells[r - count][c].clone();
                    self.cells[r][c] = src;
                }
            }
            // Clear the now-invalid rows at the top of the region
            let clear_end = (top + count).min(bot);
            for r in top..clear_end {
                for c in left..right {
                    self.cells[r][c] = Cell::default();
                }
            }
        }
    }
}

/// Highlight attributes from Neovim
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

    fn from_map(map: &HashMap<String, Value>) -> Self {
        let mut attr = HlAttr::default();

        // Extract foreground color
        if let Some(Value::Integer(fg)) = map.get("foreground") {
            if let Ok(fg_u32) = u32::try_from(fg.as_u64().unwrap_or(0)) {
                let r = ((fg_u32 >> 16) & 0xFF) as u8;
                let g = ((fg_u32 >> 8) & 0xFF) as u8;
                let b = (fg_u32 & 0xFF) as u8;
                attr.foreground = Some(Color::Rgb(r, g, b));
            }
        }

        // Extract background color
        if let Some(Value::Integer(bg)) = map.get("background") {
            if let Ok(bg_u32) = u32::try_from(bg.as_u64().unwrap_or(0)) {
                let r = ((bg_u32 >> 16) & 0xFF) as u8;
                let g = ((bg_u32 >> 8) & 0xFF) as u8;
                let b = (bg_u32 & 0xFF) as u8;
                attr.background = Some(Color::Rgb(r, g, b));
            }
        }

        // Extract style flags
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

/// Handler for Neovim RPC notifications (redraw events)
#[derive(Clone)]
pub struct NvimHandler {
    grid: Arc<Mutex<Grid>>,
    hl_attrs: Arc<Mutex<HashMap<u64, HlAttr>>>,
    cursor_pos: Arc<Mutex<(u16, u16)>>,
}

impl NvimHandler {
    pub fn new(
        grid: Arc<Mutex<Grid>>,
        hl_attrs: Arc<Mutex<HashMap<u64, HlAttr>>>,
        cursor_pos: Arc<Mutex<(u16, u16)>>,
    ) -> Self {
        Self {
            grid,
            hl_attrs,
            cursor_pos,
        }
    }

    async fn handle_redraw_event(&self, event_name: &str, args: &[Value]) {
        match event_name {
            "grid_resize"      => self.handle_grid_resize(args).await,
            "grid_clear"       => self.handle_grid_clear(args).await,
            "grid_line"        => self.handle_grid_line(args).await,
            "grid_scroll"      => self.handle_grid_scroll(args).await,
            "grid_cursor_goto" => self.handle_grid_cursor_goto(args).await,
            "hl_attr_define"   => self.handle_hl_attr_define(args).await,
            "flush"            => debug!("Redraw flush"),
            _                  => debug!("Unhandled redraw event: {}", event_name),
        }
    }

    async fn handle_grid_resize(&self, args: &[Value]) {
        for params_val in args {
            if let Value::Array(params) = params_val {
                if params.len() >= 3 {
                    if let (Some(Value::Integer(_grid)), Some(Value::Integer(width)), Some(Value::Integer(height))) =
                        (params.get(0), params.get(1), params.get(2))
                    {
                        let width = width.as_u64().unwrap_or(80) as usize;
                        let height = height.as_u64().unwrap_or(24) as usize;
                        debug!("Grid resize: {}x{}", width, height);
                        let mut grid = self.grid.lock().await;
                        grid.resize(width, height);
                    }
                }
            }
        }
    }

    async fn handle_grid_clear(&self, _args: &[Value]) {
        debug!("Grid clear");
        let mut grid = self.grid.lock().await;
        grid.clear();
    }

    /// Handle `grid_scroll(grid, top, bot, left, right, rows, cols)`.
    ///
    /// This event is emitted whenever Neovim needs to shift a screen region —
    /// most importantly when a fold is closed or opened.  Without handling it,
    /// the stale content from the collapsed region remains in our grid, making
    /// closed folds appear to take up the same space as open ones.
    async fn handle_grid_scroll(&self, args: &[Value]) {
        for params in args {
            if let Value::Array(p) = params {
                if p.len() >= 6 {
                    let top   = p.get(1).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let bot   = p.get(2).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let left  = p.get(3).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let right = p.get(4).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    // rows is signed: positive = scroll up, negative = scroll down
                    let rows  = p.get(5).and_then(|v| v.as_i64()).unwrap_or(0);
                    debug!("grid_scroll top={top} bot={bot} rows={rows}");
                    let mut grid = self.grid.lock().await;
                    grid.scroll(top, bot, left, right, rows);
                }
            }
        }
    }

    async fn handle_grid_line(&self, args: &[Value]) {
        // A single `grid_line` event batch can carry updates for many rows at once.
        // Each element of `args` is one row-update: [grid, row, col_start, cells, wrap?].
        // Process every element — previously only `args.first()` was handled, which
        // caused all rows beyond the first to be silently dropped (the root cause of
        // folded body content never appearing in the rendered grid).
        for params_val in args {
            if let Value::Array(params) = params_val {
                if params.len() >= 4 {
                    let _grid_id = params.get(0).and_then(|v| v.as_u64());
                    let row = params.get(1).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let col_start = params.get(2).and_then(|v| v.as_u64()).unwrap_or(0) as usize;

                    if let Some(Value::Array(cells)) = params.get(3) {
                        let mut grid = self.grid.lock().await;
                        let mut col = col_start;
                        let mut current_attr = 0u64;

                        for cell_data in cells {
                            if let Value::Array(cell_parts) = cell_data {
                                // Cell format: [text, hl_id?, repeat?]
                                if let Some(Value::String(text)) = cell_parts.first() {
                                    let text_str = text.as_str().unwrap_or(" ");

                                    if let Some(Value::Integer(hl_id)) = cell_parts.get(1) {
                                        current_attr = hl_id.as_u64().unwrap_or(0);
                                    }

                                    let repeat = if let Some(Value::Integer(r)) = cell_parts.get(2) {
                                        r.as_u64().unwrap_or(1) as usize
                                    } else {
                                        1
                                    };

                                    for _ in 0..repeat {
                                        if col < grid.width {
                                            grid.set_cell(
                                                row,
                                                col,
                                                Cell {
                                                    text: text_str.to_string(),
                                                    attr_id: current_attr,
                                                },
                                            );
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

    async fn handle_grid_cursor_goto(&self, args: &[Value]) {
        // Use the last cursor-goto in the batch as the definitive cursor position.
        for params_val in args {
            if let Value::Array(params) = params_val {
                if params.len() >= 3 {
                    if let (Some(Value::Integer(_grid)), Some(Value::Integer(row)), Some(Value::Integer(col))) =
                        (params.get(0), params.get(1), params.get(2))
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

    async fn handle_hl_attr_define(&self, args: &[Value]) {
        // A single `hl_attr_define` batch can define many attributes at once.
        for params_val in args {
            if let Value::Array(params) = params_val {
                if params.len() >= 4 {
                    if let (Some(Value::Integer(id)), Some(Value::Map(rgb_attrs)), _, _) =
                        (params.get(0), params.get(1), params.get(2), params.get(3))
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
            // Redraw notification contains an array of event batches
            for event_batch in args {
                if let Value::Array(events) = event_batch {
                    if let Some(Value::String(event_name)) = events.first() {
                        if let Some(event_name_str) = event_name.as_str() {
                            // Rest of the array contains arguments for this event
                            let event_args = &events[1..];
                            self.handle_redraw_event(event_name_str, event_args).await;
                        }
                    }
                }
            }
        }
    }
}

/// Render a grid snapshot to ratatui `Line`s, merging adjacent cells that
/// share the same highlight attribute into a single `Span`.
/// Exported as `pub(crate)` so unit tests can drive it without a live bridge.
pub(crate) fn render_grid_to_lines(
    grid: &Grid,
    attrs: &HashMap<u64, HlAttr>,
    scroll: usize,
    visible_height: usize,
) -> Vec<Line<'static>> {
    let start_row = scroll;
    let end_row = (start_row + visible_height).min(grid.height);
    let mut lines = Vec::new();

    for row in start_row..end_row {
        if row >= grid.cells.len() {
            break;
        }

        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut current_text = String::new();
        let mut current_attr_id = 0u64;
        let mut current_style = Style::default();

        for cell in &grid.cells[row] {
            // Flush current span when the highlight attribute changes
            if cell.attr_id != current_attr_id && !current_text.is_empty() {
                spans.push(Span::styled(current_text.clone(), current_style));
                current_text.clear();
            }

            if cell.attr_id != current_attr_id {
                current_attr_id = cell.attr_id;
                current_style = attrs
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

/// Bridge to embedded Neovim instance
pub struct NvimBridge {
    pub(crate) neovim: Neovim<Compat<ChildStdin>>,
    _child: Child,
    grid: Arc<Mutex<Grid>>,
    hl_attrs: Arc<Mutex<HashMap<u64, HlAttr>>>,
    cursor_pos: Arc<Mutex<(u16, u16)>>,
    width: u16,
    height: u16,
    buffer: Buffer<Compat<ChildStdin>>,
}

impl NvimBridge {
    /// Spawn a new Neovim instance and attach as UI
    pub async fn spawn(width: u16, height: u16) -> Result<Self> {
        debug!("Spawning Neovim with dimensions {}x{}", width, height);
        
        // Create shared state for redraw handler
        let grid = Arc::new(Mutex::new(Grid::new(width as usize, height as usize)));
        let hl_attrs = Arc::new(Mutex::new(HashMap::new()));
        let cursor_pos = Arc::new(Mutex::new((0u16, 0u16)));

        let handler = NvimHandler::new(grid.clone(), hl_attrs.clone(), cursor_pos.clone());

        // Prepare command
        let mut cmd = Command::new("nvim");
        cmd.arg("--embed")
            .arg("--clean");  // Start with minimal config

        // Create Neovim RPC session
        let (neovim, _io_handle, child) = create::new_child_cmd(&mut cmd, handler)
            .await
            .context("Failed to create Neovim session")?;

        // Attach UI
        debug!("Attaching UI");
        let mut opts = UiAttachOptions::new();
        opts.set_linegrid_external(true)
            .set_rgb(true);
        
        neovim
            .ui_attach(width as i64, height as i64, &opts)
            .await
            .context("Failed to attach UI")?;

        // Create a new buffer for the conversation
        let buffer = neovim
            .create_buf(false, true)  // not listed, scratch
            .await
            .context("Failed to create buffer")?;

        // Set current buffer
        neovim
            .set_current_buf(&buffer)
            .await
            .context("Failed to set current buffer")?;

        debug!("NvimBridge initialized successfully");

        Ok(Self {
            neovim,
            _child: child,
            grid,
            hl_attrs,
            cursor_pos,
            width,
            height,
            buffer,
        })
    }

    /// Set the content of the conversation buffer
    pub async fn set_buffer_content(&mut self, content: &str) -> Result<()> {
        // Split into lines but preserve empty lines properly
        let lines: Vec<String> = if content.is_empty() {
            vec![]
        } else {
            content.split('\n').map(|s| s.to_string()).collect()
        };

        self.buffer
            .set_lines(0, -1, false, lines)
            .await
            .context("Failed to set buffer lines")?;

        // NOTE: No foldlevel manipulation needed here.
        // configure_buffer sets foldlevel=99, and when set_lines adds content,
        // Neovim evaluates the fold expression and creates folds OPEN because
        // foldlevel=99 >= any fold level we use.  Confirmed via foldclosed()
        // diagnostic: foldclosed(2)==-1 (open) immediately after set_lines.
        // Toggling foldlevel (0→99) would temporarily close the fold, causing
        // grid_scroll events that race with the subsequent re-open and leave
        // the grid in an incorrect state.

        Ok(())
    }

    /// Get the content of the conversation buffer
    #[allow(dead_code)]
    pub async fn get_buffer_content(&self) -> Result<String> {
        let lines = self.buffer
            .get_lines(0, -1, false)
            .await
            .context("Failed to get buffer lines")?;

        Ok(lines.join("\n"))
    }

    /// Send input keys to Neovim
    pub async fn send_input(&mut self, keys: &str) -> Result<()> {
        self.neovim
            .input(keys)
            .await
            .context("Failed to send input to Neovim")?;
        Ok(())
    }

    /// Resize the UI
    pub async fn resize(&mut self, width: u16, height: u16) -> Result<()> {
        self.width = width;
        self.height = height;
        
        self.neovim
            .ui_try_resize(width as i64, height as i64)
            .await
            .context("Failed to resize UI")?;

        Ok(())
    }

    /// Configure the buffer with markdown settings and fold configuration
    pub async fn configure_buffer(&mut self) -> Result<()> {
        // Set filetype to markdown (enables basic syntax highlighting)
        if let Err(e) = self.neovim.command("setlocal filetype=markdown").await {
            debug!("Could not set filetype: {:?}", e);
        }

        // Basic display settings (wrap/tab are window-scoped; use setlocal)
        let basic_settings = r#"
local buf = vim.api.nvim_get_current_buf()
vim.cmd('setlocal wrap')
vim.cmd('setlocal linebreak')
vim.bo[buf].expandtab = true
vim.bo[buf].tabstop = 2
vim.bo[buf].shiftwidth = 2
"#;
        
        if let Err(e) = self.neovim.exec_lua(basic_settings, vec![]).await {
            debug!("Could not set basic settings: {:?}", e);
        }

        // Fold configuration — fold settings are window-scoped, must use setlocal / vim.wo.
        //
        // Design decisions:
        //
        // 1. foldlevel=99 keeps all folds open by default.  The user can use
        //    zc/zo/za to open/close individual folds, zM to close all, zR to
        //    open all.
        //
        // 2. Body lines return "=" (inherit the level of the previous line).
        //    While Neovim docs note "=" requires a backward scan, the scan only
        //    reaches back to the nearest explicit fold marker (">1", ">2") which
        //    in a conversation log is at most a few tens of lines away.  The
        //    alternative — returning an explicit number — would break tool-section
        //    nesting: after a ">2" tool header, body lines at "2" are correctly
        //    inside the level-2 fold; returning a hard-coded "1" would exit that
        //    fold immediately.
        //
        // 3. foldminlines=0: allow even a fold with a single body line to be
        //    closed.  Without this, Neovim refuses to close very short tool
        //    sections.
        let fold_config = r#"
-- Fold expression for sven conversation structure.
function _G.sven_fold_expr(lnum)
  local ok, line = pcall(vim.fn.getline, lnum)
  if not ok then return '=' end

  -- Level-1 fold headers: user message separators and agent response lines
  if line:match('^%-%-%-$')         then return '>1' end
  if line:match('^%*%*Agent:%*%*')  then return '>1' end
  if line:match('^## ')             then return '>1' end

  -- Level-2 fold headers: tool call and response summary lines
  if line:match('^🔧 %*%*Tool Call:')     then return '>2' end
  if line:match('^✅ %*%*Tool Response:') then return '>2' end

  -- Body lines inherit the fold level of the previous line.
  -- The backward scan is bounded by the distance to the nearest fold marker,
  -- which is always short in practice.
  return '='
end

-- Window-scoped options must go through setlocal, not vim.bo
vim.cmd('setlocal foldmethod=expr')
vim.cmd('setlocal foldexpr=v:lua.sven_fold_expr(v:lnum)')
vim.cmd('setlocal foldlevel=99')   -- all folds open on load / buffer update
vim.cmd('setlocal foldenable')
vim.cmd('setlocal foldminlines=0') -- allow even 1-line-body folds to collapse

"#;

        if let Err(e) = self.neovim.exec_lua(fold_config, vec![]).await {
            debug!("Could not configure folding: {:?}", e);
            // Fall back: at least ensure content is visible with no automatic folding
            let _ = self.neovim.command("setlocal foldmethod=manual foldlevel=99").await;
        }

        debug!("Buffer configuration completed");
        Ok(())
    }

    /// Set whether the buffer is modifiable
    pub async fn set_modifiable(&mut self, modifiable: bool) -> Result<()> {
        let cmd = if modifiable {
            "setlocal modifiable"
        } else {
            "setlocal nomodifiable"
        };
        
        self.neovim
            .command(cmd)
            .await
            .context("Failed to set modifiable")?;

        Ok(())
    }

    /// Refresh todo display enhancements (virtual text, highlights)
    pub async fn refresh_todo_display(&mut self) -> Result<()> {
        self.neovim
            .exec_lua("pcall(_G.sven_enhance_todos)", vec![])
            .await
            .context("Failed to refresh todo display")?;

        Ok(())
    }

    /// Add custom keymaps for conversation navigation
    #[allow(dead_code)]
    pub async fn setup_custom_keymaps(&mut self) -> Result<()> {
        // This is called once during initialization
        // Additional custom keymaps can be added here
        
        // Example: Add a keymap to collapse all tool outputs
        self.neovim
            .command("nnoremap <buffer> <leader>ct :g/^> 🔧/normal zc<CR>")
            .await
            .ok();  // Don't fail if keymap doesn't work

        Ok(())
    }

    /// Set cursor position programmatically (useful for auto-scrolling)
    #[allow(dead_code)]
    pub async fn set_cursor(&mut self, row: i64, col: i64) -> Result<()> {
        self.neovim
            .call_function("nvim_win_set_cursor", vec![Value::from(0), Value::from(vec![Value::from(row), Value::from(col)])])
            .await
            .context("Failed to set cursor")?;

        Ok(())
    }

    /// Render the grid to ratatui Lines
    pub async fn render_to_lines(&self, scroll: u16, visible_height: u16) -> Vec<Line<'static>> {
        let grid = self.grid.lock().await;
        let attrs = self.hl_attrs.lock().await;
        render_grid_to_lines(&grid, &attrs, scroll as usize, visible_height as usize)
    }

    /// Get current cursor position
    pub async fn get_cursor_pos(&self) -> (u16, u16) {
        *self.cursor_pos.lock().await
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use ratatui::style::{Color, Modifier, Style};
    use rmpv::Value;

    use super::*;

    // ─────────────────────────────────────────────────────────────────────────
    // Shared test helpers
    // ─────────────────────────────────────────────────────────────────────────

    /// Build a fresh handler with its backing shared state for inspection.
    fn make_handler() -> (NvimHandler, Arc<Mutex<Grid>>, Arc<Mutex<HashMap<u64, HlAttr>>>, Arc<Mutex<(u16, u16)>>) {
        let grid      = Arc::new(Mutex::new(Grid::new(80, 24)));
        let hl_attrs  = Arc::new(Mutex::new(HashMap::new()));
        let cursor    = Arc::new(Mutex::new((0u16, 0u16)));
        let handler   = NvimHandler::new(grid.clone(), hl_attrs.clone(), cursor.clone());
        (handler, grid, hl_attrs, cursor)
    }

    /// Encode a Neovim `grid_line` args array from (row, col_start, cells).
    /// Each cell is `(text, hl_id, repeat)`.
    fn grid_line_event(row: u64, col: u64, cells: &[(&str, u64, u64)]) -> Value {
        let cell_values: Vec<Value> = cells.iter().map(|(text, hl, repeat)| {
            Value::Array(vec![
                Value::String((*text).into()),
                Value::Integer((*hl).into()),
                Value::Integer((*repeat).into()),
            ])
        }).collect();
        Value::Array(vec![
            Value::Integer(1.into()),          // grid id (always 1)
            Value::Integer(row.into()),
            Value::Integer(col.into()),
            Value::Array(cell_values),
        ])
    }

    /// Collect all text content from rendered lines into a single string.
    fn lines_text(lines: &[ratatui::text::Line]) -> String {
        lines.iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect()
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Grid — data structure invariants
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn grid_new_allocates_correct_dimensions() {
        // Arrange
        let width = 80usize;
        let height = 24usize;

        // Act
        let g = Grid::new(width, height);

        // Assert
        assert_eq!(g.width,  width,  "grid width field");
        assert_eq!(g.height, height, "grid height field");
        assert_eq!(g.cells.len(),    height, "row count == height");
        assert_eq!(g.cells[0].len(), width,  "column count == width");
    }

    #[test]
    fn grid_cells_initialised_to_space_with_attr_zero() {
        // Arrange / Act
        let g = Grid::new(10, 5);

        // Assert — spot-check corners
        for &(r, c) in &[(0, 0), (0, 9), (4, 0), (4, 9)] {
            assert_eq!(g.cells[r][c].text,    " ", "cell ({r},{c}) text");
            assert_eq!(g.cells[r][c].attr_id, 0,   "cell ({r},{c}) attr_id");
        }
    }

    #[test]
    fn grid_set_cell_writes_text_and_attr_at_target() {
        // Arrange
        let mut g = Grid::new(10, 5);

        // Act
        g.set_cell(2, 3, Cell { text: "A".into(), attr_id: 7 });

        // Assert
        assert_eq!(g.cells[2][3].text,    "A", "text written");
        assert_eq!(g.cells[2][3].attr_id, 7,   "attr_id written");
        assert_eq!(g.cells[2][4].text,    " ", "adjacent cell unchanged");
    }

    #[test]
    fn grid_set_cell_ignores_out_of_bounds_without_panic() {
        // Arrange
        let mut g = Grid::new(10, 5);

        // Act — all three should silently no-op
        g.set_cell(100, 0,   Cell { text: "X".into(), attr_id: 1 });
        g.set_cell(0,   100, Cell { text: "X".into(), attr_id: 1 });
        g.set_cell(5,   0,   Cell { text: "X".into(), attr_id: 1 }); // row == height

        // Assert — grid unchanged
        assert_eq!(g.cells[0][0].text, " ");
    }

    #[test]
    fn grid_clear_resets_every_cell_to_default() {
        // Arrange
        let mut g = Grid::new(5, 3);
        g.set_cell(0, 0, Cell { text: "X".into(), attr_id: 9 });
        g.set_cell(2, 4, Cell { text: "Y".into(), attr_id: 1 });

        // Act
        g.clear();

        // Assert
        assert_eq!(g.cells[0][0].text,    " ", "top-left reset");
        assert_eq!(g.cells[0][0].attr_id, 0,   "top-left attr reset");
        assert_eq!(g.cells[2][4].text,    " ", "bottom-right reset");
    }

    #[test]
    fn grid_resize_updates_dimensions_and_discards_old_content() {
        // Arrange
        let mut g = Grid::new(80, 24);
        g.set_cell(0, 0, Cell { text: "X".into(), attr_id: 1 });

        // Act
        g.resize(40, 10);

        // Assert — dimensions updated
        assert_eq!(g.width,  40);
        assert_eq!(g.height, 10);
        assert_eq!(g.cells.len(),    10, "row count after resize");
        assert_eq!(g.cells[0].len(), 40, "col count after resize");
        // Old content must be gone (resize always re-allocates)
        assert_eq!(g.cells[0][0].text, " ", "content cleared after resize");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // HlAttr — colour and style mapping
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn hlattr_default_maps_to_plain_ratatui_style() {
        // Arrange
        let attr = HlAttr::default();

        // Act
        let style = attr.to_style();

        // Assert
        assert_eq!(style, Style::default());
    }

    #[test]
    fn hlattr_foreground_integer_decoded_as_rgb() {
        // Arrange — Neovim encodes colours as 0xRRGGBB integers
        let mut map = HashMap::new();
        map.insert("foreground".into(), Value::Integer(0xFF0000u32.into())); // pure red

        // Act
        let attr = HlAttr::from_map(&map);

        // Assert
        assert_eq!(attr.foreground, Some(Color::Rgb(255, 0, 0)));
        assert_eq!(attr.to_style().fg, Some(Color::Rgb(255, 0, 0)));
    }

    #[test]
    fn hlattr_background_integer_decoded_as_rgb() {
        // Arrange
        let mut map = HashMap::new();
        map.insert("background".into(), Value::Integer(0x0000FFu32.into())); // pure blue

        // Act
        let attr = HlAttr::from_map(&map);

        // Assert
        assert_eq!(attr.background, Some(Color::Rgb(0, 0, 255)));
        assert_eq!(attr.to_style().bg, Some(Color::Rgb(0, 0, 255)));
    }

    #[test]
    fn hlattr_green_channel_isolated_correctly() {
        // Arrange
        let mut map = HashMap::new();
        map.insert("foreground".into(), Value::Integer(0x00FF00u32.into())); // pure green

        // Act
        let attr = HlAttr::from_map(&map);

        // Assert — verify the middle byte ends up in the green channel
        assert_eq!(attr.foreground, Some(Color::Rgb(0, 255, 0)));
    }

    #[test]
    fn hlattr_bold_true_sets_bold_modifier() {
        // Arrange
        let mut map = HashMap::new();
        map.insert("bold".into(), Value::Boolean(true));

        // Act
        let attr  = HlAttr::from_map(&map);
        let style = attr.to_style();

        // Assert
        assert!(attr.bold, "bold field");
        assert!(style.add_modifier.contains(Modifier::BOLD), "BOLD modifier in style");
    }

    #[test]
    fn hlattr_italic_true_sets_italic_modifier() {
        // Arrange
        let mut map = HashMap::new();
        map.insert("italic".into(), Value::Boolean(true));

        // Act
        let attr  = HlAttr::from_map(&map);
        let style = attr.to_style();

        // Assert
        assert!(attr.italic, "italic field");
        assert!(style.add_modifier.contains(Modifier::ITALIC), "ITALIC modifier in style");
    }

    #[test]
    fn hlattr_bold_false_does_not_set_bold_modifier() {
        // Arrange
        let mut map = HashMap::new();
        map.insert("bold".into(), Value::Boolean(false));

        // Act
        let attr = HlAttr::from_map(&map);

        // Assert
        assert!(!attr.bold);
        assert!(!attr.to_style().add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn hlattr_combined_colour_and_bold_applied_together() {
        // Arrange
        let mut map = HashMap::new();
        map.insert("foreground".into(), Value::Integer(0xFF0000u32.into())); // red
        map.insert("bold".into(),       Value::Boolean(true));

        // Act
        let style = HlAttr::from_map(&map).to_style();

        // Assert — both colour and modifier present in the same style
        assert_eq!(style.fg, Some(Color::Rgb(255, 0, 0)));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Grid::scroll — region shift used by fold open/close
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn grid_scroll_up_positive_rows_shifts_content_and_clears_bottom() {
        // Arrange — 5-row grid; rows 0-4 contain letters A-E
        let mut g = Grid::new(4, 5);
        for r in 0..5usize {
            let ch = (b'A' + r as u8) as char;
            g.set_cell(r, 0, Cell { text: ch.to_string(), attr_id: 0 });
        }

        // Act — scroll the whole grid up by 2 rows
        g.scroll(0, 5, 0, 4, 2);

        // Assert — rows [0,3) now contain what was in [2,5); bottom 2 rows cleared
        assert_eq!(g.cells[0][0].text, "C", "row 0 ← former row 2");
        assert_eq!(g.cells[1][0].text, "D", "row 1 ← former row 3");
        assert_eq!(g.cells[2][0].text, "E", "row 2 ← former row 4");
        assert_eq!(g.cells[3][0].text, " ", "row 3 cleared (was outside source range)");
        assert_eq!(g.cells[4][0].text, " ", "row 4 cleared");
    }

    #[test]
    fn grid_scroll_down_negative_rows_shifts_content_and_clears_top() {
        // Arrange — 5-row grid; rows 0-4 contain letters A-E
        let mut g = Grid::new(4, 5);
        for r in 0..5usize {
            let ch = (b'A' + r as u8) as char;
            g.set_cell(r, 0, Cell { text: ch.to_string(), attr_id: 0 });
        }

        // Act — scroll the whole grid down by 2 rows (rows=-2)
        g.scroll(0, 5, 0, 4, -2);

        // Assert
        // Source rows [0, 3) move to dest rows [2, 5):
        //   row 0 (A) → row 2,  row 1 (B) → row 3,  row 2 (C) → row 4
        // Former rows 3 (D) and 4 (E) are overwritten by the shift.
        // Rows 0-1 are cleared (invalidated for Neovim to refill).
        assert_eq!(g.cells[0][0].text, " ", "row 0 cleared");
        assert_eq!(g.cells[1][0].text, " ", "row 1 cleared");
        assert_eq!(g.cells[2][0].text, "A", "row 2 ← former row 0");
        assert_eq!(g.cells[3][0].text, "B", "row 3 ← former row 1");
        assert_eq!(g.cells[4][0].text, "C", "row 4 ← former row 2");
    }

    #[test]
    fn grid_scroll_up_sub_region_does_not_touch_rows_outside_region() {
        // Arrange — rows 0..5; populate all
        let mut g = Grid::new(4, 6);
        for r in 0..6usize {
            let ch = (b'A' + r as u8) as char;
            g.set_cell(r, 0, Cell { text: ch.to_string(), attr_id: 0 });
        }

        // Act — scroll only the sub-region [1, 4) up by 1 row
        g.scroll(1, 4, 0, 4, 1);

        // Assert — row 0 and rows 4-5 are untouched
        assert_eq!(g.cells[0][0].text, "A", "row 0 outside region, unchanged");
        assert_eq!(g.cells[1][0].text, "C", "row 1 ← former row 2 (C)");
        assert_eq!(g.cells[2][0].text, "D", "row 2 ← former row 3 (D)");
        assert_eq!(g.cells[3][0].text, " ", "row 3 cleared (bottom of region)");
        assert_eq!(g.cells[4][0].text, "E", "row 4 outside region, unchanged");
        assert_eq!(g.cells[5][0].text, "F", "row 5 outside region, unchanged");
    }

    #[test]
    fn grid_scroll_zero_rows_is_a_noop() {
        // Arrange
        let mut g = Grid::new(4, 3);
        g.set_cell(0, 0, Cell { text: "X".into(), attr_id: 0 });

        // Act
        g.scroll(0, 3, 0, 4, 0);

        // Assert — nothing changed
        assert_eq!(g.cells[0][0].text, "X");
    }

    #[test]
    fn grid_scroll_simulates_fold_close_content_moves_up() {
        // Arrange — simulate a 6-line view:
        //   row 0: "---"          (fold header)
        //   row 1: ""             (blank)
        //   row 2: "**You:** hi"  (message body)
        //   row 3: ""             (blank)
        //   row 4: "**Agent:**"   (next section, below the fold)
        //   row 5: "response"
        let mut g = Grid::new(20, 6);
        let content = ["---", "", "**You:** hi", "", "**Agent:**", "response"];
        for (r, text) in content.iter().enumerate() {
            for (c, ch) in text.chars().enumerate() {
                g.set_cell(r, c, Cell { text: ch.to_string(), attr_id: 0 });
            }
        }

        // Act — close the fold at row 0 (rows 1-3 collapse): Neovim scrolls
        // region [1, 6) up by 3 rows so the agent section becomes adjacent.
        g.scroll(1, 6, 0, 20, 3);

        // Assert — the agent section is now at rows 1-2; rows 3-5 cleared
        let row1: String = g.cells[1].iter().map(|c| c.text.as_str()).collect::<String>().trim_end().to_string();
        let row2: String = g.cells[2].iter().map(|c| c.text.as_str()).collect::<String>().trim_end().to_string();
        assert_eq!(row1, "**Agent:**", "agent header shifted up to row 1");
        assert_eq!(row2, "response",   "agent body shifted up to row 2");
        assert_eq!(g.cells[3][0].text, " ", "row 3 cleared");
        assert_eq!(g.cells[0][0].text, "-", "fold header row 0 unchanged");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // NvimHandler — redraw protocol event parsing
    // ─────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn handler_grid_line_writes_chars_at_specified_row_and_col() {
        // Arrange
        let (handler, grid, _, _) = make_handler();
        let event = grid_line_event(3, 5, &[("H", 0, 1), ("i", 0, 1)]);

        // Act
        handler.handle_grid_line(&[event]).await;

        // Assert — both characters land at the right coordinates
        let g = grid.lock().await;
        assert_eq!(g.cells[3][5].text, "H", "row=3 col=5");
        assert_eq!(g.cells[3][6].text, "i", "row=3 col=6");
        assert_eq!(g.cells[3][4].text, " ", "col before start unchanged");
        assert_eq!(g.cells[3][7].text, " ", "col after end unchanged");
    }

    #[tokio::test]
    async fn handler_grid_line_repeat_field_fills_multiple_consecutive_cells() {
        // Arrange
        let (handler, grid, _, _) = make_handler();
        let event = grid_line_event(0, 0, &[("X", 0, 4)]);  // repeat=4

        // Act
        handler.handle_grid_line(&[event]).await;

        // Assert — exactly 4 cells filled, 5th untouched
        let g = grid.lock().await;
        assert_eq!(g.cells[0][0].text, "X", "col 0");
        assert_eq!(g.cells[0][1].text, "X", "col 1");
        assert_eq!(g.cells[0][2].text, "X", "col 2");
        assert_eq!(g.cells[0][3].text, "X", "col 3");
        assert_eq!(g.cells[0][4].text, " ", "col 4 must be untouched");
    }

    #[tokio::test]
    async fn handler_grid_line_attr_id_stored_with_each_cell() {
        // Arrange
        let (handler, grid, _, _) = make_handler();
        let event = grid_line_event(0, 0, &[("A", 7, 1)]);  // hl_id=7

        // Act
        handler.handle_grid_line(&[event]).await;

        // Assert
        let g = grid.lock().await;
        assert_eq!(g.cells[0][0].attr_id, 7, "attr_id must propagate to cell");
    }

    #[tokio::test]
    async fn handler_grid_line_multi_row_batch_writes_all_rows() {
        // Arrange — two-row update in a single `grid_line` event batch.
        // This is the normal Neovim behaviour: multiple rows are packed into
        // one batch.  Previously only `args.first()` was processed, so the
        // second (and all subsequent) rows were silently dropped — the root
        // cause of fold body content never appearing in the rendered grid.
        let (handler, grid, _, _) = make_handler();
        let row0 = grid_line_event(0, 0, &[("A", 0, 1)]);
        let row1 = grid_line_event(1, 0, &[("B", 0, 1)]);
        let row2 = grid_line_event(2, 0, &[("C", 0, 1)]);

        // Act — all three in one slice, mimicking a real Neovim batch
        handler.handle_grid_line(&[row0, row1, row2]).await;

        // Assert — every row was written
        let g = grid.lock().await;
        assert_eq!(g.cells[0][0].text, "A", "row 0");
        assert_eq!(g.cells[1][0].text, "B", "row 1");
        assert_eq!(g.cells[2][0].text, "C", "row 2");
    }

    #[tokio::test]
    async fn handler_grid_cursor_goto_stores_row_and_col() {
        // Arrange
        let (handler, _, _, cursor) = make_handler();
        let params = Value::Array(vec![
            Value::Integer(1.into()),
            Value::Integer(7.into()),   // row
            Value::Integer(12.into()),  // col
        ]);

        // Act
        handler.handle_grid_cursor_goto(&[params]).await;

        // Assert
        let pos = cursor.lock().await;
        assert_eq!(pos.0, 7,  "row stored");
        assert_eq!(pos.1, 12, "col stored");
    }

    #[tokio::test]
    async fn handler_grid_resize_updates_grid_dimensions() {
        // Arrange
        let (handler, grid, _, _) = make_handler();
        let params = Value::Array(vec![
            Value::Integer(1.into()),
            Value::Integer(120.into()),  // new width
            Value::Integer(40.into()),   // new height
        ]);

        // Act
        handler.handle_grid_resize(&[params]).await;

        // Assert
        let g = grid.lock().await;
        assert_eq!(g.width,  120);
        assert_eq!(g.height, 40);
    }

    #[tokio::test]
    async fn handler_grid_clear_resets_previously_written_cells() {
        // Arrange
        let (handler, grid, _, _) = make_handler();
        {
            let mut g = grid.lock().await;
            g.set_cell(0, 0, Cell { text: "Q".into(), attr_id: 3 });
        }

        // Act
        handler.handle_grid_clear(&[]).await;

        // Assert
        let g = grid.lock().await;
        assert_eq!(g.cells[0][0].text,    " ", "cell text reset");
        assert_eq!(g.cells[0][0].attr_id, 0,   "cell attr_id reset");
    }

    #[tokio::test]
    async fn handler_grid_scroll_delegates_scroll_to_grid() {
        // Arrange — grid with A at row 0 and B at row 2
        let (handler, grid, _, _) = make_handler();
        {
            let mut g = grid.lock().await;
            g.set_cell(0, 0, Cell { text: "A".into(), attr_id: 0 });
            g.set_cell(2, 0, Cell { text: "B".into(), attr_id: 0 });
        }
        // grid_scroll(grid=1, top=0, bot=3, left=0, right=80, rows=2, cols=0)
        let params = Value::Array(vec![
            Value::Integer(1.into()),   // grid id
            Value::Integer(0.into()),   // top
            Value::Integer(3.into()),   // bot
            Value::Integer(0.into()),   // left
            Value::Integer(80.into()),  // right
            Value::Integer(2.into()),   // rows (scroll up 2)
            Value::Integer(0.into()),   // cols
        ]);

        // Act
        handler.handle_grid_scroll(&[params]).await;

        // Assert — B (was at row 2) is now at row 0; rows 1-2 cleared
        let g = grid.lock().await;
        assert_eq!(g.cells[0][0].text, "B", "B moved from row 2 to row 0");
        assert_eq!(g.cells[1][0].text, " ", "row 1 cleared");
        assert_eq!(g.cells[2][0].text, " ", "row 2 cleared");
    }

    #[tokio::test]
    async fn handler_hl_attr_define_stores_colour_and_modifiers() {
        // Arrange
        let (handler, _, hl_attrs, _) = make_handler();
        let rgb_map = Value::Map(vec![
            (Value::String("foreground".into()), Value::Integer(0xFF0000u32.into())),
            (Value::String("bold".into()),       Value::Boolean(true)),
        ]);
        let params = Value::Array(vec![
            Value::Integer(42.into()),  // attr id
            rgb_map,
            Value::Map(vec![]),         // cterm attrs (ignored)
            Value::Array(vec![]),       // info (ignored)
        ]);

        // Act
        handler.handle_hl_attr_define(&[params]).await;

        // Assert
        let attrs = hl_attrs.lock().await;
        let attr  = attrs.get(&42).expect("attr 42 must be stored after hl_attr_define");
        assert_eq!(attr.foreground, Some(Color::Rgb(255, 0, 0)), "foreground colour");
        assert!(attr.bold, "bold flag");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // render_grid_to_lines — grid-to-ratatui conversion
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn render_produces_one_line_per_visible_row() {
        // Arrange
        let g     = Grid::new(10, 5);
        let attrs = HashMap::new();

        // Act
        let lines = render_grid_to_lines(&g, &attrs, 0, 3);

        // Assert
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn render_scroll_zero_maps_row_zero_to_first_output_line() {
        // Arrange
        let mut g = Grid::new(10, 5);
        g.set_cell(0, 0, Cell { text: "A".into(), attr_id: 0 });
        g.set_cell(1, 0, Cell { text: "B".into(), attr_id: 0 });
        let attrs = HashMap::new();

        // Act
        let lines = render_grid_to_lines(&g, &attrs, 0, 2);

        // Assert
        let row0: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        let row1: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(row0.contains('A'), "first output line comes from grid row 0");
        assert!(row1.contains('B'), "second output line comes from grid row 1");
    }

    #[test]
    fn render_scroll_offset_skips_leading_rows() {
        // Arrange — row 0 has "A", row 2 has "C"
        let mut g = Grid::new(10, 5);
        g.set_cell(0, 0, Cell { text: "A".into(), attr_id: 0 });
        g.set_cell(2, 0, Cell { text: "C".into(), attr_id: 0 });
        let attrs = HashMap::new();

        // Act — scroll=2 starts at grid row 2
        let lines = render_grid_to_lines(&g, &attrs, 2, 1);

        // Assert
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains('C'),  "row 2 content visible after scroll");
        assert!(!text.contains('A'), "row 0 content absent after scroll");
    }

    #[test]
    fn render_visible_height_limits_number_of_output_lines() {
        // Arrange
        let g     = Grid::new(10, 20);
        let attrs = HashMap::new();

        // Act
        let lines = render_grid_to_lines(&g, &attrs, 0, 5);

        // Assert
        assert_eq!(lines.len(), 5);
    }

    #[test]
    fn render_visible_height_capped_when_exceeds_grid_rows() {
        // Arrange
        let g     = Grid::new(10, 3);
        let attrs = HashMap::new();

        // Act — request more rows than exist
        let lines = render_grid_to_lines(&g, &attrs, 0, 100);

        // Assert
        assert_eq!(lines.len(), 3, "output must not exceed grid height");
    }

    #[test]
    fn render_consecutive_cells_with_same_attr_merged_into_one_span() {
        // Arrange — four cells, all attr_id=1
        let mut g = Grid::new(4, 1);
        for col in 0..4 {
            g.set_cell(0, col, Cell { text: "x".into(), attr_id: 1 });
        }
        let attrs = HashMap::new(); // attr 1 unregistered → default style

        // Act
        let lines = render_grid_to_lines(&g, &attrs, 0, 1);

        // Assert — single merged span containing all four chars
        assert_eq!(lines[0].spans.len(), 1,      "four same-attr cells → one span");
        assert_eq!(lines[0].spans[0].content, "xxxx");
    }

    #[test]
    fn render_attr_change_between_cells_splits_into_separate_spans() {
        // Arrange — two cells with attr 1 followed by two with attr 2
        let mut g = Grid::new(4, 1);
        g.set_cell(0, 0, Cell { text: "A".into(), attr_id: 1 });
        g.set_cell(0, 1, Cell { text: "B".into(), attr_id: 1 });
        g.set_cell(0, 2, Cell { text: "C".into(), attr_id: 2 });
        g.set_cell(0, 3, Cell { text: "D".into(), attr_id: 2 });
        let mut attrs = HashMap::new();
        attrs.insert(1, HlAttr { foreground: Some(Color::Green), ..HlAttr::default() });
        attrs.insert(2, HlAttr { foreground: Some(Color::Red),   ..HlAttr::default() });

        // Act
        let lines = render_grid_to_lines(&g, &attrs, 0, 1);

        // Assert — two spans with correct text and colours
        assert_eq!(lines[0].spans.len(), 2, "attr boundary must create new span");
        assert_eq!(lines[0].spans[0].content,    "AB");
        assert_eq!(lines[0].spans[0].style.fg,   Some(Color::Green));
        assert_eq!(lines[0].spans[1].content,    "CD");
        assert_eq!(lines[0].spans[1].style.fg,   Some(Color::Red));
    }

    #[test]
    fn render_unregistered_attr_id_uses_default_style() {
        // Arrange — cell references attr_id 99, which is not in the map
        let mut g = Grid::new(2, 1);
        g.set_cell(0, 0, Cell { text: "?".into(), attr_id: 99 });
        let attrs: HashMap<u64, HlAttr> = HashMap::new();

        // Act
        let lines = render_grid_to_lines(&g, &attrs, 0, 1);

        // Assert — must not panic; style falls back to default
        let span = &lines[0].spans[0];
        assert_eq!(span.style, Style::default());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // NvimBridge — integration tests against a real Neovim process
    //
    // These run unconditionally. When `nvim` is not in PATH they skip
    // automatically without failing, making them safe in CI environments.
    // ─────────────────────────────────────────────────────────────────────────

    mod nvim_integration {
        use tokio::time::{sleep, Duration};

        use super::{lines_text, super::*};

        /// Returns false when `nvim` is not available, causing the calling
        /// test to return early (skip) rather than fail.
        fn nvim_available() -> bool {
            std::process::Command::new("nvim")
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }

        /// Spawn a 80×24 bridge for testing; panics on failure.
        async fn spawn_bridge() -> NvimBridge {
            NvimBridge::spawn(80, 24).await
                .expect("NvimBridge::spawn failed — is nvim installed?")
        }

        // ── Spawn ─────────────────────────────────────────────────────────────

        #[tokio::test]
        async fn spawn_creates_a_live_bridge() {
            // Arrange
            if !nvim_available() { return; }

            // Act
            let bridge = spawn_bridge().await;

            // Assert — basic dimension fields were initialised
            assert_eq!(bridge.width,  80);
            assert_eq!(bridge.height, 24);
        }

        // ── Buffer content ────────────────────────────────────────────────────

        #[tokio::test]
        async fn set_buffer_content_then_get_returns_same_text() {
            // Arrange
            if !nvim_available() { return; }
            let mut bridge  = spawn_bridge().await;
            let content     = "Hello\nWorld\nLine three";

            // Act
            bridge.set_buffer_content(content).await
                .expect("set_buffer_content must succeed");
            sleep(Duration::from_millis(100)).await;
            let got = bridge.get_buffer_content().await
                .expect("get_buffer_content must succeed");

            // Assert — Neovim may strip a trailing newline, compare trimmed
            assert_eq!(got.trim(), content.trim());
        }

        #[tokio::test]
        async fn set_buffer_content_empty_clears_buffer() {
            // Arrange
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.set_buffer_content("initial content").await.unwrap();
            sleep(Duration::from_millis(50)).await;

            // Act
            bridge.set_buffer_content("").await
                .expect("set_buffer_content(\"\") must succeed");
            sleep(Duration::from_millis(50)).await;
            let got = bridge.get_buffer_content().await.unwrap();

            // Assert
            assert!(got.trim().is_empty(), "buffer must be empty; got: {:?}", got);
        }

        #[tokio::test]
        async fn set_buffer_content_overwrites_previous_content() {
            // Arrange
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.set_buffer_content("old line").await.unwrap();
            sleep(Duration::from_millis(50)).await;

            // Act
            bridge.set_buffer_content("new line").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            let got = bridge.get_buffer_content().await.unwrap();

            // Assert
            assert!(got.contains("new line"), "new content present");
            assert!(!got.contains("old line"), "old content must be gone");
        }

        // ── Configuration ─────────────────────────────────────────────────────

        #[tokio::test]
        async fn configure_buffer_succeeds_without_error() {
            // Arrange
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;

            // Act
            let result = bridge.configure_buffer().await;

            // Assert
            assert!(result.is_ok(), "configure_buffer must not return an error: {:?}", result);
        }

        #[tokio::test]
        async fn configure_buffer_preserves_content_set_afterwards() {
            // Arrange
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.configure_buffer().await.unwrap();
            sleep(Duration::from_millis(100)).await;

            // Act
            let content = "---\n\n**You:** hello\n\n**Agent:** world";
            bridge.set_buffer_content(content).await.unwrap();
            sleep(Duration::from_millis(100)).await;
            let got = bridge.get_buffer_content().await.unwrap();

            // Assert
            assert!(got.contains("hello"), "user message retained after configure");
            assert!(got.contains("world"), "agent message retained after configure");
        }

        // ── Modifiable ────────────────────────────────────────────────────────

        #[tokio::test]
        async fn set_modifiable_false_prevents_edit_via_input() {
            // Arrange
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.set_buffer_content("locked content").await.unwrap();
            sleep(Duration::from_millis(100)).await;

            // Act — mark read-only, then try to type in insert mode
            bridge.set_modifiable(false).await
                .expect("set_modifiable(false) must not fail");
            bridge.send_input("iATTEMPT<Esc>").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            let got = bridge.get_buffer_content().await.unwrap();

            // Assert — the typed text must not appear
            assert!(!got.contains("ATTEMPT"),
                "nomodifiable buffer must reject typed input; content: {:?}", got);
        }

        #[tokio::test]
        async fn set_modifiable_true_allows_edit_via_input() {
            // Arrange
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.set_buffer_content("editable").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            bridge.set_modifiable(false).await.unwrap();

            // Act — re-enable, then type
            bridge.set_modifiable(true).await
                .expect("set_modifiable(true) must not fail");
            bridge.send_input("GAAPPENDED<Esc>").await.unwrap();  // G=end, A=append
            sleep(Duration::from_millis(150)).await;
            let got = bridge.get_buffer_content().await.unwrap();

            // Assert
            assert!(got.contains("APPENDED"),
                "modifiable buffer must accept typed input; content: {:?}", got);
        }

        // ── Resize ────────────────────────────────────────────────────────────

        #[tokio::test]
        async fn resize_updates_stored_dimensions() {
            // Arrange
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;

            // Act
            bridge.resize(120, 40).await
                .expect("resize must not fail");

            // Assert — Rust-side dimensions updated
            assert_eq!(bridge.width,  120);
            assert_eq!(bridge.height, 40);
        }

        // ── Rendering ────────────────────────────────────────────────────────

        #[tokio::test]
        async fn render_to_lines_reflects_buffer_content() {
            // Arrange
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.set_buffer_content("Hello from Neovim").await.unwrap();
            // Wait for Neovim to process the buffer update and send redraw events
            sleep(Duration::from_millis(200)).await;

            // Act
            let lines = bridge.render_to_lines(0, 24).await;

            // Assert
            assert!(!lines.is_empty(), "render must produce lines");
            let text = lines_text(&lines);
            assert!(text.contains("Hello from Neovim"),
                "rendered text must include what was set; got first 200 chars: {:?}",
                &text[..text.len().min(200)]);
        }

        #[tokio::test]
        async fn render_to_lines_after_content_change_reflects_new_text() {
            // Arrange
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.set_buffer_content("first version").await.unwrap();
            sleep(Duration::from_millis(100)).await;

            // Act — replace content
            bridge.set_buffer_content("second version").await.unwrap();
            sleep(Duration::from_millis(200)).await;
            let lines = bridge.render_to_lines(0, 24).await;

            // Assert
            let text = lines_text(&lines);
            assert!(text.contains("second version"), "updated content visible");
            assert!(!text.contains("first version"),  "old content gone");
        }

        // ── Cursor / input ────────────────────────────────────────────────────

        #[tokio::test]
        async fn send_input_j_moves_cursor_to_next_row() {
            // Arrange
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.set_buffer_content("line1\nline2\nline3").await.unwrap();
            sleep(Duration::from_millis(100)).await;

            // Act
            bridge.send_input("j").await
                .expect("send_input must not fail");
            sleep(Duration::from_millis(100)).await;
            let (row, _col) = bridge.get_cursor_pos().await;

            // Assert
            assert_eq!(row, 1, "pressing 'j' must move cursor to row 1");
        }

        #[tokio::test]
        async fn send_input_gg_moves_cursor_to_first_row() {
            // Arrange
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.set_buffer_content("line1\nline2\nline3").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            bridge.send_input("G").await.unwrap(); // go to last line
            sleep(Duration::from_millis(100)).await;

            // Act
            bridge.send_input("gg").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            let (row, _) = bridge.get_cursor_pos().await;

            // Assert
            assert_eq!(row, 0, "'gg' must return cursor to row 0");
        }

        // ── Fold diagnostics ──────────────────────────────────────────────────

        #[tokio::test]
        async fn diagnose_fold_state_step_by_step() {
            // This diagnostic test probes the exact fold state at each step to
            // understand why folds appear closed despite foldlevel=99.
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;

            // Step 1: configure buffer
            bridge.configure_buffer().await.unwrap();
            sleep(Duration::from_millis(150)).await;

            // Read foldlevel immediately after configure_buffer
            let fl_after_config: Value = bridge.neovim
                .eval("&foldlevel")
                .await
                .unwrap_or(Value::from(-1i64));
            let fm_after_config: Value = bridge.neovim
                .eval("&foldmethod")
                .await
                .unwrap_or(Value::from("unknown"));

            // Step 2: set content
            bridge.set_buffer_content("---\n\nbody\n").await.unwrap();
            sleep(Duration::from_millis(200)).await;

            // Read foldlevel after set_buffer_content
            let fl_after_content: Value = bridge.neovim
                .eval("&foldlevel")
                .await
                .unwrap_or(Value::from(-1i64));
            // Check if line 2 (body) is in a closed fold
            let fc2: Value = bridge.neovim
                .eval("foldclosed(2)")
                .await
                .unwrap_or(Value::from(-99i64));

            eprintln!("foldlevel after configure: {:?}", fl_after_config);
            eprintln!("foldmethod after configure: {:?}", fm_after_config);
            eprintln!("foldlevel after content:   {:?}", fl_after_content);
            eprintln!("foldclosed(2) after content: {:?}  (-1=open, N=first line of fold)", fc2);

            // Just assert foldlevel was set — we're using this test for diagnostics
            // The fold state info is printed above and helps debug integration issues.
            assert!(fl_after_config.as_u64().unwrap_or(0) >= 1,
                "foldlevel must be >= 1 after configure_buffer; got: {:?}", fl_after_config);
        }

        // ── Fold behaviour ────────────────────────────────────────────────────

        #[tokio::test]
        async fn closing_a_fold_hides_its_body_content() {
            // Arrange — two-turn conversation; the first turn's body includes the
            // unique marker "UNIQUE_BODY_CONTENT" which must be visible when the
            // fold is open and invisible when closed.
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.configure_buffer().await.unwrap();
            sleep(Duration::from_millis(100)).await;
            let content = concat!(
                "---\n",
                "\n",
                "**You:** UNIQUE_BODY_CONTENT\n",
                "\n",
                "**Agent:** first answer\n",
                "\n",
                "---\n",
                "\n",
                "**You:** second question\n",
            );
            bridge.set_buffer_content(content).await.unwrap();
            sleep(Duration::from_millis(200)).await;

            // Verify the body is visible before closing
            let before = lines_text(&bridge.render_to_lines(0, 24).await);
            assert!(before.contains("UNIQUE_BODY_CONTENT"),
                "body content must be visible before fold is closed; rendered: {:?}",
                &before[..before.len().min(300)]);

            // Act — position cursor on the first '---' fold header and close the fold
            bridge.send_input("gg").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            bridge.send_input("zc").await.unwrap();
            sleep(Duration::from_millis(250)).await;

            // Assert — body content is no longer visible in the rendered grid
            let after = lines_text(&bridge.render_to_lines(0, 24).await);
            assert!(!after.contains("UNIQUE_BODY_CONTENT"),
                "body content must be hidden after fold is closed; rendered: {:?}",
                &after[..after.len().min(300)]);
        }

        #[tokio::test]
        async fn reopening_a_fold_makes_body_content_visible_again() {
            // Arrange
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.configure_buffer().await.unwrap();
            sleep(Duration::from_millis(100)).await;
            let content = "---\n\n**You:** REOPEN_MARKER\n\n**Agent:** world\n";
            bridge.set_buffer_content(content).await.unwrap();
            sleep(Duration::from_millis(200)).await;

            // Close the fold
            bridge.send_input("ggzc").await.unwrap();
            sleep(Duration::from_millis(200)).await;
            let hidden = lines_text(&bridge.render_to_lines(0, 24).await);
            assert!(!hidden.contains("REOPEN_MARKER"),
                "marker must be hidden after close");

            // Act — reopen
            bridge.send_input("zo").await.unwrap();
            sleep(Duration::from_millis(250)).await;
            let visible = lines_text(&bridge.render_to_lines(0, 24).await);

            // Assert — marker is visible again
            assert!(visible.contains("REOPEN_MARKER"),
                "body content must reappear after fold is reopened; rendered: {:?}",
                &visible[..visible.len().min(300)]);
        }

        // ── Full lifecycle ────────────────────────────────────────────────────

        #[tokio::test]
        async fn full_tui_startup_lifecycle_preserves_all_conversation_parts() {
            // Arrange — mirrors what the TUI does on startup:
            //   spawn → configure → load conversation → read back
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.configure_buffer().await.unwrap();
            sleep(Duration::from_millis(100)).await;
            let content = "---\n\n**You:** hello\n\n**Agent:** world\n\n🔧 **Tool Call: read_file**\n```\npath:/tmp/x\n```\n\n✅ **Tool Response: read_file**\n```\ncontents\n```\n";

            // Act
            bridge.set_buffer_content(content).await.unwrap();
            sleep(Duration::from_millis(200)).await;
            let got = bridge.get_buffer_content().await.unwrap();

            // Assert — all conversation parts survive the full configure→load cycle
            assert!(got.contains("hello"),     "user message in buffer");
            assert!(got.contains("world"),     "agent message in buffer");
            assert!(got.contains("read_file"), "tool name in buffer");
            assert!(got.contains("contents"),  "tool output in buffer");
        }

        #[tokio::test]
        async fn full_tui_startup_lifecycle_produces_rendered_output() {
            // Arrange — same lifecycle, but verifying the renderer produces lines.
            // After configure_buffer activates foldmethod=expr, Neovim needs a full
            // fold-evaluation cycle; we only assert that the first visible row
            // carries content, not that all lines are simultaneously visible.
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.configure_buffer().await.unwrap();
            sleep(Duration::from_millis(150)).await;
            let content = "first line only";

            // Act
            bridge.set_buffer_content(content).await.unwrap();
            sleep(Duration::from_millis(250)).await;
            let lines = bridge.render_to_lines(0, 24).await;

            // Assert — renderer emits rows and at least the first row carries the text
            assert!(!lines.is_empty(), "render must produce lines after full setup");
            let rendered = lines_text(&lines);
            assert!(rendered.contains("first line only"),
                "first row of content must appear in rendered output; got: {:?}",
                &rendered[..rendered.len().min(200)]);
        }
    }
}
