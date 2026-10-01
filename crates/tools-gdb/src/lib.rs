// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! GDB/MI debugging tools (Unix only -- GDB signal APIs are not available on
//! Windows). Its only sven-tier dependencies are `sven-tool-api` (kernel)
//! and the foundation crates `sven-config`/`sven-hsm`.
//!
//! The `tools.gdb` configuration section ([`GdbConfig`]) is always compiled,
//! so a configuration file naming it loads in every build. The tools that act
//! on it need the `mi` feature, which brings in the GDB/MI client.
mod config;
pub use config::GdbConfig;

#[cfg(all(unix, feature = "mi"))]
pub mod command;
#[cfg(all(unix, feature = "mi"))]
pub mod compound;
#[cfg(all(unix, feature = "mi"))]
pub mod connect;
pub mod discovery;
#[cfg(all(unix, feature = "mi"))]
pub mod interrupt;
#[cfg(all(unix, feature = "mi"))]
pub mod start_server;
#[cfg(all(unix, feature = "mi"))]
pub mod state;
#[cfg(all(unix, feature = "mi"))]
pub mod status;
#[cfg(all(unix, feature = "mi"))]
pub mod stop;
#[cfg(all(unix, feature = "mi"))]
pub mod wait_stopped;

#[cfg(all(unix, feature = "mi"))]
pub use command::GdbCommandTool;
#[cfg(all(unix, feature = "mi"))]
pub use compound::GdbTool;
#[cfg(all(unix, feature = "mi"))]
pub use connect::GdbConnectTool;
#[cfg(all(unix, feature = "mi"))]
pub use interrupt::GdbInterruptTool;
#[cfg(all(unix, feature = "mi"))]
pub use start_server::GdbStartServerTool;
#[cfg(all(unix, feature = "mi"))]
pub use state::GdbSessionState;
#[cfg(all(unix, feature = "mi"))]
pub use status::GdbStatusTool;
#[cfg(all(unix, feature = "mi"))]
pub use stop::GdbStopTool;
#[cfg(all(unix, feature = "mi"))]
pub use wait_stopped::GdbWaitStoppedTool;

// ─── OutputCategory contract tests ───────────────────────────────────────────
//
// Pins each tool's declared `OutputCategory`: the executor's truncation
// strategy depends on it, so a silent change would change what the model
// sees of every oversized result.
#[cfg(all(test, unix, feature = "mi"))]
mod output_category_tests {
    use super::*;
    use std::sync::Arc;
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
