mod cli;

use std::io::{self, Read, Write};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::Context;
use tracing_subscriber::{filter::EnvFilter, fmt, prelude::*};

use cli::{Cli, Commands};
use clap::Parser;
use sven_ci::{CiOptions, CiRunner, ConversationOptions, ConversationRunner};
use sven_input::history;
use sven_tui::{App, AppOptions};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    init_logging(cli.verbose);

    // Handle subcommands first (before loading config)
    if let Some(cmd) = &cli.command {
        match cmd {
            Commands::Completions { shell } => {
                cli::print_completions(*shell);
                return Ok(());
            }
            Commands::ShowConfig => {
                let config = sven_config::load(cli.config.as_deref())?;
                println!("{}", toml::to_string_pretty(&config).unwrap_or_default());
                return Ok(());
            }
            Commands::Chats { limit } => {
                print_chats(*limit);
                return Ok(());
            }
        }
    }

    let config = Arc::new(sven_config::load(cli.config.as_deref())?);

    if cli.is_headless() {
        run_ci(cli, config).await
    } else {
        run_tui(cli, config).await
    }
}

/// Print the list of saved conversations to stdout.
fn print_chats(limit: usize) {
    match history::list(Some(limit)) {
        Ok(entries) if entries.is_empty() => {
            println!("No saved conversations found.");
            println!("Conversations are stored in: {}", history::history_dir().display());
        }
        Ok(entries) => {
            println!(
                "{:<45}  {:<16}  {:<5}  {}",
                "ID (use with --resume)", "DATE", "TURNS", "TITLE"
            );
            println!("{}", "-".repeat(95));
            for e in &entries {
                let display_id = if e.id.len() > 44 {
                    format!("{}…", &e.id[..43])
                } else {
                    e.id.clone()
                };
                let date = e.timestamp.replace('T', " ");
                let date = &date[..16.min(date.len())]; // YYYY-MM-DD HH:MM
                let title = if e.title.chars().count() > 50 {
                    format!("{}…", e.title.chars().take(49).collect::<String>())
                } else {
                    e.title.clone()
                };
                println!("{:<45}  {:<16}  {:<5}  {}", display_id, date, e.turns, title);
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
///
/// Each line fed to fzf is tab-separated:
///   `<id>\t<date>\t<title>\t<turns>`
///
/// fzf displays only columns 3–4 (title + turns) with column 2 (date) in the
/// prompt, keeping the ID hidden for clean parsing.  Returns the selected ID,
/// or `None` if the user cancelled.
fn pick_chat_with_fzf() -> anyhow::Result<Option<String>> {
    let entries = history::list(None).context("listing saved conversations")?;
    if entries.is_empty() {
        anyhow::bail!(
            "No saved conversations found.\n\
             Start a conversation with sven first, then use --resume to continue it."
        );
    }

    // Build tab-separated lines: ID \t DATE \t TITLE \t TURNS
    let lines: String = entries
        .iter()
        .map(|e| {
            let date = e.timestamp.replace('T', " ");
            let date = &date[..16.min(date.len())];
            let turns_label = if e.turns == 1 { "1 turn".to_string() } else { format!("{} turns", e.turns) };
            format!("{}\t{}\t{}\t{}", e.id, date, e.title, turns_label)
        })
        .collect::<Vec<_>>()
        .join("\n");

    let mut child = std::process::Command::new("fzf")
        .args([
            "--delimiter=\t",
            "--with-nth=3,2,4",   // display: title | date | turns  (ID column is hidden)
            "--tabstop=1",
            "--header=Resume conversation  (Enter: open · Esc: cancel)",
            "--header-first",
            "--height=50%",
            "--min-height=10",
            "--reverse",
            "--no-sort",          // preserve recency order
            "--bind=ctrl-/:toggle-preview",
            "--preview=echo {}",  // show the raw line (including id) in preview
            "--preview-window=down:2:wrap:hidden",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context(
            "failed to launch fzf — make sure fzf is installed\n\
             (https://github.com/junegunn/fzf or `apt install fzf`)"
        )?;

    // Write the list to fzf's stdin then close it.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(lines.as_bytes());
    }

    let output = child.wait_with_output()?;

    if !output.status.success() {
        // Exit 1 = no match / user pressed Esc
        return Ok(None);
    }

    let selected = String::from_utf8_lossy(&output.stdout);
    let selected = selected.trim();
    if selected.is_empty() {
        return Ok(None);
    }

    // First tab-delimited field is the hidden ID.
    let id = selected.split('\t').next().unwrap_or("").trim().to_string();
    if id.is_empty() {
        anyhow::bail!("fzf returned an unexpected selection: {selected:?}");
    }
    Ok(Some(id))
}

async fn run_ci(cli: Cli, config: Arc<sven_config::Config>) -> anyhow::Result<()> {
    // --resume in headless mode: resolve to a history file then run as conversation.
    if let Some(id) = &cli.resume {
        if id.is_empty() {
            anyhow::bail!(
                "--resume requires an explicit ID in headless mode.\n\
                 Use 'sven chats' to list available conversations."
            );
        }
        let file_path = history::resolve(id)
            .with_context(|| format!("resolving conversation id '{id}'"))?;

        // If a new prompt was given, append it as a pending ## User section.
        if let Some(prompt) = &cli.prompt {
            use std::fmt::Write as _;
            let current = std::fs::read_to_string(&file_path)
                .with_context(|| format!("reading {}", file_path.display()))?;
            let mut updated = current.trim_end().to_string();
            let _ = write!(updated, "\n\n## User\n\n{}\n", prompt.trim());
            std::fs::write(&file_path, &updated)
                .with_context(|| format!("appending user message to {}", file_path.display()))?;
        }

        let content = std::fs::read_to_string(&file_path)
            .with_context(|| format!("reading {}", file_path.display()))?;
        let opts = ConversationOptions {
            mode: cli.mode,
            model_override: cli.model,
            file_path,
            content,
        };
        return ConversationRunner::new(config).run(opts).await;
    }

    // Conversation file mode: load history, execute pending ## User, append results
    if cli.conversation {
        let file_path = cli.file.as_ref()
            .ok_or_else(|| anyhow::anyhow!("--conversation requires --file <path>"))?
            .clone();
        let content = std::fs::read_to_string(&file_path)
            .with_context(|| format!("reading conversation file {}", file_path.display()))?;
        let opts = ConversationOptions {
            mode: cli.mode,
            model_override: cli.model,
            file_path,
            content,
        };
        return ConversationRunner::new(config).run(opts).await;
    }

    // Standard CI mode: read input, parse steps, run
    let input = if let Some(path) = &cli.file {
        std::fs::read_to_string(path)
            .with_context(|| format!("reading input file {}", path.display()))?
    } else if !is_stdin_tty() {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf).context("reading stdin")?;
        buf
    } else {
        String::new()
    };

    let opts = CiOptions {
        mode: cli.mode,
        model_override: cli.model,
        input,
        extra_prompt: cli.prompt,
    };

    CiRunner::new(config).run(opts).await
}

async fn run_tui(cli: Cli, config: Arc<sven_config::Config>) -> anyhow::Result<()> {
    use ratatui::crossterm::{
        execute,
        event::{
            EnableMouseCapture, DisableMouseCapture,
            KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
        },
    };

    // Resolve history BEFORE entering raw-terminal mode.  If anything goes
    // wrong (bad ID, parse error, fzf cancelled) we can print a normal error
    // message without corrupting the terminal state.
    let initial_history = match &cli.resume {
        None => None,
        Some(id) => {
            let actual_id = if id.is_empty() {
                // No ID given — launch fzf so the user can pick interactively.
                match pick_chat_with_fzf()? {
                    Some(picked) => picked,
                    None => return Ok(()), // user cancelled fzf
                }
            } else {
                id.clone()
            };

            let (parsed, path) = history::load(&actual_id)
                .with_context(|| format!("loading conversation '{actual_id}'"))?;

            let segments: Vec<sven_tui::ChatSegment> = parsed
                .history
                .into_iter()
                .map(sven_tui::ChatSegment::Message)
                .collect();
            Some((segments, path))
        }
    };

    let terminal = ratatui::init();
    let _ = execute!(std::io::stderr(), EnableMouseCapture);
    
    // Enable keyboard enhancement so the terminal reports distinct escape sequences for
    // Shift+Enter vs plain Enter (and other modifier combos). Same flags as codex.
    // If the terminal doesn't support it, this will fail silently; Ctrl+J remains a newline fallback.
    let _ = execute!(
        std::io::stderr(),
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
        )
    );

    let opts = AppOptions {
        mode: cli.mode,
        initial_prompt: cli.prompt,
        initial_history,
        no_nvim: cli.no_nvim,
    };

    let app = App::new(config, opts);
    let result = app.run(terminal).await;

    // Clean up: restore terminal state
    let _ = execute!(std::io::stderr(), PopKeyboardEnhancementFlags);
    let _ = execute!(std::io::stderr(), DisableMouseCapture);
    ratatui::restore();

    result
}

fn init_logging(verbosity: u8) {
    let level = match verbosity {
        0 => "warn",
        1 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level));

    tracing_subscriber::registry()
        .with(fmt::layer().with_target(false).with_writer(std::io::stderr))
        .with(filter)
        .init();
}

fn is_stdin_tty() -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        unsafe { libc::isatty(io::stdin().as_raw_fd()) != 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}
