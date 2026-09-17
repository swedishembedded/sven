// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

use sven_session_store::{list_all_sessions, migrate_legacy_chats, trace_session::session_dir};

/// Print the list of saved sessions to stdout.
pub(crate) fn print_chats(limit: usize) {
    match list_all_sessions(Some(limit)) {
        Ok(entries) if entries.is_empty() => {
            println!("No saved sessions found.");
            println!("Sessions are stored in: {}", session_dir().display());
        }
        Ok(entries) => {
            println!(
                "{:<38}  {:<16}  {:<9}  TITLE",
                "ID (use with --resume)", "UPDATED", "STATUS"
            );
            println!("{}", "-".repeat(95));
            for e in &entries {
                let display_id = if e.session_id.len() > 37 {
                    format!("{}...", &e.session_id[..34])
                } else {
                    e.session_id.clone()
                };
                let date = e.updated_at.format("%Y-%m-%d %H:%M").to_string();
                let title = if e.title.chars().count() > 50 {
                    format!("{}...", e.title.chars().take(49).collect::<String>())
                } else {
                    e.title.clone()
                };
                let status = if e.is_legacy {
                    "legacy".to_string()
                } else {
                    format!("{:?}", e.status)
                };
                println!("{:<38}  {:<16}  {:<9}  {}", display_id, date, status, title);
            }
            println!("\nTotal: {} session(s)", entries.len());
            println!("Sessions dir: {}", session_dir().display());
        }
        Err(e) => {
            eprintln!("Error listing sessions: {e}");
            std::process::exit(1);
        }
    }
}

/// `sven migrate-sessions`: bulk-convert every legacy `.yaml` chat to ATIF.
pub(crate) fn run_migrate_sessions_command(dry_run: bool) -> anyhow::Result<()> {
    let summary = migrate_legacy_chats(dry_run)?;
    let verb = if dry_run { "would migrate" } else { "migrated" };
    if summary.migrated.is_empty()
        && summary.skipped_existing.is_empty()
        && summary.failed.is_empty()
    {
        println!("no legacy chat sessions found - nothing to migrate.");
        return Ok(());
    }
    println!(
        "{} {} session(s){}.",
        verb,
        summary.migrated.len(),
        if dry_run { "" } else { " to ATIF" }
    );
    if !summary.skipped_existing.is_empty() {
        println!(
            "{} session(s) already had a trajectory file - left untouched.",
            summary.skipped_existing.len()
        );
    }
    if !summary.failed.is_empty() {
        eprintln!("{} session(s) failed:", summary.failed.len());
        for (id, err) in &summary.failed {
            eprintln!("  {id}: {err}");
        }
        std::process::exit(1);
    }
    Ok(())
}
