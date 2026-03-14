---
name: repo-structure
description: "Provides the authoritative layout of the sven repository. Load when the task involves: navigating the repo (finding where code lives), adding or moving crates/modules/files, modifying CI or release workflows, understanding build/test/release targets, restructuring directories, or any task where knowing where things are avoids a full exploration. Do NOT load for tasks that only edit code inside a single already-known file."
---

# sven — Repository Structure

## Top-level layout

```
sven/
├── src/                        # Binary entry points
│   ├── main.rs
│   └── cli.rs
├── crates/                     # Workspace crates (see below)
├── tests/
│   ├── e2e/
│   │   └── basic/              # All bats end-to-end tests (01–08 + helpers + ci_mode)
│   └── fixtures/               # Shared test fixtures (mock_responses.yaml, plan.md, …)
├── docs/                       # User-facing markdown docs (00–09) + technical/ sub-dir
├── scripts/
│   ├── install.sh              # curl-pipe installer
│   ├── release-build.sh        # local multi-platform artifact builder
│   └── build-deb.sh            # manual .deb packager (cross-compile use)
├── .github/
│   ├── actions/sven/           # Reusable composite action (runs sven in CI)
│   └── workflows/
│       ├── ci.yml              # Push/PR to main: lint, cargo test, e2e-basic, smoke build
│       └── release.yml         # Tag push v*.*.*: e2e-basic gate → builds → publish
├── .agents/
│   └── skills/                 # Agent skills for this repo
├── .sven/
│   ├── knowledge/              # Per-crate knowledge files
│   ├── plans/                  # Feature planning docs
│   ├── tasks/                  # Task tracking docs
│   └── workflow/               # Workflow examples
├── Makefile                    # Primary developer interface (see targets below)
├── Cargo.toml                  # Workspace root
├── Cargo.lock
├── Cross.toml                  # cross-rs config for aarch64 builds
├── release.toml                # cargo-release config
└── .sven.yaml                  # sven agent configuration
```

## Workspace crates (`crates/`)

| Crate | Purpose |
|-------|---------|
| `sven-bootstrap` | First-run setup and self-update logic |
| `sven-ci` | Headless/CI runner and output formatters |
| `sven-config` | Config file parsing (`.sven.yaml`, per-project) |
| `sven-core` | Core agent loop, conversation model, provider abstraction |
| `sven-image` | Image attachment support |
| `sven-input` | Stdin/file/pipe input handling |
| `sven-mcp` | MCP (Model Context Protocol) client integration |
| `sven-model` | LLM provider drivers (OpenAI, Anthropic, mock, …) |
| `sven-node` | P2P agent node: task/session/room executors, agent builder, tools |
| `sven-p2p` | libp2p networking layer, wire types, protocol constants |
| `sven-runtime` | Tokio runtime wiring and process lifecycle |
| `sven-tools` | 18-tool toolkit (file, shell, grep, todo, GDB, …) |
| `sven-tui` | Terminal UI (interactive mode) |

## Key Makefile targets

| Target | Description |
|--------|-------------|
| `build` | `cargo build` (debug) |
| `release` | `cargo build --release` |
| `test` | `cargo test --all` |
| `tests/e2e/basic` | Build + run all bats tests in `tests/e2e/basic/` |
| `tests/e2e` | Alias → `tests/e2e/basic` (add more suites here as they are created) |
| `deb` | Build Debian package |
| `fmt` / `check` | Format / Clippy lint |
| `docs` / `docs-pdf` | Build user-guide markdown / PDF |
| `release/patch\|minor\|major` | Runs `tests/e2e/basic` then bumps version via cargo-release |
| `release/build` | Build release artifacts into `dist/` |
| `release/tag` | Create + push annotated git tag |
| `release/publish` | Upload `dist/` to GitHub Release via `gh` |

## CI/release flow

```
Push to main / PR  →  ci.yml
  ├── check        (fmt, clippy, cargo test)
  ├── e2e-basic    (cargo build + make tests/e2e/basic)
  └── build-smoke  (cargo build --release)

Push v*.*.* tag  →  release.yml
  ├── e2e-basic            ← gate: release aborted if any test fails
  ├── build-linux-x86_64   (parallel, needs e2e-basic implicitly via publish)
  ├── build-linux-aarch64  (needs x86_64 for completions)
  ├── build-macos          (continue-on-error: true)
  └── publish              (needs e2e-basic + x86_64 + aarch64)
```

## E2E test suite (`tests/e2e/basic/`)

All tests use `--model mock` (no API key or network required). Hardware-gated tests in
`07_gdb_workflows.bats` (Tiers 2–3) self-skip unless `SVEN_TEST_JLINK=1` is set.

| File | Scope |
|------|-------|
| `01_cli.bats` | CLI flags, subcommands, completions |
| `02_ci_mode.bats` | Headless activation, exit codes, stdin |
| `03_mock_responses.bats` | Mock model match types, tool-call sequences |
| `04_pipeline.bats` | sven-to-sven piping, stdin sources |
| `05_new_tools.bats` | 18-tool toolkit end-to-end |
| `06_headless_enhancements.bats` | Output formats, frontmatter, artifacts, timeouts |
| `07_gdb_workflows.bats` | GDB tools (Tier 1 always runs; Tiers 2–3 need hardware) |
| `08_trace_output.bats` | Trace tokens, tool call/result IDs, pipe chains |
| `helpers.bash` | Shared helpers: `BIN`, `FIXTURES`, `sven_mock`, `assert_output_contains` |

---

## Keeping this skill up to date

**Update this file whenever you make any of the following changes:**

- Add, remove, or rename a crate under `crates/`
- Move or rename a top-level directory or file (e.g. tests, scripts, docs)
- Add a new e2e test file under `tests/e2e/`
- Add a new CI workflow or modify job dependencies in `ci.yml` / `release.yml`
- Add or change a `Makefile` target that affects the build/test/release flow

Edit only the relevant table row or section — do not rewrite sections that have not changed.
