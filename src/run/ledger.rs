// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven ledger`: summarize the token ledger of a saved trajectory.

use std::path::Path;

use anyhow::Context;
use sven_session_store::step_ledger;
use sven_session_store::trace_session::load_session_from;

/// Print the ledger summary of the trajectory at `file`, subagents included:
/// a short table, or with `json` the `LedgerSummary` as one
/// JSON object.
pub(crate) fn run_ledger_command(file: &Path, json: bool) -> anyhow::Result<()> {
    let trajectory =
        load_session_from(file).with_context(|| format!("reading {}", file.display()))?;
    let summary = step_ledger::summarize(&trajectory)
        .with_context(|| format!("summarizing {}", file.display()))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else if summary.requests == 0 {
        println!(
            "{} holds no token ledger (it was recorded before the ledger existed, or the run made no model request).",
            file.display()
        );
    } else {
        print!("{}", summary.render_table());
    }
    Ok(())
}
