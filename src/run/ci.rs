// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::io::{self, Read};
use std::sync::Arc;

use anyhow::Context;

use crate::cli::{Cli, OutputFormatArg};
use sven_ci::{find_project_root, CiOptions, CiRunner, OutputFormat};
use sven_vocab::AgentMode;

/// How long stdin may stay silent before the wait is announced. Long enough
/// that an ordinary pipe never prints it, short enough that a stuck run is
/// explained before anyone concludes sven has hung.
const STDIN_NOTICE_DELAY_MS: u64 = 2_000;

/// Read stdin to end, announcing the wait if it does not finish promptly.
///
/// Only reached when stdin is input by contract ([`Cli::reads_stdin`]), so a
/// slow producer is waited for. After a short grace period the wait is
/// announced on stderr, so a stuck run explains itself instead of appearing
/// hung.
fn read_stdin_to_end() -> anyhow::Result<String> {
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        let result = io::stdin().read_to_string(&mut buf).map(|_| buf);
        let _ = tx.send(result);
    });

    let waited = rx.recv_timeout(std::time::Duration::from_millis(STDIN_NOTICE_DELAY_MS));
    let result = match waited {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            eprintln!(
                "[sven:info] waiting for stdin: it is the task input, but nothing has arrived yet. \
                 Pass the task as a PROMPT argument if stdin is not meant to be read."
            );
            rx.recv().context("reading stdin")?
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            anyhow::bail!("stdin reader stopped unexpectedly")
        }
    };
    result.context("reading stdin")
}

pub(crate) async fn run_ci(
    mut cli: Cli,
    config: Arc<sven_bootstrap::Config>,
) -> anyhow::Result<()> {
    // ── Detect project root ──────────────────────────────────────────────────
    let project_root = find_project_root().ok();

    // ── --resume in headless mode ────────────────────────────────────────────
    // Maps onto the same semantics as an explicit `--trace PATH`: the
    // resolved session file becomes both the load source and the
    // write-back target, so a headless resume appends to the same session
    // rather than forking a new one under an auto-log path. `--load-trace`
    // alone does NOT imply write-back (see `CiOptions::load_trace`'s doc
    // comment), so `--resume` must map onto `--trace`, never `--load-trace`.
    if let Some(id) = &cli.resume {
        if id.is_empty() {
            anyhow::bail!(
                "--resume requires an explicit ID in headless mode.\n\
                 Use 'sven chats' to list available sessions."
            );
        }
        let path = sven_session_store::resolve_session_id(id)
            .with_context(|| format!("resolving session id '{id}'"))?;
        cli.trace = Some(path);
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

    let load_trace = cli.effective_load_trace()?.cloned().or_else(|| {
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
    // prompt. Stdin is read only when it is input by contract
    // (`Cli::reads_stdin`); with --stdin and a positional prompt (e.g.
    // `cmd | sven --stdin "fix these errors"`), stdin is appended to the prompt
    // with a blank line and passed as the single user message.
    let (input, extra_prompt) = if file_is_trace {
        // The file is an ATIF trajectory document, not a workflow.  New
        // workflow input (if any) comes from stdin.
        if cli.reads_stdin() {
            (read_stdin_to_end()?, cli.prompt.clone())
        } else {
            (String::new(), cli.prompt.clone())
        }
    } else if let Some(path) = &cli.file {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading input file {}", path.display()))?;
        (content, cli.prompt.clone())
    } else if cli.reads_stdin() {
        let stdin_content = read_stdin_to_end()?;
        // Keep positional prompt as extra_prompt so the runner can use it when
        // stdin is a piped conversation (e.g. `sven 'plan' | sven --stdin 'summarize'`).
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
    //     as history (`sven '…' | sven --stdin 'next task'`).
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
    // document replayed as history (`sven '…' | sven --stdin 'next task'`). Genuine
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
        // `--attach` is loaded into the first user turn by `CiRunner` alone
        // (`RunOptions::attachments`); `RuntimeRunner` has no attachment path
        // at all, so routing an attach run to it silently dropped the flag and
        // the model received a bare prompt. Treat it like every other feature
        // only `CiRunner` implements.
        && cli.attach.is_empty()
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
                                 \tsven 'task1' | sven --stdin 'task2'\n\
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
            cfg.model = cfg.resolve_model(m);
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
