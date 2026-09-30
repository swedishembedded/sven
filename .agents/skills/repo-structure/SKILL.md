---
name: repo-structure
description: "Provides the authoritative layout of the sven repository. Load when the task involves: navigating the repo (finding where code lives), adding or moving crates/modules/files, modifying CI or release workflows, understanding build/test/release targets, restructuring directories, or any task where knowing where things are avoids a full exploration. Do NOT load for tasks that only edit code inside a single already-known file."
---

# sven - Repository Structure

## Top-level layout

```
sven/
├── src/                        # The `sven` binary
│   ├── main.rs
│   ├── cli/                    # clap subcommand definitions
│   └── run/                    # one module per run mode (tui, ci, agent, task, ...)
├── crates/                     # Workspace crates (see below)
├── samples/                    # Standalone applications built on the SDK facade
├── xtask/                      # `cargo run -p xtask -- arch` enforces architecture.toml
├── tests/
│   ├── e2e/
│   │   └── basic/              # All bats end-to-end tests + helpers.bash
│   ├── fixtures/               # Shared test fixtures (mock_responses.yaml, plan.md, ...)
│   └── *.rs                    # Workspace-level integration tests
├── docs/                       # User-facing markdown docs + technical/ + adr/
├── formal/                     # TLA+ models of the HSM kernel (`make formal`)
├── benchmarks/                 # Terminal-Bench harness (`make benchmark`)
├── site/                       # Project website (`make site/build`)
├── scripts/
│   ├── install.sh              # curl-pipe installer
│   ├── release-build.sh        # local multi-platform artifact builder
│   ├── build-deb.sh            # manual .deb packager (cross-compile use)
│   ├── gates/                  # checks run by `make check/gates`
│   └── hooks/                  # git hooks (`make hooks/install`)
├── .github/
│   └── workflows/
│       ├── ci.yml              # Push/PR: check, e2e-basic, build-smoke, minimal-profile, macOS/Windows
│       └── release.yml         # Tag push: e2e-basic gate → builds → publish
├── .agents/
│   └── skills/                 # Agent skills for this repo
├── Makefile                    # Primary developer interface (see targets below)
├── Cargo.toml                  # Workspace root; `[workspace.package].version` is the release version
├── Cargo.lock
├── architecture.toml           # Crate tiers and dependency rules (checked by xtask)
├── Cross.toml                  # cross-rs config for aarch64 builds
└── release.toml                # cargo-release config
```

## Workspace crates (`crates/`)

Tiers are defined in `architecture.toml`; a crate may depend only on lower
tiers unless an edge is listed there.

| Crate | Purpose |
|-------|---------|
| `sven-hsm` | Pure HSM dispatch (Super walk + LCA), typed `Effect`/`Event`, `PermissionPolicy`, audit, replay, outward observation plane |
| `sven-kernel` | tokio Active Object runtime that drives a `sven-hsm` machine: `Runtime`/`ErasedRuntime`, `EffectExecutor`, `ChildSpawner`, `Clock` |
| `sven-machines` | Pure `Machine` impls: `ReactiveAgentMachine`, `SdlcMachine`, `TaskMachine`, `UiTestMachine`, `VerifiedTaskMachine`; `ModeRegistry`; shared `loop_core` |
| `sven-executors` | Effect executors (the only I/O layer): turn, tool, user, timer, audit, internal, verify, composite; the `ThreadStore` |
| `sven-turn` | Turn primitives: `stream_turn`, context compaction, tool-argument JSON repair, system-prompt assembly |
| `sven-vocab` | Shared data vocabulary (`ToolCall`, `ToolOutput`, `SessionEvent`, ...); zero sven deps |
| `sven-chain` | Hash-chained append-only JSONL log primitives |
| `sven-control` | Transport-agnostic operator control protocol and its kernel mappings |
| `sven-bootstrap` | `RuntimeBuilder` (assembles the kernel from config + mode), tool-registry building, `KernelAgentSession`, `TaskTool` |
| `sven-config` | Config schema and loading (`.sven.yaml`, `.sven/config.yaml`, `~/.config/sven/config.yaml`) |
| `sven-workspace` | Project-root, git and CI detection; skill, command, agent and knowledge discovery |
| `sven-model` | `ModelProvider` trait, request/response types, budget gate, driver registry |
| `sven-model-catalog` | Static model catalog data + live-cache overlay |
| `sven-model-drivers` | Concrete provider drivers and the `from_config` factory |
| `sven-model-mock` | `--model mock` providers (`MockProvider`, `YamlMockProvider`) |
| `sven-session-model` | Surface-agnostic chat rendering model and the `## User`/`## Sven` markdown codec |
| `sven-session-store` | ATIF trajectory-backed session store (plus read-only legacy YAML import) |
| `atif` | ATIF v1.7 trajectory model, validator and persistence; zero sven deps |
| `sven-image` | Image loading/resizing/base64 for attachments |
| `sven-audio` | Audio loading, WAV decoding and resampling |
| `sven-tool-api` | `Tool` trait, `ToolDisplay`, approval policy, `PermissionRequester`, tool events |
| `sven-tool-registry` | `ToolRegistry`, `ToolPolicy` |
| `sven-tools-fs` | File read/write/edit/find/attach tools and subprocess output buffer tools |
| `sven-tools-exec` | `shell` tool |
| `sven-tools-web` | `web_fetch`, `web_search`, `grep`, `read_lints` |
| `sven-tools-ctx` | RLM context store tools, knowledge tools, compound `memory` tool |
| `sven-tools-agent` | `system`, `todo`, `ask_question`, `skill` |
| `sven-tools-gdb` | GDB/MI debugging tools and the compound `gdb` tool (Unix only) |
| `sven-tools-android` | Typed Android device control over ADB |
| `sven-memory` | Semantic (vector + BM25) memory store and its tools |
| `sven-team` | Agent team coordination: shared task lists, team config, lifecycle tools |
| `sven-mcp-client` | MCP client: external MCP servers as a tool/prompt/resource source |
| `sven-mcp` | MCP server exposing sven tools |
| `sven-acp` | ACP (Agent Client Protocol) server |
| `sven-commands` | `SlashCommand` vocabulary and builtin `/…` commands |
| `sven-frontend` | Shared agent-wiring layer for frontends |
| `sven-sdk` / `sven-sdk-macros` | The public framework facade and its `#[agent]` macro |
| `sven-ci` | Headless/CI runner (`CiRunner` + `RuntimeRunner`) and output formatters |
| `sven-tui` | Terminal UI |
| `sven-tui-nvim` | Embedded Neovim client for the TUI's edit mode |

