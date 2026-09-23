CARGO   ?= cargo
# CARGO_HOME selects where registry caches, compiled crates, and binaries are
# stored.  ALL build paths (make, cargo, CI, agent) MUST use the same value
# or Cargo will see different absolute source paths in its fingerprint files
# and perform a full rebuild every time you switch.
#
# Rule: never override this with a different directory between invocations.
# The ?= lets the caller / CI override it; the default is the conventional
# writable home.  If the system CARGO_HOME is read-only (a root-owned rustup
# install, for example) set the variable before calling make:
#   CARGO_HOME=~/.cargo make build/debug
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

.PHONY: all build build/debug build/release release test tests/e2e tests/e2e/basic deb deb/debug deb/release clean help fmt \
        check check/fmt check/clippy check/gates check/paths check/deps check/samples check/arch hooks/install docs docs-pdf \
        formal \
        samples/list \
        release/build release/publish release/tag \
        release/patch release/minor release/major \
        _require-cargo-release \
        site/build site/publish site/serve \
        benchmark benchmark/build benchmark/terminal-bench benchmark/report

all: build

## build          - alias for build/debug (debug build CLI + TUI)
build: build/debug

## release        - alias for build/release (optimised release build CLI + TUI)
release: build/release

## build/debug    - debug build (CLI + TUI)
build/debug:
	$(CARGO) build $(CARGO_FLAGS)

## build/release  - optimised release build (CLI + TUI)
build/release:
	$(CARGO) build --release $(CARGO_FLAGS)

## test      - run all unit + integration tests
# --workspace is required: the root Cargo.toml defines both a [workspace] and
# its own [package] (the `sven` binary), so plain `cargo test` here tests
# ONLY that root package - every other crate's own unit tests (sven-core,
# sven-tui, sven-executors, ...) get compiled as library dependencies but
# their #[cfg(test)] modules never run. Confirmed: without --workspace this
# silently skipped 2800+ tests across 26 crates while still exiting 0.
test:
	$(CARGO) test --workspace $(CARGO_FLAGS)

## tests/e2e/basic - run all basic end-to-end tests (requires bats-core)
## All tests use the mock model; hardware-gated tests in 07 self-skip without SVEN_TEST_JLINK=1.
tests/e2e/basic: build
	bats tests/e2e/basic/

## tests/e2e - run all end-to-end test suites (requires bats-core)
tests/e2e: tests/e2e/basic

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

## benchmark/build        - build a static musl binary for use inside benchmark containers
benchmark/build:
	@if ! rustup target list --installed | grep -q "$(MUSL_TARGET)"; then \
	    echo "Adding Rust target $(MUSL_TARGET)..."; \
	    sudo -E env PATH=$$PATH rustup target add $(MUSL_TARGET); \
	fi
	@if ! command -v musl-gcc >/dev/null 2>&1; then \
	    echo "musl-gcc not found. Installing musl-tools..."; \
	    sudo apt-get install -y musl-tools; \
	fi
	$(CARGO) build --release --target $(MUSL_TARGET) $(CARGO_FLAGS)
	@echo "Static binary: $(SVEN_BIN_PATH)"
	@file $(SVEN_BIN_PATH)

## benchmark              - build static binary, run all benchmarks, generate report
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

## benchmark/report        - regenerate report from existing result files
benchmark/report:
	@[ -d target/benchmark ] || { \
	    echo "No benchmark results found. Run 'make benchmark' first."; \
	    exit 1; }
	@python3 benchmarks/report.py target/benchmark > target/benchmark/report.md
	@cat target/benchmark/report.md

# ── Debian package targets ─────────────────────────────────────────────────────
## deb         - build optimised release Debian package (default; output in target/debian/)
deb: deb/release

## deb/debug   - build debug Debian package (no optimizations, fast build)
deb/debug: build
	@bash scripts/build-deb.sh --binary target/debug/sven --out-dir $(DEB_OUT)

## deb/release - build fully optimised release Debian package
deb/release: release
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

