// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::io::{self, Read};
use std::sync::Arc;

use anyhow::Context;

use crate::cli::{Cli, OutputFormatArg};
use crate::run::logging::is_stdin_tty;
use sven_ci::{find_project_root, CiOptions, CiRunner, OutputFormat};
use sven_config::AgentMode;
use sven_session_store::history;

pub(crate) async fn run_ci(cli: Cli, config: Arc<sven_config::Config>) -> anyhow::Result<()> {
    // ── Detect project root ──────────────────────────────────────────────────
    let project_root = find_project_root().ok();

    // ── --resume in headless mode ────────────────────────────────────────────
    if let Some(id) = &cli.resume {
        if id.is_empty() {
            anyhow::bail!(
                "--resume requires an explicit ID in headless mode.\n\
                 Use 'sven chats' to list available conversations."
            );
        }
        let file_path =
            history::resolve(id).with_context(|| format!("resolving conversation id '{id}'"))?;

        if let Some(prompt) = &cli.prompt {
            use std::fmt::Write as _;
            let current = std::fs::read_to_string(&file_path)
                .with_context(|| format!("reading {}", file_path.display()))?;
            let mut updated = current.trim_end().to_string();
            let _ = write!(updated, "\n\n## User\n\n{}\n", prompt.trim());
            std::fs::write(&file_path, &updated)
                .with_context(|| format!("appending user message to {}", file_path.display()))?;
        }

        // Legacy: resume via ConversationRunner for markdown conversation files.
        use sven_ci::{ConversationOptions, ConversationRunner};
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

    // ── Resolve effective trace I/O paths ─────────────────────────────────────
    // --file pointing to a .json is treated as --load-trace automatically
    // (an ATIF trajectory document is a single JSON file, unlike the old
    // line-delimited .jsonl format --load-jsonl used to auto-detect).
    let file_is_trace = cli
        .file
        .as_ref()
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("json"))
        .unwrap_or(false);

    let load_trace = cli.effective_load_trace().cloned().or_else(|| {
        if file_is_trace {
            cli.file.clone()
        } else {
            None
        }
    });

    let output_trace = cli.effective_output_trace().cloned();

    // ── Read workflow input ──────────────────────────────────────────────────
    // When --file points to a .json trace document, there is no separate
    // workflow file; we read from stdin (or use an empty input) for the new
    // prompt. When stdin is piped and a positional prompt is given (e.g.
    // `cmd | sven "fix these errors"`), we append stdin to the prompt with a
    // blank line and pass that as the single user message.
    let (input, extra_prompt) = if file_is_trace {
        // The file is an ATIF trajectory document, not a workflow.  New
        // workflow input (if any) comes from stdin.
        if !is_stdin_tty() {
            let mut buf = String::new();
            io::stdin()
                .read_to_string(&mut buf)
                .context("reading stdin")?;
            (buf, cli.prompt.clone())
        } else {
            (String::new(), cli.prompt.clone())
        }
    } else if let Some(path) = &cli.file {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading input file {}", path.display()))?;
        (content, cli.prompt.clone())
    } else if !is_stdin_tty() {
        let mut buf = String::new();
        io::stdin()
            .read_to_string(&mut buf)
            .context("reading stdin")?;
        let stdin_content = buf;
        // Keep positional prompt as extra_prompt so the runner can use it when
        // stdin is a piped conversation (e.g. `sven 'plan' | sven 'summarize'`).
        // The runner merges it into the step for plain-text stdin, or uses it as
        // the new task for conversation/JSONL input.
        (stdin_content, cli.prompt.clone())
    } else {
        (String::new(), cli.prompt.clone())
    };

    // ── HSM kernel path ───────────────────────────────────────────────────────
    // Every headless run is driven by the HSM kernel. There are two kernel-backed
    // entry points and the routing below picks between them:
    //
    //   * `RuntimeRunner` — the reactive-agent single-turn path: it drives one
    //     turn to completion and streams a conversation document. It handles a
    //     fresh single prompt *and* a piped prior-conversation document replayed
    //     as history (`sven '…' | sven 'next task'`).
    //   * `CiRunner` — the multi-step workflow orchestrator, also kernel-backed
    //     (it runs every turn on the kernel via `KernelAgent`). It owns the
    //     workflow features that `RuntimeRunner` does not: markdown `--file`,
    //     JSONL, `--var` templating, `--artifacts-dir`, `--dry-run`,
    //     `--output-format json/compact`, `--output-last-message`,
    //     `--system-prompt-file`, chat load/save.
    //
    // A run that uses any of those workflow features falls through to `CiRunner`
    // to preserve them; everything else takes the `RuntimeRunner` path.
    // `sdlc`/`chat` modes always use `RuntimeRunner` (no `CiRunner` path exists).
    let mode_forces_runtime_runner = matches!(cli.mode, AgentMode::Sdlc | AgentMode::Chat);

    // Piped stdin that itself looks like a prior sven conversation document is
    // replayed as history (parsed into prior messages + a trailing pending
    // turn), not concatenated into a single prompt.
    let input_is_conversation = input.lines().any(|line| {
        matches!(
            line.trim_end(),
            "## User" | "## Sven" | "## Tool" | "## Tool Result"
        )
    });

    // The kernel `RuntimeRunner` drives one reactive-agent turn to completion.
    // It handles both a fresh single prompt *and* a piped prior-conversation
    // document replayed as history (`sven '…' | sven 'next task'`). Genuine
    // multi-step workflow features (workflow `--file`, `--var` templating,
    // `--artifacts-dir`, `--dry-run`, JSON/JSONL/compact output, chat I/O,
    // `--system-prompt-file`, `--output-last-message`) live in `CiRunner`; a run
    // using any of them falls through to preserve those features. `sdlc`/`chat`
    // modes always use `RuntimeRunner`.
    let workflow_features_absent = cli.file.is_none()
        && matches!(cli.output_format, OutputFormatArg::Conversation)
        && cli.artifacts_dir.is_none()
        && !cli.dry_run
        && cli.output_last_message.is_none()
        && cli.system_prompt_file.is_none()
        && cli.vars.is_empty()
        && cli.effective_output_trace().is_none();

    if load_trace.is_none() && (mode_forces_runtime_runner || workflow_features_absent) {
        let kernel_mode = std::env::var("SVEN_MODE").unwrap_or_else(|_| {
            match cli.mode {
                AgentMode::Chat => "chat",
                AgentMode::Sdlc => "sdlc",
                _ => "agent",
            }
            .to_string()
        });
        // Resolve the new prompt and any prior history to replay.
        //
        // When stdin is a prior sven conversation document, parse it into
        // history + a trailing pending user turn. The new task is the CLI
        // positional prompt (if any), else the pending turn. History is seeded
        // into the kernel thread so the turn sees full context. Otherwise stdin
        // is plain text: trim it (notably the trailing newline piped stdin
        // always carries so exact-match model routing sees `"ping"`, not
        // `"ping\n"`) and merge with any positional prompt.
        let (prompt, history) = if input_is_conversation {
            match sven_session_store::parse_conversation(&input) {
                Ok(conv) => {
                    let new_task = extra_prompt
                        .as_ref()
                        .map(|p| p.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .or(conv.pending_user_input);
                    match new_task {
                        Some(task) => (task, conv.history),
                        None => {
                            eprintln!(
                                "[sven:error] Piped conversation has no pending task.\n\
                                 \n\
                                 To continue a piped conversation provide a prompt:\n\
                                 \n\
                                 \tsven 'task1' | sven 'task2'\n\
                                 \n\
                                 Or end the piped output with an unanswered ## User section\n\
                                 so the next sven instance picks it up automatically."
                            );
                            std::process::exit(2);
                        }
                    }
                }
                // If the document fails to parse, fall back to treating stdin as
                // a plain single prompt rather than losing the run.
                Err(e) => {
                    eprintln!(
                        "[sven:warn] Failed to parse piped input as conversation ({e}), \
                         treating as plain prompt"
                    );
                    let prompt = match &extra_prompt {
                        Some(p) if !input.trim().is_empty() => {
                            format!("{}\n\n{}", input.trim(), p.trim())
                        }
                        Some(p) => p.trim().to_string(),
                        None => input.trim().to_string(),
                    };
                    (prompt, Vec::new())
                }
            }
        } else {
            let prompt = match &extra_prompt {
                Some(p) if !input.trim().is_empty() => {
                    format!("{}\n\n{}", input.trim(), p.trim())
                }
                Some(p) => p.trim().to_string(),
                None => input.trim().to_string(),
            };
            (prompt, Vec::new())
        };
        // Apply the `--model` override into the config the kernel builds from
        // (the legacy CiRunner does the same before constructing its agent).
        let kernel_config = if let Some(m) = &cli.model {
            let mut cfg = (*config).clone();
            cfg.model = sven_model::resolve_model_from_config(&cfg, m);
            Arc::new(cfg)
        } else {
            config.clone()
        };
        let runner = sven_ci::RuntimeRunner::new(kernel_config);
        let code = runner
            .run(sven_ci::RuntimeRunnerOptions {
                mode: kernel_mode,
                agent_mode: cli.mode,
                prompt,
                history,
                project_root: project_root.clone(),
                timeout_secs: cli.run_timeout,
                step_timeout_secs: cli.step_timeout,
                max_tokens_budget: cli.max_tokens,
                append_system_prompt: cli.append_system_prompt.clone(),
                no_system: cli.no_system || cli.bare,
                no_tools: cli.no_tools || cli.bare,
                trace_level: cli.verbose,
            })
            .await;
        std::process::exit(code);
    }

    // ── Parse template variables ──────────────────────────────────────────────
    let mut vars: HashMap<String, String> = HashMap::new();
    for spec in &cli.vars {
        if let Some((k, v)) = sven_ci::template::parse_var(spec) {
            vars.insert(k, v);
        } else {
            eprintln!(
                "[sven:warn] Ignoring invalid --var argument: {spec:?}  (expected KEY=VALUE)"
            );
        }
    }

    // ── Map CLI output format ─────────────────────────────────────────────────
    let output_format = match cli.output_format {
        OutputFormatArg::Conversation => OutputFormat::Conversation,
        OutputFormatArg::Json => OutputFormat::Json,
        OutputFormatArg::Compact => OutputFormat::Compact,
        OutputFormatArg::Jsonl => OutputFormat::Jsonl,
    };

    let input_from_file = cli.file.is_some() && !file_is_trace;

    let opts = CiOptions {
        mode: cli.mode,
        model_override: cli.model,
        input,
        extra_prompt,
        input_from_file,
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
        no_system: cli.no_system || cli.bare,
        no_tools: cli.no_tools || cli.bare,
        trace_level: cli.verbose,
        load_trace,
        output_trace,
        rerun_toolcalls: cli.rerun_toolcalls,
        regen_system_prompt: cli.regen_system_prompt,
        max_tokens_budget: cli.max_tokens,
        attachments: cli.attach,
    };

    CiRunner::new(config).run(opts).await
}
