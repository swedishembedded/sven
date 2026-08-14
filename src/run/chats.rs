// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

use std::io::Write;
use std::process::Stdio;

use anyhow::Context;
use sven_session_store::history;

/// Print the list of saved conversations to stdout.
pub(crate) fn print_chats(limit: usize) {
    match history::list(Some(limit)) {
        Ok(entries) if entries.is_empty() => {
            println!("No saved conversations found.");
            println!(
                "Conversations are stored in: {}",
                history::history_dir().display()
            );
        }
        Ok(entries) => {
            println!(
                "{:<45}  {:<16}  {:<5}  TITLE",
                "ID (use with --resume)", "DATE", "TURNS"
            );
            println!("{}", "-".repeat(95));
            for e in &entries {
                let display_id = if e.id.len() > 44 {
                    format!("{}...", &e.id[..43])
                } else {
                    e.id.clone()
                };
                let date = e.timestamp.replace('T', " ");
                let date = &date[..16.min(date.len())];
                let title = if e.title.chars().count() > 50 {
                    format!("{}...", e.title.chars().take(49).collect::<String>())
                } else {
                    e.title.clone()
                };
                println!(
                    "{:<45}  {:<16}  {:<5}  {}",
                    display_id, date, e.turns, title
                );
            }
            println!("\nTotal: {} conversation(s)", entries.len());
            println!("History dir: {}", history::history_dir().display());
        }
        Err(e) => {
            eprintln!("Error listing conversations: {e}");
            std::process::exit(1);
        }
    }
}

/// Launch `fzf` and let the user pick a conversation to resume.
pub(crate) fn pick_chat_with_fzf() -> anyhow::Result<Option<String>> {
    let entries = history::list(None).context("listing saved conversations")?;
    if entries.is_empty() {
        anyhow::bail!(
            "No saved conversations found.\n\
             Start a conversation with sven first, then use --resume to continue it."
        );
    }

    let lines: String = entries
        .iter()
        .map(|e| {
            let date = e.timestamp.replace('T', " ");
            let date = &date[..16.min(date.len())];
            let turns_label = if e.turns == 1 {
                "1 turn".to_string()
            } else {
                format!("{} turns", e.turns)
            };
            format!("{}\t{}\t{}\t{}", e.id, date, e.title, turns_label)
        })
        .collect::<Vec<_>>()
        .join("\n");

    let mut child = std::process::Command::new("fzf")
        .args([
            "--delimiter=\t",
            "--with-nth=3,2,4",
            "--tabstop=1",
            "--header=Resume conversation  (Enter: open · Esc: cancel)",
            "--header-first",
            "--height=50%",
            "--min-height=10",
            "--reverse",
            "--no-sort",
            "--bind=ctrl-/:toggle-preview",
            "--preview=echo {}",
            "--preview-window=down:2:wrap:hidden",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context(
            "failed to launch fzf - make sure fzf is installed\n\
             (https://github.com/junegunn/fzf or `apt install fzf`)",
        )?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(lines.as_bytes());
    }

    let output = child.wait_with_output()?;

    if !output.status.success() {
        return Ok(None);
    }

    let selected = String::from_utf8_lossy(&output.stdout);
    let selected = selected.trim();
    if selected.is_empty() {
        return Ok(None);
    }

    let id = selected.split('\t').next().unwrap_or("").trim().to_string();
    if id.is_empty() {
        anyhow::bail!("fzf returned an unexpected selection: {selected:?}");
    }
    Ok(Some(id))
}