## docs      - build single-file markdown user guide (output: target/docs/sven-user-guide.md)
docs:
	@mkdir -p target/docs
	@printf '' > target/docs/sven-user-guide.md
	@for f in docs/[0-9][0-9]-*.md docs/providers.md; do \
		if [ -f "$$f" ]; then \
			cat "$$f" >> target/docs/sven-user-guide.md; \
			printf '\n---\n\n' >> target/docs/sven-user-guide.md; \
		fi; \
	done
	@echo "User guide written to target/docs/sven-user-guide.md"

## docs-pdf  - build PDF user guide (requires pandoc + a LaTeX distribution)
docs-pdf: docs
	@command -v pandoc >/dev/null 2>&1 || { \
		echo "Error: pandoc is not installed."; \
		echo "Install it with: sudo apt install pandoc texlive-xetex texlive-fonts-recommended"; \
		exit 1; \
	}
	pandoc target/docs/sven-user-guide.md \
		--metadata-file=docs/metadata.yaml \
		--resource-path=docs \
		--pdf-engine=xelatex \
		--toc \
		--toc-depth=2 \
		--number-sections \
		--highlight-style=tango \
		-o target/docs/sven-user-guide.pdf
	@echo "PDF guide written to target/docs/sven-user-guide.pdf"

## fmt       - format all code
fmt:
	$(CARGO) fmt --all

## formal    - machine-check the TLA+ models of the HSM kernel (needs a JRE)
# Not part of `make check`: the models need a JRE and a pinned 2 MB download,
# and `check` is the inner loop. Several of them are EXPECTED to fail -- they
# pin down designs the engine rejected -- so the runner compares each model
# against the outcome the suite declares for it rather than against TLC's exit
# code. See formal/README.md.
formal:
	bash formal/run-tla.sh

## check     - formatting, text gates, architecture ratchet, then clippy
# In that order on purpose: a rustfmt drift, a baked-in machine path or an
# illegal crate edge should fail in seconds, not after a full workspace lint.
# Every gate below is reachable from HERE, not only from an installed git
# hook -- a check that lives only in `make hooks/install` enforces nothing on a
# fresh clone that never ran it.
check: check/fmt check/gates check/arch check/clippy

## check/fmt - rustfmt shape, checked not applied (`make fmt` fixes it)
# First, because it is the cheapest thing that can fail and the least
# interesting to argue about. Briefly a script with an exception list, while
# a concurrent change held the last few unformatted files; that list reached
# zero and the script went with it.
check/fmt:
	$(CARGO) fmt --all -- --check

## check/clippy - lint the workspace, warnings are errors
check/clippy:
	$(CARGO) clippy --workspace --all-targets $(CARGO_FLAGS) -- -D warnings

## check/gates - every text gate: no absolute machine paths, no brain dependency,
##              samples that stay behind the SDK facade.
##              Together they cost well under a second.
check/gates: check/paths check/deps check/samples

## check/paths - no baked-in absolute machine path in any tracked (or new,
##              not-yet-tracked) file
check/paths:
	bash scripts/gates/check-no-machine-paths.sh

## check/deps - sven must stay buildable with no brain checkout: no brain-*
##              dependency, declared or transitively resolved
check/deps:
	bash scripts/gates/check-no-brain-dependency.sh

## check/samples - a sample depends on the SDK facade and nothing else, and its
##              package name matches its path (see samples/README.md)
check/samples:
	bash scripts/gates/check-samples.sh

# A sample is a standalone APPLICATION built on the public `sven-sdk` facade,
# living in samples/<category>/<name>/. Unlike sibling repos, samples here ARE
# built and tested with the workspace: one depends only on the SDK, so it costs
# almost nothing, and excluding it is how a sample rots into something that no
# longer compiles against the framework it demonstrates.
#
#   make samples/list
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

# samples/<a>/<b> -> package `sample-<a>-<b>`. check/samples enforces it.
sample_pkg = sample-$(subst /,-,$(1))

