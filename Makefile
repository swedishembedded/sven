CARGO   ?= cargo
# CARGO_HOME selects where registry caches, compiled crates, and binaries are
# stored.  ALL build paths (make, cargo, CI, agent) MUST use the same value
# or Cargo will see different absolute source paths in its fingerprint files
# and perform a full rebuild every time you switch.
#
# Rule: never override this with a different directory between invocations.
# This pins cargo's default (~/.cargo) so make, git hooks, CI and agent shells
# agree even when the caller's environment sets something else.
CARGO_HOME = $(HOME)/.cargo
export CARGO_HOME

# Wrap rustc with sccache when available for shared compilation caching across builds.
# Install with: cargo install sccache
# Override with: make RUSTC_WRAPPER=""  to disable.
ifneq ($(shell command -v sccache 2>/dev/null),)
export RUSTC_WRAPPER = sccache
endif

NPROC := $(shell nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)
CARGO_FLAGS ?= --jobs $(NPROC)
VERSION := $(shell grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
TAG     := v$(VERSION)
DIST    ?= dist
DEB_OUT := target/debian
REPO    := swedishembedded/sven

# Every workspace package (root, crates, xtask, samples): one sed over the
# manifests, so adding a crate adds its build/crate/<package> and
# test/crate/<package> targets. The first `name =` after [package] is the
# package name.
CRATE_PACKAGES := $(shell sed -sn '/^\[package\]/,/^name *=/s/^name *= *"\(.*\)"/\1/p' Cargo.toml crates/*/Cargo.toml samples/*/*/Cargo.toml xtask/Cargo.toml)

# MAKE TARGETS (rule: AGENTS.md, "Make targets"). A target is
# `make <concept>/<subconcept>/.../<action>`: concept first, verb last, one
# segment per directory level of a test or sample hierarchy, and every prefix
# is itself a target so `make <TAB>` descends one level at a time. Where a set
# follows from the tree (gates, e2e suites, crates, samples, papers) it is
# GENERATED as explicit targets by static pattern rules over a computed list -
# never `%` pattern rules, which bash completion cannot see - so adding a
# directory adds a target and nobody maintains a list. `make help` prints the
# tree from the ##@ section and ## doc lines; scripts/gates/check-make-targets.sh
# (check/gates/make-targets) enforces the naming. An area target is either the
# default action for its area (build, test, check, docs, benchmark, papers) or,
# where no default is safe or sensible (release, site, package, hooks, samples,
# test/crate, build/crate), only lists its children and does nothing else.

# ---------------------------------------------------------------------------
##@ Build and package
## Debug and release builds, one crate on demand, and the packages made from them.

## all - alias for build
all: build

## build - default: build/debug (debug build of the CLI + TUI)
build: build/debug

## build/debug - debug build (CLI + TUI)
build/debug:
	$(CARGO) build $(CARGO_FLAGS)

## build/release - optimised release build (CLI + TUI)
build/release:
	$(CARGO) build --release $(CARGO_FLAGS)

## build/crate - lists the per-package build targets; builds nothing
build/crate:
	@bash scripts/make-help.sh build/crate

## build/crate/<package> - debug-build one workspace package (not part of `make build`)
BUILD_CRATE_TARGETS := $(addprefix build/crate/,$(CRATE_PACKAGES))
$(BUILD_CRATE_TARGETS): build/crate/%:
	$(CARGO) build -p $* $(CARGO_FLAGS)

## package - lists the package targets; builds nothing
package:
	@bash scripts/make-help.sh package

## package/dist - build release artifacts for the current platform into dist/
package/dist:
	@bash scripts/release-build.sh --out-dir $(DIST)

## package/deb - default: package/deb/release (output in target/debian/)
package/deb: package/deb/release

## package/deb/debug - build debug Debian package (no optimizations, fast build)
package/deb/debug: build
	@bash scripts/build-deb.sh --binary target/debug/sven --out-dir $(DEB_OUT)

## package/deb/release - build fully optimised release Debian package
package/deb/release: build/release
	@if command -v cargo-deb >/dev/null 2>&1; then \
		echo "Using cargo-deb..."; \
		mkdir -p target/completions; \
		target/release/sven completions bash > target/completions/sven.bash; \
		target/release/sven completions zsh  > target/completions/_sven; \
		target/release/sven completions fish > target/completions/sven.fish; \
		$(CARGO) deb --output $(DEB_OUT) $(CARGO_FLAGS); \
	else \
		echo "cargo-deb not found, using scripts/build-deb.sh..."; \
		bash scripts/build-deb.sh --out-dir $(DEB_OUT); \
	fi

# ---------------------------------------------------------------------------
##@ Verify
## Tests, static checks, formal models and benchmarks. Only `test`, `check` and
## `benchmark` run their default set; the other areas are opt-in.

## test - default: workspace unit + integration tests, then test/features, test/gates
##   (test/e2e, test/harness, test/crate and test/formal are separate)
# --workspace is required: the root Cargo.toml defines both a [workspace] and
# its own [package] (the `sven` binary), so plain `cargo test` here tests
# ONLY that root package - every other crate's own unit tests (sven-core,
# sven-tui, sven-executors, ...) get compiled as library dependencies but
# their #[cfg(test)] modules never run. Confirmed: without --workspace this
# silently skipped 2800+ tests across 26 crates while still exiting 0.
test:
	$(CARGO) test --workspace $(CARGO_FLAGS)
	$(MAKE) test/features
	$(MAKE) test/gates

## test/features - the feature-off configurations' own tests (see check/features)
test/features:
	@for p in $(FEATURE_OFF_PACKAGES); do \
		echo "test -p $$p --no-default-features"; \
		$(CARGO) test -q -p $$p --no-default-features $(CARGO_FLAGS) || exit 1; \
	done

## test/gates - self-tests of the text gates on scratch trees and Makefiles (bats, no build)
##   Proves the gates fail on bad input, not only pass on this tree.
test/gates:
	bats tests/gates/

## test/crate - lists the per-package test targets; tests nothing
test/crate:
	@bash scripts/make-help.sh test/crate

## test/crate/<package> - run one workspace package's tests (not part of `make test`)
TEST_CRATE_TARGETS := $(addprefix test/crate/,$(CRATE_PACKAGES))
$(TEST_CRATE_TARGETS): test/crate/%:
	$(CARGO) test -p $* $(CARGO_FLAGS)

# End-to-end suites: tests/e2e/<suite>/<NN_name>.bats becomes
# test/e2e/<suite> (the whole directory) and test/e2e/<suite>/<name>.
E2E_SUITES := $(sort $(patsubst tests/e2e/%/,%,$(dir $(wildcard tests/e2e/*/*.bats))))
E2E_SUITE_TARGETS := $(addprefix test/e2e/,$(E2E_SUITES))
E2E_TEST_TARGETS := $(shell ls tests/e2e/*/*.bats 2>/dev/null | sed -E 's|^tests/e2e/([^/]+)/[0-9]+_(.*)\.bats$$|test/e2e/\1/\2|; s|_|-|g')

## test/e2e - default: every end-to-end suite (requires bats-core)
test/e2e: $(E2E_SUITE_TARGETS)

## test/e2e/<suite> - run one end-to-end suite (requires bats-core), e.g. test/e2e/basic
##   The basic suite uses the mock model; hardware-gated tests in 07 self-skip
##   without SVEN_TEST_JLINK=1.
$(E2E_SUITE_TARGETS): test/e2e/%: build
	bats tests/e2e/$*/

## test/e2e/<suite>/<test> - run one bats file of a suite, e.g. test/e2e/basic/cli
$(E2E_TEST_TARGETS): test/e2e/%: build
	bats tests/e2e/$(dir $*)[0-9]*_$(subst -,_,$(notdir $*)).bats

## test/e2e/capture - a real model server answers an agent run; the ATIF record must equal the server's account
##   Needs BRAIN_BIN (a brain binary) and MODELS_DIR (holding Qwen/Qwen3-0.6B); DEVICE is
##   brain's device name (default cpu, which takes minutes: name a GPU for the short run).
##   Exit 3 means a prerequisite is missing and nothing was tested, never a pass.
test/e2e/capture:
	$(CARGO) build -p sven-sdk --example capture_run
	scripts/e2e/capture.sh "$(BRAIN_BIN)" "$${CARGO_TARGET_DIR:-target}/debug/examples/capture_run" "$(MODELS_DIR)" "$(or $(DEVICE),cpu)"

## test/e2e/basic/deferred - the basic suite with every invocation run under --tool-disclosure deferred (requires bats-core)
test/e2e/basic/deferred: build
	SVEN_E2E_TOOL_DISCLOSURE=deferred bats tests/e2e/basic/

## test/e2e/basic/instructions-deferred - the basic suite with every invocation run under --skill-disclosure, --subagent-disclosure and --knowledge-disclosure deferred (requires bats-core)
test/e2e/basic/instructions-deferred: build
	SVEN_E2E_INSTRUCTION_DISCLOSURE=deferred bats tests/e2e/basic/

## test/e2e/basic/demote - the basic suite with every invocation run under --context-assembly demote (requires bats-core)
test/e2e/basic/demote: build
	SVEN_E2E_CONTEXT_ASSEMBLY=demote bats tests/e2e/basic/

# Harness suites run capped (HARNESS_JOBS build jobs, 4 test threads) so they
# stay light on a shared machine. Each suite's cargo invocation differs, so
# they are written out rather than generated.
HARNESS_JOBS ?= 4
HARNESS_TEST = $(CARGO) test --jobs $(HARNESS_JOBS)
HARNESS_TARGETS := test/harness/spec test/harness/transform test/harness/runtime \
                   test/harness/sdk test/harness/testkit test/harness/samples test/harness/cli

## test/harness - default: every harness suite below, on mock models only (opt-in: not part of `make test`)
test/harness: $(HARNESS_TARGETS)

## test/harness/spec - harness document language: sven-harness-spec
test/harness/spec:
	$(HARNESS_TEST) -p sven-harness-spec -- --test-threads=4

## test/harness/transform - harness transforms: sven-harness-transform
test/harness/transform:
	$(HARNESS_TEST) -p sven-harness-transform -- --test-threads=4

## test/harness/runtime - harness runtime: sven-harness-runtime
test/harness/runtime:
	$(HARNESS_TEST) -p sven-harness-runtime --features transform -- --test-threads=4

## test/harness/sdk - harness through the SDK: run, replay, authz, children and the 1000-run stress test
test/harness/sdk:
	$(HARNESS_TEST) -p sven-sdk --test harness_run --test harness_replay --test harness_authz --test harness_host_policy --test harness_children --test harness_stress -- --test-threads=4

## test/harness/testkit - harness test kit: sven-harness-testkit
test/harness/testkit:
	$(HARNESS_TEST) -p sven-harness-testkit -- --test-threads=4

## test/harness/samples - the SDK runner sample over the harness samples
test/harness/samples:
	$(HARNESS_TEST) -p sample-harnesses-sdk-runner -- --test-threads=4

## test/harness/cli - the `sven harness` command line
test/harness/cli:
	$(HARNESS_TEST) -p sven --test harness_cli --test harness_production --test harness_views --test harness_isolation -- --test-threads=4

## test/harness/local-model - OPT-IN: the live-local case of the ticket-routing sample against a server you already run
##   Needs ENDPOINT=<url> (and optionally MODEL=<provider/name>) on the make command
##   line; refuses with exit 2 without ENDPOINT. Nothing is pulled or started.
##   Not part of test/harness.
MODEL ?= ollama/qwen3:8b
ENDPOINT ?=
LOCAL_MODEL_SAMPLE = samples/harnesses/ticket-routing
test/harness/local-model:
	@test -n "$(ENDPOINT)" || { echo "test/harness/local-model: pass ENDPOINT=<url> of a model server you are already running (optional MODEL=<provider/name>); nothing is pulled or started"; exit 2; }
	$(CARGO) run --jobs $(HARNESS_JOBS) -p sven -- harness test $(LOCAL_MODEL_SAMPLE) --case live-local --model $(MODEL) --model-endpoint $(ENDPOINT)

## test/e2e/delegation - lists the delegation checks; runs nothing
test/e2e/delegation:
	@bash scripts/make-help.sh test/e2e/delegation

## test/e2e/delegation/local-model - OPT-IN: a real delegated coding task on a model server you already run
##   A scripted parent delegates the repair of a failing test, through the task tool, to a sub-agent on a
##   DeepSeek-compatible or brain endpoint; the repository's own tests judge the result. Needs
##   ENDPOINT=<url> and MODEL=<name> (and optionally KEY_FILE=<file>, DRIVER=<id>, default deepseek, and
##   NO_THINKING=1 for a Qwen3 behind brain); refuses
##   with exit 2 without them. Nothing is pulled or started. Not part of any default target.
DRIVER ?= deepseek
KEY_FILE ?=
test/e2e/delegation/local-model: build
	@test -n "$(ENDPOINT)" || { echo "test/e2e/delegation/local-model: pass ENDPOINT=<url> MODEL=<name> of a model server you are already running"; exit 2; }
	bash tests/e2e/delegation/local-model.sh --endpoint $(ENDPOINT) --model $(MODEL) --driver $(DRIVER) $(if $(KEY_FILE),--key-file $(KEY_FILE)) $(if $(NO_THINKING),--no-thinking)

## test/formal - machine-check the TLA+ models of the HSM kernel (needs a JRE; not part of `make test`)
# Not part of `make check`: the models need a JRE and a pinned 2 MB download,
# and `check` is the inner loop. Several of them are EXPECTED to fail -- they
# pin down designs the engine rejected -- so the runner compares each model
# against the outcome the suite declares for it rather than against TLC's exit
# code. See formal/README.md.
test/formal:
	bash formal/run-tla.sh

## check - default: check/fmt, check/gates, check/arch, check/features, check/clippy, check/doc
# In that order on purpose: a rustfmt drift, a baked-in machine path or an
# illegal crate edge should fail in seconds, not after a full workspace lint.
# Every gate below is reachable from HERE, not only from an installed git
# hook -- a check that lives only in `make hooks/install` enforces nothing on a
# fresh clone that never ran it.
check: check/fmt check/gates check/arch check/features check/clippy check/doc

## check/fmt - rustfmt shape, checked not applied (`make fmt` fixes it)
# First, because it is the cheapest thing that can fail and the least
# interesting to argue about. Briefly a script with an exception list, while
# a concurrent change held the last few unformatted files; that list reached
# zero and the script went with it.
check/fmt:
	$(CARGO) fmt --all -- --check

## check/features - the feature-off configurations build and lint clean: the
##   minimal assemblies (sven-bootstrap and sven-sdk without default features)
##   and the crates with feature-gated transports and media. The workspace
##   build unifies every feature on, so nothing else would ever compile them.
##   One package per run: features unify across the packages of a run.
FEATURE_OFF_PACKAGES = sven-model-drivers sven-tools-fs sven-tools-gdb sven-bootstrap sven-sdk
check/features:
	@for p in $(FEATURE_OFF_PACKAGES); do \
		echo "clippy -p $$p --no-default-features"; \
		$(CARGO) clippy -q -p $$p --no-default-features --all-targets -- -D warnings || exit 1; \
	done

## check/clippy - lint the workspace, warnings are errors
check/clippy:
	$(CARGO) clippy --workspace --all-targets $(CARGO_FLAGS) -- -D warnings

## check/doc - rustdoc builds for the workspace with every intra-doc link
##   resolving (rustdoc stops documenting a crate at its first broken link, so
##   an unchecked one hides every later one)
check/doc:
	RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" $(CARGO) doc --no-deps --workspace $(CARGO_FLAGS)

## check/arch - enforce architecture.toml (crate tiers, dead deps, file-size ratchet,
##   and the `minimal` cargo feature profile's forbidden-crate list)
check/arch:
	$(CARGO) run -q -p xtask -- arch
	$(CARGO) run -q -p xtask -- arch --profile minimal

# Text gates: scripts/gates/check-<gate>.sh is the target check/gates/<gate>.
# A gate that needs flags would be an explicit override of its generated rule.
GATE_NAMES := $(patsubst scripts/gates/check-%.sh,%,$(wildcard scripts/gates/check-*.sh))
GATE_TARGETS := $(addprefix check/gates/,$(GATE_NAMES))

## check/gates - default: every text gate below. Together they cost well under a second.
check/gates: $(GATE_TARGETS)

## check/gates/<gate> - run scripts/gates/check-<gate>.sh alone (one target per script)
##   Each script's header says what it enforces.
$(GATE_TARGETS): check/gates/%: scripts/gates/check-%.sh
	bash $<

# ── Benchmark targets ─────────────────────────────────────────────────────────
# Run sven against Terminal-Bench 2.0 via Harbor and generate a Markdown report.
#
# Prerequisites:
#   pip install -r benchmarks/requirements.txt   (installs harbor + jinja2)
#   OPENROUTER_API_KEY                           (required for the default free model)
#   ANTHROPIC_API_KEY / OPENAI_API_KEY / ...       (whichever key the model needs)
#
# Override options:
#   MODEL                - model string (default: openrouter/openrouter/free)
#                          e.g.: make benchmark MODEL=anthropic/claude-sonnet-4-6
#   HARBOR_CONCURRENCY   - parallel Harbor workers (default: 4)
#   SVEN_BENCH_TIMEOUT   - per-task timeout in seconds (default: 1800)
#   HARBOR_FLAGS         - any extra flags passed verbatim to 'harbor run'

HARBOR_CONCURRENCY  ?= 4
SVEN_BENCH_TIMEOUT  ?= 1800
HARBOR_FLAGS        ?=
MUSL_TARGET         := x86_64-unknown-linux-musl
SVEN_BIN_PATH       := $(CURDIR)/target/$(MUSL_TARGET)/release/sven
MODEL               ?= openrouter/openrouter/free

## benchmark/build - build a static musl binary for use inside benchmark containers
benchmark/build:
	@if ! rustup target list --installed | grep -q "$(MUSL_TARGET)"; then \
	    echo "Adding Rust target $(MUSL_TARGET)..."; \
	    rustup target add $(MUSL_TARGET); \
	fi
	@if ! command -v musl-gcc >/dev/null 2>&1; then \
	    echo "musl-gcc not found. Installing musl-tools..."; \
	    sudo apt-get install -y musl-tools; \
	fi
	$(CARGO) build --release --target $(MUSL_TARGET) $(CARGO_FLAGS)
	@echo "Static binary: $(SVEN_BIN_PATH)"
	@file $(SVEN_BIN_PATH)

## benchmark - default: benchmark/build, benchmark/terminal-bench, then a report
benchmark: benchmark/build benchmark/terminal-bench
	@python3 benchmarks/report.py target/benchmark > target/benchmark/report.md
	@echo ""
	@echo "Report written to target/benchmark/report.md"
	@echo "Preview:"; echo ""; head -40 target/benchmark/report.md

## benchmark/terminal-bench - run Terminal-Bench 2.0 via Harbor (requires harbor)
benchmark/terminal-bench: benchmark/build
	@command -v harbor >/dev/null 2>&1 || { \
	    echo "harbor not found."; \
	    echo "Install it with: pip install -r benchmarks/requirements.txt"; \
	    exit 1; }
	@mkdir -p target/benchmark
	@# Create a 'docker' shim that calls 'sudo docker', placed first on PATH
	@# so Harbor's hardcoded "docker compose ..." invocations go through sudo.
	@mkdir -p target/benchmark/.bin
	@printf '#!/bin/sh\nexec sudo -E docker "$$@"\n' > target/benchmark/.bin/docker
	@chmod +x target/benchmark/.bin/docker
	PATH=$(CURDIR)/target/benchmark/.bin:$$PATH \
	SVEN_BIN_PATH=$(SVEN_BIN_PATH) \
	SVEN_BENCH_TIMEOUT=$(SVEN_BENCH_TIMEOUT) \
	SVEN_MODEL=$(MODEL) \
	harbor run \
	    -d terminal-bench@2.0 \
	    --agent-import-path benchmarks.sven_agent:SvenInstalledAgent \
	    -o target/benchmark/terminal-bench \
	    -n $(HARBOR_CONCURRENCY) \
	    -k 1 \
	    $(HARBOR_FLAGS)

## test/benchmarks - the Harbor adapter's pure helpers (python3 unittest; imports no harbor)
test/benchmarks:
	python3 -I -m unittest discover -s benchmarks -p 'test_*.py'

## benchmark/report - regenerate report from existing result files
benchmark/report:
	@[ -d target/benchmark ] || { \
	    echo "No benchmark results found. Run 'make benchmark' first."; \
	    exit 1; }
	@python3 benchmarks/report.py target/benchmark > target/benchmark/report.md
	@cat target/benchmark/report.md

## benchmark/ledger - token-ledger baseline: replay fixed scenarios against the mock provider and write per-scenario ledger summaries plus a report naming the sven commit; OUT=DIR chooses where (default: outside the repository), DISCLOSURE=full|deferred picks the tool disclosure mode measured, INSTRUCTIONS=full|deferred the skill, subagent and knowledge listings, ASSEMBLY=off|demote the context assembly
benchmark/ledger: build
	@bash benchmarks/ledger/run.sh $(if $(OUT),--out $(OUT),) $(if $(DISCLOSURE),--tool-disclosure $(DISCLOSURE),) $(if $(INSTRUCTIONS),--instruction-disclosure $(INSTRUCTIONS),) $(if $(ASSEMBLY),--context-assembly $(ASSEMBLY),)

## benchmark/ablation - ablation benchmark: every suite under every variant of benchmarks/ablation/matrix.yaml for several seeds with a scripted model, also with a model that fetches what shortened results left out; raw records and a JSON + Markdown report go to OUT (default: outside the repository); SEEDS=N, SUITES=a,b, VARIANTS=A,B, BRAIN_URL=URL and TRAINED_MODEL=ID narrow or extend a run
benchmark/ablation: build
	@cargo build $(CARGO_FLAGS) -p sven-ablation
	@bash benchmarks/ablation/run --out $(or $(OUT),$(or $(TMPDIR),/tmp)/sven-ablation) --fallback-probe --recovery \
	    $(if $(SEEDS),--seeds $(SEEDS),) $(if $(SUITES),--suites $(SUITES),) \
	    $(if $(VARIANTS),--variants $(VARIANTS),) $(if $(BRAIN_URL),--brain-url $(BRAIN_URL),) \
	    $(if $(TRAINED_MODEL),--trained-model $(TRAINED_MODEL),)

# ---------------------------------------------------------------------------
##@ Documentation
## The documentation publication, the user manual and the papers.

NODE ?= node
DOCS_PUBLISHER ?= tools/docs-publisher/docs-publisher.mjs
# The publication in build/docs is what the website imports. The manual and the
# PDF are cut from it, in the navigation order of docs/manifest.yaml.
DOCS_OUT := build/docs
MANUAL_SECTIONS ?= Getting started,Guides,Concepts,Reference,Examples
MANUAL_MD := target/docs/sven-user-guide.md

# The vendored publisher is pinned by its checksum unless DOCS_PUBLISHER is
# overridden on the command line (to try a newer build of it).
_docs-publisher-pin:
	@$(if $(filter command line,$(origin DOCS_PUBLISHER)),true,\
	  cd tools/docs-publisher && sha256sum -c docs-publisher.mjs.sha256 >/dev/null)

## docs - default: build the single-file user manual (target/docs/sven-user-guide.md); MANUAL_SECTIONS selects the navigation sections it contains
docs: docs/build
	@$(NODE) scripts/docs-manual.mjs --dir $(DOCS_OUT) --out $(MANUAL_MD) --sections "$(MANUAL_SECTIONS)"

## docs/build - build the documentation publication in build/docs (offline; no Rust build, no downloads)
docs/build: _docs-publisher-pin
	@$(NODE) $(DOCS_PUBLISHER) build --root . --manifest docs/manifest.yaml --out $(DOCS_OUT)
	@$(NODE) $(DOCS_PUBLISHER) verify --dir $(DOCS_OUT)

## docs/check - build twice and compare, check coverage, links and anchors, then cut the manual from the result
docs/check: _docs-publisher-pin
	@$(NODE) $(DOCS_PUBLISHER) check --root . --manifest docs/manifest.yaml
	@$(MAKE) --no-print-directory docs

## docs/clean - remove the generated documentation output
docs/clean:
	rm -rf $(DOCS_OUT) target/docs/sven-user-guide.md target/docs/sven-user-guide.pdf

## docs/pdf - build the PDF user manual (requires pandoc + a LaTeX distribution)
docs/pdf: docs
	@command -v pandoc >/dev/null 2>&1 || { \
		echo "Error: pandoc is not installed."; \
		echo "Install it with: sudo apt install pandoc texlive-xetex texlive-fonts-recommended"; \
		exit 1; \
	}
	pandoc $(MANUAL_MD) \
		--metadata-file=docs/metadata.yaml \
		--resource-path=$(DOCS_OUT) \
		--pdf-engine=xelatex \
		--toc \
		--toc-depth=2 \
		--number-sections \
		--highlight-style=tango \
		-o target/docs/sven-user-guide.pdf
	@echo "PDF guide written to target/docs/sven-user-guide.pdf"

PAPER_DIRS := $(patsubst %/Makefile,%,$(wildcard papers/*/Makefile))
PAPER_TARGETS := $(addsuffix /pdf,$(PAPER_DIRS))

## papers - default: papers/pdf (every paper's PDF; see papers/paper.mk)
papers: papers/pdf

## papers/pdf - build every paper's PDF
papers/pdf: $(PAPER_TARGETS)

## papers/<name> - build that paper's PDF (same as papers/<name>/pdf)
$(PAPER_DIRS): %: %/pdf

## papers/<name>/pdf - build one paper's PDF
$(PAPER_TARGETS): papers/%/pdf:
	$(MAKE) -C papers/$*

# ---------------------------------------------------------------------------
##@ Release and site
## Everything here changes something outside the working tree (tags, GitHub,
## the production site). The `release` and `site` areas only list their targets.

## release - lists the release targets; does nothing (package/dist builds the artifacts, build/release the binary)
release:
	@bash scripts/make-help.sh release

## release/tag - create an annotated git tag for current version and push
release/tag:
	@echo "Current version: $(VERSION)"
	@if git rev-parse "$(TAG)" >/dev/null 2>&1; then \
	    echo "error: tag $(TAG) already exists. Bump the version first."; \
	    exit 1; \
	fi
	@if [ -n "$$(git status --porcelain)" ]; then \
	    echo "error: working tree is dirty. Commit your changes before tagging."; \
	    git status --short; \
	    exit 1; \
	fi
	@echo "Tagging $(TAG) on $$(git rev-parse --short HEAD)..."
	@git tag -a "$(TAG)" -m "Release $(TAG)"
	@git push origin "$(TAG)"
	@echo ""
	@echo "Tag $(TAG) pushed - GitHub Actions release workflow will start."
	@echo "Watch: https://github.com/$(REPO)/actions"

## release/publish - create a GitHub Release and upload dist/ artifacts via gh CLI
release/publish:
	@if [ -z "$$(ls $(DIST)/sven-* 2>/dev/null)" ]; then \
	    echo "error: no artifacts found in $(DIST)/"; \
	    echo "       Run 'make package/dist' first."; \
	    exit 1; \
	fi
	@if ! command -v gh >/dev/null 2>&1; then \
	    echo "error: gh CLI not found."; \
	    echo "       Install from https://cli.github.com/"; \
	    exit 1; \
	fi
	@echo "Publishing $(TAG) to github.com/$(REPO)..."
	@ARTIFACTS=$$(find $(DIST) -maxdepth 1 -type f | sort | tr '\n' ' '); \
	gh release create "$(TAG)" \
	    --repo "$(REPO)" \
	    --title "sven $(TAG)" \
	    --generate-notes \
	    $$ARTIFACTS
	@echo ""
	@echo "Release: https://github.com/$(REPO)/releases/tag/$(TAG)"

## release/patch - bump patch version (0.1.x to 0.1.x+1), tag, push; triggers CI
release/patch: _require-cargo-release test test/e2e/basic
	@cargo release patch -p sven --execute --no-confirm
	@git push origin main --follow-tags

## release/minor - bump minor version (0.x.0 to 0.x+1.0), tag, push; triggers CI
release/minor: _require-cargo-release test test/e2e/basic
	@cargo release minor -p sven --execute --no-confirm
	@git push origin main --follow-tags

## release/major - bump major version (x.0.0 to x+1.0.0), tag, push; triggers CI
release/major: _require-cargo-release test test/e2e/basic
	@cargo release major -p sven --execute --no-confirm
	@git push origin main --follow-tags

## site - lists the site targets; does nothing (site/publish uploads to production)
site:
	@bash scripts/make-help.sh site

## site/build - build the sven-site Docker image
site/build:
	$(MAKE) -C site build

## site/publish - upload the Docker image to swedishembedded.com via SSH
site/publish:
	$(MAKE) -C site publish

## site/serve - serve the site locally on http://localhost:3000
site/serve:
	$(MAKE) -C site serve

# Install cargo-release if not already present.
_require-cargo-release:
	@if ! cargo release --version >/dev/null 2>&1; then \
	    echo "cargo-release not found - installing..."; \
	    cargo install cargo-release --locked; \
	fi

# ---------------------------------------------------------------------------
##@ Samples and tooling
## Standalone sample applications, git hooks, formatting and housekeeping.

# A sample is a standalone APPLICATION built on the public `sven-sdk` facade,
# living in samples/<category>/<name>/. Unlike sibling repos, samples here ARE
# built and tested with the workspace: one depends only on the SDK, so it costs
# almost nothing, and excluding it is how a sample rots into something that no
# longer compiles against the framework it demonstrates.
#
#   make samples
#   make samples/study/svf/build
#   make samples/study/svf/run ARGS="--errata real"
#
# `build` and `run` are separate and `run` does NOT invoke cargo: a built sample
# is an ordinary binary, and a `run` that shelled out to cargo would make the
# toolchain a runtime dependency of every demonstration. The cost is that `run`
# cannot notice a stale binary, so it prints what it executes and when that file
# was built.
SAMPLE_PROFILE ?= release
SAMPLE_CARGO_FLAGS = $(if $(filter release,$(SAMPLE_PROFILE)),--release,)
ARGS ?=

# samples/<a>/<b> -> package `sample-<a>-<b>`. check/gates/samples enforces it.
sample_pkg = sample-$(subst /,-,$(1))

## samples - lists the samples and how to build and run them; builds nothing
samples:
	@echo "Samples (make samples/<path>/{build,run}):"
	@find samples -mindepth 3 -maxdepth 3 -name Cargo.toml 2>/dev/null \
		| sed -E 's|^samples/(.*)/Cargo\.toml$$|\1|' | sort | while read -r s; do \
			printf '  %-28s %s\n' "$$s" \
				"$$(sed -n 's/^description = "\(.*\)"/\1/p' "samples/$$s/Cargo.toml" | head -1)"; \
		done

SAMPLE_PATHS := $(patsubst samples/%/Cargo.toml,%,$(wildcard samples/*/*/Cargo.toml))
SAMPLE_CATEGORIES := $(sort $(patsubst %/,%,$(dir $(SAMPLE_PATHS))))
SAMPLE_CATEGORY_TARGETS := $(addprefix samples/,$(SAMPLE_CATEGORIES))
SAMPLE_NAME_TARGETS := $(addprefix samples/,$(SAMPLE_PATHS))
SAMPLE_BUILD_TARGETS := $(addsuffix /build,$(SAMPLE_NAME_TARGETS))
SAMPLE_RUN_TARGETS := $(addsuffix /run,$(SAMPLE_NAME_TARGETS))

## samples/<category> - lists the samples of a category; builds nothing
$(SAMPLE_CATEGORY_TARGETS):
	@bash scripts/make-help.sh $@

## samples/<category>/<name> - lists a sample's actions (build, run); does nothing
$(SAMPLE_NAME_TARGETS):
	@bash scripts/make-help.sh $@

## samples/<category>/<name>/build - build one sample, e.g. samples/study/svf/build
$(SAMPLE_BUILD_TARGETS): samples/%/build:
	$(CARGO) build $(SAMPLE_CARGO_FLAGS) -p $(call sample_pkg,$*)

## samples/<category>/<name>/run - run a built sample; ARGS="..." is passed through
$(SAMPLE_RUN_TARGETS): samples/%/run:
	@bin="target/$(SAMPLE_PROFILE)/$(call sample_pkg,$*)"; \
	if [ ! -x "$$bin" ]; then \
		echo "error: $$bin does not exist - run 'make samples/$*/build' first"; exit 1; \
	fi; \
	echo "running $$bin (built $$(date -r "$$bin" '+%Y-%m-%d %H:%M:%S'))"; \
	exec "$$bin" $(ARGS)

## hooks - lists the hook targets; installs nothing
hooks:
	@bash scripts/make-help.sh hooks

## hooks/install - install this repo's git pre-commit hooks (one-time per clone;
##   writes outside version control, so it is never automatic).
##   Prefers the `pre-commit` framework, since .pre-commit-config.yaml
##   carries the same gates plus rustfmt/clippy/ASCII hygiene; falls
##   back to scripts/hooks/pre-commit, which needs only bash and git.
##   Refuses to clobber a hook it did not write.
hooks/install:
	@if [ -e .git/hooks/pre-commit ] && ! grep -q 'scripts/gates/check-no-machine-paths.sh' .git/hooks/pre-commit \
	   && ! grep -q 'pre-commit' .git/hooks/pre-commit; then \
	    echo "error: .git/hooks/pre-commit exists and was not installed by this repo."; \
	    echo "       Inspect it, then remove it and re-run 'make hooks/install'."; \
	    exit 1; \
	fi
	@if command -v pre-commit >/dev/null 2>&1; then \
	    pre-commit install; \
	    echo "installed: .git/hooks/pre-commit (pre-commit framework, .pre-commit-config.yaml)"; \
	else \
	    install -m 755 scripts/hooks/pre-commit .git/hooks/pre-commit; \
	    echo "installed: .git/hooks/pre-commit (standalone; 'pip install pre-commit' for the full set)"; \
	fi

## fmt - format all code (the bare leaf: one action; `check/fmt` is its checking twin)
fmt:
	$(CARGO) fmt --all

## clean - remove build artefacts
clean:
	$(CARGO) clean
	rm -rf target/debian target/debian-staging target/completions target/docs target/benchmark $(DIST)

## help - show every target as a tree, grouped by section
help:
	@bash scripts/make-help.sh

# ---------------------------------------------------------------------------
##@ Generated files
## Committed files that are derived from other sources; each rewrites them in place.

## generate - lists the generators; rewrites nothing
generate:
	@bash scripts/make-help.sh generate

## generate/harness - lists the harness generators; rewrites nothing
generate/harness:
	@bash scripts/make-help.sh generate/harness

## generate/harness/sample-bundle - rewrite the custom-helper sample's bundle.yaml from its document
##   The harness runtime's sample_bundle test (test/harness/runtime) only checks it.
generate/harness/sample-bundle:
	$(HARNESS_TEST) -p sven-harness-runtime --features transform --test sample_bundle -- --bless

## generate/config - lists the configuration generators; rewrites nothing
generate/config:
	@bash scripts/make-help.sh generate/config

## generate/config/golden - rewrite the golden records of the configuration fixtures (review the diff)
##   The frontend's config_golden test (test/crate/sven-frontend) only checks them.
generate/config/golden:
	$(CARGO) test --jobs $(HARNESS_JOBS) -p sven-frontend --test config_golden -- --bless

# Declared last so every generated list above is already defined.
.PHONY: all build build/debug build/release $(BUILD_CRATE_TARGETS) build/crate \
        test test/features test/gates test/e2e test/e2e/basic/deferred test/e2e/basic/instructions-deferred test/e2e/basic/demote $(E2E_SUITE_TARGETS) $(E2E_TEST_TARGETS) \
        test/benchmarks test/e2e/delegation test/e2e/delegation/local-model test/harness test/harness/local-model $(HARNESS_TARGETS) \
        generate generate/harness generate/harness/sample-bundle \
        generate/config generate/config/golden \
        test/crate $(TEST_CRATE_TARGETS) test/formal \
        check check/fmt check/features check/clippy check/doc check/arch \
        check/gates $(GATE_TARGETS) \
        fmt clean help \
        package package/dist package/deb package/deb/debug package/deb/release \
        docs docs/build docs/check docs/clean docs/pdf _docs-publisher-pin papers papers/pdf $(PAPER_DIRS) $(PAPER_TARGETS) \
        samples hooks hooks/install $(SAMPLE_CATEGORY_TARGETS) $(SAMPLE_NAME_TARGETS) \
        $(SAMPLE_BUILD_TARGETS) $(SAMPLE_RUN_TARGETS) \
        release release/tag release/publish release/patch release/minor release/major \
        _require-cargo-release \
        site site/build site/publish site/serve \
        benchmark benchmark/build benchmark/terminal-bench benchmark/report benchmark/ledger benchmark/ablation
