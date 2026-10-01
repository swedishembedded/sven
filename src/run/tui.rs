// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use anyhow::Context;

use crate::cli::Cli;
use sven_config::AgentMode;
use sven_session_store::{parse_frontmatter, parse_workflow};
use sven_tui::{App, AppOptions, ModelDirective, NodeBackend, QueuedMessage};

/// Whether the Kitty keyboard-enhancement flags were actually pushed for this
/// run. Push/Pop is a per-terminal *stack*: pushing unconditionally (without
/// checking terminal support) and popping unconditionally on every exit path
/// used to leave the pair unbalanced whenever a path popped without having
/// pushed (or vice versa on a terminal that silently ignores the CSI), which
/// can leave the user's terminal stuck in enhanced-keyboard mode after sven
/// exits. Set once in `run_tui` right after the support probe; read by every
/// teardown path (normal exit, panic, SIGTERM/SIGINT) so each pops iff it
/// pushed.
static KEYBOARD_ENHANCEMENT_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub(crate) async fn run_tui(mut cli: Cli, config: Arc<sven_config::Config>) -> anyhow::Result<()> {
    use ratatui::crossterm::{
        event::{
            DisableMouseCapture, EnableMouseCapture, KeyboardEnhancementFlags,
            PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
        },
        execute,
    };

    // Auto-detect node-proxy mode: when SVEN_NODE_URL and SVEN_NODE_TOKEN
    // are present (injected by the node into web PTY sessions), connect the
    // TUI to the running node so the agent has full P2P peer access.
    let node_backend = {
        let url = std::env::var("SVEN_NODE_URL")
            .or_else(|_| std::env::var("SVEN_GATEWAY_URL"))
            .ok();
        let token = std::env::var("SVEN_NODE_TOKEN")
            .or_else(|_| std::env::var("SVEN_GATEWAY_TOKEN"))
            .ok();
        let insecure = std::env::var("SVEN_NODE_INSECURE")
            .or_else(|_| std::env::var("SVEN_GATEWAY_INSECURE"))
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        match (url, token) {
            (Some(url), Some(token)) => Some(NodeBackend {
                url,
                token,
                insecure,
            }),
            _ => None,
        }
    };

    if cli.approval == sven_config::ApprovalMode::Manual && node_backend.is_some() {
        anyhow::bail!(
            "--approval manual needs a local session: in node-proxy mode \
             (SVEN_NODE_URL) the node answers its own approvals"
        );
    }

    // `--resume <id>` resolves to the same `--trace PATH` semantics as an
    // explicit `--trace` flag (both load source and sync-after-every-turn
    // target - see `AppOptions::trace_path`'s doc comment). Bare `--resume`
    // (no id) instead opens the in-TUI session picker at startup.
    let open_resume_picker = match &cli.resume {
        None => false,
        Some(id) if id.is_empty() => true,
        Some(id) => {
            let path = sven_session_store::resolve_session_id(id)
                .with_context(|| format!("resolving session id '{id}'"))?;
            cli.trace = Some(path);
            false
        }
    };

    // Install a panic hook that restores the terminal to a usable state before
    // printing the panic message.  Without this, a panic while in raw-mode /
    // alternate-screen leaves the terminal permanently garbled.
    // Use stdout (same fd as ratatui) - stderr may be redirected to /dev/null
    // below so escape sequences written there would never reach the terminal.
    {
        use ratatui::crossterm::{
            event::{DisableMouseCapture, PopKeyboardEnhancementFlags},
            execute,
            terminal::{disable_raw_mode, LeaveAlternateScreen},
        };
        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            if KEYBOARD_ENHANCEMENT_ACTIVE.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
            }
            let _ = execute!(std::io::stdout(), LeaveAlternateScreen, DisableMouseCapture,);
            original_hook(info);
        }));
    }

    let terminal = ratatui::init();
    // Setup escape sequences go to stderr.  ratatui owns stdout (via its
    // CrosstermBackend) and may buffer/reorder writes; using the independent
    // stderr fd avoids that.  Stderr still points to the real terminal here
    // because the dup2 redirect below has not happened yet.
    let _ = execute!(std::io::stderr(), EnableMouseCapture);

    // Push the Kitty keyboard-enhancement flags exactly once, and only on a
    // terminal that actually implements the protocol. This used to be pushed
    // unconditionally here AND a second time in `sven_tui::App::run` (on
    // stdout), against a single Pop below - an imbalanced stack that could
    // leave a legacy `ESC [ A`-style arrow-key sequence half-parsed as a
    // literal character on terminals that don't support the protocol at all
    // (the flags are silently ignored, so sven still relies on the raw
    // escape-timeout heuristic it would otherwise disambiguate away), and
    // could leave the terminal wedged in enhanced mode after an unclean exit.
    // `KEYBOARD_ENHANCEMENT_ACTIVE` records the outcome so every teardown
    // path (normal exit, panic, SIGTERM/SIGINT) pops iff this pushed.
    //
    // REPORT_ALL_KEYS_AS_ESCAPE_CODES makes even plain Enter arrive as
    // `\x1b[13u`, ensuring every Enter variant carries its modifiers (plain
    // Enter = no modifiers, Shift+Enter = modifier 2, etc.) and is parsed by
    // crossterm as a distinct event.
    let keyboard_enhanced =
        ratatui::crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    if keyboard_enhanced {
        let _ = execute!(
            std::io::stderr(),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                    | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
            )
        );
    }
    KEYBOARD_ENHANCEMENT_ACTIVE.store(keyboard_enhanced, std::sync::atomic::Ordering::SeqCst);

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
        let sink_path = std::env::var("SVEN_LOG_FILE").unwrap_or_else(|_| "/dev/null".to_string());
        if let Ok(f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&sink_path)
        {
            unsafe {
                let fd = f.into_raw_fd();
                libc::dup2(fd, libc::STDERR_FILENO);
                libc::close(fd);
            }
        }
    }
    // On non-Unix platforms (e.g. Windows), stderr redirection via dup2 is not
    // available without platform-specific APIs. Tracing is suppressed via
    // LevelFilter::OFF above, which is sufficient for TUI mode.
    #[cfg(not(unix))]
    {
        let _ = std::env::var("SVEN_LOG_FILE");
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
            event::{DisableMouseCapture, PopKeyboardEnhancementFlags},
            execute,
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
        if KEYBOARD_ENHANCEMENT_ACTIVE.load(std::sync::atomic::Ordering::SeqCst) {
            let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
        }
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen, DisableMouseCapture,);
        std::process::exit(1);
    });

    // ── Load workflow into initial TUI queue ─────────────────────────────────
    // If --file points to a markdown workflow, parse the steps and push them
    // into the TUI queue so the user can review them before they are sent.
    // The file must NOT be an ATIF trace document; that's handled via --load-trace.
    let file_is_trace = cli
        .file
        .as_ref()
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("json"))
        .unwrap_or(false);

    let initial_queue: Vec<QueuedMessage> = if let Some(path) = &cli.file {
        if !file_is_trace {
            match std::fs::read_to_string(path) {
                Ok(content) => {
                    let (fm, body) = parse_frontmatter(&content);
                    let _ = fm; // Frontmatter used by runner, not TUI queue loader
                    let config_ref = config.clone();
                    let mut wf = parse_workflow(body);
                    let mut q = Vec::new();
                    while let Some(step) = wf.steps.pop() {
                        // Resolve per-step model string into a ModelDirective
                        let model_transition = step.options.model.as_deref().map(|name| {
                            let cfg = sven_model::resolve_model_from_config(&config_ref, name);
                            ModelDirective::SwitchTo(Box::new(cfg))
                        });
                        // Resolve per-step mode string into an AgentMode
                        let mode_transition = step.options.mode.as_deref().and_then(|m| match m {
                            "research" => Some(AgentMode::Research),
                            "plan" => Some(AgentMode::Plan),
                            "agent" => Some(AgentMode::Agent),
                            "chat" => Some(AgentMode::Chat),
                            "sdlc" => Some(AgentMode::Sdlc),
                            _ => None,
                        });
                        q.push(QueuedMessage {
                            content: step.content,
                            model_transition,
                            mode_transition,
                        });
                    }
                    q
                }
                Err(e) => {
                    eprintln!(
                        "[sven:warn] Could not read workflow file {}: {e}",
                        path.display()
                    );
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    // Resolve trace paths for TUI: --load-trace feeds initial history; output
    // goes to --output-trace (or --trace which combines both). This is the
    // ONE session-persistence flag family for the TUI - loaded and saved as
    // a native ATIF trajectory (see `crates/tui/src/app/mod.rs`/`chat_ops.rs`).
    let trace_load_path = cli.effective_load_trace()?.cloned();
    let trace_save_path = cli.effective_output_trace().cloned();

    let opts = AppOptions {
        mode: cli.mode,
        approval: cli.approval,
        initial_prompt: cli.prompt,
        no_nvim: !cli.nvim,
        model_override: cli.model,
        trace_path: trace_save_path,
        load_trace_path: trace_load_path,
        initial_queue,
        node_backend,
        open_resume_picker,
    };

    let app = App::new(config, opts);
    let result = app.run(terminal).await;

    if KEYBOARD_ENHANCEMENT_ACTIVE.load(std::sync::atomic::Ordering::SeqCst) {
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();

    result
}
