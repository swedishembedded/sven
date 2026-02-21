mod cli;

use std::io::{self, Read};
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
                "{:<45}  {:<19}  {}",
                "ID (use with --resume)", "DATE", "TITLE"
            );
            println!("{}", "-".repeat(90));
            for e in &entries {
                // Shorten the ID for display (timestamp only if slug is long)
                let display_id = if e.id.len() > 44 {
                    format!("{}…", &e.id[..43])
                } else {
                    e.id.clone()
                };
                let date = e.timestamp.replace('T', " ").replace('-', "-");
                let title = if e.title.len() > 50 {
                    format!("{}…", &e.title[..49])
                } else {
                    e.title.clone()
                };
                println!("{:<45}  {:<19}  {}", display_id, &date[..19.min(date.len())], title);
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

async fn run_ci(cli: Cli, config: Arc<sven_config::Config>) -> anyhow::Result<()> {
    // --resume in headless mode: resolve to a history file then run as conversation.
    if let Some(id) = &cli.resume {
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
        event::{EnableMouseCapture, DisableMouseCapture},
    };

    let terminal = ratatui::init();

    // Enable mouse reporting so scroll-wheel events reach us.
    let _ = execute!(std::io::stderr(), EnableMouseCapture);

    // If --resume was given, load the conversation history.
    let initial_history = if let Some(id) = &cli.resume {
        let (parsed, path) = history::load(id)
            .with_context(|| format!("loading conversation '{id}'"))?;
        // Convert history messages into ChatSegments.
        let segments: Vec<sven_tui::ChatSegment> = parsed
            .history
            .into_iter()
            .map(sven_tui::ChatSegment::Message)
            .collect();
        Some((segments, path))
    } else {
        None
    };

    let opts = AppOptions {
        mode: cli.mode,
        initial_prompt: cli.prompt,
        initial_history,
    };

    let app = App::new(config, opts);
    let result = app.run(terminal).await;

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
