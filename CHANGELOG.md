# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Migrating from 2.0.0
- `ToolCapability` gains `SpawnChild`, and `Effect::InstantiateSubmachine` now requires it: a machine that spawns children must allow `SpawnChild` in the spawning state, and an exhaustive `match` on `ToolCapability` needs the new arm.
- `ChildSpawner::spawn_child` takes a `run: ChildRun` parameter (the contract and the child's cancel scope).
- New public fields on structs that are not `#[non_exhaustive]`: `ToolsConfig.disabled`, `AgentConfig.child_run_timeout_secs`, `TurnLimits.max_output_tokens`, `IntegrationProviders.approver` (a `ChildApprover`), `TeamMember.deny_tools`. A struct literal needs the field or `..Default::default()`.
- `SubagentUpdate` gains `TokensUsed`; an exhaustive `match` needs the arm.
- `TaskState::RunningTools` is gone.

### Added
- `ChildRunContract` (`sven-hsm`): the capabilities, budgets and deadline a child run holds; `narrow` only ever tightens them. `PermissionPolicy::{allows, allows_in_every_state, requires_approval, ceiling_in, ceiling_in_every_state, intersect}`, `ToolCapability::ALL`, `known_capability_for_tool_name`.
- `sven-kernel`: `CancelScope`, `DeadlineTimer`, `ChildRun`, `ErasedRuntime::spawn_child_run`, `Runtime`/`ErasedRuntime::{cancel, cancel_scope}`, `RUN_CANCELLED`, `EventSink::closed`.
- `ToolRegistry::remove`, the `tools.disabled` configuration, `agent.child_run_timeout_secs` (default 3600).
- `sven acp serve --max-tool-rounds / --max-output-tokens / --wall-clock-secs / --disable-tool / --permission-timeout-secs`; its prompt responses report the turn's token usage.
- `sven_team::limits` (`MemberLimits`, `TokenAllowance`, `TeamConfig::{reserve_tokens, settle_tokens}`), `TeamConfigStore::{upsert, reserve_tokens, settle_tokens}`, `TeamDefinition::apply_to`; `GateApprover` and `ChildApprover` in `sven-bootstrap`; `sven_sdk::machine::GatedCall`.

### Changed
- **Every child run is held to a contract.** A child holds only what its parent holds in the spawning state, narrowed by the spawner's terms; cancelling, aborting or dropping the parent cancels its children; `ErasedRuntime::spawn_child_run` applies the contract's policy and cancels the run at its deadline.
- **Headless SDLC children now follow the session's approval behaviour.** A task child's questions and approval requests go to the parent session's question and approval channels - answered by a person in the TUI, auto-approved by the headless runner, as the parent's own - instead of ending the task. They hold only what the parent holds in `Execution` (no network), take at most `agent.max_tool_rounds` rounds, write at most the session's configured output cap per response, and stop after `agent.child_run_timeout_secs`; their tool calls in flight are aborted with them.
- **TUI users now get sub-agent approval prompts.** A `task` sub-agent's permission request is allowed outright only when the parent would run the call itself without asking anyone (a known tool its policy allows without approval, and no host that asks about every call); everything else - shell, MCP tools - is shown on the parent's own approval gate with the tool and its command or path, or goes to the IDE over ACP. It is refused only when there is neither. Sub-agents only start in a mode within the parent's ceiling (an SDLC session starts none), run under the parent's disabled tools and budgets, wait for approvals until their deadline, and are reported as failed when they exit or time out before finishing.
- **The round default for `RuntimeBuilder` sessions now honours `agent.max_tool_rounds`** (default 200); CLI, TUI and ACP sessions previously stopped at the reactive machine's built-in 16.
- A question or approval still pending when its run stops is withdrawn from the frontend.
- Team members are held to their team's limits: `deny_tools`, `max_iterations` and `token_budget`. The budget is reserved per run under the team-config lock and charged with everything the run and its sub-agents used; a member answers its approvals itself, refusing its denied tools. `sven team start` writes the team config (under the lock) before spawning. Teammates honour `--model` and their definition's instructions, back off and then fail on an unreadable team config, and do not repeat their initial task on a restart. Team-config writes are locked across processes.

### Fixed
- `spawn_teammate` no longer passes an undefined `--team-lead-peer` flag that made every spawned teammate fail to start.
- A teammate started with a `task_prompt` puts it on the task board as its first task; it was dropped.
- A turn's output-token limit applies even when the model's context window is unknown.
- `agent.stream_idle_timeout_secs` no longer draws an unrecognised-key warning.

## [2.0.0] - 2026-09-30

### Migrating an SDK application from 1.x
- `Agent::send` returns a `RunOutcome`; read `.reply` where the string was used, and match `conclusion` (`Success`, `Cancelled`, `Timeout`, `BudgetExhausted`, `Waiting`).
- An `Engine` offers no built-in tools unless asked: add `.toolset(Toolset::coding())` (or `research()`) to keep them.
- A question the model asks parks the run (`RunConclusion::Waiting`, `RunOutcome::question`); answer it with `Agent::answer`.
- `SVEN_STREAM_CHUNK_TIMEOUT_SECS` is gone; set `agent.stream_idle_timeout_secs` in the configuration.
- `RunOutcome` and `Question` are `#[non_exhaustive]`: read their fields, do not construct them.
- `HumanGate::Approval` carries `call: Option<GatedCall>` (the tool's name and arguments it gates); a pattern that names the variant's fields needs `call` or `..`.
- `CallError` is `#[non_exhaustive]` and gains `Stopped { conclusion }`; match it with a wildcard arm.
- The learning subsystem is gone (`sven learn`, `/learn`, `tools.memory.learning`, the `AssimilateKnowledge` and `IngestDocument` capabilities); an old `learning:` section loads and is ignored with a warning.
- The built-in file tools are built with a `PathScope` (`::default()` for none); `RuntimeContext`/`AgentRuntimeContext` gain `path_scope`.
- `RunConclusion` serializes as its snake-case name (`"budget_exhausted"`), so a host can store how a run ended.

### Changed
- **BREAKING (sven-tools-fs, sven-tools-web, sven-tools-exec)**: the path-taking tools are built with a `PathScope`. `ReadFileTool`, `WriteTool`, `EditFileTool`, `FindFileTool` and `GrepTool` are no longer unit structs: construct them with `::default()` (unconfined, as before) or `::new(scope)`. `ShellTool` gains a `scope` field (`ShellTool { timeout_secs, ..Default::default() }`), and `AttachFileTool` and `TaskTool` gain `with_scope`.
- **BREAKING (sven-bootstrap, sven-turn)**: `RuntimeContext` and `AgentRuntimeContext` gain a `path_scope` field, which carries the scope to the built-in tools; a struct literal without `..Default::default()` adds it. The CLI and TUI leave it unconfined.
- **BREAKING (sven-sdk)**: typed calls take bounds: `Agent::call_with` and `Engine::call_with` accept `RunOptions` covering the whole call, correction attempts included, and a call a bound stopped fails with the new `CallError::Stopped { conclusion }`. `CallError` is `#[non_exhaustive]`. `sven_sdk::schemars` re-exports the schema crate a method's return type derives `JsonSchema` from.
- **BREAKING (sven-sdk)**: `HumanGate::Approval` carries `call: Option<GatedCall>`, the tool call it gates (name and arguments), so an application can decide per tool rather than per capability. A pattern that names the variant's fields needs `call` or `..`. Underneath, `Effect::RequestHumanApproval` and `ApprovalRequest` carry it too.
- **BREAKING**: the stream idle timeout is configuration: `agent.stream_idle_timeout_secs` (default 300) replaces the `SVEN_STREAM_CHUNK_TIMEOUT_SECS` environment variable, so an embedding application sets it per engine rather than per process. **Migration**: move the value into the config file (`${VAR}` expansion still lets the environment supply it). `sven_turn::stream_turn` takes `impl Into<TurnLimits>` (a `ThinkingBudget` still converts) and `TurnExecutor::with_thinking_budget` is `with_turn_limits(TurnLimits::from_agent_config(..))`.
- **BREAKING (sven-sdk)**: a question the model asks parks the run instead of blocking it. The run ends with `RunConclusion::Waiting` and `RunOutcome::question` (`Question { id, prompt, options }`); `Agent::answer(id, text)` (or `answer_with(id, text, RunOptions)`) continues it, also on an agent suspended and resumed in another process. The answer becomes the result of the `ask_question` call that asked. `Toolset::coding()` and `Toolset::research()` now offer `ask_question`; asking parks the run. `RunOutcome` and `Question` are `#[non_exhaustive]`. **Migration**: handle `RunConclusion::Waiting` where conclusions are matched exhaustively. Underneath: `RunConclusion::Waiting` concludes as an unknown outcome (no reward is stamped), and `sven-ci` maps its needs-a-human exit to it; a parked question records the conversation's id for the call that asked (`PendingQuestion::call_ref`, `Event::QuestionAsked::call_ref`), without which an answer after a restart had no call to attach to; the kernel retires a parked call from its in-flight work, so a run parked on a question reaches quiescence instead of waiting forever; `ToolSetProfile` routes `ask_question` through `Questions::{Answered, Parked, Unavailable}` instead of an optional channel.
- **BREAKING (sven-sdk)**: `ApprovalPolicy` gains `Ask`, built with `ApprovalPolicy::ask(|gate: HumanGate| ...)`: the application answers each approval request and question through the reply channel the gate carries, now or later. `ApprovalPolicy` is no longer `Copy`/`PartialEq`.
- **BREAKING (sven-sdk)**: `Agent::send` returns a `RunOutcome { conclusion, reply, usage }` instead of the reply string, and `Agent::send_with(text, RunOptions)` bounds a run: `RunOptions::cancel(CancelToken)`, `::deadline(Duration)` and `::max_output_tokens(u64)`. `conclusion` is a `RunConclusion` (`Success`, `Cancelled`, `Timeout`, `BudgetExhausted`); a turn wrapped up because the tool-round budget ran out reports `BudgetExhausted`, not `Success`. `Usage` counts are `None` when the provider reported none. A run stopped by a bound is an outcome, not an error, and its history and kernel state are kept. **Migration**: read `.reply` from the outcome where the string was used.
- **BREAKING (sven-sdk)**: an `Engine` gives its agents no built-in tools unless asked. `EngineBuilder::toolset(Toolset)` selects them: `Toolset::none()` (the default: no built-in tools and no MCP servers, only tools registered with `EngineBuilder::tool`), `Toolset::coding()` and `Toolset::research()`. **Migration**: an application that relied on the built-in tools adds `.toolset(Toolset::coding())`. Underneath, `RuntimeBuilder::with_builtin_tools(BuiltinTools)` selects the same sets for any surface (`BuiltinTools::Detect` keeps the application's detected profile), and the reactive agent's clarification post-check only routes a closing question to `ask_question` when the session registers that tool.

### Removed
- **BREAKING (sven-memory, sven-vocab, sven-hsm, sven-executors, sven-bootstrap, sven-config, CLI)**: removed the learning subsystem. sven is an agent execution framework; training a model on what an agent learned belongs to the application that does it, built on `sven-sdk`. Deleted: the `assimilate_fact` and `ingest_document` tools (`AssimilateFactTool`, `IngestDocumentTool`, `ProvenanceIndex`); the pending-facts ledger (`PendingFactsLedger`, `LedgerEntry`, `PendingFactRecord`, `DocumentRecord`, `FrozenProbe`, `LedgerError`); the drain and its submitters (`PendingFactsDrain`, `FactSubmitter`, `FactOutcome`, `FactReport`, `GateNumbers`, `DrainError`, `claim_sole_drain`, `LocalFactSubmitter`, `LocalStudy`, `submitter_from_config` and the `*_PLACEHOLDER` constants); `sven_memory::{diagnose, format_report, DoctorReport, DoctorCheck, CheckStatus}`; the rule expander (`sven_memory::{expand, Instance, Split}`) and `sven_vocab::rule`; `sven learn flush` and `sven learn doctor`; the `/learn` slash command and the `knowledge-extract` persona; `LearningConfig` (`tools.memory.learning`); `ToolCapability::AssimilateKnowledge` and `ToolCapability::IngestDocument`; `KnowledgeApprovals` and `UserExecutor::with_knowledge_approvals`; `ProvenanceSink` and `ToolExecutor::with_provenance_sink`; `LedgerAdmission` and `FactSource::ledger_admission`; the `fact_ledger`, `provenance_index` and `knowledge_approvals` fields of `IntegrationProviders`. **Migration**: a `tools.memory.learning` section still loads and is ignored with one warning; delete it to silence the warning. sven no longer reads or writes `~/.config/sven/memory/pending-facts.jsonl`, its `.cursor.json` or `~/.config/sven/memory/learning/`; keep them for whatever trains on them, or delete them. A match on `ToolCapability` drops the two arms, and a permission policy that listed them drops them; a tool of your own that declared either picks the bucket describing what it touches (`ReadFile`, `WriteFile`, ...), and asks for a human through `ApprovalPolicy` or a policy's `require_approval`. A tool name starting with `assimilate_` or `ingest_` is no longer special in `capability_for_tool_name`. Semantic memory (`semantic_memory`, recall framing of records stamped with provenance or a session scope), parked questions (`QuestionLedger`, `sven questions`), `ContentDigest`, `FactSource` and `ToolOutput::provenance` are unchanged.
- **BREAKING (workspace)**: `sven-machines` depends only on `sven-hsm` and `sven-vocab`. Removed the `sven-llm` crate: `ThreadStore` is `sven_executors::ThreadStore`, `TurnRequest` and `TURN_KIND` are in `sven_vocab` (`sven_executors::turn::TURN_KIND` is gone), and its unused `LlmError` is deleted. `AgentRuntimeContext` moved to `sven_turn`. `sven_machines` no longer re-exports `sven-turn` items (`prompts`, `stream_turn`, `ThinkingBudget`, compaction, ...); import them from `sven_turn`. The unused `sven_machines::{Session, TurnRecord}` are deleted.
- **BREAKING (sven-hsm, sven-executors)**: removed `Effect::CreateCheckpoint`, `Effect::RollbackToCheckpoint` (and their `EffectKind`s), `ToolCapability::Rollback`, `CheckpointExecutor` and `CompositeExecutorBuilder::{with_checkpoints, with_checkpoint_slot}`. No machine emitted either effect, and creating a checkpoint ran `git stash push --include-untracked`, which takes the user's uncommitted work out of the worktree. A match on `Effect`, `EffectKind` or `ToolCapability` drops those arms; a policy that listed `Rollback` drops it.
- **BREAKING (workspace)**: removed the `sven-tools` re-export crate. Import the `Tool` trait and its vocabulary (`Tool`, `ToolCall`, `ToolOutput`, `ApprovalPolicy`, `PermissionRequester`, `events`, `tool_summary`) from `sven-tool-api`, and `ToolRegistry`, `ToolSchema`, `SharedTools`, `SharedToolDisplays` and `ToolPolicy` from `sven-tool-registry`, which also re-exports the common `sven-tool-api` items.
- **BREAKING (workspace, CLI, docs, site)**: removed the P2P node and the managed-agents control plane. Deleted the `sven-p2p`/`sven-node*`/`sven-cloud*`/`sven-companion`/`sven-metering`/`sven-wire`/`sven-tools-p2p` crates, the `sven node`/`sven connect`/`sven peer`/`sven cloud`/`sven share` subcommands, the `--node-url` proxy modes of `sven acp serve`/`sven mcp serve`, the operator console and its `/tenant` and `/share` slash commands, the `docs/08-node.md`/`docs/09-collaboration.md`/`docs/technical/node.md`/`docs/technical/session-room-protocol.md` guides, and the P2P marketing section on the site. Everything sven does locally is unaffected: the TUI, the headless CI runner, workflows, teams and tasks, skills, the knowledge base, GDB debugging, MCP (server and client), ACP, the scheduler, channels, email, calendar, voice, and semantic memory all stay.

### Added
- **sven-sdk**: `EngineBuilder::project_root(path)` makes a directory the agents' world. The built-in file tools (`read_file`, `write_file`, `edit_file`, `find_file`, `grep`, `attach_file`) resolve relative paths against it and refuse any path that resolves outside it, symlinks included; `shell` and `task` start there and refuse a `workdir` outside it; the audit log is `<root>/.sven/audit.jsonl`; skills, personas, knowledge and the project context file are discovered from it. A shell command can still reach outside the root: it confines the file tools and where commands start, not a command. `Engine::project_root` reads it back; `build` fails with `CallError::Precondition` for a root that is not an existing directory. Without a root nothing changes.
- **sven-tool-api**: `PathScope` (and `PathScopeError`), the one path-confinement rule: `..` resolved lexically, symlinks followed through the longest existing prefix, a dangling symlink refused.
- **samples/agent/task**: an application on `sven-sdk` alone that drafts and publishes release notes with three tools of its own, bounded runs, a parked question answered after a suspend and resume, an approval the application decides by checking the draft, an independent check and an ATIF trajectory. `--demo` runs on a scripted model.
- **sven-sdk**: `model::ToolContentPart` is exported, so a multi-part tool result can be read through the facade.
- **sven-sdk**: `Agent::trajectory()` exports the agent's work as an ATIF document (`sven_sdk::atif`), built from the conversation the kernel holds and recording the model and the tool definitions it was offered. `AgentState` records both (older states read back with none).
- **`xtask arch`**: `[[forbidden]]` rules in `architecture.toml`, checked transitively over normal dependencies (`error[ARCH-010]` names the offending path). `sven-machines` may not reach the model, tool, turn, executor, kernel, config or workspace crates, and `sven-hsm` may not reach the model, tool or config crates.
- **`formal/`, `make formal`**: a machine-checked TLA+ model suite for the HSM kernel, modelled on whale's. `Hsm.tla` states what `crates/hsm/src/dispatch.rs` promises for *every* state and *every* event rather than for the transitions a test author thought of: that between dispatches the set of states the machine is inside is exactly the active leaf and its ancestors, each entered once and exited once; that a transition between two states under a shared ancestor never exits and re-enters it; that every state/event pair ends in a transition, an internal transition or an explicit refusal; and that a dispatch returns. `Submachine.tla` checks that a child hands control back exactly once, and `EffectDelivery.tla` checks what `sven-kernel`'s `run_effects` does with an effect the policy refuses. Ten of the sixteen configurations are *expected to fail* and the runner checks that they do, on a named property: seven pin down a design the engine rejected or a behaviour it had until this change (exiting from the handling state instead of the active leaf, taking the source's parent as the LCA, treating a self-transition as local, the unguarded drill, the unrestricted restore, and the two consequences of not checking a newly installed child), two are tracked gaps the code does not close today (`NoOrphanedChild`, `NoSilentStall`), and one prices a deliberate trade-off (`InnocentEffectSurvivesARefusal`). A suite in which everything passes cannot show it is checking anything. `formal/README.md` says what each model establishes and what it assumes; the `tla2tools.jar` it runs on is pinned and SHA-256-verified by `formal/fetch-tools.sh` and never committed. Not part of `make check` (it needs a JRE and a download); `make formal` runs it in about a minute.
- **`AGENTS.md`, `docs/technical/crate-architecture.md`**: rewritten against the final post-refactor graph (Phase 7.6 of the refactor plan). Both had drifted badly across the refactor's earlier phases — `AGENTS.md`'s crate table and "Making cross-cutting changes" checklist still named pre-split crates (`sven-tools`'s now-deleted `builtin/` directory, the monolithic `sven-model`) and described a `UiEvent`→`AgentEvent` "adapter" that Phase 3 deleted (both are now plain aliases for the same `sven_vocab::SessionEvent`); `crate-architecture.md` was far worse — a hand-drawn 5-layer ASCII diagram and full per-crate writeup describing `sven-core`/`sven-runtime`/`sven-graph`/`sven-input`/a Slint GUI, all renamed or deleted, and actively recommending the dead graph DSL ("prefer authoring a new `Graph` via `GraphBuilder`... over writing a new hardcoded Rust machine") as the preferred way to add a machine — guidance that would send a future agent at a subsystem deleted in Phase 1.3 for being dead *and* broken. `crate-architecture.md` is rewritten to **not** duplicate the per-crate inventory a second time — it now points to `AGENTS.md`'s crate table (one line per crate, organized by the current 10-tier system) and `architecture.toml` (mechanically authoritative, checked by `xtask arch` on every `make check`) instead, keeping only the narrative content neither of those can carry (why tiers, testing strategy per tier, rules for adding new code). This consolidation is itself the fix for how the old version went stale: it was hand-maintained in a third place nothing ever checked against the real graph.
- **`sven`**: `sven migrate-sessions [--dry-run]` (Phase 7.2 of the refactor plan) — one-shot bulk conversion of every legacy `.yaml` chat under `chat_document::chat_dir()` to the ATIF `.json` trajectory format, via a new `sven_session_store::migrate_legacy_chats`. Idempotent (never overwrites a session that already has a `.json` file) and non-destructive (original `.yaml` files are never touched). **Plan correction**: the plan's Phase 7.1/7.2 premise — "port `tui/app/session_manager.rs` to `trace_session`" and "delete `chat_document.rs` + `conversation.rs`" — is stale. Both migrations already happened in earlier phases of this refactor: `chat_document.rs`'s own module doc already states "nothing in sven ever writes a `.yaml` file anymore"; its one remaining reference in `session_manager.rs` is a test pinning the legacy-import path (`from_trajectory_into_legacy_import_produces_working_entry`), and `sven-ci`'s `conversation.rs`/`ConversationRunner` is deliberately kept as the `--resume` path for legacy markdown conversation files (`src/run/ci.rs`, explicitly commented `// Legacy: resume via ConversationRunner`). Deleting either would be a **regression** — it would break resuming/opening any session saved before the ATIF migration — not a cleanup. Both stay as permanent, intentional legacy-read support; only the missing "bulk migrate" tool from 7.2 was real, and is what this entry adds.
- **`sven`**: split `trace_session.rs`'s 1281-line (of 2609) `#[cfg(test)] mod tests` into its own file, `trace_session_tests.rs`, via `#[path]` — shrinking the "real" file from 2444 (its `[[allow.large_file]]` high-water mark) to 1331 lines. Prompted by needing to add the `migrate_legacy_chats` tests above without growing an already-oversized allowlisted file further; `xtask arch --bless` regenerated the allowlist (dropping the old 2444-line entry, adding a fresh 1287-line one for the new test file, still over 800 but a new file with no history to ratchet against).
- **`sven-frontend`**: removed 4 confirmed-dead parameters from `kernel_session_task`/`run_kernel_session_task` (Phase 7.3 of the refactor plan, which cited "14 positional args, 5 unused" — found 4 here, `#[allow(clippy::too_many_arguments)]` still present on both since even trimmed they're at 10/12) — `_shared_tools`, `_shared_tool_displays`, `_buffer_store`, `_mcp_refresh_rx` were already underscore-prefixed (never read in the function body) and are now gone from the signature entirely, along with the matching dead local-variable computations at both call sites in `sven-tui` (`app/run.rs`, `app/session_lifecycle.rs`) and the crate's own test helper. Mechanical and low-risk (grep-verified dead, not a redesign); a full options-struct/builder for the remaining ~10-12 args was considered and not done — this function has exactly 2 call sites, both internal to `sven-tui`, which doesn't clear the bar for a builder abstraction over a plain positional list. Surfaced `sven-tools-fs` as a newly-dead direct dependency of `sven-frontend` (its only use was the now-removed `OutputBufferStore` parameter) — removed.
- **`sven-mcp`/`sven-acp`**: standalone `sven-mcp`/`sven-acp` binaries (Phase 6.2 of the refactor plan) — the same MCP server and ACP agent as `sven mcp`/`sven acp`, buildable and runnable without linking the rest of the monolithic `sven` binary's closure (no TUI, no unrelated protocol servers). Each crate gained a `cli` module carrying its own clap grammar + handler, shared verbatim between its standalone `[[bin]]` and the monolithic `sven` binary's `Commands::{Mcp,Acp}` dispatch (which now just re-exports and calls into it) — avoiding the alternative of two copies of CLI code drifting apart. **Verification**: `cargo build --workspace --all-targets` / `xtask arch` (incl. `--profile minimal`) / `make check` / `make test` / `make tests/e2e/basic` (353/353) all green; `sven --help`/`sven {mcp,acp} --help`/`sven completions bash` diffed identical against pre-split; each new binary's `--help` smoke-tested directly.
- **`xtask`**: implemented the `--profile <name>` forbidden-crate check (Phase 6.3 of the refactor plan; the flag itself and a "not yet implemented" stub had existed since Phase 0.1). `cargo run -p xtask -- arch --profile minimal` shells out to `cargo tree -p sven --no-default-features --features minimal -e normal` and fails with `error[ARCH-007]` if the resolved closure contains any of `slint`/`ratatui`/`git2`/`openssl-sys`/`webauthn-rs`/`portable-pty`/`gdbmi`/`axum`/`teloxide`/`rusqlite`/`nvim-rs`. Wired into `make check/arch` (and therefore `make check`) unconditionally — the check is a `cargo tree` invocation with no compilation, ~1s, so there's no reason to defer it to a separate slow job the way the actual cross-target build below is. Added a `minimal-profile` CI job (`.github/workflows/ci.yml`) that both runs this check and does a real `cross build -p sven --no-default-features --features minimal --target aarch64-unknown-linux-musl` — proving the profile cross-compiles for a real constrained target, not just resolves on the host. **Not verified locally**: this sandbox's rustup install is root-owned, so `rustup target add aarch64-unknown-linux-musl` fails for the non-root build user; the job's structure mirrors the existing (presumably CI-verified) `build-linux-aarch64` release job's `cross`/target pattern, but the actual cross-compile has only been confirmed to type-check via `cargo tree`, not to build.
- **CI**: removed `libfontconfig-dev` from every `apt-get install` in `.github/workflows/{ci,release}.yml` (5 steps in `ci.yml`, 2 in `release.yml`, 3 of them otherwise-empty "Install Linux build dependencies" steps deleted outright). Dead since Phase 1.2 deleted `crates/gui` (Slint) — `Cross.toml`'s comment already noted the same removal for its own `pre-build` step, but the two GitHub Actions workflow files were missed. Confirmed via `cargo tree -e normal | grep -i fontconfig` (no output) before removing.
- **`sven` binary**: `[features]` on the root crate (Phase 6.1 of the refactor plan) — `tui` (`sven-tui` + `ratatui`), `network` (`sven-acp`/`sven-mcp`/`sven-team`), `gdb` (forwards to `sven-bootstrap/gdb`), and `minimal` (none of the above — headless CI/tool/index/map/tee/reduce only). `default = ["tui", "network", "gdb"]`, so the default build's behavior, `--help` output, and dependency closure are unchanged — verified via `cargo build --workspace --all-targets` / `xtask arch` / `make check` / `make test` / `make tests/e2e/basic` (353/353) all green with default features. The gated `Commands` enum variants and their `run::*`/`cli::*` handler modules disappear together under `#[cfg(feature = "network")]` so the dispatch `match` stays exhaustive; the bare-`sven`-with-a-TTY default action falls back to a clear `anyhow::bail!` explaining which feature to rebuild with, rather than failing to compile or panicking. `cargo tree -p sven --no-default-features --features minimal -e normal` confirms `ratatui`/`git2`/`openssl-sys`/`webauthn-rs`/`portable-pty`/`gdbmi`/`axum`/`rusqlite`/`nvim-rs`/`slint` are all absent from the resolved closure. Getting `gdbmi` excluded required `sven-bootstrap`'s dependency edges (from both this crate and `sven-ci`, its only two consumers reachable in a minimal build) to set `default-features = false` and have the root crate's own `gdb` feature forward `sven-bootstrap/gdb` explicitly — Cargo's per-target feature unification means a single un-disabled edge anywhere in the graph re-enables a dependency's default features workspace-wide for that build target, silently defeating the exclusion.
- **`sven-bootstrap`**: new `gdb` cargo feature (default-on, Phase 6.1 of the refactor plan), gating the `sven-tools-gdb` dependency and both `GdbTool` registration sites in `registry.rs`. Previously the GDB/MI tool suite (and its `gdbmi` dependency) was linked into every build on unix targets unconditionally (`#[cfg(unix)]` only); it is now `#[cfg(all(unix, feature = "gdb"))]`, so a build that disables the feature (`--no-default-features`) drops `gdbmi` and its closure entirely, verified via `cargo check -p sven-bootstrap --all-targets --no-default-features`.

### Fixed
- **sven-executors**: the verifier's path jail follows symlinks, so a `FileExists`, `FileHash` or `JsonPredicate` spec can no longer read a file outside its root through a link inside it. A relative path whose `..` stays inside the root is now accepted.
- **sven-tools-web**: `grep` passes its pattern and path to `rg`/`grep` after `--`, so a model-supplied value shaped like an option (`--pre=<cmd>` runs a command) is searched for rather than obeyed.
- **`sven-turn`**: the system prompt sent the model to a `load_skill` tool that does not exist; it now names the `skill` tool and its `load` action, and no longer leaks source indentation into the paragraph. The skill docs said the same and described a per-agent knowledge hint that was never implemented; both are corrected.
- **`sven-executors`, `sven-machines`, `sven-vocab`**: a tool call refused before it ran - a human rejected it, the permission gate forbade it, tools were disabled, no executor could take it - got a `ToolFailed` but no result in the conversation thread, so the next request carried an assistant tool call with no answer, which hosted providers reject. The turn executor now answers every such call in the thread before calling the model (`ThreadStore::answer_open_calls`), with the reason the machine recorded (`TurnRequest::refused_calls`) or a generic one.
- **`sven-machines`, `sven-kernel`**: an approved tool call never ran. `loop_core` granted the capability and started a new model turn instead of issuing the held call, leaving the call in history with no result; it now issues the call exactly as proposed. The kernel also served a quiescence capture while an approval request was waiting for its answer, so an SDK run ended before a host that answers asynchronously could reply; a pending approval now counts as in-flight work like a running tool call. An approval request no executor could put to anyone is answered `HumanRejected` rather than `EffectFailed`.
- **BREAKING (CLI)**: `--load-trace PATH` with no file at `PATH` fails instead of silently starting a fresh session, which ran the task without the history it was meant to continue. `--trace PATH` still starts a new session on a fresh path. **Migration**: use `--trace` where the file is meant to be created on the first run.
- **`sven-bootstrap`, `sven-config`**: a sub-agent spawned by the `task` tool, and the research sub-agent, lost a named `providers:` entry's endpoint, key and mock script and fell back to the driver's defaults; under the mock provider it was handed a model name it could not resolve. Sub-agents are now given the provider alias the active model came from (`Config::model_reference`), so they resolve the same endpoint and key as their parent.
- **BREAKING (CLI)**: `--load-trace PATH` with no file at `PATH` fails instead of silently starting a fresh session, which ran the task without the history it was meant to continue. `--trace PATH` still starts a new session on a fresh path. **Migration**: use `--trace` where the file is meant to be created on the first run.
- **`sven-bootstrap`, `sven-config`**: a sub-agent spawned by the `task` tool, and the research sub-agent, lost a named `providers:` entry's endpoint, key and mock script and fell back to the driver's defaults; under the mock provider it was handed a model name it could not resolve. Sub-agents are now given the provider alias the active model came from (`Config::model_reference`), so they resolve the same endpoint and key as their parent.
- **BREAKING (CLI)**: a headless run with a positional PROMPT no longer reads stdin unless `--stdin` is given. It read any non-terminal stdin to end, so under a CI runner, service manager or tool harness that hands the child an inherited pipe nobody writes to or closes, a run that already had its task waited forever. Whether stdin is read is now decided from the arguments alone (`Cli::reads_stdin`): always without a PROMPT (stdin is the task), and alongside one only with `--stdin`, which appends it to the prompt as before. Migrate `cmd | sven "fix these errors"` to `cmd | sven --stdin "fix these errors"` and `sven 'task1' | sven 'task2'` to `sven 'task1' | sven --stdin 'task2'`; `cmd | sven`, `sven map` and `sven reduce` are unchanged.
- **`sven-hsm`, `sven-kernel`, `sven-executors`, `sven-machines`**: an effect that was not carried out left the machine waiting forever. A `CompositeExecutor` with no slot for an effect (an unrecognised `CallLlm` kind, no turn/tool/user/timer/verify executor, `InstantiateSubmachine` without a spawner) logged and dropped it, and a non-tool batch the permission gate refused went to the audit trail but never back to the machine. Both now answer with `sven_kernel::failure_event`: `LlmFailed` for `CallLlm`, `ToolFailed` for `CallTool`, and the new `Event::EffectFailed { kind, error }` for anything else a machine may wait on. Every machine leaves a waiting state on `EffectFailed` as it does on `LlmFailed`; the verified-task machine ends ungraded when its verifier cannot run. `InstantiateSubmachine` now passes the permission gate with the rest of its dispatch instead of being spawned before validation. `formal/tla/EffectDelivery.tla`'s `NoSilentStall` holds for the shipped configuration; `EffectDeliveryUnanswered.cfg` is the rejected alternative. **API note**: `Event` and `EventKind` gain `EffectFailed`; a downstream exhaustive `match` needs an arm.
- **`sven-hsm`**: `drill_into_composites` had no bound. It re-dispatches `Init` to whatever state it just entered and follows the answer, and nothing in `Machine`'s signature constrains an `Init` target to a descendant - so a machine whose initial transitions name each other spun in that loop forever, inside the runtime's single consumer task, and the agent stopped answering with no error, no log line and no way back. The drill now stops at a state it would enter twice in one drill (and `debug_assert!`s, because a cyclic `Init` is a contract violation): a well-formed machine descends strictly and never revisits, so no legal behaviour changes. Found by `formal/tla/Hsm.tla`, which checks `DispatchTerminates`; `HsmUnguardedDrill.cfg` is the loop as it was.
- **`sven-hsm`**: a snapshot could be resumed into a *composite* state. `Machine::all_states()` enumerates composites because coverage tooling needs them, and `restore`/`restore_in_place` accepted anything it enumerated - so a session could resume into `Session` rather than into the `Idle` or `Generating` beneath it. A running machine never rests there (every dispatch drills past a composite into a substate), so the substate's entry action had not run and every event its substates handle was ignored: an agent sitting in a superstate answering nothing, with a healthy looking state label. Both entry points now refuse with the new `RestoreError::CompositeState`, which names the substates that made it composite, and `ErasedMachine::all_state_labels()` reports only the labels a snapshot can actually name - matching what its doc always claimed. Whether a state is composite is read off the hierarchy the machine already declares, so no machine had to change. **API note**: `RestoreError` gains a variant; a downstream exhaustive `match` on it needs an arm.
- **`sven-hsm`**: `Submachine::instantiate_child` did not notice a child that its own initial transition had already finished. The host looked for completion in exactly one place - after routing an event - which is every way a child can become terminal except the first, so a child built for work that turned out to be done was installed as if it were live, the parent was never told, and the next event (if one ever came) was dispatched into a machine that had already ended. `instantiate_child` now announces it at install time and never installs it. Found by `formal/tla/Submachine.tla`.

### Plan corrections
- **`sven-commands`**: new crate (refactor plan Phase 5.21) carrying the `SlashCommand` trait, `CommandRegistry`/`CommandContext`/`CommandResult`/`ImmediateAction`, the fuzzy-completion engine, the `/command` and `/agent-name` dynamic-command factories (from skill/subagent discovery), MCP-prompt slash commands, and the builtin `/…` commands (`abort`, `clear`, `model`, `mode`, `new`, `provider`, `quit`, `refresh`, `approve`/`reject`/`agents`/`tasks`/`architect`, `skills`/`subagents`/`peers`/`context`/`tools`/`mcp`, `think-limit`) — pulled out of `sven-frontend::commands` (2,838 LOC across 20 files). **Plan correction**: the plan placed `sven-commands` at the `wiring` tier alongside `sven-frontend`; the real dependency floor is `sven-machines` (the `/think-limit` command calls `sven_machines::{ThinkingBudget, thinking_budget_override, set_thinking_budget_override}`), which sits one tier below `wiring`, and the crate needs nothing from `sven-frontend` itself. Assigning `assembly` — the tightest tier that legally holds `sven_config`/`sven_workspace`/`sven_mcp_client`/`sven_model`/`sven_machines` — makes `sven-frontend -> sven-commands` a normal downward edge instead of requiring a same-tier `[[same_layer]]` exception like `sven-tools -> sven-tool-registry`. `sven-frontend` depends on `sven-commands` and re-exports it verbatim at its historical module path (`pub use sven_commands as commands;` in `crates/frontend/src/lib.rs`), so `sven-tui`'s ~10 `sven_frontend::commands::*` call sites and doc-links needed zero changes.
- **`sven` binary (`src/`)**: split `src/main.rs` (2,866 lines) and `src/cli.rs` (1,589 lines) into a maintainable module tree, refactor plan Phase 5.22 — pure mechanical move, **zero behaviour change** (verified below). `src/cli.rs` becomes `src/cli/mod.rs` (the top-level `Cli` struct, `Commands` dispatch enum, `OutputFormatArg`, `print_completions`) plus one submodule per subcommand group's own nested `*Commands` enum. `src/main.rs` shrinks to just `fn main()`'s argument-parsing/dispatch (`mod cli; mod run;` + ~230 lines) and gains `src/run/`, one module per subcommand-group *handler* mirroring `cli`'s grouping; `run::team` also carries the `--team-name` teammate-polling-loop handler (`run_as_teammate`) since it is reached from `main()`, not from `cli::TeamCommands`, but is otherwise a `sven team` concern. All moved functions/structs are `pub(crate)`; the previously single-file cross-references become ordinary `use crate::run::{tui,chats,logging}::...` imports. No file in the new tree exceeds 293 lines (`run::ci`), all comfortably under the 800-line ratchet — `cargo run -p xtask -- arch --bless` drops the two now-stale `[[allow.large_file]]` entries for `src/main.rs`/`src/cli.rs`. **Verification**: beyond the standard `cargo build --workspace --all-targets` / `xtask arch` / `cargo clippy --bin sven --all-targets -D warnings` / `cargo test --bin sven` gate, a stash-based before/after diff of `sven --help`, `sven team --help`, and `sven completions bash` (the most exhaustive single artifact clap can produce from the grammar) against the pre-split binary came back **byte-for-byte identical** on every one.
- **`sven-tools-web`**: new domain-tier crate (5.5 of the refactor plan) carrying information-gathering tools -- internet fetch/search (`web_fetch`, `web_search`) and read-only codebase diagnostics that shell out to an external binary (`grep`, `search_codebase`, `read_lints`) -- split out of `sven-tools`'s `builtin/web/` + `builtin/search/{grep,search_codebase}.rs` + `builtin/system/read_lints.rs`. Verified before committing to the plan's literal "web+search -> web" directory grouping: none of the five files has a code-level cross-ref to any other `builtin/` subdirectory or to each other beyond the kernel-tier `Tool` vocabulary, so unlike the `context+knowledge` and `system` splits (see `sven-tools-ctx`/`sven-tools-agent`, above), this grouping needed no correction -- it really is the plan's original conceptual pairing, not a forced coupling. `search_knowledge.rs`, which did live in `builtin/search/` alongside `grep.rs`/`search_codebase.rs`, had already decoupled from that directory into `sven-tools-ctx` (real `SharedKnowledge` coupling). `read_lints.rs` joins this crate rather than `sven-tools-agent` on shape, not coupling: a read-only, subprocess-backed diagnostic tool with no state and no self-modification of the agent, same as `grep`/`search_codebase`.
- With `sven-tools-web` landed, `crates/tools/src/builtin/` is now fully empty and deleted -- all ~18k LOC of concrete tool implementations the refactor plan targeted have moved to `sven-tools-{fs,exec,web,ctx,agent,gdb}`. `read_image.rs`, the one file left once the other six were carved out, moved to `sven-tools-fs` (it reads a file and produces tool output about its content, the same shape as `ReadFileTool`, and already depended on `sven-image`, which `read_file.rs`'s inline image detection also uses). `sven-tools` itself is reduced to a genuinely thin Phase 5.1 re-export shim over `sven-tool-api`/`sven-tool-registry` (its `Cargo.toml` dropped every dependency `builtin/` needed, down to just `sven-tool-api`/`sven-tool-registry`/`sven-hsm`). **Plan correction**: the `sven-tools -> sven-tool-registry` same-tier `architecture.toml` exception is *not* deleted by finishing the `builtin/` split, contrary to the plan's stated resolution condition -- `sven-tools` depends on `sven-tool-registry` (services tier) for its `ToolRegistry` re-export regardless of how much code `builtin/` holds, which is exactly the same-tier edge the exception exists for. Deleting it requires deleting `sven-tools` outright and repointing every one of its ~20 remaining consumers directly at `sven-tool-api`/`sven-tool-registry`; that consumer-repointing sweep is out of scope for the `builtin/` split and is the tracked follow-up. The exception's `why` text is updated in place to record this rather than deleted.
- **`sven-tools-agent`**: new domain-tier crate (5.6 of the refactor plan) carrying agent self-management tools -- the compound `system` tool (mode/model switching, MCP server add/remove), `todo` (session task planning), `ask_question` (mid-turn human-in-the-loop questions), and `skill` (load/list the agent's own available skills) -- split out of `sven-tools`'s `builtin/system/{system,todo,ask_question,skill}.rs`. The plan's original terse listing named only `system.rs` for this tier and left `skill.rs`/`memory.rs`/`todo.rs`/`ask_question.rs`/`read_lints.rs` as an open judgment call; resolved per-file: `memory.rs` had a real dependency on the knowledge tools that forced it into `sven-tools-ctx` instead (see that crate's Added entry, above); `read_lints.rs` has no cross-refs to anything and stays behind in `sven-tools` pending the `sven-tools-web` split, where it will bundle with `grep`/`search_codebase`; `todo.rs`/`ask_question.rs` have zero cross-refs and group naturally with `system.rs`'s self-modification; `skill.rs` was the closest call (it loads reference content, which reads like the context/knowledge tools) but what it loads is the agent's own operating instructions, not external project reference material -- kept in the "agent introspects/configures itself" category on that distinction, with no code-level coupling forcing either placement. `Question`/`QuestionRequest` (defined in `ask_question.rs`) are used across ~10 consumer crates (`sven-tui`, `sven-acp`, `sven-bootstrap`, `sven-frontend`) as the tool↔UI question-relay vocabulary; all repointed at the new crate.
- **`sven-tools-ctx`**: new domain-tier crate (5.4 of the refactor plan) carrying the memory-mapped RLM context store (`context_open`/`context_read`/`context_grep`), project knowledge (`list_knowledge`/`search_knowledge`), and the compound `memory` tool, split out of `sven-tools`'s `builtin/context/` + `builtin/knowledge/`. **Plan correction**: the plan's literal "context+knowledge -> ctx" directory grouping needed two real fixes on inspection — `search_knowledge` physically lived in `builtin/search/` (bundled with the unrelated `grep`/`search_codebase` codebase-search tools), but it and `list_knowledge` are the only two consumers of `sven_workspace::SharedKnowledge`, so it moved here instead of following its directory into the future `sven-tools-web`; and `builtin/system/memory.rs` — which the prior agent's notes flagged as a candidate for the "agent" grouping — turned out to directly construct and delegate to both `ListKnowledgeTool` and `SearchKnowledgeTool` (`MemoryTool::new` builds both internally), a real dependency forcing it into this crate too. `builtin/context/` itself has zero code-level cross-refs to the knowledge/memory tools; the two subtrees share a crate on the plan's original conceptual grouping ("reference material loaded into the agent's context"), not a forced coupling. `GrepMatch`, shared with `sven-tools-fs`'s `buffer/store.rs`, moved to `sven-tool-api` (kernel tier) in the prior split specifically so this crate wouldn't need to depend on `sven-tools-fs` for it. Two integration test files (`context_integration.rs`, `context_pipeline_test.rs`) had fixture constants hardcoding the old `crates/tools/src/builtin/context` path to read known-content real source files at test time; repointed at the new location (the directory still holds the same 6 `.rs` files, so no assertion values needed to change).
- **`sven-tools-gdb`**: new domain-tier crate carrying the GDB/MI debugging tools (`gdb_connect`, `gdb_start_server`, `gdb_command`, `gdb_interrupt`, `gdb_stop`, `gdb_status`, `gdb_wait_stopped`, the compound `GdbTool`, and server-binary/ELF discovery) split out of `sven-tools`'s `builtin/gdb/` (5.7 of the refactor plan's god-crate splits — done first, as the plan intended, since it was verified to be the cleanest candidate: zero cross-refs into any other `builtin/` subdirectory). Unix only (GDB signal APIs are unavailable on Windows); depends only on `sven-tool-api` (kernel) plus the foundation crates `sven-config`/`sven-hsm`. `sven-bootstrap` now depends on it directly instead of going through `sven-tools`.
- **`sven-tools-fs`**: new domain-tier crate (5.2 of the refactor plan) carrying direct file I/O (`read_file`/`write_file`/`edit_file`/`delete_file`/`find_file`) and the streaming subprocess-output buffer tools (`buf_read`/`buf_grep`/`buf_status`, `OutputBufferStore`) split out of `sven-tools`'s `builtin/file/` + `builtin/buffer/`. The two subtrees were verified to have zero cross-refs into each other or into any other `builtin/` subdirectory before merging them into one crate; `buffer/mod.rs`'s doc comment claiming buffers are "created by the task and (future) shell tools" turned out to be aspirational, not a real dependency. `GrepMatch` — a plain data struct shared by `buffer/store.rs` and (still-unmigrated) `context/store.rs` — moved to `sven-tool-api` (kernel tier) ahead of this split so both future `fs`/`ctx` domain crates can depend on one shared home instead of either owning the other.
- **`sven-tools-exec`**: new domain-tier crate (5.3 of the refactor plan) carrying the free-form `shell` tool and `run_terminal_command`, split out of `sven-tools`'s `builtin/shell/` + `builtin/terminal/`. Confirmed real coupling (`terminal` imports `shell::head_tail_truncate`), so the two subtrees move together as one crate rather than splitting further, exactly as the plan's grouping predicted.
- **`sven-tui-nvim`**: new foundation-tier crate (refactor plan Phase 5, TUI split) carrying the embedded Neovim client pulled out of `sven-tui`'s `src/nvim/` (`NvimBridge`, the redraw-grid model, the nvim-rpc notification handler, and the grid-to-ratatui-`Line` renderer — 2711 LOC, unchanged from the plan's original audit). **Plan correction**: the plan filed this crate at "surface" tier (grouped with `sven-tui`, its only consumer); the real dependency graph puts it at "foundation" — it has zero `sven-*` dependencies (only `nvim-rs`, `rmpv`, `ratatui`, `tokio`, `anyhow`, `tracing`, `async-trait`), confirmed by `xtask arch --bless` independently recomputing the same tier. `sven-tui` now depends on it like any other foundation crate; the nvim-rs/rmpv dependency closure this extraction was meant to isolate is unchanged in this commit (`sven-tui` still depends on `sven-tui-nvim` unconditionally) — the plan's suggested cargo feature flag is left for a follow-up, since wiring `#[cfg(feature = "nvim")]` through `app/mod.rs`, `term_events.rs`, `dispatch_chat.rs`, and `chat_ops.rs`'s `nvim.bridge` checks is a second, separable change or the crate boundary that makes it possible is now in place.

### Changed
- **`sven-tui`**: split the two oversized files the plan flagged (`app/dispatch.rs` 1787 LOC, `app/mod.rs` 1750 LOC — both grown slightly from the plan's original audit, confirmed by direct measurement) into focused modules under `crates/tui/src/app/`, each handling one concern: `construct.rs` (`App::new` + ATIF-record-to-segment conversion), `render.rs` (the `view()` draw function + chat-list/peers scroll-offset helpers), `run.rs` (the top-level async event loop + MCP event forwarding), `session_lifecycle.rs` (new/switch-session, the active-session snapshot, per-session agent spawn), `test_support.rs` (`#[cfg(test)]`-only `App` constructors used by `submit.rs`'s test suite), `dispatch_input.rs` (raw input-buffer editing keybindings + the character/word-boundary helpers + the edit-buffer redirect they share with `dispatch()`), and `dispatch_chat.rs` (chat-segment operations, scrolling incl. the embedded-Neovim bridge, search, and mouse/selection handling). `dispatch.rs` itself shrinks to the top-level `Action` router plus focus/navigation, the message queue, slash-command completion, submit, the team picker, and the chat-list sidebar; `mod.rs` shrinks to the `App`/`AppOptions` type definitions and module wiring. Net: `dispatch.rs` 1787 → 768 lines, `mod.rs` 1750 → 154 lines, `sven-tui` itself stays one crate (this was an in-crate file split, not a new crate). No behavior change — every `Action` variant's match arm body moved verbatim; `xtask arch --bless` regenerated the file-size ratchet from the new tree.

### Added
- **`sven-model-catalog`**: new kernel-tier crate carrying the static model catalog data pulled out of the `sven-model` god crate (refactor plan Phase 5) — `ModelCatalogEntry`, `static_catalog()`, `lookup()`/`lookup_by_model_name()`, and the on-disk live-cache overlay (`load_disk_cache`/`cache_update`/`is_cache_stale`). Zero heavy dependencies (serde/serde_json/serde_yaml/dirs only — no reqwest, no provider SDKs). `sven-model` re-exports it verbatim at `sven_model::catalog` (`pub use sven_model_catalog as catalog;`) so the ~20 existing `sven_model::catalog::*`/`sven_model::InputModality`/`sven_model::ModelCatalogEntry` call sites across the workspace needed no changes. `sven-tools` (whose only use of the old `sven-model` was a single `static_catalog()` call in its own tests) now depends on `sven-model-catalog` directly instead, making the `sven-tools -> sven-model` edge disappear entirely rather than just getting lighter.
- **`sven-model-mock`**: new services-tier crate carrying the `--model mock` test/dev `ModelProvider` implementations pulled out of `sven-model` — `MockProvider` (fixed echo) and `YamlMockProvider` (YAML-scripted responses, used by bats/e2e tests). `sven-model-drivers`'s `from_config()` factory depends on it as a normal (non-dev) dependency for the unconditional `provider: "mock"` match arm; every other consumer (16+ crates across executors/bootstrap/frontend) only needs it as a `[dev-dependencies]` entry, since every existing use site was inside `#[cfg(test)]` or an integration-test binary. The natural seam for Phase 6's "stop shipping mock in release builds" feature flag is exactly that one match arm in `sven-model-drivers::build_inner` plus this dependency edge.
- **`sven-model-drivers`**: new services-tier crate carrying the 34 provider driver implementations (`anthropic.rs`, `aws.rs`, `cohere.rs`, `google.rs`, `openai.rs`, `openai_compat/`) and the `from_config`/`from_config_probed`/`build_inner` factory pulled out of `sven-model` — this is where `reqwest`, SigV4 signing (`sha2`/`hex`/`chrono`), and every provider's own dependency closure now live, so `sven-model` itself no longer pays for any of it. Depends on `sven-model` (kernel) and `sven-model-mock` (services, same-tier exception — see `architecture.toml`). All ~13 call sites of `sven_model::from_config`/`from_config_probed` across the workspace (`sven-bootstrap`, `sven-frontend`, `sven-ci`, `src/main.rs`) now call `sven_model_drivers::from_config`/`from_config_probed` instead.

### Changed
- **BREAKING (`sven-model`)**: shrunk to the kernel-tier vocabulary the plan intended — the `ModelProvider` trait, request/response types (`CompletionRequest`, `ResponseEvent`, `Message`, ...), the pure prompt-size budget gate (`budget`), image-support sanitisation (`sanitize`), the static driver-metadata registry (`registry`), and the `ModelResolver`/`resolve_model_cfg`/`resolve_model_from_config` config-resolution logic — down from ~10,090 LOC (`lib.rs` alone was 1,721 lines) to 713 lines total, with zero heavy dependencies (no `reqwest`, `tokio`, `tracing`, `serde_yaml`, `regex`, `sha2`, `hex`, `chrono`, or `dirs` — down to `sven-config`, `sven-model-catalog`, `anyhow`, `serde`, `serde_json`, `async-trait`, `futures`). The concrete provider drivers moved to `sven-model-drivers`, the catalog data to `sven-model-catalog`, and the `--model mock` providers to `sven-model-mock` (see Added, above). **Plan correction**: the plan's one-line description had `ModelResolver` moving to `sven-model-drivers` alongside the concrete providers ("new: 34 providers, `openai_compat`, `ModelResolver`"); on inspection `ModelResolver`/`resolve_model_cfg`/`resolve_model_from_config` never construct a driver — they only produce a `sven_config::ModelConfig` (via `registry::get_driver` and `catalog::lookup`, both of which also stayed in the kernel crate) — so they have no `reqwest` dependency and correctly belong in `sven-model` with the rest of the pure vocabulary, not in the drivers crate. Every crate that only calls `resolve_model_from_config`/`resolve_model_cfg`/`ModelResolver` (`sven-tui`, `sven-control`, parts of `sven-ci`/`sven-bootstrap`/`sven-frontend`) needed no dependency or call-site changes at all as a result.

### Added
- **`trace`**: new crate (package name `trace`, deliberately without the `sven-` prefix) implementing the [ATIF v1.7](https://github.com/harbor-framework/harbor/blob/main/rfcs/0001-trajectory-format.md) agent-trajectory format end to end — full schema (multimodal content, subagent trajectory embedding/refs, context-management convention, RL-oriented token/logprob fields), spec validation, atomic whole-document JSON persistence with concurrent-modification detection, header-only fast reads for listing, and NDJSON step streaming.
- **sven-input**: `trace_session` module — ATIF-backed session storage (save/load/list) and bidirectional turn assembly between `sven_model::Message` streams and ATIF `TraceStep`s, replacing the previous YAML `ChatDocument`/JSONL `ConversationRecord` formats as the canonical persistence layer for headless runs, the TUI, and the GUI.

### Changed
- **BREAKING (sven-ci, CLI)**: `--jsonl`/`--load-jsonl`/`--output-jsonl` and `--chat`/`--load-chat`/`--output-chat` are replaced by a single `--trace`/`--load-trace`/`--output-trace` flag family backed by ATIF. `--output-format json` now emits the full ATIF trajectory document instead of the old bespoke JSON summary; `--output-format jsonl` streams ATIF `TraceStep` objects (one per line) instead of the old `ConversationRecord` shape. The auto-log path is now `.sven/logs/<timestamp>.atif.json` (a single JSON document) instead of `.sven/logs/<timestamp>.jsonl` (an append-only line stream).
- **BREAKING (sven-tui, sven-gui)**: interactive session storage moved from `~/.local/share/sven/chats/*.yaml` to ATIF trajectory documents under `~/.local/share/sven/sessions/*.json`. Pre-existing `.yaml` sessions are still discoverable and openable (read-only import); once reopened, subsequent saves write the new ATIF format.
- All crate directories dropped the `sven-` prefix (`crates/sven-hsm` → `crates/hsm`, etc.); Cargo package names are unchanged (`sven-hsm` still depends on `sven-hsm = { path = "../hsm" }`), so this does not affect downstream consumers of the published crates.

### Removed
- **sven-ci**: deleted the dead, never-wired `jsonl_export`/`write_jsonl_trace` fine-tuning-export module (zero callers outside its own tests).
- **Audit (sven-hsm/sven-executors)**: the kernel runtime now flushes the hash-chained audit log (`.sven/audit.jsonl`) after **every dispatch**, including the terminal one — machines no longer need to emit `PersistAudit` for records to reach disk.
- **Audit (sven-hsm)**: `ToolAuditRecord` now carries tenant/actor attribution (stamped via the new `Context::push_tool_audit`); `AuditTrailHandle` gained cursor-based `records_from`/`tool_records_from` accessors.

### Changed
- **BREAKING (sven-mcp 2.0.0, sven-acp 2.0.0)**: version bumps corrected from minor to major to reflect the behavior breaks already shipped in this cycle — `call_tool` deny-by-default for `Ask`-policy tools without a wired `PermissionRequester`, and TLS verification on by default in `serve_stdio_node_proxy`. Existing 1.x integrations that relied on unattended shell/write tools or self-signed nodes must opt in explicitly (`with_permission_requester`, `ConnectOptions::insecure_dev`/`extra_ca_pem`).
- **Audit (sven-executors)**: `AuditExecutor` now serializes concurrent writers through an advisory `<log>.lock` file and re-reads the chain tip on every flush, so concurrent sessions on one workspace can no longer fork/corrupt the chain; partial writes are truncated and retried (at-least-once) instead of permanently breaking verification; legacy pre-chain log files are rotated to `<log>.legacy-<timestamp>` so new records remain verifiable; entry hashing uses an explicitly key-sorted canonical JSON form independent of serde_json build features.

### Fixed
- **Audit docs (sven-executors)**: module documentation no longer overclaims tamper-evidence — the hash chain detects accidental corruption and non-adaptive tampering only; an actor with write access can recompute the whole chain (no HMAC/anchoring is implemented).

### Added
- **Workspace**: `xtask` architecture-ratchet checker (`cargo run -p xtask -- arch`, wired into `make check`) plus `architecture.toml`, enforcing crate-tier dependency legality, dead/unused internal dependency detection, and a file-size ratchet across all workspace crates. Method documented at `.claude/skills/programming/rust/architecture.md`.

### Changed
- **Workspace**: all crate `version` fields now inherit `version.workspace = true` instead of drifting independently (was `sven-hsm` 0.5.0 next to `sven-acp` 2.0.0 next to the workspace's own 1.10.2 — nothing is published separately, so per-crate versions carried no information).
- **sven-tools**: `SystemTool::new` takes an extra `Vec<ModelCatalogEntry>` parameter (the `switch_model` fuzzy-search catalog). `sven-tools` no longer depends on `sven-model` in production — that was its only call site (`static_catalog()`, used to fuzzy-match a `/model` query against provider/id/name). `sven-bootstrap` (which already depends on both) builds the catalog and passes it in; `sven-model` moves to `sven-tools`'s `[dev-dependencies]` for the tests that exercise real match scoring.

### Removed
- **Workspace**: removed 11 internal dependencies the architecture checker found with zero real use sites — 1 from a manual audit (`sven-frontend`→`sven-executors`) plus 10 more the checker itself surfaced on its first run: `sven`→`sven-core`; `sven-acp`→`sven-model`; `sven-channels`/`sven-integrations`/`sven-memory`/`sven-scheduler`→`sven-config`; `sven-llm`→`sven-hsm`; `sven-memory`→`sven-model`; `sven-ci`→`sven-frontend`.
- **BREAKING (CLI, sven-gui)**: deleted `crates/gui` (the Slint desktop GUI), the `sven --gui`/`-g` CLI flag, the `sven.desktop` application-menu launcher, and the `.agents/skills/slint-gui` skill. The TUI (`sven` with no flags, in a terminal) is the only interactive local surface; headless (`--headless`/`-H`) is unaffected. `libfontconfig-dev` is no longer required to cross-compile for aarch64 (`Cross.toml`).
- **sven-graph, sven-core**: deleted the graph-machine DSL (`crates/graph` + `core/src/machines/graph/`, 3116 LOC) that `AGENTS.md` had instructed contributors to prefer for new agent behavior. It was never adopted (`GraphMachine::` was constructed only in its own tests, no `.graph` assets ever existed, `ModeRegistry` never registered one) and had active correctness bugs (its template renderer corrupted non-ASCII text; its "final" event pattern matched any completion including timeouts; its timer cancellation cancelled the wrong timer). See `docs/adr/0001-delete-graph-dsl.md`. `core/src/machines/loop_core.rs` (already shared by `ReactiveAgentMachine` and `SdlcMachine`) remains the way to add a new machine.
- **sven-core**: deleted `AgentEventVisitor` (24 methods, `core/src/events.rs`) — a visitor trait written to solve the same "adding an event variant silently drops it in some consumer" problem the parallel `UiEvent`/`AgentEvent` enums have, with zero implementors anywhere in the workspace. Its all-default-no-op-methods shape reproduces the exact silent-drop failure mode it was meant to prevent, just less visibly than a wildcard match arm. `AgentEvent` and its consumers are unaffected; a real fix for the underlying problem is tracked for the event-model-unification phase of the current refactor.
- **sven-frontend**: deleted `frontend/src/control.rs`, a hand-maintained, stringly-typed mirror of the operator control protocol, kept only because (per its own doc comment) `sven-frontend` "cannot import the canonical types" — stale: `sven-control` was extracted as a standalone crate specifically so `sven-frontend` could depend on it directly. The mirror's `SessionInfo.state: String` had forced a third parallel enum (`SessionPhase`, now deleted) to re-parse the wire string that `sven_control::SessionState` already models directly. `sven_control::ControlEvent` gains a `#[serde(other)] Unknown` catch-all (the mirror's resilience property for a real network boundary, now available to every consumer, not just the frontend) so this loses no behavior.
- **Workspace**: merged 10 copies of the same auto-approve `tokio::select!` loop (CI's two headless runners had byte-identical same-named `auto_approve` functions; 8 further test/demo call sites each hand-rolled the same 15-line block inline) into `sven_bootstrap::KernelChannels::auto_approve()`. Every unattended-session call site now reads `tokio::spawn(bundle.channels.auto_approve())`.
- **sven-bootstrap, sven-ci**: fixed two `RuntimeContext` duplication bugs. `RuntimeContext::auto_detect()`'s skill/agent/knowledge/git/CI detection is now reachable at an already-resolved project root via the new `auto_detect_at`/`auto_detect_with`, replacing two hand-written `RuntimeContext { .. }` literals in `sven-ci` that had drifted from the real detection logic: `RuntimeRunner`'s copy hardcoded `knowledge_drift_note: None`, so `--output-trace`/one-shot runs never warned about stale knowledge docs while `CiRunner` runs did (both now share one code path and both warn). Separately, the TUI's `kernel_session_task` accepted `shared_skills`/`shared_agents` parameters and silently discarded them (`_`-prefixed) in favor of calling `RuntimeContext::auto_detect()` fresh — a second full skill/agent discovery pass on every TUI startup. It now reuses what the TUI already discovered via `RuntimeContext::auto_detect_with`. Regression tests added in `bootstrap::context::tests` for both.

### Added
- **sven-bootstrap**: `KernelChannels::auto_approve()` — the canonical unattended-session gate consumer (see above).

### Added
- **sven-chain**: new crate — the sha256 hash-chain primitives (`append_chain`/`read_chain`/`verify_chain`, `ChainedLine`, `ChainError`, `GENESIS_HASH`) extracted verbatim out of `sven-executors`' `audit.rs`, with zero dependencies on other sven crates.

### Changed
- **sven-executors**: `AuditExecutor` now builds on `sven_chain` instead of defining the chain format itself. `sven-executors` drops its now-unused `sha2`/`serde`/`thiserror` dependencies (they backed only the code that moved).
- **sven-vocab**: new crate — `ToolCall`, `ToolOutput`/`ToolOutputPart`, `ToolSchema`, and `OutputCategory` moved out of `sven-tools` into a new zero-dependency crate below it. `sven-tools` re-exports all four at its crate root unchanged, so every existing `sven_tools::ToolCall` (etc.) call site across the workspace needed no changes. `sven-control` — which needed only these pure data types, not the tool-execution/registry machinery — now depends on `sven-vocab` instead of `sven-tools`, deleting the `control → tools` upward-tier edge (the last `[[allow.upward]]` exception in `architecture.toml`; none remain).
- **sven-llm**: `ConversationStore` renamed to `ThreadStore` (`crates/llm/src/conversation.rs` and all 11 call sites) — a "one name, one concept" violation caught during the original audit, where a second, unrelated `ConversationStore` shared the name but not the concept.

### Changed
- **sven-vocab**: `AgentMode` (from `sven-config`), `CompactionStrategyUsed`/`PeerInfo` (from `sven-core`), `TodoItem`/`TodoStatus`/`SubagentUpdate` (from `sven-tools`), and `CollabEvent` (from `sven-core`) moved down into `sven-vocab`, alongside `ToolCall`/`ToolOutput`/`ToolSchema`. Every old location re-exports its type unchanged, so no downstream call sites needed to change. This is the prerequisite for Phase 3 of the refactor plan (unifying `AgentEvent`/`UiEvent` into one `SessionEvent`): that enum has to live in a foundation-tier crate below both `sven-core` and `sven-hsm`, so its payload types have to live there first. `sven-config` gains a `sven-vocab` dependency (a new same-tier `[[same_layer]]` exception in `architecture.toml`: both are leaf "foundation" crates with no dependents at that tier, so it can never become a cycle).
- **BREAKING (sven-core, sven-hsm)**: `sven_core::AgentEvent` and `sven_hsm::UiEvent` are now both re-exports of one type, `sven_vocab::SessionEvent`. These had been two independently hand-maintained enums bridged by a translator pair (`agent_event_to_ui` / `ui_event_to_agent_event`) that had already drifted — `ui_event_to_agent_event`'s catch-all silently dropped any event neither side had gotten around to mapping. `SessionEvent` keeps `AgentEvent`'s typed payloads (so consumers pattern-match on real types, e.g. `ToolCall`, `Vec<TodoItem>`, `AgentMode`, `SubagentUpdate`, instead of opaque `serde_json::Value`/`String`) plus `Transition`, the kernel's per-dispatch trace event that had no `AgentEvent` equivalent. Both translators are now the identity function `Some(ev)` — kept in place rather than deleted outright, per the migration plan's identity-transform pattern for a risky enum merge: land the type unification first with everything still compiling, verify green, delete the now-pointless indirection in a follow-up commit. Every direct consumer of the old shapes was migrated in this same commit: `sven-control`'s `ui_event_to_control`, `sven-executors`' turn/tool/remote-tool emitters, `sven-bootstrap`'s tool-event forwarder, `sven-ci`'s headless runners, and both ACP bridge functions.
- **sven-acp**: `ui_event_to_session_update` now delegates to `agent_event_to_session_update` instead of duplicating it against a separate, already-diverged `UiEvent` shape. The duplicate had a `ModeChanged` handler that pattern-matched on a lowercase-only string (`"research"`, `"plan"`, else `Agent`) — since `ModeChanged` now carries a typed `AgentMode` with no string round-trip at all, this also fixes the live bug where a kernel-driven Research or Plan session reported itself to the IDE as Agent mode.

### Removed
- **sven-executors, sven-bootstrap**: deleted `agent_event_to_ui`/`ui_event_to_agent_event`, the two `AgentEvent`↔`UiEvent` translators, now that both names are re-exports of the same `sven_vocab::SessionEvent` and the previous commit had already degenerated both to `Some(ev)`. Their three call sites (`sven-executors`' turn-stream forwarder, `sven-bootstrap`'s observation bridge, `sven-ci`'s `KernelAgent`) now forward the event directly. Deleted the three `kernel_bridge` tests that only exercised the translator round-trip (`maps_full_turn_ui_event_sequence`, `subagent_started_survives_kernel_round_trip`, `collab_event_survives_kernel_round_trip`) — with no translation happening, they had degenerated into asserting a value equals itself.

### Changed
- **sven-ci**: extracted `tool_output_snippet` (the verbose-only, length-capped ` output=…` snippet on `[sven:tool:result]` lines) into `crate::output` as the one shared implementation. `RuntimeRunner` (`runner/runtime_runner.rs`) had it as a named function; `CiRunner` (`runner/event.rs`) carried a byte-identical copy inlined into its `ToolCallFinished` arm. Both now call the shared version.
- **sven-ci**: `kernel_agent.rs`'s `reduce_history` — which decides which `AgentEvent`s get replayed into a rebuilt session's seed history — named every `SessionEvent` variant explicitly instead of a trailing `_ => {}`, so a future variant that plausibly belongs in history forces a decision here instead of silently landing in the catch-all. Same fix for `kernel_mode`/`mode_to_kernel_mode` (`sven-ci`, `sven-frontend`): the three coding-family `AgentMode` variants that fall back to the `"agent"` kernel machine are now named rather than `_ => "agent"`, so a new mode forces the same explicit "which machine does this run on" decision. (Explored applying `#[deny(clippy::wildcard_enum_match_arm)]` crate-wide per the refactor plan's Phase 3.6, but it fires on every enum in these crates, not just the session-event ones — `ChatSegment`, `MessageContent`, `ControlCommand`, third-party protocol enums, etc. — a ~30-site audit disproportionate to the actual risk; deferred.)
- **BREAKING (sven-control)**: `ControlEvent::OutputDelta`/`OutputComplete`/`ToolCall`/`ToolResult`/`AgentError` are replaced by one variant, `ControlEvent::Session { session_id, event: SessionEvent }`, forwarding the kernel's real event verbatim instead of re-encoding it into a bespoke, lossy shape. This was the last of the `AgentEvent`/`UiEvent` unification's "6 adapters → 1" promise (see the `ui_event_to_control` doc comment) and fixes two live bugs: tool results rendered with a fabricated empty tool name (`ToolResult` never carried one; every consumer did `tool_name: String::new()`), and operators never saw the dozen-plus variants (todo updates, mode/model changes, context compaction, the transition trace, ...) `ui_event_to_control`'s old `_ => None` silently dropped. `ui_event_to_control` is now total (`ControlEvent`, not `Option<ControlEvent>`) — it no longer interprets `TurnComplete`/`Aborted` into `SessionState::Completed`/`Cancelled` itself, so every session-owning drain loop now decides and broadcasts that transition explicitly and unconditionally for both outcomes, rather than relying on the wrapper's old implicit, `Aborted`-asymmetric behavior. `AgentError`'s `session_id: Option<Uuid>` is gone — no production call site ever constructed it with `None`, so session-scoped errors are just `Session { event: SessionEvent::Error(message), .. }` now.

### Added
- **sven-session-model**: new crate — `ChatSegment` (the chat-display entry type) and its segment-slice helpers (`segment_at_line`, `segment_editable_text`, `segment_is_removable`, `segment_is_rerunnable`, `segment_tool_call_id`, `segment_short_preview`, `messages_for_resubmit`), moved out of `sven-frontend` verbatim. Pure, no tokio, no I/O — every frontend (TUI, CI, a future GUI) needs some version of "fold a `SessionEvent` stream into a displayable conversation," and this is the one place it lives. `sven-frontend::segment` re-exports it unchanged, so no downstream call site needed to change. First step of Phase 3.7. `sven-core::prompts::format_collab_event` also moved here (`sven-core` re-exports it unchanged, text preserved verbatim) since it exists purely to format `ChatSegment::CollabEvent` entries. The rest of Phase 3.7 (`SessionFold`, consolidating the three `## User`/`## Sven` codecs, porting TUI/CI to it, extracting `sven-markdown`) is not done in this commit.
- **sven-session-model**: `MachineProjection`/`projection_to_session_state` also moved here from `sven-frontend`, split from the tokio broadcast plumbing (`ProjectionTx`/`ProjectionRx`/`projection_channel`) that carries it, which stays in `sven-frontend` since it genuinely needs a runtime. `sven-frontend::projection` re-exports the pure parts unchanged.
- **sven-session-model**: the `## User`/`## Sven`/`## Tool`/`## Tool Result` markdown conversation codec (`parse_conversation`, `serialize_conversation`/`serialize_conversation_turn[_with_metadata]`, `TurnMetadata`, `ParseError`) moves here from `sven-input`. **Correction to the refactor plan's premise**: the plan's cluster J2 described this as "3 copies of the same codec" (`input/conversation.rs`, `tui/chat/markdown.rs`, `ci/runtime_runner.rs`). Closer reading shows that's wrong for two of the three — `sven-tui`'s `chat/markdown.rs` parses a *different* format entirely (`**You:**`/`**Agent:**` bold-prefix, for round-tripping the live Neovim edit buffer, not a persisted file), and `sven-ci`'s `runtime_runner.rs` only ever *writes* the H2 format by streaming deltas directly to stdout — it never parses its own output back, so there's no parser duplication with `sven-input`'s copy, only a few lines of inline tool-envelope-JSON construction that duplicate what the serializer already does (left as-is; not worth extracting on its own). `sven-input`'s copy was the one real, actively-used implementation (`sven-ci`'s `runner/helpers.rs`/`runner/mod.rs`, `sven-input`'s own `history.rs` legacy chat log, and `src/main.rs`'s piped-stdin parsing), just misplaced in a crate with file-I/O concerns rather than a pure one. `sven-input::conversation` re-exports it unchanged (and keeps the unrelated full-fidelity JSONL format, `ConversationRecord`/`parse_jsonl_full`/`serialize_jsonl_records`, which has no markdown equivalent and stays where it was). `sven-session-model` moves from "domain" to "kernel" tier (a new `[[same_layer]]` exception for `sven-session-model → sven-model`) so `sven-input` (services) can depend on it downward. `tui/chat/markdown.rs` is untouched — porting it to share more with the H2 codec would mean merging two genuinely different formats, not deduplicating one.
- **sven-session-model**: `tool_result_insert_position` — a pure helper that finds where a `ToolCallFinished` result should land in a `ChatSegment` slice (immediately after its matching `ToolCall`, correlated by `call_id`, so streamed text or other segments arriving in between don't separate a call from its result). Extracted after auditing the plan's `SessionFold` idea against the actual codebase: a full pure fold over `SessionEvent` turned out not to be extractable, since every real handler is inseparable from surface-specific async side effects (`sven-tui`'s does terminal rendering, nvim-buffer sync, and history persistence inline; `sven-ci`'s writes directly to stdout) — there's no `ChatModel::apply` that could live in a tokio-free crate without either dragging that plumbing down into it or stripping it out and breaking the surfaces. What *was* genuinely duplicated, and had diverged, was this one piece of correlation logic: `sven-tui` had it copy-pasted byte-for-byte between the foreground handler (`app/agent_events.rs`) and the background-session handler (`app/session_manager.rs`), and a third copy in the subagent-update handler (`apply_subagent_update`) had silently regressed to an unconditional push with no correlation at all — a subagent's tool result could end up appended after unrelated later content instead of next to its call. All three now call the one shared function; the subagent path gained the `call_id` correlation (and matching `expand_level` index shift) the other two already had, fixing that regression. `sven-frontend::segment`/`sven-tui::chat::segment` re-export it unchanged.

Phase 3.7 closes here. **Another correction to the plan's premise, same pattern as the codec cluster**: it called for extracting a `sven-markdown` crate absorbing `frontend/src/markdown.rs` and `tui/src/markdown.rs`, on the grounds that both the TUI and (the now-deleted) GUI needed the shared block parser. With `crates/gui` gone (Phase 1.2, before this refactor branch started touching events), `sven_frontend::markdown::{parse_markdown_blocks, MarkdownBlock}` has exactly one consumer left in the whole workspace: `sven-tui`'s own `markdown.rs`, which is itself irreducibly `ratatui`-specific (`Style`/`Color`/`Line`/`Span`) and could never move to a foundation-tier crate anyway. There is no cross-surface duplication left to deduplicate, so no `sven-markdown` crate is extracted; `frontend/src/markdown.rs` stays where it is — already a pure, dependency-light module one crate above `sven-tui`, a legal downward edge, not a smell.

### Added
- **sven-turn**: new crate (Phase 4.1) — the impure turn-execution primitives moved verbatim out of `sven-core`: the single-turn LLM streaming call (`stream_turn`, `ModelResolver`, `ThinkingBudget`, `to_model_schemas`, `AbortedError`), context compaction (`compact_session[_with_strategy]`, `emergency_compact`, `prepare_compaction`/`finish_compaction`, `smart_truncate`, `CompactionPlan`), tool-argument JSON repair (`tool_slots::attempt_json_repair`), and system-prompt assembly (`prompts::{system_prompt, PromptContext, CollabEvent, ...}`). None of this is part of the pure `Machine` state-transition layer that gives `sven-core` its "types go down, behaviour stays up" shape — `stream_turn` makes the real async `sven_model::ModelProvider` call and `compact` decides what to summarize, both I/O-adjacent turn primitives, not machine transitions. Placed at "domain" tier, directly below `sven-core`/`sven-executors` (both "machines"), so either can depend on it without depending on each other. `sven-core` re-exports everything unchanged (`sven_core::stream_turn`, `sven_core::prompts::*`, `sven_core::ThinkingBudget`, etc. all still resolve), so no downstream call site needed to change in this commit — Phase 4.2 repoints `sven-executors` (the heaviest consumer) at `sven-turn` directly and deletes the `sven-executors -> sven-core` same-tier exception this move was tracked against. `sven-core`'s own remaining code no longer touches `sven-tools`, `anyhow`, or `futures` at all, so those three (plus `tokio`/`tracing`/`async-trait`/`regex`/`thiserror`/`tokio-stream`, none referenced outside the moved files either) drop out of its `Cargo.toml`.

### Changed
- **sven-executors** (Phase 4.2): repointed at `sven-turn` directly (`stream_turn`, `to_model_schemas`, `AbortedError`, `ModelResolver`, `ThinkingBudget`, `prepare_compaction`/`finish_compaction`/`emergency_compact`, `CompactionPlan`, `smart_truncate`) instead of going through `sven-core`'s re-exports; `sven-core` itself moves to `[dev-dependencies]` (`turn.rs`/`tool.rs`'s integration tests exercise a real `ReactiveAgentMachine` end to end, a legitimate dev-only need). This deletes the `sven-executors -> sven-core` same-tier `architecture.toml` exception outright — `cargo tree -p sven-executors -e normal` no longer contains `sven-core` at all. Also collapsed `turn.rs`'s two names for the same event type: it imported both `sven_core::AgentEvent` and `sven_hsm::UiEvent`, which have been the same `sven_vocab::SessionEvent` since Phase 3 — now uses `UiEvent` uniformly (already imported from `sven-hsm`, which `sven-executors` depends on regardless), so `CompactionStrategyUsed` comes from `sven-hsm` too instead of `sven-core`. AGENTS.md's "all I/O happens in executors" claim is corrected to name `sven-turn` as where the real network call physically lives (reached only through an `EffectExecutor`, never from a transition) rather than implying it's inside `sven-executors`' own module tree, and gains a crate-table row for `sven-turn`.
- **BREAKING**: `trace` (the crate) renamed to `atif` (Phase 4.3). It implements the ATIF trajectory format, but the old name read as the `tracing` crate at every call site (`trace::Trajectory`, `use trace::{...}`) — confusing in a codebase that also uses `tracing` extensively for logging. Directory `crates/trace` → `crates/atif`, package name `trace` → `atif`; every consumer (`sven-input::trace_session`, `sven-ci`'s workflow/replay I/O, `sven-tui`'s session save/load) updated. Pure rename, no behavior change — `architecture.toml`'s tier entry and `AGENTS.md`'s crate-table row move with it, and the row's stale "sole consumer" claim (it names three) is corrected while there.
- **BREAKING**: `sven-runtime` renamed to `sven-workspace` (Phase 4.3). "Runtime" already named five other things in this codebase (`hsm::Runtime<M>`, `hsm::ErasedRuntime`, `bootstrap::RuntimeBuilder`, `core::AgentRuntimeContext`, this crate) and this one is none of those — it's project/workspace discovery: root detection, skill/agent/knowledge scanning, git context, CI-environment detection. Directory `crates/runtime` → `crates/workspace`, package `sven-runtime` → `sven-workspace`; all 20 real consumers (`sven-bootstrap`, `sven-ci`, `sven-turn`, `sven-tools`' knowledge/skill/memory builtins, `sven-tui`, `sven-frontend`) updated. `docs/technical/skill-system.md`/`knowledge-base.md` and `.agents/skills/repo-structure/SKILL.md`, which describe this crate's behavior directly, updated too; the large `docs/technical/crate-architecture.md` rewrite stays deferred to Phase 7.6 as planned, to avoid four rounds of churn across this phase's four renames.
- **BREAKING**: `sven-input` renamed to `sven-session-store` (Phase 4.3, third of the four renames). "Input" never described what this crate does — ATIF trajectory-backed session persistence (`trace_session`), legacy YAML chat import, markdown history — and `.agents/skills/repo-structure/SKILL.md` had drifted into describing it as "stdin/file/pipe input handling" (which is `sven-ci`'s job), a real symptom of the name being actively misleading rather than just imprecise. Directory `crates/input` → `crates/session-store`, package `sven-input` → `sven-session-store`; all consumers updated (`sven-ci`, `sven-frontend`, `sven-tui`, the root `sven` binary's own `src/main.rs` and its top-level `tests/{integration,adversarial}_test.rs`). `docs/technical/pipe-composition.md` and the repo-structure skill doc updated in passing; also caught two `trace::` references there that Phase 4.3's `trace`→`atif` rename missed because it only swept `.rs` files, not docs.
- **BREAKING**: `sven-core` renamed to `sven-machines` (Phase 4.3, fourth and last of the four renames, closing Phase 4.3). "Core" undersold what changed about this crate across Phases 4.1–4.2: it used to be a grab-bag of the pure `Machine` impls *and* the impure turn-execution primitives (`stream_turn`, `compact`, prompts), with a name that gave no signal either way; now that the impure half lives in `sven-turn`, what's left really is just the `Machine` impls the "machines" tier is named for — `sven-machines` says that directly. Directory `crates/core` → `crates/machines`, package `sven-core` → `sven-machines`; all 36 real consumers updated (`sven-acp`, `sven-bootstrap`, `sven-ci`, `sven-config`, `sven-executors`' tests, `sven-frontend`, `sven-hsm`, `sven-model`, `sven-tui`, the root binary). `AGENTS.md` updated throughout, including its `core/src/machines/…`-style path references (now `machines/src/machines/…`) — the one doc explicitly kept in sync at every step of Phase 4.3 rather than deferred, since it's the routing guide every agent reads first. Verified with a full `make tests/e2e/basic` run (353 assertions, all green) given this is the widest-reaching of the four renames.

### Added
- **sven-kernel**: new crate (Phase 4.4) — the tokio Active Object runtime extracted out of `sven-hsm`'s `runtime.rs` (1219 lines): `Runtime<M>`/`ErasedRuntime` (the two consumer-loop implementations), `EffectExecutor`/`ChildSpawner` (async traits), `EventSink`, `Clock`/`SystemClock`/`VirtualClock`, `TimerService`. Placed at "services" tier, one above `sven-hsm` (foundation), so `sven-hsm`'s pure vocabulary (`Machine`, `Effect`, `Event`, `Context`, `PermissionPolicy`, `AuditRecord`) has no tokio dependency of its own — only the execution engine that drives it does. `sven-hsm` keeps `tokio` for one thing only: `observation::ObservationSink`'s broadcast channel, the outward event-vocabulary plane, not the active-object engine — moving that too would force a `sven-kernel` dependency onto every one of `UiEvent`'s many consumers for no benefit, so the plan's "sven-hsm becomes tokio-free" is honored in spirit (the execution engine is fully out) rather than literally (one broadcast channel stays, because it's vocabulary, not execution).
- **sven-hsm**: new `report` module — `RuntimeStatus`, `RuntimeReport<M>`, `ErasedReport`, `StateLabel`, `AuditTrailHandle` moved here rather than to `sven-kernel` with the rest of `runtime.rs`, even though the kernel is what publishes them. These are pure data snapshots with no tokio dependency of their own ("types go down, behaviour stays up"), and keeping them at `sven-hsm`'s foundation tier is what lets lower-tier crates that only need to *read* a runtime snapshot — `sven-session-model`'s `MachineProjection::from_status` chief among them — do so without pulling in the whole tokio execution engine, which would have been an illegal upward tier edge (`sven-session-model` sits at "kernel" tier, below "services" where `sven-kernel` lives).

### Changed
- **19 crates** repointed at `sven-kernel` for the moved types (`sven-bootstrap`, `sven-ci`, `sven-executors`, plus their test suites) — each `use sven_hsm::{...}` import split into the parts that stayed (`Context`, `Effect`, `Event`, `Hsm`, `PermissionPolicy`, `ObservationSink`, `UiEvent`, ...) and the parts that moved (`Runtime`, `ErasedRuntime`, `EffectExecutor`, `EventSink`, `ChildSpawner`, `Clock`, `SystemClock`, `VirtualClock`, `TimerService`). `sven-hsm`'s `tests/runtime.rs` and `tests/child_spawner.rs` (454 lines, exercising exactly the machinery that moved) relocated to `sven-kernel/tests/`, along with a copy of the shared `common/mod.rs` Machine-fixture module (Rust integration tests can't share modules across crate boundaries; the fixtures themselves have zero tokio dependency, so duplicating this dev-only file is the pragmatic call over a bigger restructure). `sven-hsm`'s remaining `tests/{engine,permissions,submachine}.rs` — the pure-dispatch tests — keep their own copy of `common/mod.rs` unchanged.
- **sven-bootstrap** (Phase 4.7): `child_spawner.rs::SdlcChildSpawner` — one of the plan's "hand-rolled kernel assemblies" that bypass `RuntimeBuilder` — now builds its machine as a type-erased `Box<dyn ErasedMachine>` and spawns it with `ErasedRuntime::spawn` instead of the generic `Runtime::spawn`. `Runtime<M>`'s consumer loop never emits `Effect::PersistAudit`; `ErasedRuntime`'s does, after every dispatch. The call site does not configure a durable audit sink today (no `.with_audit()`/`.with_audit_trail()`), so this changes no currently-observable behavior — what it fixes is the latent structural gap where this path *could never* get audit persistence even if one were wired up later, unlike every other kernel session in the codebase. Drop-in swap: the call site already only reads `report.ctx` from the join result, which `ErasedReport` provides identically to `RuntimeReport<M>`.

### Fixed
- **Refactor-plan correction (Phase 4.5)**: the plan called for deleting `Runtime<M>` ("production uses only `ErasedRuntime`"), converting `EventSink` to a trait, and making `SessionBundle::runtime` private. None hold up against actual usage, found while working Phase 4.4: `Runtime<M>` is used in production by the same two hand-rolled assemblies Phase 4.7 targets, *and* as the ergonomic statically-typed test harness in ~30 call sites across nearly every executor's own unit tests plus `sven-kernel`'s integration suites — deleting it would force type-erasure boilerplate into simple unit tests for no benefit. `Runtime<M>` vs `ErasedRuntime` isn't a duplicate-implementation smell; it's a legitimate static-type-known vs. runtime-type-chosen pair. No call site or test evidences a need for more than one `EventSink` implementation. `SessionBundle::runtime`'s ~9 call sites are all `drop(bundle.runtime)` (shut down now) or `let _runtime = bundle.runtime` (keep alive) as a partial move *after* the bundle's other fields have already been moved out elsewhere in the same function — the standard `Drop`-based owned-resource-handle pattern (same shape as `std::thread::JoinHandle`), not a leaked implementation detail. None of Phase 4.5 is implemented as a result; Phase 4.6 (`permission_policy()` onto the `Machine` trait) is deferred for a related reason — the policy `runtime_builder.rs` selects depends on `AgentMode` (Plan/Research get a write-restricted policy) as well as machine type, so a plain `Machine::permission_policy(&self)` trait method doesn't cleanly capture the existing behavior without either threading extra context through the trait or making machines mode-aware at construction; revisit alongside a real design for that, not as a mechanical move.

### Added
- **sven-tool-api**: new crate (Phase 5.1, kernel tier) — the `Tool` trait and its full interface split out of the `sven-tools` god-crate (previously ~17,900 LOC, fan-in 21, the highest in the workspace): `Tool`, `ToolDisplay`, `ToolDisplayRegistry`, `ApprovalPolicy`, `PermissionRequester`, the `ToolEvent`/`TodoItem`/`TodoStatus`/`SubagentUpdate` tool→agent-loop event vocabulary, the pure display/summary helpers (`format_tools_list`, `tool_smart_summary`, `shorten_path`, `tool_category`, `tool_icon`), and the typed-parameter extraction helpers every tool implementation uses (`require_str`/`opt_str`/`opt_u64`/`opt_bool`, now `pub` instead of `pub(crate)` since domain-tool crates outside `sven-tools` itself will need them). Depends only on `sven-config`/`sven-hsm`/`sven-vocab` (all foundation tier).
- **sven-tool-registry**: new crate (Phase 5.1, services tier) — the concrete `ToolRegistry` (lookup/execute/schema listing, MCP hot-swap, approval-gated `execute_with_requester`/`execute_unattended`) plus the config-driven policy engines that decide an `ApprovalPolicy` for a command: `ToolPolicy` (glob-pattern auto-approve/deny) and `RolePolicy` (per-role tool denial + the `fs_root` path jail). Builds on `sven-tool-api`'s trait/type vocabulary.

### Fixed
- **Refactor-plan correction (Phase 5.1)**: the plan's one-line split ("sven-tool-registry: `ToolRegistry`, `ApprovalPolicy`, `ToolPolicy`/`RolePolicy`") puts `ApprovalPolicy` at the wrong tier. `Tool::default_policy(&self) -> ApprovalPolicy` is part of the `Tool` trait's own signature, so `ApprovalPolicy` cannot live at a higher tier than the trait without every crate implementing `Tool` needing an illegal upward edge to `sven-tool-registry` (services) from wherever it sits (domain, for the eventual `sven-tools-*` split). `ApprovalPolicy` and `PermissionRequester` (whose `request_permission` signature also names `ToolCall`, already kernel-tier vocabulary) moved to `sven-tool-api` instead; only the config-driven *engines* that decide an `ApprovalPolicy` (`ToolPolicy`, `RolePolicy`) stayed in `sven-tool-registry`, which is exactly the split the plan's own reasoning ("types go down, behaviour stays up") implies once the trait dependency is traced through. The plan's terse listing also omitted four existing `sven-tools` top-level modules with no natural home in either one-liner (`events.rs`, `tool_summary.rs`, `display.rs`, `params.rs`); all four depend on nothing above `sven-config`/`sven-hsm`/`sven-vocab` and are consumed by tools and/or surface-tier crates (never by the registry itself), so they moved to `sven-tool-api` alongside the trait they describe.

### Changed
- **sven-tools**: reduced to the ~18k LOC of concrete built-in tool implementations (`builtin/`) plus a re-export shim at the crate root and at the `tool`/`policy`/`events`/`registry` module paths, so none of the ~20 crates still writing `sven_tools::ToolRegistry`, `sven_tools::ApprovalPolicy`, `sven_tools::tool::ToolCall`, etc. needed to change (same "pure move + re-export, delete the shim later" pattern Phase 2.1 used for `sven-vocab`). `sven-tools -> sven-tool-registry` is a new same-tier (services) `architecture.toml` exception, tracked to be deleted once `builtin/` is carved into the domain-tier `sven-tools-{fs,exec,web,ctx,agent,gdb}` crates (5.2-5.7) and this crate either shrinks to nothing or is deleted outright.
- **sven-mcp-client**: repointed at `sven-tool-api` directly instead of `sven-tools` (it only ever used `Tool`/`ToolCall`/`ToolOutput`/`ApprovalPolicy`/`OutputCategory`/`ToolCapability` — trait-level vocabulary, never the registry). This closes out the `architecture.toml` same-layer exception recorded against this exact edge when the ratchet was first seeded, which named this repointing as its own resolution condition; the exception is deleted.

## [1.9.0] - 2026-03-22

### Added
- **sven-frontend**: shared agent-wiring layer extracted from `sven-tui` for reuse across frontends.
- **sven-gui**: Slint desktop GUI; **`sven-ui` merged into `sven --gui`** (single binary).
- **GUI**: session persistence; **lazy chat loading**; **per-session token usage** persisted; full **markdown** in the chat view; markdown in **thinking and tool result** bubbles; scrollable tool bubbles; **todo** tool results in chat; vertical chat layout; sidebar **search** filter; SearchInput/SearchBar styling; Slint GUI skill.
- **Channels (`sven-channels`)**: `Channel` trait and multi-platform adapters; E2E integration tests for channel → manager → reply.
- **Scheduler (`sven-scheduler`)**: cron, interval, and one-shot jobs.
- **Integrations (`sven-integrations`)**: email, calendar, and voice.
- **Memory (`sven-memory`)**: SQLite store with FTS5 semantic search.
- **Config**: schema for channels, scheduler, email, calendar, voice, memory, and webhooks.
- **Node**: generic **webhook** endpoints; proactive agent integrations wired through bootstrap and tool registry.
- **Skills**: load system-installed skills from `/usr/share` and `/usr/local`; **always reload skill content from disk** when the agent loads a skill.

### Fixed
- **GUI**: nested-runtime panic on `--gui`; Slint layout, interaction, display, ask-question UX, picker, queue, spinners, markdown wrapping, session list busy indicator, completion, errors, clear, highlighting (multiple rounds of fixes).
- **TUI**: markdown aligned with GUI (block quote vs list); first completed todo item no longer shows a stray bullet.

### Changed
- **GUI**: large UI refactor; removed standalone "current tool" display in favor of clearer chat-centric UX.
- **Docs**: README condensed and expanded with accurate feature lists; user guides for channels, scheduler, email, calendar, voice, memory, webhooks, and use cases; **AGENTS.md** updated for sven-frontend, sven-gui, and dual-binary layout.

## [1.8.1] - 2026-03-15

### Added
- **Tests (`sven-model`)**: coverage that the full MCP tool schema is passed through to the model API.

### Fixed
- **MCP**: omit `null` `capabilities` in `initialize` for strict servers; show **disabled** status when `enabled: false` in config.
- **MCP**: wait for tools in headless mode and refresh on `ToolsChanged`.
- **TUI**: compile fixes for **ratatui 0.30**; `CompletionOverlay` viewport now driven by `ListState` (removed fixed `max_visible`).

## [1.8.0] - 2026-03-14

### Added
- **MCP client**: broad client-side MCP server support (SSE, sessions, OAuth integration).
- **MCP OAuth**: PKCE flow with auto-auth, token lifecycle, and **scope discovery** (no manual scope lists).
- **CLI**: `sven mcp` auth flow; default OAuth redirect **`sven://sven.mcp`** with container fallback; **`cursor://`** support.
- **Release**: `make` release targets accept **`--no-confirm`** for non-interactive runs.

### Fixed
- **MCP / OAuth**: RFC-compliant PKCE; stop perpetual auth loops; trigger OAuth on **401**; skip OAuth flows in **headless/CI**; improved SSE and session handling.
- **Config**: recognize `mcp_servers`; fix **SKILL.md** frontmatter parsing.

### Changed
- **Headless**: apply settings on startup; reduce unlabelled noise; show context path.
- **TUI**: focus the input pane when it is clicked.

## [1.7.5] - 2026-03-13

### Added
- **TUI**: **conversation cost** in the status bar.
- **GDB**: `gdb_start_server` Makefile target made target-agnostic.
- **Docs**: `AGENTS.md` added.

### Fixed
- **Tokens / UI**: status bar token display; subagents run at correct depth; restore subagent chat hierarchy on restart with new chat at top; suppress noisy tracing when a subagent runs `acp serve`.
- **Subagents & sessions**: subagent UX, agent error handling, and background session completeness.
- **sven-config**: default model auto-detection priority (**OpenRouter** first) with regression test.

### Changed
- **CLI**: auto-enable **headless** mode when a **positional prompt** is provided.
- **TUI**: focus the chat list on click so keyboard navigation applies immediately.

## [1.7.4] - 2026-03-12

### Added
- **OpenRouter Auto & Free routers**: `openrouter/auto` and `openrouter/free`
  are registered in the model catalog and resolvable via `--model
  openrouter/auto` / `--model openrouter/free`. `openrouter/auto` is the
  default out-of-the-box model (replacing `openai/gpt-4o`) when
  `OPENROUTER_API_KEY` is available. `auto_router_allowed_models` in
  `driver_options` maps to the nested `plugins` structure expected by the
  OpenRouter Auto Router API.
- **Benchmark**: Terminal-Bench 2.0 evaluation via Harbor.

### Fixed
- **TUI drag/resize**: unified system - `SplitPrefs` extracted from `LayoutCache` for durable split dimensions; `anchor_offset` on `ResizeDrag` so borders track the grab point; `PeersSplitBorder` in `HitArea`; single `hit_test()` path.
- **Peers-split border drag**: use `peers_pane.y + peers_pane.height` as the sidebar bottom (fixes upward-drag snap to minimum).
- **Subagents**: inherit the **live model** from the parent.
- **Cache / tokens**: cache hit rate capped near ~49% from double-counted tokens - corrected.
- **CI**: record resolved model in chat output documents.

### Changed
- **Model catalog**: `models.yaml` parsed once per process via `OnceLock` `catalog_ref()`; lookups use the cached slice instead of cloning on every call.
- **Model provider**: `check_api_key_requirement` and `transform_openrouter_options` split into private helpers in `lib.rs`.
- **Repository**: `.gitignore` extended for Python `__pycache__` paths.

## [1.7.3] - 2026-03-11

### Added
- **Compound system tools**: built-in tools consolidated into action-dispatched
  compound tools for cleaner tool surface and fewer tool slots.

### Fixed
- Live timestamp removed from stable system prompt to avoid unnecessary prompt
  churn and improve caching.
- Model and mode transitions now apply immediately in the TUI instead of
  waiting for the next user message.
- OpenRouter: Anthropic prompt caching enabled for Anthropic models.
- `switch_model` takes effect in the next model turn (correct staging behavior).
- Mode upgrades from within a conversation are now allowed (tools no longer
  block mode changes).
- Grey-on-grey rendering artifacts in pager and chat view eliminated.
- Command prompt for `/command` is loaded from disk on each run so edits are
  picked up without restart.
- Modifier+click on chat content is ignored so the terminal can open links.
- OpenSSL cross-compilation for macOS and Linux aarch64 in CI.

## [1.7.2] - 2026-03-11

### Fixed
- E2E tests: replaced hardcoded local paths and forward `context_open` args in CI.

## [1.7.1] - 2026-03-11

### Fixed
- Clippy: remove empty line after doc comment in `task_tool.rs`.
- CI: centralize build commands via Makefile and fix macOS OpenSSL cross-compilation.

## [1.7.0] - 2026-03-10

### Added
- **ACP (Agent Client Protocol)**: full protocol compliance across server and
  client roles; `--model`/`--provider` for `acp serve`, model inheritance.
- **Unified todo tool**: replaces `todo_write` with a single tool supporting
  read/add/update/set actions.
- **Chat tree view**: subagent sessions shown as children in the TUI.
- **ToolDisplay trait**: chat view tool labels and summaries use shared display logic.
- **LLM-generated chat titles** and delete-active-chat; animation updates.
- **Compound tools**: 42 built-in tools consolidated into 14 compound tools.
- **P2P peers pane** in TUI; keyboard-first segment actions (per-line icons removed).
- Website: overhauled copy, SEO, and section SVGs; logo and hero font (JetBrains Mono).
- Comprehensive adversarial test suite (64 Rust + 25 Bats tests).

### Fixed
- Peers pane resize drag, peer list population, and P2P dial noise.
- Subagent exit code, chat pane keybindings, peers-split drag direction.
- Full tool result shown at expand level 2 in TUI.
- Task tool returns result immediately when PromptResponse arrives.
- Thought block display and subagent user message.
- Subagent inactivity timeout during long tool calls (bootstrap).
- Chat display and welcome screen after streaming refactor.
- Tool rendering, shell intent, subagent blocking, and character width (CJK ambiguous).
- Chat title race so LLM-generated title is used when available.
- Merge positional prompt with stdin in CI runner (not in main); workflow only for `-f`.
- File deletion (sven-input); trailing empty lines in segments; delete as default button.
- Build errors: `From<anyhow::Error>` for `FileModifiedError`, `ToolDisplayInfo` export.
- Pre-commit runs clippy only on staged files.

### Changed
- Streaming seasoning/thinking shown without backticks in gray dim text; thinking
  content preview instead of word count; tool scan animation sinusoidal;
  streaming cursor blink ~500ms; timeouts to prevent indefinite hangs.
- ACP used for subagent communication with structured streaming.
- Refactor: eliminate duplication and decouple crate architecture.

## [1.6.0] - 2026-03-09

### Added
- **Multi-session TUI**: run several conversations simultaneously without
  restarting sven. Collapsible chat list sidebar (toggle with `Ctrl+B`) with
  live status; `n` new session, `Enter` switch, `d` delete, `a` archive.
- **Background sessions**: switching away does not interrupt the agent; spinner
  shows running sessions; events buffered when you return.
- **YAML persistence**: sessions saved to `~/.config/sven/history/<id>.yaml`,
  restored on launch; last active session restored on startup.
- Per-session model and mode; mouse drag to resize chat list sidebar.
- **Inspector overlay**: skills, subagents, peers, context; `/tools` inspector
  with node-proxy support.
- **Parallel tool slots** (sven-core): streaming dispatch with multiple
  concurrent tool slots.
- Vim-style pane navigation (focus chat list / main pane).

### Fixed
- `/new` now creates a proper new session in the sidebar with isolated agent
  (no bleed from old session).
- Per-session `/model` and `/mode` state; queued messages and `abort_pending`
  cleared on new session; message-edit state cleared on new session.
- JSONL log path tracked per-session; state leakage on new sessions fixed
  (input, queue, edit state, agent state isolated).
- Chat list click no longer triggers wrong session's segment actions (HitArea
  hit-test); duplicate agent on initial session removed; message loss on exit
  fixed (session state saved).
- `wait_for_message` no longer drops replies when peer responds before waiter
  registers.
- Neovim double-response bug; inspector and pager UX improved.
- TodoUpdate segment ordering in tool output; chat pane selection and
  scrollbar ghost/stuck rendering.

### Changed
- Chat document formatting (sven-input); mouse routing centralized (HitArea);
  inspector overlay dead `kind` field removed; docs for parallel tool slots and
  multi-session TUI.

## [1.5.0] - 2026-03-09

### Added
- **Team orchestration** in sven-node with layered tool architecture (sven-team
  crate + TUI).

### Fixed
- Team tool confusion that caused the model to duplicate work and misuse APIs.
- Teammate task execution and node restart resilience.

## [1.4.0] - 2026-03-08

### Added
- **ACP (Agent Client Protocol) server** for IDE integration (e.g. Zed).
- **Agent team orchestration** (sven-team crate + TUI); multi-agent orchestration
  roadmap.
- Site: install script at install route; install action and package updates.

### Fixed
- ACP bridge: drop TextComplete/ThinkingComplete to prevent double output.
- Chat view display and find_file glob matching; subagent model wiring.
- Team picker keys through dispatch and missing key bindings.
- Clippy lints; pre-commit fails on format changes.
- CI and installation route fixes.

### Changed
- Docs: Zed ACP config snippet (`agent_servers`, not `assistant.provider`);
  README mentions ACP IDE integration.

## [1.3.2] - 2026-03-07

### Added
- `sven tool list` and `sven tool call` commands for direct tool invocation from the CLI
- Support for executing inline `<invoke>` tool calls emitted by MiniMax and similar models
- Subagent streaming via process-based TaskTool (replaces in-process execution)
- `/clear` and `/new` TUI commands for conversation management
- Cache hit percentage displayed inline after token counts in the status bar
- Cumulative session token totals shown instead of per-turn counts
- Context percentage now uses cumulative tokens for accuracy

### Fixed
- Unicode corruption when multi-byte characters span SSE chunk boundaries
- Token tracking for multi-call turns; simplified completion menu
- Correct `ctx%` denominator; exact provider token counts shown in status bar
- Grouped segment rendering and hardcoded test paths in TUI
- `/install` endpoint now served as plain text instead of SPA fallback

### Changed
- Site container runtime switched from nginx to `serve`
- Built-in tools refactored into categorised modules
- Types hardened and boilerplate eliminated across the core agent codebase
- Site placeholder images replaced with real sven SVG illustrations

## [1.3.0] - 2026-03-05

### Added
- **RLM context tools**: memory-mapped large-content analysis with `context_query`, per-sub-query timeouts, and real-time UI drain
- **TUI UX overhaul**: clean hierarchical agentic interface with improved input handling, multiline paste, and rendering fixes
- Clock-driven animations for thinking indicator, tool-scan, and stream cursor in TUI
- Shell-style input history in TUI
- Markdown table rendering in TUI
- Mouse drag selection in TUI
- `max_output_tokens` and `max_input_tokens` config fields per model
- Provider-first config structure with environment variable expansion
- React marketing landing page for [agentsven.com](https://agentsven.com)
- Install script served at `/install` from the site container
- Latest release version injected at site build time
- 70 integration tests for RLM context tools on real data
- 39-test end-to-end suite for `edit_file` tool
- bats end-to-end tests for context tools and error handling

### Fixed
- Garbled welcome logo - normalised row widths and fixed connector colours
- Welcome screen logo colours and tagline URL
- Multiline paste display, completion double-slash, and rendering artefacts
- Mid-turn mode consistency and mode display in status bar
- aarch64 OpenSSL build in CI (non-fatal fallback)

## [1.2.3] - 2026-03-03

### Fixed
- TUI node-proxy mode: connect, stream, and lock model/mode to node correctly
- Node-proxy TUI now forwards Resubmit events to the node (streamed responses)
- Stale waiter slots in the session multiplexer
- Session depth accumulation regression; aarch64 release build

### Changed
- Renamed `gateway` → `node` across the entire codebase for naming consistency

## [1.2.2] - 2026-03-03

### Fixed
- All circular message-loop paths across P2P channels (three separate loop vectors closed)
- Infinite session echo loops and delegation chain corruption in P2P network
- Dual-delivery of messages on the P2P session bus

### Changed
- Removed model auto-nudge from sven-core
- Updated dependency versions across Cargo workspace

## [1.2.1] - 2026-03-03

### Fixed
- Node model defaults, error messages, and CLI ergonomics improvements

## [1.2.0] - 2026-03-03

### Added
- **Agent-to-agent collaboration** via libp2p session messaging and named rooms
- **Browser web terminal** on sven-node with WebAuthn passkey authentication and PTY sessions
- **Plug-and-play TLS** for sven-node using Tailscale and a local CA

### Changed
- Complete overhaul of sven-tui with a modern ratatui architecture

## [1.1.0] - 2026-03-01

### Added
- MCP server support via `sven mcp serve`
- Node-proxy mode for `sven mcp serve` (proxies MCP requests through a remote node)

### Changed
- Updated SPDX licence tags across all crates

## [1.0.4] - 2026-03-01

### Fixed
- macOS OpenSSL build linkage

## [1.0.3] - 2026-03-01

### Fixed
- CI pipeline and release workflow errors

## [1.0.2] - 2026-03-01

### Added
- Initial public release
- Multi-arch release pipeline with CI builds for Linux x86_64/aarch64 and macOS, plus curl-pipe install script
- Codified context infrastructure: three-tier knowledge base for large codebases
- Agent-to-agent task routing over P2P with named rooms
- sven-node heartbeat, separate control-plane configuration, and configurable agent listen address
- Peer allow-list and mDNS local discovery for P2P networks
- `find_file` tool (unified from `glob` / `glob_file_search`)
- Apache 2.0 licence

### Fixed
- 12 P2P security vulnerabilities for hostile network deployment
- Circular delegation false-positive caused by empty peer ID in P2P
- Agent stall nudge firing on legitimate single-tool-call + answer patterns

[Unreleased]: https://github.com/bosun-ai/sven/compare/v1.9.0...HEAD
[1.9.0]: https://github.com/bosun-ai/sven/releases/tag/v1.9.0
[1.8.1]: https://github.com/bosun-ai/sven/releases/tag/v1.8.1
[1.8.0]: https://github.com/bosun-ai/sven/releases/tag/v1.8.0
[1.7.5]: https://github.com/bosun-ai/sven/releases/tag/v1.7.5
[1.7.4]: https://github.com/bosun-ai/sven/releases/tag/v1.7.4
[1.7.3]: https://github.com/bosun-ai/sven/releases/tag/v1.7.3
[1.7.2]: https://github.com/bosun-ai/sven/releases/tag/v1.7.2
[1.7.1]: https://github.com/bosun-ai/sven/releases/tag/v1.7.1
[1.7.0]: https://github.com/bosun-ai/sven/releases/tag/v1.7.0
[1.6.0]: https://github.com/bosun-ai/sven/releases/tag/v1.6.0
[1.5.0]: https://github.com/bosun-ai/sven/releases/tag/v1.5.0
[1.4.0]: https://github.com/bosun-ai/sven/releases/tag/v1.4.0
[1.3.2]: https://github.com/bosun-ai/sven/releases/tag/v1.3.2
[1.3.0]: https://github.com/bosun-ai/sven/releases/tag/v1.3.0
[1.2.3]: https://github.com/bosun-ai/sven/releases/tag/v1.2.3
[1.2.2]: https://github.com/bosun-ai/sven/releases/tag/v1.2.2
[1.2.1]: https://github.com/bosun-ai/sven/releases/tag/v1.2.1
[1.2.0]: https://github.com/bosun-ai/sven/releases/tag/v1.2.0
[1.1.0]: https://github.com/bosun-ai/sven/releases/tag/v1.1.0
[1.0.4]: https://github.com/bosun-ai/sven/releases/tag/v1.0.4
[1.0.3]: https://github.com/bosun-ai/sven/releases/tag/v1.0.3
[1.0.2]: https://github.com/bosun-ai/sven/releases/tag/v1.0.2
