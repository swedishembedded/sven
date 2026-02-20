mod cli;

use std::io::{self, Read};
use std::sync::Arc;

use anyhow::Context;
use tracing_subscriber::{filter::EnvFilter, fmt, prelude::*};

use cli::{Cli, Commands};
use clap::Parser;
use sven_ci::{CiOptions, CiRunner};
use sven_tui::{App, AppOptions};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    init_logging(cli.verbose);

    // Handle subcommands first
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
        }
    }

    let config = Arc::new(sven_config::load(cli.config.as_deref())?);

    if cli.is_headless() {
        run_ci(cli, config).await
    } else {
        run_tui(cli, config).await
    }
}

async fn run_ci(cli: Cli, config: Arc<sven_config::Config>) -> anyhow::Result<()> {
    // Read input: file > stdin > empty
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

    let opts = AppOptions {
        mode: cli.mode,
        initial_prompt: cli.prompt,
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
