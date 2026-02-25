// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
mod cli;

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::Context;
use tracing_subscriber::{filter::EnvFilter, fmt, prelude::*};

use cli::{Cli, Commands, OutputFormatArg};
use clap::Parser;
use sven_ci::{
    CiOptions, CiRunner, ConversationOptions, ConversationRunner,
    OutputFormat, find_project_root,
};
use sven_input::{history, parse_frontmatter, parse_workflow};
use sven_model::catalog::ModelCatalogEntry;
use sven_model::Message as SvenMessage;
use sven_tui::{App, AppOptions};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // In TUI mode writing to stderr corrupts the ratatui display.
    // Suppress all tracing output unless the caller explicitly opts in by
    // setting SVEN_LOG_FILE (writes to that file) or by passing --verbose
    // (writes to stderr — only useful with headless / CI mode).
    let is_tui = !cli.is_headless() && cli.command.is_none();
    init_logging(cli.verbose, is_tui);

    // Handle subcommands first (before loading config)
    if let Some(cmd) = &cli.command {
        match cmd {
            Commands::Completions { shell } => {
                cli::print_completions(*shell);
                return Ok(());
            }
            Commands::ShowConfig => {
                let config = sven_config::load(cli.config.as_deref())?;
                println!("{}", serde_yaml::to_string(&config).unwrap_or_default());
                return Ok(());
            }
            Commands::Chats { limit } => {
                print_chats(*limit);
                return Ok(());
            }
            Commands::Validate { file } => {
                return validate_workflow(file);
            }
            Commands::ListModels { provider, refresh, json } => {
                let config = sven_config::load(cli.config.as_deref())?;
                return list_models_cmd(&config, provider.as_deref(), *refresh, *json).await;
            }
            Commands::ListProviders { verbose, json } => {
                return list_providers_cmd(*verbose, *json);
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

/// Validate a workflow file: parse frontmatter, count steps, report to stdout.
fn validate_workflow(file: &std::path::Path) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(file)
        .with_context(|| format!("reading workflow file {}", file.display()))?;

    let (frontmatter, markdown_body) = parse_frontmatter(&content);

    let workflow = parse_workflow(markdown_body);

    // Title: frontmatter overrides H1
    let title = frontmatter.as_ref().and_then(|fm| fm.title.as_deref())
        .or(workflow.title.as_deref());
    if let Some(t) = title {
        println!("Title: {t}");
    }

    if let Some(fm) = &frontmatter {
        println!("Frontmatter: OK");
        if let Some(m) = &fm.mode {
            println!("  mode: {m}");
        }
        if let Some(m) = &fm.model {
            println!("  model: {m}");
        }
        if let Some(t) = fm.step_timeout_secs {
            println!("  step_timeout_secs: {t}");
        }
        if let Some(t) = fm.run_timeout_secs {
            println!("  run_timeout_secs: {t}");
        }
        if let Some(vars) = &fm.vars {
            println!("  vars ({}):", vars.len());
            for (k, v) in vars {
                println!("    {k} = {v}");
            }
        }
    } else {
        println!("Frontmatter: (none)");
    }

    if let Some(preamble) = &workflow.system_prompt_append {
        println!("Preamble: {} chars (appended to system prompt)", preamble.chars().count());
    }

    let mut queue = workflow.steps;
    let total = queue.len();
    println!("Steps: {total}");

    let mut i = 0;
    while let Some(step) = queue.pop() {
        i += 1;
        let label = step.label.as_deref().unwrap_or("(unlabelled)");
        let mode = step.options.mode.as_deref().unwrap_or("(inherit)");
        let provider = step.options.provider.as_deref().unwrap_or("(inherit)");
        let model = step.options.model.as_deref().unwrap_or("(inherit)");
        let timeout = step.options.timeout_secs
            .map(|t| format!("{t}s"))
            .unwrap_or_else(|| "(inherit)".to_string());
        println!("  Step {i}/{total}: {label:?}  mode={mode}  provider={provider}  model={model}  timeout={timeout}");
        if !step.content.is_empty() {
            let preview = step.content.chars().take(80).collect::<String>();
            let ellipsis = if step.content.chars().count() > 80 { "…" } else { "" };
            println!("    {preview}{ellipsis}");
        }
    }

    println!("\nWorkflow is valid.");
    Ok(())
}

/// List available models, optionally querying the provider API for live data.
async fn list_models_cmd(
    config: &sven_config::Config,
    provider_filter: Option<&str>,
    refresh: bool,
    as_json: bool,
) -> anyhow::Result<()> {
    // Validate provider filter against the registry.
    if let Some(prov) = provider_filter {
        if sven_model::get_driver(prov).is_none() {
            eprintln!("Unknown provider: {prov:?}");
            eprintln!("\nAvailable providers (run `sven list-providers` for details):");
            for d in sven_model::list_drivers() {
                eprintln!("  {:20} {}", d.id, d.name);
            }
            anyhow::bail!("Invalid provider: {prov}");
        }
    }

    let entries: Vec<ModelCatalogEntry> = if refresh {
        // Query the configured (or filtered) provider's live API.
        let model_cfg = if let Some(prov) = provider_filter {
            let mut c = config.model.clone();
            c.provider = prov.to_string();
            c
        } else {
            config.model.clone()
        };
        let model = sven_model::from_config(&model_cfg)?;
        let mut live = model.list_models().await?;
        if let Some(prov) = provider_filter {
            live.retain(|e| e.provider == prov);
        }
        live
    } else {
        // Use static catalog only.
        let mut all = sven_model::catalog::static_catalog();
        if let Some(prov) = provider_filter {
            all.retain(|e| e.provider == prov);
        }
        all.sort_by(|a, b| a.provider.cmp(&b.provider).then(a.id.cmp(&b.id)));
        all
    };

    if as_json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    if entries.is_empty() {
        println!("No models found.");
        return Ok(());
    }

    // Determine column widths.
    let id_w = entries.iter().map(|e| e.id.len()).max().unwrap_or(10).max(10);
    let prov_w = entries.iter().map(|e| e.provider.len()).max().unwrap_or(8).max(8);

    println!(
        "{:<id_w$}  {:<prov_w$}  {:>12}  {:>16}  DESCRIPTION",
        "ID", "PROVIDER", "CTX WINDOW", "MAX OUT TOKENS",
        id_w = id_w, prov_w = prov_w,
    );
    println!("{}", "-".repeat(id_w + prov_w + 50));

    for e in &entries {
        let ctx = if e.context_window == 0 {
            "  -".to_string()
        } else {
            format!("{:>12}", e.context_window)
        };
        let max_out = if e.max_output_tokens == 0 {
            "  -".to_string()
        } else {
            format!("{:>16}", e.max_output_tokens)
        };
        println!(
            "{:<id_w$}  {:<prov_w$}  {}  {}  {}",
            e.id, e.provider, ctx, max_out, e.description,
            id_w = id_w, prov_w = prov_w,
        );
    }
    println!("\nTotal: {} model(s)", entries.len());
    Ok(())
}

/// List all registered model providers.
fn list_providers_cmd(verbose: bool, as_json: bool) -> anyhow::Result<()> {
    let drivers = sven_model::list_drivers();

    if as_json {
        #[derive(serde::Serialize)]
        struct ProviderJson {
            id: &'static str,
            name: &'static str,
            description: &'static str,
            default_api_key_env: Option<&'static str>,
            default_base_url: Option<&'static str>,
            requires_api_key: bool,
        }
        let rows: Vec<ProviderJson> = drivers.iter().map(|d| ProviderJson {
            id: d.id,
            name: d.name,
            description: d.description,
            default_api_key_env: d.default_api_key_env,
            default_base_url: d.default_base_url,
            requires_api_key: d.requires_api_key,
        }).collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    println!("Supported Model Providers ({} total)\n", drivers.len());

    if verbose {
        for d in drivers {
            println!("  {} — {}", d.id, d.name);
            println!("    {}", d.description);
            if let Some(env) = d.default_api_key_env {
                println!("    API key env : {env}");
            }
            if let Some(url) = d.default_base_url {
                println!("    Default URL : {url}");
            }
            if !d.requires_api_key {
                println!("    Auth        : none required");
            }
            println!();
        }
    } else {
        let id_w = drivers.iter().map(|d| d.id.len()).max().unwrap_or(10).max(10);
        let name_w = drivers.iter().map(|d| d.name.len()).max().unwrap_or(8).max(8);
        println!("{:<id_w$}  {:<name_w$}  DESCRIPTION", "ID", "NAME");
        println!("{}", "-".repeat(id_w + name_w + 40));
        for d in drivers {
            println!("{:<id_w$}  {:<name_w$}  {}", d.id, d.name, d.description);
        }
        println!("\nUse `sven list-providers --verbose` for API key and URL details.");
        println!("Use `sven list-models --provider <ID>` to see models for a specific provider.");
    }
    Ok(())
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
                "{:<45}  {:<16}  {:<5}  TITLE",
                "ID (use with --resume)", "DATE", "TURNS"
            );
            println!("{}", "-".repeat(95));
            for e in &entries {
                let display_id = if e.id.len() > 44 {
                    format!("{}…", &e.id[..43])
                } else {
                    e.id.clone()
                };
                let date = e.timestamp.replace('T', " ");
                let date = &date[..16.min(date.len())];
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
fn pick_chat_with_fzf() -> anyhow::Result<Option<String>> {
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
            let turns_label = if e.turns == 1 { "1 turn".to_string() } else { format!("{} turns", e.turns) };
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
            "failed to launch fzf — make sure fzf is installed\n\
             (https://github.com/junegunn/fzf or `apt install fzf`)"
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

async fn run_ci(cli: Cli, config: Arc<sven_config::Config>) -> anyhow::Result<()> {
    // ── Detect project root (used in all CI modes) ───────────────────────────
    let project_root = find_project_root().ok();

    // ── --jsonl conversation file ─────────────────────────────────────────────
    if let Some(jsonl_path) = &cli.jsonl {
        let file_path = jsonl_path.clone();

        // Create parent directories and the file if it does not exist yet.
        if !file_path.exists() {
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating parent dirs for {}", file_path.display()))?;
            }
            std::fs::write(&file_path, "")
                .with_context(|| format!("creating {}", file_path.display()))?;
        }

        // If a positional prompt was given, append it as a JSONL user message.
        if let Some(prompt) = &cli.prompt {
            let user_msg = SvenMessage::user(prompt.trim());
            let line = serde_json::to_string(&user_msg).context("serializing user message")?;
            let mut content = std::fs::read_to_string(&file_path)
                .with_context(|| format!("reading {}", file_path.display()))?;
            if !content.is_empty() && !content.ends_with('\n') {
                content.push('\n');
            }
            content.push_str(&line);
            content.push('\n');
            std::fs::write(&file_path, &content)
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

    // ── --resume in headless mode ────────────────────────────────────────────
    if let Some(id) = &cli.resume {
        if id.is_empty() {
            anyhow::bail!(
                "--resume requires an explicit ID in headless mode.\n\
                 Use 'sven chats' to list available conversations."
            );
        }
        let file_path = history::resolve(id)
            .with_context(|| format!("resolving conversation id '{id}'"))?;

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

    // ── Conversation file mode ───────────────────────────────────────────────
    // Trigger when --conversation is set, OR when --file points to a .jsonl file
    // (JSONL files are conversation files, not workflow files).
    let file_is_jsonl = cli.file.as_ref()
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("jsonl"))
        .unwrap_or(false);

    if cli.conversation || file_is_jsonl {
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

    // ── Standard CI mode ─────────────────────────────────────────────────────
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

    // ── Parse template variables from --var flags ────────────────────────────
    let mut vars: HashMap<String, String> = HashMap::new();
    for spec in &cli.vars {
        if let Some((k, v)) = sven_ci::template::parse_var(spec) {
            vars.insert(k, v);
        } else {
            eprintln!("[sven:warn] Ignoring invalid --var argument: {spec:?}  (expected KEY=VALUE)");
        }
    }

    // ── Map CLI output format to OutputFormat ────────────────────────────────
    let output_format = match cli.output_format {
        OutputFormatArg::Conversation => OutputFormat::Conversation,
        OutputFormatArg::Json => OutputFormat::Json,
        OutputFormatArg::Compact => OutputFormat::Compact,
    };

    let opts = CiOptions {
        mode: cli.mode,
        model_override: cli.model,
        input,
        extra_prompt: cli.prompt,
        project_root,
        output_format,
        artifacts_dir: cli.artifacts_dir,
        vars,
        step_timeout_secs: cli.step_timeout,
        run_timeout_secs: cli.run_timeout,
        dry_run: cli.dry_run,
        output_last_message: cli.output_last_message,
        system_prompt_file: cli.system_prompt_file,
        append_system_prompt: cli.append_system_prompt,
        trace_level: cli.verbose,
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

    let initial_history = match &cli.resume {
        None => None,
        Some(id) => {
            let actual_id = if id.is_empty() {
                match pick_chat_with_fzf()? {
                    Some(picked) => picked,
                    None => return Ok(()),
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

    // Install a panic hook that restores the terminal to a usable state before
    // printing the panic message.  Without this, a panic while in raw-mode /
    // alternate-screen leaves the terminal permanently garbled.
    // Use stdout (same fd as ratatui) — stderr may be redirected to /dev/null
    // below so escape sequences written there would never reach the terminal.
    {
        use ratatui::crossterm::{
            execute,
            event::DisableMouseCapture,
            terminal::{disable_raw_mode, LeaveAlternateScreen},
        };
        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = execute!(
                std::io::stdout(),
                LeaveAlternateScreen,
                DisableMouseCapture,
            );
            original_hook(info);
        }));
    }

    let terminal = ratatui::init();
    // Setup escape sequences go to stderr.  ratatui owns stdout (via its
    // CrosstermBackend) and may buffer/reorder writes; using the independent
    // stderr fd avoids that.  Stderr still points to the real terminal here
    // because the dup2 redirect below has not happened yet.
    let _ = execute!(std::io::stderr(), EnableMouseCapture);
    let _ = execute!(
        std::io::stderr(),
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
        )
    );

    // Redirect stderr to /dev/null (or SVEN_LOG_FILE) AFTER setup is done.
    // From this point on stderr is a sink; all cleanup escape sequences use
    // stdout instead (see below).  This is the defence against subprocess
    // output corrupting the TUI: any process that inherits our stderr fd
    // writes to /dev/null instead of the raw terminal.
    // Tracing is already suppressed via LevelFilter::OFF above; this catches
    // anything else (dynamic libraries, C extensions, etc.).
    #[cfg(unix)]
    {
        use std::os::unix::io::IntoRawFd;
        let sink_path = std::env::var("SVEN_LOG_FILE")
            .unwrap_or_else(|_| "/dev/null".to_string());
        if let Ok(f) = std::fs::OpenOptions::new()
            .write(true).create(true).append(true)
            .open(&sink_path)
        {
            unsafe {
                let fd = f.into_raw_fd();
                libc::dup2(fd, libc::STDERR_FILENO);
                libc::close(fd);
            }
        }
    }

    // Spawn a background task that listens for SIGTERM / SIGINT from the OS
    // (e.g. `kill <pid>` or systemd shutdown).  These signals bypass the
    // normal Rust panic/drop machinery, so we must handle them explicitly to
    // restore the terminal before the process exits.  In raw-mode, Ctrl-C is
    // received as a key event and handled by the TUI; real SIGINT only arrives
    // when the process is sent the signal from outside.
    // Uses stdout for all escape sequences (stderr is now /dev/null).
    tokio::spawn(async move {
        use ratatui::crossterm::{
            execute,
            event::DisableMouseCapture,
            terminal::{disable_raw_mode, LeaveAlternateScreen},
        };
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm = match signal(SignalKind::terminate()) {
                Ok(s) => s,
                Err(_) => return,
            };
            let mut sigint = match signal(SignalKind::interrupt()) {
                Ok(s) => s,
                Err(_) => return,
            };
            tokio::select! {
                _ = sigterm.recv() => {}
                _ = sigint.recv()  => {}
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        let _ = disable_raw_mode();
        let _ = execute!(
            std::io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
        );
        std::process::exit(1);
    });

    let opts = AppOptions {
        mode: cli.mode,
        initial_prompt: cli.prompt,
        initial_history,
        no_nvim: !cli.nvim,
        model_override: cli.model,
        jsonl_path: cli.jsonl,
    };

    let app = App::new(config, opts);
    let result = app.run(terminal).await;

    let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();

    result
}

fn init_logging(verbosity: u8, is_tui: bool) {
    // In TUI mode tracing output written to stderr corrupts the ratatui
    // display.  We suppress all logging unless the caller opts in:
    //   • Set SVEN_LOG_FILE=/path/to/file  → logs go to that file (any mode)
    //   • Set RUST_LOG=...                 → respects the env filter
    //   • Pass --verbose (-v)              → enables debug/trace (headless only)
    if is_tui {
        // Check for an explicit log file — advanced debugging only.
        if let Ok(log_path) = std::env::var("SVEN_LOG_FILE") {
            use std::sync::Mutex;
            if let Ok(file) = std::fs::OpenOptions::new()
                .create(true).append(true).open(&log_path)
            {
                let filter = EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| EnvFilter::new("debug"));
                let _ = tracing_subscriber::registry()
                    .with(fmt::layer()
                        .with_target(true)
                        .with_ansi(false)
                        .with_writer(Mutex::new(file)))
                    .with(filter)
                    .try_init();
                return;
            }
        }
        // No log file: suppress all output so the TUI is not corrupted.
        let _ = tracing_subscriber::registry()
            .with(tracing_subscriber::filter::LevelFilter::OFF)
            .try_init();
        return;
    }

    let level = match verbosity {
        0 => "warn",
        1 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level));

    let _ = tracing_subscriber::registry()
        .with(fmt::layer().with_target(false).with_writer(std::io::stderr))
        .with(filter)
        .try_init();
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
