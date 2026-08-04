// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Embedded Neovim integration state.

use std::sync::Arc;

use crate::nvim::NvimBridge;

/// State for the optional embedded Neovim process.
pub(crate) struct NvimState {
    /// The Neovim RPC bridge.  `None` when `disabled` is true or when startup
    /// failed.
    pub bridge: Option<Arc<tokio::sync::Mutex<NvimBridge>>>,
    /// Notified by Neovim when the buffer needs a ratatui redraw.
    pub flush_notify: Option<Arc<tokio::sync::Notify>>,
    /// Notified by Neovim when the user submits via `:w` / `<C-CR>`.
    pub submit_notify: Option<Arc<tokio::sync::Notify>>,
    /// Notified by Neovim when the user quits via `:q` / `:qa`.
    pub quit_notify: Option<Arc<tokio::sync::Notify>>,
    /// True when Neovim is disabled (`--no-nvim`).
    pub disabled: bool,
}

impl NvimState {
    pub fn new(disabled: bool) -> Self {
        Self {
            bridge: None,
            flush_notify: None,
            submit_notify: None,
            quit_notify: None,
            disabled,
        }
    }
}