## Key Makefile targets

Run `make help` for the full list.

| Target | Description |
|--------|-------------|
| `build` / `release` | Debug / release build |
| `test` | `cargo test --workspace` |
| `tests/e2e/basic` | Build + run all bats tests in `tests/e2e/basic/` |
| `tests/e2e` | Alias → `tests/e2e/basic` |
| `check` | `check/fmt`, `check/gates`, `check/arch`, `check/features`, `check/clippy`, `check/doc` |
| `fmt` | `cargo fmt --all` |
| `deb` | Build Debian package |
| `docs` / `docs-pdf` | Build user-guide markdown / PDF |
| `formal` | Machine-check the TLA+ models |
| `release/patch\|minor\|major` | Bump version via cargo-release |
| `release/build` | Build release artifacts into `dist/` |
| `release/tag` | Create + push annotated git tag |
| `release/publish` | Upload `dist/` to GitHub Release via `gh` |

## CI/release flow

```
Push / PR  →  ci.yml
  ├── check            (cargo fmt --check, make check, make test)
  ├── e2e-basic        (make tests/e2e/basic)
  ├── build-smoke      (make release)
  ├── minimal-profile  (xtask arch --profile minimal)
  ├── check-macos      (continue-on-error)
  └── check-windows    (continue-on-error)

Push tag  →  release.yml
  ├── e2e-basic            ← gate for publish
  ├── build-linux-x86_64
  ├── build-linux-aarch64  (needs x86_64; continue-on-error)
  ├── build-macos          (continue-on-error)
  ├── build-windows        (continue-on-error)
  └── publish              (needs e2e-basic + build-linux-x86_64)
```

## E2E test suite (`tests/e2e/basic/`)

All tests use `--model mock` (no API key or network required). Hardware-gated tests in
`07_gdb_workflows.bats` (Tiers 2-3) self-skip unless `SVEN_TEST_JLINK=1` is set.

| File | Scope |
|------|-------|
| `01_cli.bats` | CLI flags, subcommands, completions |
| `02_ci_mode.bats` | Headless activation, exit codes, stdin |
| `03_mock_responses.bats` | Mock model match types, tool-call sequences |
| `04_pipeline.bats` | sven-to-sven piping, stdin sources |
| `05_new_tools.bats` | Built-in tool set end-to-end |
| `06_headless_enhancements.bats` | Output formats, frontmatter, artifacts, timeouts |
| `07_gdb_workflows.bats` | GDB tools (Tier 1 always runs; Tiers 2-3 need hardware) |
| `08_trace_output.bats` | Trace tokens, tool call/result IDs, pipe chains |
| `09_edit_file.bats` | `edit_file` tool |
| `10_context_tools.bats` | RLM memory-mapped context tools |
| `11_error_handling.bats` | Error handling and graceful failure |
| `12_adversarial.bats` | Adversarial inputs |
| `13_semantic_memory.bats` | `semantic_memory` remember → recall round trip |
| `14_resume.bats` | `sven chats` / `--resume` against the ATIF session store |
| `15_verified_task.bats` | `sven task run` with an independent verifier |
| `16_agent_step.bats` | `sven agent step`, the CLI surface of the SDK |
| `17_stdin.bats` | Which headless runs read stdin |
| `18_subagent_provider.bats` | `task` sub-agents run on the parent's provider |
| `helpers.bash` | Shared helpers: `BIN`, `FIXTURES`, `sven_mock`, `assert_output_contains` |

---

## Keeping this skill up to date

**Update this file whenever you make any of the following changes:**

- Add, remove, or rename a crate under `crates/`
- Move or rename a top-level directory or file (e.g. tests, scripts, docs)
- Add a new e2e test file under `tests/e2e/`
- Add a new CI workflow or modify job dependencies in `ci.yml` / `release.yml`
- Add or change a `Makefile` target that affects the build/test/release flow

Edit only the relevant table row or section - do not rewrite sections that have not changed.