samples/list:
	@echo "Samples (make samples/<path>/{build,run}):"
	@find samples -mindepth 3 -maxdepth 3 -name Cargo.toml 2>/dev/null \
		| sed -E 's|^samples/(.*)/Cargo\.toml$$|\1|' | sort | while read -r s; do \
			printf '  %-28s %s\n' "$$s" \
				"$$(sed -n 's/^description = "\(.*\)"/\1/p' "samples/$$s/Cargo.toml" | head -1)"; \
		done

samples/%/build:
	$(CARGO) build $(SAMPLE_CARGO_FLAGS) -p $(call sample_pkg,$*)

samples/%/run:
	@bin="target/$(SAMPLE_PROFILE)/$(call sample_pkg,$*)"; \
	if [ ! -x "$$bin" ]; then \
		echo "error: $$bin does not exist - run 'make samples/$*/build' first"; exit 1; \
	fi; \
	echo "running $$bin (built $$(date -r "$$bin" '+%Y-%m-%d %H:%M:%S'))"; \
	exec "$$bin" $(ARGS)

## check/arch - enforce architecture.toml (crate tiers, dead deps, file-size ratchet,
##              and the `minimal` cargo feature profile's forbidden-crate list)
check/arch:
	$(CARGO) run -q -p xtask -- arch
	$(CARGO) run -q -p xtask -- arch --profile minimal

## hooks/install - install this repo's git pre-commit hooks (one-time per clone;
##              writes outside version control, so it is never automatic).
##              Prefers the `pre-commit` framework, since .pre-commit-config.yaml
##              carries the same gates plus rustfmt/clippy/ASCII hygiene; falls
##              back to scripts/hooks/pre-commit, which needs only bash and git.
##              Refuses to clobber a hook it did not write.
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

## clean     - remove build artefacts
clean:
	$(CARGO) clean
	rm -rf target/debian target/debian-staging target/completions target/docs target/benchmark $(DIST)

## help      - show this message
help:
	@grep -E '^##' Makefile | sed 's/^## /  /'
	@echo ""
	@echo "  Site targets:"
	@grep -E '^##' site/Makefile 2>/dev/null | sed 's/^## /    /' || true

# ── Release targets ───────────────────────────────────────────────────────────
## release/build   - build release artifacts for current platform into dist/
release/build:
	@bash scripts/release-build.sh --out-dir $(DIST)

## release/tag     - create an annotated git tag for current version and push
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
	@echo "Tag $(TAG) pushed → GitHub Actions release workflow will start."
	@echo "Watch: https://github.com/$(REPO)/actions"

## release/publish - create a GitHub Release and upload dist/ artifacts via gh CLI
release/publish:
	@if [ -z "$$(ls $(DIST)/sven-* 2>/dev/null)" ]; then \
	    echo "error: no artifacts found in $(DIST)/"; \
	    echo "       Run 'make release/build' first."; \
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

## release/patch   - bump patch version (0.1.x→0.1.x+1), tag, push → triggers CI
release/patch: _require-cargo-release test tests/e2e/basic
	@cargo release patch -p sven --execute --no-confirm
	@git push origin main --follow-tags

## release/minor   - bump minor version (0.x.0→0.x+1.0), tag, push → triggers CI
release/minor: _require-cargo-release test tests/e2e/basic
	@cargo release minor -p sven --execute --no-confirm
	@git push origin main --follow-tags

## release/major   - bump major version (x.0.0→x+1.0.0), tag, push → triggers CI
release/major: _require-cargo-release test tests/e2e/basic
	@cargo release major -p sven --execute --no-confirm
	@git push origin main --follow-tags

# ── Site targets ─────────────────────────────────────────────────────────────
## site/build   - build the sven-site Docker image
site/build:
	$(MAKE) -C site build

## site/publish - upload the Docker image to swedishembedded.com via SSH
site/publish:
	$(MAKE) -C site publish

## site/serve   - serve the site locally on http://localhost:3000
site/serve:
	$(MAKE) -C site serve

# Install cargo-release if not already present.
.PHONY: _require-cargo-release
_require-cargo-release:
	@if ! cargo release --version >/dev/null 2>&1; then \
	    echo "cargo-release not found - installing..."; \
	    cargo install cargo-release --locked; \
	fi
