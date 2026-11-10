// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! How a prepared child is run to its end.

use async_trait::async_trait;
use sven_tool_api::tool::ToolOutput;

use super::session::{run_acp_session, SpawnArgs};

/// Runs a prepared child to its end and returns its result. The seam between
/// what the task tool decides (where, in which mode, what happens to the
/// child's changes) and the process that does the work.
#[async_trait]
pub(super) trait ChildRunner: Send + Sync {
    async fn run(&self, args: SpawnArgs) -> ToolOutput;
}

/// Starts the child as a `sven acp serve` process.
pub(super) struct AcpProcessRunner;

#[async_trait]
impl ChildRunner for AcpProcessRunner {
    async fn run(&self, args: SpawnArgs) -> ToolOutput {
        // Subagents always run with depth 0.
        let depth_for_env = 0u32;
        // The ACP ClientSideConnection is !Send (uses LocalBoxFuture internally).
        // We run the entire ACP session in a dedicated OS thread with its own
        // single-threaded tokio runtime + LocalSet.  Using a plain std::thread
        // avoids any interaction between the outer multi-threaded runtime and the
        // inner single-threaded one.
        let handle_id = args.handle_id.clone();
        let (result_tx, result_rx) = tokio::sync::oneshot::channel::<ToolOutput>();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build sub-agent runtime");
            let local = tokio::task::LocalSet::new();
            let output = rt.block_on(local.run_until(run_acp_session(args, depth_for_env)));
            let _ = result_tx.send(output);
        });
        result_rx
            .await
            .unwrap_or_else(|_| ToolOutput::err(&handle_id, "sub-agent thread died unexpectedly"))
    }
}
