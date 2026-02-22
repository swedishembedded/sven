// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! `NvimBridge`: manages the embedded Neovim process, buffer content, and the
//! connection between Neovim's redraw grid and the ratatui render pipeline.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use nvim_rs::{
    compat::tokio::Compat,
    create::tokio as create,
    exttypes::Buffer,
    Neovim,
    uioptions::UiAttachOptions,
};
use ratatui::text::Line;
#[cfg(test)]
use rmpv::Value;
use tokio::{
    process::{Child, ChildStdin, Command},
    sync::{Mutex, Notify},
};
use tracing::debug;

use super::{
    grid::{Grid, HlAttr},
    handler::NvimHandler,
    render::render_grid_to_lines,
};

/// Bridge to an embedded Neovim instance.
pub struct NvimBridge {
    pub(crate) neovim: Neovim<Compat<ChildStdin>>,
    _child: Child,
    grid: Arc<Mutex<Grid>>,
    hl_attrs: Arc<Mutex<HashMap<u64, HlAttr>>>,
    cursor_pos: Arc<Mutex<(u16, u16)>>,
    pub width: u16,
    /// Number of rows in the Neovim grid (== chat pane inner height).
    /// Pass this as `visible_height` to `render_to_lines`; never pass a
    /// smaller value or a non-zero scroll offset.
    pub height: u16,
    buffer: Buffer<Compat<ChildStdin>>,
    /// Fired by `NvimHandler` after every `flush` event.  The TUI render loop
    /// waits on this so it re-renders immediately after Neovim finishes
    /// processing each input.
    pub flush_notify: Arc<Notify>,
    /// Fired when Neovim sends `sven_submit` (triggered by `:w`).
    pub submit_notify: Arc<Notify>,
    /// Fired when Neovim sends `sven_quit` (triggered by `:q` / `:qa`).
    pub quit_notify: Arc<Notify>,
}

