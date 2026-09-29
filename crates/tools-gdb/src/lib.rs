// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! GDB/MI debugging tools (Unix only -- GDB signal APIs are not available on
//! Windows). Its only sven-tier dependencies are `sven-tool-api` (kernel)
//! and the foundation crates `sven-config`/`sven-hsm`.
pub mod command;
pub mod compound;
pub mod connect;
pub mod discovery;
pub mod interrupt;
pub mod start_server;
pub mod state;
pub mod status;
pub mod stop;
pub mod wait_stopped;

pub use command::GdbCommandTool;
pub use compound::GdbTool;
pub use connect::GdbConnectTool;
pub use interrupt::GdbInterruptTool;
pub use start_server::GdbStartServerTool;
pub use state::GdbSessionState;
pub use status::GdbStatusTool;
pub use stop::GdbStopTool;
pub use wait_stopped::GdbWaitStoppedTool;

// ─── OutputCategory contract tests ───────────────────────────────────────────
//
// Pins each tool's declared `OutputCategory`: the executor's truncation
// strategy depends on it, so a silent change would change what the model
// sees of every oversized result.
#[cfg(test)]
mod output_category_tests {
    use super::*;
    use std::sync::Arc;
    use sven_config::GdbConfig;
    use sven_tool_api::tool::{OutputCategory, Tool};
    use tokio::sync::Mutex;

    fn gdb_state() -> Arc<Mutex<state::GdbSessionState>> {
        Arc::new(Mutex::new(state::GdbSessionState::default()))
    }

    #[test]
    fn gdb_command_is_headtail() {
        let t = GdbCommandTool::new(gdb_state(), GdbConfig::default());
        assert_eq!(t.output_category(), OutputCategory::HeadTail);
    }

    #[test]
    fn gdb_wait_stopped_is_headtail() {
        let t = GdbWaitStoppedTool::new(gdb_state());
        assert_eq!(t.output_category(), OutputCategory::HeadTail);
    }

    #[test]
    fn gdb_interrupt_is_headtail() {
        let t = GdbInterruptTool::new(gdb_state());
        assert_eq!(t.output_category(), OutputCategory::HeadTail);
    }
}
