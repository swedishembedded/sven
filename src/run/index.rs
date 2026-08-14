// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

use crate::cli::IndexCommands;

// ── Index command handler ─────────────────────────────────────────────────────

pub(crate) fn run_index_command(cmd: &IndexCommands) -> anyhow::Result<()> {
    let project_root =
        sven_ci::find_project_root().unwrap_or_else(|_| std::path::PathBuf::from("."));
    match cmd {
        IndexCommands::Build { quiet } => sven_ci::index::cmd_build(&project_root, *quiet),
        IndexCommands::Query { query, limit } => {
            sven_ci::index::cmd_query(&project_root, query, *limit)
        }
        IndexCommands::Stats => sven_ci::index::cmd_stats(&project_root),
    }
}