impl NvimBridge {
    /// Spawn a new Neovim instance and attach as UI.
    pub async fn spawn(width: u16, height: u16) -> Result<Self> {
        debug!("Spawning Neovim with dimensions {}x{}", width, height);

        let grid          = Arc::new(Mutex::new(Grid::new(width as usize, height as usize)));
        let hl_attrs      = Arc::new(Mutex::new(HashMap::new()));
        let cursor_pos    = Arc::new(Mutex::new((0u16, 0u16)));
        let flush_notify  = Arc::new(Notify::new());
        let submit_notify = Arc::new(Notify::new());
        let quit_notify   = Arc::new(Notify::new());

        let handler = NvimHandler::new(
            grid.clone(), hl_attrs.clone(), cursor_pos.clone(),
            flush_notify.clone(), submit_notify.clone(), quit_notify.clone(),
        );

        let mut cmd = Command::new("nvim");
        cmd.arg("--embed").arg("--clean");

        let (neovim, _io_handle, child) = create::new_child_cmd(&mut cmd, handler)
            .await
            .context("Failed to create Neovim session")?;

        debug!("Attaching UI");
        let mut opts = UiAttachOptions::new();
        opts.set_linegrid_external(true).set_rgb(true);
        neovim
            .ui_attach(width as i64, height as i64, &opts)
            .await
            .context("Failed to attach UI")?;

        let buffer = neovim
            .create_buf(false, true)
            .await
            .context("Failed to create buffer")?;
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
            flush_notify,
            submit_notify,
            quit_notify,
        })
    }

    /// Replace the entire conversation buffer content.
    pub async fn set_buffer_content(&mut self, content: &str) -> Result<()> {
        let lines: Vec<String> = if content.is_empty() {
            vec![]
        } else {
            content.split('\n').map(|s| s.to_string()).collect()
        };
        self.buffer
            .set_lines(0, -1, false, lines)
            .await
            .context("Failed to set buffer lines")?;
        Ok(())
    }

    /// Read the current content of the conversation buffer.
    pub async fn get_buffer_content(&self) -> Result<String> {
        let lines = self.buffer
            .get_lines(0, -1, false)
            .await
            .context("Failed to get buffer lines")?;
        Ok(lines.join("\n"))
    }

    /// Forward key input to Neovim.
    pub async fn send_input(&mut self, keys: &str) -> Result<()> {
        self.neovim
            .input(keys)
            .await
            .context("Failed to send input to Neovim")?;
        Ok(())
    }

    /// Resize the Neovim UI.
    pub async fn resize(&mut self, width: u16, height: u16) -> Result<()> {
        self.width  = width;
        self.height = height;
        self.neovim
            .ui_try_resize(width as i64, height as i64)
            .await
            .context("Failed to resize UI")?;
        Ok(())
    }

    /// Apply markdown filetype, fold expression, wrap settings, and register
    /// the `:w` / `:q` custom commands for this buffer.
    pub async fn configure_buffer(&mut self) -> Result<()> {
        if let Err(e) = self.neovim.command("setlocal filetype=markdown").await {
            debug!("Could not set filetype: {:?}", e);
        }

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

        let fold_config = r#"
-- Fold expression for sven conversation structure.
function _G.sven_fold_expr(lnum)
  local ok, line = pcall(vim.fn.getline, lnum)
  if not ok then return '=' end

  -- Level-1 fold headers: user message separators, agent response lines,
  -- and metadata headers for tool calls/results/thinking
  if line:match('^%-%-%-$')              then return '>1' end
  if line:match('^%*%*Agent:%*%*')       then return '>1' end
  if line:match('^%*%*Agent:tool_call:') then return '>1' end
  if line:match('^%*%*Agent:thinking%*%*') then return '>1' end
  if line:match('^%*%*Tool:')            then return '>1' end
  if line:match('^%*%*You:%*%*')         then return '>1' end
  if line:match('^%*%*System:%*%*')      then return '>1' end
  if line:match('^## ')                  then return '>1' end

  -- Level-2 fold headers: visual display lines for tool calls/responses/thinking
  if line:match('^🔧 %*%*Tool Call:')     then return '>2' end
  if line:match('^✅ %*%*Tool Response:') then return '>2' end
  if line:match('^💭 %*%*Thought')        then return '>2' end

  -- Body lines inherit the fold level of the previous line.
  return '='
end

vim.cmd('setlocal foldmethod=expr')
vim.cmd('setlocal foldexpr=v:lua.sven_fold_expr(v:lnum)')
vim.cmd('setlocal foldlevel=1')
vim.cmd('setlocal foldenable')
vim.cmd('setlocal foldminlines=0')
"#;
        if let Err(e) = self.neovim.exec_lua(fold_config, vec![]).await {
            debug!("Could not configure folding: {:?}", e);
            let _ = self.neovim.command("setlocal foldmethod=manual foldlevel=99").await;
        }

        let submit_handler = r#"
local buf = vim.api.nvim_get_current_buf()

local function submit_conversation()
  vim.bo[buf].modified = false
  vim.rpcnotify(1, 'sven_submit', {})
end

vim.api.nvim_buf_create_user_command(buf, 'Submit', submit_conversation, {
  desc = 'Submit the conversation to the agent'
})
vim.api.nvim_buf_create_user_command(buf, 'W', submit_conversation, {
  desc = 'Submit the conversation to the agent (alias for :Submit)'
})
vim.api.nvim_buf_create_user_command(buf, 'w', submit_conversation, {
  desc = 'Submit the conversation to the agent (overrides write)'
})

local function quit_sven()
  vim.rpcnotify(1, 'sven_quit', {})
end

vim.api.nvim_buf_create_user_command(buf, 'q',  quit_sven, { desc = 'Quit sven' })
vim.api.nvim_buf_create_user_command(buf, 'qa', quit_sven, { desc = 'Quit sven (alias for :q)' })

vim.keymap.set('n', '<2-LeftMouse>', function()
  local line = vim.fn.line('.')
  if vim.fn.foldlevel(line) > 0 then
    vim.cmd('normal! za')
  end
end, { buffer = buf, silent = true, desc = 'Toggle fold under cursor' })
"#;
        if let Err(e) = self.neovim.exec_lua(submit_handler, vec![]).await {
            debug!("Could not configure submit/quit handlers: {:?}", e);
        }

        debug!("Buffer configuration completed");
        Ok(())
    }

    /// Toggle the buffer's `modifiable` flag.
    pub async fn set_modifiable(&mut self, modifiable: bool) -> Result<()> {
        let cmd = if modifiable { "setlocal modifiable" } else { "setlocal nomodifiable" };
        self.neovim
            .command(cmd)
            .await
            .context("Failed to set modifiable")?;
        Ok(())
    }

    /// Refresh todo display enhancements (virtual text, highlights).
    pub async fn refresh_todo_display(&mut self) -> Result<()> {
        self.neovim
            .exec_lua("pcall(_G.sven_enhance_todos)", vec![])
            .await
            .context("Failed to refresh todo display")?;
        Ok(())
    }

    /// Render the current Neovim grid to ratatui `Line`s.
    pub async fn render_to_lines(&self, scroll: u16, visible_height: u16) -> Vec<Line<'static>> {
        let grid  = self.grid.lock().await;
        let attrs = self.hl_attrs.lock().await;
        render_grid_to_lines(&grid, &attrs, scroll as usize, visible_height as usize)
    }

    /// Return the current cursor position (0-indexed grid row/col) as tracked
    /// by `NvimHandler`.
    pub async fn get_cursor_pos(&self) -> (u16, u16) {
        *self.cursor_pos.lock().await
    }

    /// Query Neovim for the 1-indexed buffer line the cursor is on.
    #[cfg(test)]
    pub async fn get_cursor_line_in_buffer(&self) -> Result<i64> {
        let result = self.neovim
            .call_function("line", vec![Value::from(".")])
            .await
            .context("Failed to query cursor line from Neovim")?;
        match result {
            Value::Integer(n) => Ok(n.as_i64().unwrap_or(1)),
            other => Err(anyhow::anyhow!("Unexpected result from line('.'): {:?}", other)),
        }
    }

    /// Query Neovim for the total number of lines in the current buffer.
    #[cfg(test)]
    pub async fn get_buffer_line_count(&self) -> Result<i64> {
        let result = self.neovim
            .call_function("line", vec![Value::from("$")])
            .await
            .context("Failed to query buffer line count from Neovim")?;
        match result {
            Value::Integer(n) => Ok(n.as_i64().unwrap_or(1)),
            other => Err(anyhow::anyhow!("Unexpected result from line('$'): {:?}", other)),
        }
    }

    /// Evaluate a Vimscript expression and return the raw msgpack value.
    #[cfg(test)]
    pub async fn eval_vim(&self, expr: &str) -> Result<Value> {
        self.neovim
            .eval(expr)
            .await
            .context("Failed to eval Vimscript expression")
    }
}

// ── Integration tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    mod nvim_integration {
        use tokio::time::{sleep, Duration};

        use crate::nvim::bridge::NvimBridge;
        use rmpv::Value;

        fn nvim_available() -> bool {
            std::process::Command::new("nvim")
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }

        async fn spawn_bridge() -> NvimBridge {
            NvimBridge::spawn(80, 24).await
                .expect("NvimBridge::spawn failed — is nvim installed?")
        }

        async fn spawn_configured_bridge() -> NvimBridge {
            let mut bridge = spawn_bridge().await;
            bridge.configure_buffer().await
                .expect("configure_buffer must not fail in tests");
            sleep(Duration::from_millis(100)).await;
            bridge
        }

        fn lines_text(lines: &[ratatui::text::Line]) -> String {
            lines.iter()
                .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
                .collect()
        }

        fn two_turn_conversation() -> String {
            let t1u = "---\n\n**You:** Hello there\n";
            let t1a = "\n**Agent:** Hi! How can I help you today?\n";
            let t2u = "---\n\n**You:** What is 2+2?\n";
            let t2a = "\n**Agent:** 2+2 equals 4.\n";
            format!("{t1u}{t1a}{t2u}{t2a}")
        }

        fn conversation_with_tool_call() -> String {
            let user     = "---\n\n**You:** Search for something\n";
            let tc       = "\n**Agent:tool_call:call_abc**\n🔧 **Tool Call: search**\n```json\n{\"q\":\"something\"}\n```\n";
            let tr       = "\n**Tool:call_abc**\n✅ **Tool Response: search**\n```\n{\"results\":[]}\n```\n";
            let response = "\n**Agent:** I found nothing.\n";
            format!("{user}{tc}{tr}{response}")
        }

        fn conversation_with_long_response() -> String {
            let user = "---\n\n**You:** Tell me a long story\n";
            let long_line: String = "WORD ".repeat(20);
            let agent = format!("\n**Agent:** {long_line}\n");
            format!("{user}{agent}")
        }

        #[tokio::test]
        async fn spawn_creates_a_live_bridge() {
            if !nvim_available() { return; }
            let bridge = spawn_bridge().await;
            assert_eq!(bridge.width,  80);
            assert_eq!(bridge.height, 24);
        }

        #[tokio::test]
        async fn set_buffer_content_then_get_returns_same_text() {
            if !nvim_available() { return; }
            let mut bridge  = spawn_bridge().await;
            let content     = "Hello\nWorld\nLine three";
            bridge.set_buffer_content(content).await.expect("set_buffer_content must succeed");
            sleep(Duration::from_millis(100)).await;
            let got = bridge.get_buffer_content().await.expect("get_buffer_content must succeed");
            assert_eq!(got.trim(), content.trim());
        }

        #[tokio::test]
        async fn set_buffer_content_empty_clears_buffer() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.set_buffer_content("initial content").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.set_buffer_content("").await.expect("set_buffer_content(\"\") must succeed");
            sleep(Duration::from_millis(50)).await;
            let got = bridge.get_buffer_content().await.unwrap();
            assert!(got.trim().is_empty(), "buffer must be empty; got: {:?}", got);
        }

        #[tokio::test]
        async fn set_buffer_content_overwrites_previous_content() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.set_buffer_content("old line").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.set_buffer_content("new line").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            let got = bridge.get_buffer_content().await.unwrap();
            assert!(got.contains("new line"), "new content present");
            assert!(!got.contains("old line"), "old content must be gone");
        }

        #[tokio::test]
        async fn configure_buffer_succeeds_without_error() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            let result = bridge.configure_buffer().await;
            assert!(result.is_ok(), "configure_buffer must not return an error: {:?}", result);
        }

        #[tokio::test]
        async fn configure_buffer_preserves_content_set_afterwards() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.configure_buffer().await.unwrap();
            sleep(Duration::from_millis(100)).await;
            let content = "---\n\n**You:** hello\n\n**Agent:** world";
            bridge.set_buffer_content(content).await.unwrap();
            sleep(Duration::from_millis(100)).await;
            let got = bridge.get_buffer_content().await.unwrap();
            assert!(got.contains("hello"), "user message retained after configure");
            assert!(got.contains("world"), "agent message retained after configure");
        }

        #[tokio::test]
        async fn set_modifiable_false_prevents_edit_via_input() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.set_buffer_content("locked content").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            bridge.set_modifiable(false).await.expect("set_modifiable(false) must not fail");
            bridge.send_input("iATTEMPT<Esc>").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            let got = bridge.get_buffer_content().await.unwrap();
            assert!(!got.contains("ATTEMPT"),
                "nomodifiable buffer must reject typed input; content: {:?}", got);
        }

        #[tokio::test]
        async fn set_modifiable_true_allows_edit_via_input() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.set_buffer_content("editable").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            bridge.set_modifiable(false).await.unwrap();
            bridge.set_modifiable(true).await.expect("set_modifiable(true) must not fail");
            bridge.send_input("GAAPPENDED<Esc>").await.unwrap();
            sleep(Duration::from_millis(150)).await;
            let got = bridge.get_buffer_content().await.unwrap();
            assert!(got.contains("APPENDED"),
                "modifiable buffer must accept typed input; content: {:?}", got);
        }

        #[tokio::test]
        async fn resize_updates_stored_dimensions() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.resize(120, 40).await.expect("resize must not fail");
            assert_eq!(bridge.width,  120);
            assert_eq!(bridge.height, 40);
        }

        #[tokio::test]
        async fn render_to_lines_reflects_buffer_content() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.set_buffer_content("Hello from Neovim").await.unwrap();
            sleep(Duration::from_millis(200)).await;
            let lines = bridge.render_to_lines(0, 24).await;
            assert!(!lines.is_empty(), "render must produce lines");
            let text = lines_text(&lines);
            assert!(text.contains("Hello from Neovim"),
                "rendered text must include what was set; got first 200 chars: {:?}",
                &text[..text.len().min(200)]);
        }

        #[tokio::test]
        async fn render_to_lines_after_content_change_reflects_new_text() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.set_buffer_content("first version").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            bridge.set_buffer_content("second version").await.unwrap();
            sleep(Duration::from_millis(200)).await;
            let lines = bridge.render_to_lines(0, 24).await;
            let text = lines_text(&lines);
            assert!(text.contains("second version"), "updated content visible");
            assert!(!text.contains("first version"),  "old content gone");
        }

        #[tokio::test]
        async fn send_input_j_moves_cursor_to_next_row() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.set_buffer_content("line1\nline2\nline3").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            bridge.send_input("j").await.expect("send_input must not fail");
            sleep(Duration::from_millis(100)).await;
            let (row, _) = bridge.get_cursor_pos().await;
            assert_eq!(row, 1, "pressing 'j' must move cursor to row 1");
        }

        #[tokio::test]
        async fn send_input_gg_moves_cursor_to_first_row() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.set_buffer_content("line1\nline2\nline3").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            bridge.send_input("G").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            bridge.send_input("gg").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            let (row, _) = bridge.get_cursor_pos().await;
            assert_eq!(row, 0, "'gg' must return cursor to row 0");
        }

        #[tokio::test]
        async fn diagnose_fold_state_step_by_step() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.configure_buffer().await.unwrap();
            sleep(Duration::from_millis(150)).await;
            let fl_after_config = bridge.neovim.eval("&foldlevel").await.unwrap_or(Value::from(-1i64));
            bridge.set_buffer_content("---\n\nbody\n").await.unwrap();
            sleep(Duration::from_millis(200)).await;
            let fl_after_content = bridge.neovim.eval("&foldlevel").await.unwrap_or(Value::from(-1i64));
            eprintln!("foldlevel after configure: {:?}", fl_after_config);
            eprintln!("foldlevel after content:   {:?}", fl_after_content);
            assert!(fl_after_config.as_u64().unwrap_or(0) >= 1,
                "foldlevel must be >= 1 after configure_buffer; got: {:?}", fl_after_config);
        }

        #[tokio::test]
        async fn fold_commands_execute_without_error() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.configure_buffer().await.unwrap();
            sleep(Duration::from_millis(100)).await;
            let content = concat!(
                "---\n\n**You:** first\n\n**Agent:** response\n\n",
                "---\n\n**You:** second\n"
            );
            bridge.set_buffer_content(content).await.unwrap();
            sleep(Duration::from_millis(200)).await;
            bridge.send_input("gg").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.send_input("zc").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            bridge.send_input("zo").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            bridge.send_input("za").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            let final_content = bridge.get_buffer_content().await.unwrap();
            assert!(final_content.contains("first"),  "content preserved after fold operations");
            assert!(final_content.contains("second"), "all messages intact");
        }

        #[tokio::test]
        async fn fold_all_and_unfold_all_commands_execute() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.configure_buffer().await.unwrap();
            sleep(Duration::from_millis(100)).await;
            let content = concat!(
                "---\n\n**You:** first\n\n**Agent:** response1\n\n",
                "---\n\n**You:** second\n\n**Agent:** response2\n"
            );
            bridge.set_buffer_content(content).await.unwrap();
            sleep(Duration::from_millis(200)).await;
            bridge.send_input("zM").await.unwrap();
            sleep(Duration::from_millis(150)).await;
            let closed = bridge.get_buffer_content().await.unwrap();
            bridge.send_input("zR").await.unwrap();
            sleep(Duration::from_millis(150)).await;
            let open = bridge.get_buffer_content().await.unwrap();
            assert_eq!(closed, open, "fold display commands must not alter buffer content");
            assert!(open.contains("first"),     "all content preserved");
            assert!(open.contains("response2"), "all content preserved");
        }

        #[tokio::test]
        async fn full_tui_startup_lifecycle_preserves_all_conversation_parts() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.configure_buffer().await.unwrap();
            sleep(Duration::from_millis(100)).await;
            let content = "---\n\n**You:** hello\n\n**Agent:** world\n\n🔧 **Tool Call: read_file**\n```\npath:/tmp/x\n```\n\n✅ **Tool Response: read_file**\n```\ncontents\n```\n";
            bridge.set_buffer_content(content).await.unwrap();
            sleep(Duration::from_millis(200)).await;
            let got = bridge.get_buffer_content().await.unwrap();
            assert!(got.contains("hello"),     "user message in buffer");
            assert!(got.contains("world"),     "agent message in buffer");
            assert!(got.contains("read_file"), "tool name in buffer");
            assert!(got.contains("contents"),  "tool output in buffer");
        }

        #[tokio::test]
        async fn full_tui_startup_lifecycle_produces_rendered_output() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.configure_buffer().await.unwrap();
            sleep(Duration::from_millis(150)).await;
            bridge.set_buffer_content("first line only").await.unwrap();
            sleep(Duration::from_millis(250)).await;
            let lines = bridge.render_to_lines(0, 24).await;
            assert!(!lines.is_empty(), "render must produce lines after full setup");
            let rendered = lines_text(&lines);
            assert!(rendered.contains("first line only"),
                "first row of content must appear in rendered output; got: {:?}",
                &rendered[..rendered.len().min(200)]);
        }

        #[tokio::test]
        async fn edit_first_message_updates_buffer_content_at_correct_position() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.configure_buffer().await.unwrap();
            sleep(Duration::from_millis(100)).await;
            let initial = "---\n\n**You:** ORIGINAL_TEXT\n\n**Agent:** response\n";
            bridge.set_buffer_content(initial).await.unwrap();
            sleep(Duration::from_millis(200)).await;
            bridge.set_modifiable(true).await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.send_input("3G").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.send_input("f ").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.send_input("w").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.send_input("ciW").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.send_input("EDITED_TEXT").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.send_input("<Esc>").await.unwrap();
            sleep(Duration::from_millis(200)).await;
            let final_content = bridge.get_buffer_content().await.unwrap();
            assert!(final_content.contains("EDITED_TEXT"),
                "edited text must appear in buffer; got: {}", final_content);
            assert!(!final_content.contains("ORIGINAL_TEXT"),
                "original text must be replaced; got: {}", final_content);
        }

        #[tokio::test]
        async fn edit_middle_message_preserves_earlier_and_later_content() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.configure_buffer().await.unwrap();
            sleep(Duration::from_millis(100)).await;
            let initial = concat!(
                "---\n\n**You:** first\n\n**Agent:** answer1\n\n",
                "---\n\n**You:** MIDDLE_ORIGINAL\n\n**Agent:** answer2\n\n",
                "---\n\n**You:** third\n"
            );
            bridge.set_buffer_content(initial).await.unwrap();
            sleep(Duration::from_millis(200)).await;
            bridge.set_modifiable(true).await.unwrap();
            bridge.send_input("gg").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.send_input("/MIDDLE_ORIGINAL<CR>").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            bridge.send_input("ciw").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.send_input("MIDDLE_EDITED").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.send_input("<Esc>").await.unwrap();
            sleep(Duration::from_millis(200)).await;
            let final_content = bridge.get_buffer_content().await.unwrap();
            assert!(final_content.contains("first"),          "earlier content preserved");
            assert!(final_content.contains("third"),          "later content preserved");
            assert!(final_content.contains("MIDDLE_EDITED"),  "edited text present");
            assert!(!final_content.contains("MIDDLE_ORIGINAL"), "original text replaced");
        }

        #[tokio::test]
        async fn edit_last_message_does_not_affect_earlier_messages() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.configure_buffer().await.unwrap();
            sleep(Duration::from_millis(100)).await;
            let initial = "---\n\n**You:** preserved\n\n**Agent:** LAST_ORIGINAL\n";
            bridge.set_buffer_content(initial).await.unwrap();
            sleep(Duration::from_millis(200)).await;
            bridge.set_modifiable(true).await.unwrap();
            bridge.send_input("gg").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.send_input("/LAST_ORIGINAL<CR>").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            bridge.send_input("A").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.send_input("_SUFFIX").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.send_input("<Esc>").await.unwrap();
            sleep(Duration::from_millis(200)).await;
            let final_content = bridge.get_buffer_content().await.unwrap();
            assert!(final_content.contains("preserved"),             "earlier message untouched");
            assert!(final_content.contains("LAST_ORIGINAL_SUFFIX"),  "edit applied at end");
        }

        #[tokio::test]
        async fn edited_buffer_with_tool_calls_parses_to_valid_messages() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.configure_buffer().await.unwrap();
            sleep(Duration::from_millis(100)).await;
            let initial = concat!(
                "---\n\n**You:** EDIT_ME\n\n",
                "**Agent:tool_call:id1**\n",
                "🔧 **Tool Call: glob**\n",
                "```json\n",
                r#"{"pattern":"*.rs"}"#, "\n",
                "```\n\n",
                "**Tool:id1**\n",
                "✅ **Tool Response: glob**\n",
                "```\n",
                "file.rs\n",
                "```\n\n",
                "**Agent:** Found one file\n",
            );
            bridge.set_buffer_content(initial).await.unwrap();
            sleep(Duration::from_millis(250)).await;
            bridge.set_modifiable(true).await.unwrap();
            bridge.send_input("gg").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.send_input("/EDIT_ME<CR>").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            bridge.send_input("ciw").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.send_input("EDITED").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.send_input("<Esc>").await.unwrap();
            sleep(Duration::from_millis(250)).await;
            let edited_md = bridge.get_buffer_content().await.unwrap();
            assert!(edited_md.contains("EDITED"),                   "edit must be present");
            assert!(edited_md.contains("glob"),                     "tool call preserved");
            assert!(edited_md.contains("id1"),                      "tool ID preserved");
            assert!(edited_md.contains("**You:**"),                 "user header present");
            assert!(edited_md.contains("**Agent:tool_call:"),       "tool call header present");
            assert!(edited_md.contains("**Tool:id1**"),             "tool result header present");
            assert!(edited_md.contains("**Agent:**"),               "agent text header present");
        }

        #[tokio::test]
        async fn g_moves_cursor_to_last_line_and_content_is_visible() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            let lines: Vec<String> = (1..=10).map(|i| format!("buffer_line_{i}")).collect();
            bridge.set_buffer_content(&lines.join("\n")).await.unwrap();
            sleep(Duration::from_millis(150)).await;
            bridge.send_input("gg").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            let (row_top, _) = bridge.get_cursor_pos().await;
            assert_eq!(row_top, 0);
            bridge.send_input("G").await.unwrap();
            sleep(Duration::from_millis(200)).await;
            let (row_G, _) = bridge.get_cursor_pos().await;
            assert!(row_G > row_top, "G must move cursor below row 0; got row_G={row_G}");
            let rendered = bridge.render_to_lines(0, bridge.height).await;
            let all_text: String = rendered.iter()
                .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref().to_string()))
                .collect();
            assert!(all_text.contains("buffer_line_10"),
                "last buffer line must be visible after G; rendered (first 400): {:?}",
                &all_text[..all_text.len().min(400)]);
        }

        #[tokio::test]
        async fn g_is_idempotent_second_press_gives_same_cursor_row() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            let content: String = (1..=15).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
            bridge.set_buffer_content(&content).await.unwrap();
            sleep(Duration::from_millis(150)).await;
            bridge.send_input("gg").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            bridge.send_input("G").await.unwrap();
            sleep(Duration::from_millis(200)).await;
            let (row1, _) = bridge.get_cursor_pos().await;
            bridge.send_input("G").await.unwrap();
            sleep(Duration::from_millis(150)).await;
            let (row2, _) = bridge.get_cursor_pos().await;
            assert_eq!(row1, row2, "G must be idempotent: first={row1}, second={row2}");
        }

        #[tokio::test]
        async fn g_on_buffer_longer_than_grid_scrolls_and_shows_last_lines() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            let grid_height = bridge.height as usize;
            let n = grid_height * 2;
            let content: String = (1..=n).map(|i| format!("row{i:03}")).collect::<Vec<_>>().join("\n");
            bridge.set_buffer_content(&content).await.unwrap();
            sleep(Duration::from_millis(200)).await;
            bridge.send_input("gg").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            bridge.send_input("G").await.unwrap();
            sleep(Duration::from_millis(250)).await;
            let last_label = format!("row{n:03}");
            let rendered = bridge.render_to_lines(0, bridge.height).await;
            let all_text: String = rendered.iter()
                .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref().to_string()))
                .collect();
            assert!(all_text.contains(&last_label),
                "last buffer line '{last_label}' must appear after G on tall buffer; \
                 rendered (first 500): {:?}", &all_text[..all_text.len().min(500)]);
        }

        #[tokio::test]
        async fn gg_after_g_returns_cursor_to_grid_row_zero() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            let content: String = (1..=20).map(|i| format!("ln{i}")).collect::<Vec<_>>().join("\n");
            bridge.set_buffer_content(&content).await.unwrap();
            sleep(Duration::from_millis(150)).await;
            bridge.send_input("G").await.unwrap();
            sleep(Duration::from_millis(200)).await;
            bridge.send_input("gg").await.unwrap();
            sleep(Duration::from_millis(150)).await;
            let (row, _) = bridge.get_cursor_pos().await;
            assert_eq!(row, 0, "gg after G must return cursor to grid row 0");
            let rendered = bridge.render_to_lines(0, bridge.height).await;
            let all_text: String = rendered.iter()
                .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref().to_string()))
                .collect();
            assert!(all_text.contains("ln1"),
                "first line 'ln1' must appear after gg; rendered: {:?}",
                &all_text[..all_text.len().min(300)]);
        }

        #[tokio::test]
        async fn insert_mode_cursor_row_matches_row_containing_typed_text() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.set_buffer_content("alpha\nbeta\ngamma").await.unwrap();
            sleep(Duration::from_millis(150)).await;
            bridge.send_input("gg").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.send_input("j").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.send_input("A").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.send_input("UNIQUE_MARKER").await.unwrap();
            sleep(Duration::from_millis(150)).await;
            let (cursor_row, _) = bridge.get_cursor_pos().await;
            let rendered = bridge.render_to_lines(0, bridge.height).await;
            assert!((cursor_row as usize) < rendered.len(),
                "cursor_row={cursor_row} must be within rendered line count={}", rendered.len());
            let row_text: String = rendered[cursor_row as usize].spans.iter()
                .map(|s| s.content.as_ref())
                .collect();
            assert!(row_text.contains("UNIQUE_MARKER"),
                "cursor row {cursor_row} must contain typed text; got: {:?}", row_text);
            bridge.send_input("<Esc>").await.unwrap();
        }

        #[tokio::test]
        async fn insert_mode_at_specific_line_number_cursor_aligned() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            let content: String = (1..=8).map(|i| format!("content_{i}")).collect::<Vec<_>>().join("\n");
            bridge.set_buffer_content(&content).await.unwrap();
            sleep(Duration::from_millis(150)).await;
            bridge.send_input("5G").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            bridge.send_input("A").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.send_input("_EDITED").await.unwrap();
            sleep(Duration::from_millis(150)).await;
            let (cursor_row, _) = bridge.get_cursor_pos().await;
            let rendered = bridge.render_to_lines(0, bridge.height).await;
            let row_text: String = rendered.get(cursor_row as usize)
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .unwrap_or_default();
            assert!(row_text.contains("_EDITED"),
                "typed suffix must appear at cursor row {cursor_row}; got: {:?}", row_text);
            assert!(row_text.contains("content_5"),
                "row {cursor_row} must contain 'content_5'; got: {:?}", row_text);
            bridge.send_input("<Esc>").await.unwrap();
        }

        #[tokio::test]
        async fn render_to_lines_with_scroll_zero_is_correct_usage() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.set_buffer_content("first_line\nsecond_line\nthird_line").await.unwrap();
            sleep(Duration::from_millis(150)).await;
            let lines = bridge.render_to_lines(0, bridge.height).await;
            let text: String = lines.iter()
                .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref().to_string()))
                .collect();
            assert!(text.contains("first_line"),  "scroll=0 must show grid row 0");
            assert!(text.contains("second_line"), "scroll=0 must show grid row 1");
            assert!(text.contains("third_line"),  "scroll=0 must show grid row 2");
        }

        #[tokio::test]
        async fn render_to_lines_with_nonzero_scroll_hides_top_content() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.set_buffer_content("TOP_LINE\nsecond\nthird\nfourth\nfifth").await.unwrap();
            sleep(Duration::from_millis(150)).await;
            let lines_wrong = bridge.render_to_lines(3, bridge.height - 3).await;
            let text_wrong: String = lines_wrong.iter()
                .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref().to_string()))
                .collect();
            assert!(!text_wrong.contains("TOP_LINE"),
                "scroll=3 must hide grid row 0 (TOP_LINE); if this fails, the scroll bug has been re-introduced");
        }

        #[tokio::test]
        async fn g_with_fold_expression_reaches_actual_last_buffer_line() {
            if !nvim_available() { return; }
            let mut bridge = spawn_configured_bridge().await;
            let content = two_turn_conversation();
            bridge.set_buffer_content(&content).await.unwrap();
            sleep(Duration::from_millis(200)).await;
            bridge.send_input("gg").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            bridge.send_input("G").await.unwrap();
            sleep(Duration::from_millis(250)).await;
            let cursor_line = bridge.get_cursor_line_in_buffer().await.expect("get_cursor_line_in_buffer must succeed");
            let last_line   = bridge.get_buffer_line_count().await.expect("get_buffer_line_count must succeed");
            assert_eq!(cursor_line, last_line,
                "G with fold expression active must land on the last buffer line; \
                 cursor={cursor_line}, last={last_line}\nBuffer content:\n{content}");
        }

        #[tokio::test]
        async fn g_with_multi_turn_conversation_cursor_reaches_last_line() {
            if !nvim_available() { return; }
            let mut bridge = spawn_configured_bridge().await;
            let mut content = String::new();
            for i in 1..=5 {
                content.push_str(&format!("---\n\n**You:** Question number {i}\n"));
                content.push_str(&format!("\n**Agent:** Answer number {i}. This is the response to question {i}.\n"));
            }
            bridge.set_buffer_content(&content).await.unwrap();
            sleep(Duration::from_millis(200)).await;
            bridge.send_input("gg").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            bridge.send_input("G").await.unwrap();
            sleep(Duration::from_millis(250)).await;
            let cursor_line = bridge.get_cursor_line_in_buffer().await.unwrap();
            let last_line   = bridge.get_buffer_line_count().await.unwrap();
            assert_eq!(cursor_line, last_line,
                "G on multi-turn conversation must reach last line; cursor={cursor_line}, total={last_line}");
            let rendered = bridge.render_to_lines(0, bridge.height).await;
            let all_text: String = rendered.iter()
                .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref().to_string()))
                .collect();
            assert!(all_text.contains("Answer number 5"),
                "after G the last agent response must be visible; grid text: {:?}",
                &all_text[..all_text.len().min(500)]);
        }

        #[tokio::test]
        async fn g_with_tool_call_conversation_cursor_reaches_last_line() {
            if !nvim_available() { return; }
            let mut bridge = spawn_configured_bridge().await;
            let content = conversation_with_tool_call();
            bridge.set_buffer_content(&content).await.unwrap();
            sleep(Duration::from_millis(200)).await;
            bridge.send_input("gg").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            bridge.send_input("G").await.unwrap();
            sleep(Duration::from_millis(250)).await;
            let cursor_line = bridge.get_cursor_line_in_buffer().await.unwrap();
            let last_line   = bridge.get_buffer_line_count().await.unwrap();
            assert_eq!(cursor_line, last_line,
                "G with tool-call folds must reach last line; cursor={cursor_line}, total={last_line}\nBuffer:\n{content}");
        }

        #[tokio::test]
        async fn g_with_long_wrapping_response_cursor_reaches_last_buffer_line() {
            if !nvim_available() { return; }
            let mut bridge = spawn_configured_bridge().await;
            let content = conversation_with_long_response();
            bridge.set_buffer_content(&content).await.unwrap();
            sleep(Duration::from_millis(200)).await;
            bridge.send_input("gg").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            bridge.send_input("G").await.unwrap();
            sleep(Duration::from_millis(250)).await;
            let cursor_line = bridge.get_cursor_line_in_buffer().await.unwrap();
            let last_line   = bridge.get_buffer_line_count().await.unwrap();
            assert_eq!(cursor_line, last_line,
                "G with wrapping last line must reach last buffer line; cursor={cursor_line}, total={last_line}");
        }

        #[tokio::test]
        async fn g_is_idempotent_with_fold_expression() {
            if !nvim_available() { return; }
            let mut bridge = spawn_configured_bridge().await;
            let content = two_turn_conversation();
            bridge.set_buffer_content(&content).await.unwrap();
            sleep(Duration::from_millis(200)).await;
            bridge.send_input("gg").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            bridge.send_input("G").await.unwrap();
            sleep(Duration::from_millis(200)).await;
            let first_cursor = bridge.get_cursor_line_in_buffer().await.unwrap();
            let last_line    = bridge.get_buffer_line_count().await.unwrap();
            bridge.send_input("G").await.unwrap();
            sleep(Duration::from_millis(150)).await;
            let second_cursor = bridge.get_cursor_line_in_buffer().await.unwrap();
            assert_eq!(first_cursor,  last_line,     "first G must land on last line");
            assert_eq!(first_cursor,  second_cursor, "second G must be idempotent");
        }

        #[tokio::test]
        async fn level1_folds_open_level2_folds_closed_after_configure_and_set_content() {
            if !nvim_available() { return; }
            let mut bridge = spawn_configured_bridge().await;
            let content = conversation_with_tool_call();
            bridge.set_buffer_content(&content).await.unwrap();
            sleep(Duration::from_millis(200)).await;
            let closed = bridge.eval_vim("max(map(range(1, line('$')), 'foldclosed(v:val)'))").await.unwrap();
            let max_foldclosed = match closed { Value::Integer(n) => n.as_i64().unwrap_or(-1), _ => -1 };
            assert!(max_foldclosed > -1,
                "With foldlevel=1 and a tool call, at least one level-2 fold should be closed; \
                 got max_foldclosed={max_foldclosed}");
            let first_closed = bridge.eval_vim("foldclosed(1)").await.unwrap_or(Value::from(-1i64));
            let first_closed_val = match first_closed { Value::Integer(n) => n.as_i64().unwrap_or(-1), _ => -1 };
            assert_eq!(first_closed_val, -1,
                "Level-1 fold headers must remain open with foldlevel=1; \
                 line 1 reports foldclosed={first_closed_val}");
        }

        #[tokio::test]
        async fn insert_mode_after_g_appends_at_last_buffer_line() {
            if !nvim_available() { return; }
            let mut bridge = spawn_configured_bridge().await;
            let content = two_turn_conversation();
            bridge.set_buffer_content(&content).await.unwrap();
            sleep(Duration::from_millis(200)).await;
            bridge.send_input("G").await.unwrap();
            sleep(Duration::from_millis(200)).await;
            bridge.send_input("A").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.send_input("APPENDED_MARKER").await.unwrap();
            sleep(Duration::from_millis(150)).await;
            bridge.send_input("<Esc>").await.unwrap();
            sleep(Duration::from_millis(150)).await;
            let buf = bridge.get_buffer_content().await.unwrap();
            let lines: Vec<&str> = buf.trim_end_matches('\n').lines().collect();
            let last_content_line = lines.last().copied().unwrap_or("");
            assert!(last_content_line.contains("APPENDED_MARKER"),
                "APPENDED_MARKER must be on the last content line; last_line={last_content_line:?}");
        }

        #[tokio::test]
        async fn insert_mode_i_places_text_at_cursor_line_not_below() {
            if !nvim_available() { return; }
            let mut bridge = spawn_configured_bridge().await;
            let content =
                "---\n\n**You:** First\n\
                 \n**Agent:** Response one\n\
                 ---\n\n**You:** Second\n\
                 \n**Agent:** Response two UNIQUE_END\n";
            bridge.set_buffer_content(&content).await.unwrap();
            sleep(Duration::from_millis(200)).await;
            bridge.send_input("G").await.unwrap();
            sleep(Duration::from_millis(200)).await;
            let cursor_line_before = bridge.get_cursor_line_in_buffer().await.unwrap();
            bridge.send_input("i").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.send_input("INSERT_HERE").await.unwrap();
            sleep(Duration::from_millis(150)).await;
            bridge.send_input("<Esc>").await.unwrap();
            sleep(Duration::from_millis(150)).await;
            let cursor_line_after = bridge.get_cursor_line_in_buffer().await.unwrap();
            assert_eq!(cursor_line_before, cursor_line_after,
                "entering insert mode must not move cursor; before={cursor_line_before}, after={cursor_line_after}");
            let buf = bridge.get_buffer_content().await.unwrap();
            let lines: Vec<&str> = buf.split('\n').collect();
            let idx = (cursor_line_before - 1) as usize;
            let line_content = lines.get(idx).copied().unwrap_or("");
            assert!(line_content.contains("INSERT_HERE"),
                "INSERT_HERE must be on buffer line {cursor_line_before}; got: {:?}", line_content);
        }

        #[tokio::test]
        async fn edit_at_specific_buffer_line_updates_correct_content() {
            if !nvim_available() { return; }
            let mut bridge = spawn_configured_bridge().await;
            let content =
                "---\n\n**You:** Turn one\n\
                 \n**Agent:** Answer one\n\
                 ---\n\n**You:** Turn two EDIT_TARGET\n\
                 \n**Agent:** Answer two\n";
            bridge.set_buffer_content(&content).await.unwrap();
            sleep(Duration::from_millis(200)).await;
            let target_line = bridge.eval_vim("search('EDIT_TARGET', 'n')").await.unwrap();
            let target_line_num = match target_line { Value::Integer(n) => n.as_i64().unwrap_or(0), _ => panic!("search() must return integer") };
            assert!(target_line_num > 0, "EDIT_TARGET must exist in the buffer; got line={target_line_num}");
            bridge.send_input(&format!("{target_line_num}G")).await.unwrap();
            sleep(Duration::from_millis(150)).await;
            let cursor_after_nav = bridge.get_cursor_line_in_buffer().await.unwrap();
            assert_eq!(cursor_after_nav, target_line_num,
                "{target_line_num}G must place cursor on line {target_line_num}; got {cursor_after_nav}");
            bridge.send_input("A").await.unwrap();
            sleep(Duration::from_millis(50)).await;
            bridge.send_input("_EDITED").await.unwrap();
            sleep(Duration::from_millis(150)).await;
            bridge.send_input("<Esc>").await.unwrap();
            sleep(Duration::from_millis(150)).await;
            let buf = bridge.get_buffer_content().await.unwrap();
            let lines: Vec<&str> = buf.split('\n').collect();
            let edited_line = lines.get((target_line_num - 1) as usize).copied().unwrap_or("");
            assert!(edited_line.contains("_EDITED"),    "edit must land on line {target_line_num}; got: {:?}", edited_line);
            assert!(edited_line.contains("EDIT_TARGET"), "original text must still be on line {target_line_num}; got: {:?}", edited_line);
        }

        #[tokio::test]
        async fn buffer_line_count_matches_set_content_line_count() {
            if !nvim_available() { return; }
            let mut bridge = spawn_configured_bridge().await;
            let content = two_turn_conversation();
            let expected_lines = content.split('\n').count() as i64;
            bridge.set_buffer_content(&content).await.unwrap();
            sleep(Duration::from_millis(200)).await;
            let actual_lines = bridge.get_buffer_line_count().await.unwrap();
            assert_eq!(actual_lines, expected_lines,
                "Neovim line count must match our split-line count; expected={expected_lines}, actual={actual_lines}");
        }

        #[tokio::test]
        async fn g_with_very_long_conversation_reaches_last_line() {
            if !nvim_available() { return; }
            let mut bridge = spawn_configured_bridge().await;
            let mut content = String::new();
            for i in 1..=20 {
                content.push_str(&format!("---\n\n**You:** This is user question number {i}\n"));
                content.push_str(&format!("\n**Agent:** This is the detailed answer to question {i}. The agent provides a thorough explanation covering multiple aspects.\n"));
            }
            let expected_last = content.split('\n').count() as i64;
            bridge.set_buffer_content(&content).await.unwrap();
            sleep(Duration::from_millis(300)).await;
            bridge.send_input("gg").await.unwrap();
            sleep(Duration::from_millis(100)).await;
            bridge.send_input("G").await.unwrap();
            sleep(Duration::from_millis(300)).await;
            let cursor_line = bridge.get_cursor_line_in_buffer().await.unwrap();
            let last_line   = bridge.get_buffer_line_count().await.unwrap();
            assert_eq!(last_line, expected_last, "buffer line count must equal our content split count");
            assert_eq!(cursor_line, last_line,
                "G on a {last_line}-line buffer (grid height {}) must reach last line; cursor={cursor_line}",
                bridge.height);
        }

        #[tokio::test]
        async fn g_after_rapid_content_updates_reaches_current_last_line() {
            if !nvim_available() { return; }
            let mut bridge = spawn_configured_bridge().await;
            let base = "---\n\n**You:** Hello\n";
            let mut streaming = String::from("**Agent:** ");
            for chunk in ["Hello", " there!", " How", " can", " I", " help", " you", " today?", " I'm", " ready."] {
                streaming.push_str(chunk);
                let content = format!("{base}{streaming}");
                bridge.set_buffer_content(&content).await.unwrap();
            }
            sleep(Duration::from_millis(200)).await;
            bridge.send_input("G").await.unwrap();
            sleep(Duration::from_millis(250)).await;
            let cursor_line = bridge.get_cursor_line_in_buffer().await.unwrap();
            let last_line   = bridge.get_buffer_line_count().await.unwrap();
            assert_eq!(cursor_line, last_line,
                "G after rapid content updates must reach last line; cursor={cursor_line}, total={last_line}");
        }

        #[tokio::test]
        async fn g_after_content_update_reaches_new_last_line() {
            if !nvim_available() { return; }
            let mut bridge = spawn_configured_bridge().await;
            let first_content = "---\n\n**You:** First message\n\n**Agent:** First response\n";
            bridge.set_buffer_content(first_content).await.unwrap();
            sleep(Duration::from_millis(200)).await;
            bridge.send_input("G").await.unwrap();
            sleep(Duration::from_millis(200)).await;
            let line_after_first_g = bridge.get_cursor_line_in_buffer().await.unwrap();
            let first_last = bridge.get_buffer_line_count().await.unwrap();
            assert_eq!(line_after_first_g, first_last, "first G must reach last line of first content");
            let second_content = format!(
                "{first_content}---\n\n**You:** Second message\n\n**Agent:** Second response\n"
            );
            bridge.set_buffer_content(&second_content).await.unwrap();
            sleep(Duration::from_millis(200)).await;
            bridge.send_input("G").await.unwrap();
            sleep(Duration::from_millis(250)).await;
            let line_after_second_g = bridge.get_cursor_line_in_buffer().await.unwrap();
            let second_last = bridge.get_buffer_line_count().await.unwrap();
            assert!(second_last > first_last, "second buffer must be larger");
            assert_eq!(line_after_second_g, second_last,
                "G after content update must reach NEW last line; cursor={line_after_second_g}, total={second_last}");
        }

        #[tokio::test]
        async fn flush_notify_fires_after_input_is_processed() {
            if !nvim_available() { return; }
            let mut bridge = spawn_bridge().await;
            bridge.set_buffer_content("line1\nline2\nline3").await.unwrap();
            sleep(Duration::from_millis(150)).await;
            let notify = bridge.flush_notify.clone();
            bridge.send_input("G").await.unwrap();
            let notified = tokio::time::timeout(
                Duration::from_millis(500),
                notify.notified(),
            ).await;
            assert!(notified.is_ok(),
                "flush_notify must fire within 500 ms after send_input");
        }
    }
}
