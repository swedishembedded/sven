CARGO   ?= cargo
# Use the user-writable registry when the system CARGO_HOME is read-only.
# Override with: make CARGO_HOME=/path/to/cargo
export CARGO_HOME = $(HOME)/.cargo

# Wrap rustc with sccache when available for shared compilation caching across builds.
# Install with: cargo install sccache
# Override with: make RUSTC_WRAPPER=""  to disable.
ifneq ($(shell command -v sccache 2>/dev/null),)
export RUSTC_WRAPPER = sccache
endif

NPROC := $(shell nproc)
CARGO_FLAGS ?= --jobs $(NPROC)
VERSION := $(shell grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
DEB_OUT := target/debian

.PHONY: all build release test bats bats-fast deb clean help fmt check docs docs-pdf \
        relay relay-release p2p-client p2p-client-release p2p p2p-release p2p-test

all: build

## build     – debug build
build:
	$(CARGO) build $(CARGO_FLAGS)

## release   – optimised release build
release:
	$(CARGO) build --release $(CARGO_FLAGS)

## test      – run all unit + integration tests
test:
	$(CARGO) test $(CARGO_FLAGS)

## bats      – run end-to-end bats tests (requires bats-core)
bats: build
	bats tests/bats/01_cli.bats \
	     tests/bats/02_ci_mode.bats \
	     tests/bats/03_mock_responses.bats \
	     tests/bats/04_pipeline.bats

## bats-fast – run bats tests without rebuilding
bats-fast:
	bats tests/bats/01_cli.bats \
	     tests/bats/02_ci_mode.bats \
	     tests/bats/03_mock_responses.bats \
	     tests/bats/04_pipeline.bats

## deb       – build a Debian package (output in target/debian/)
deb: release
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

## docs      – build single-file markdown user guide (output: target/docs/sven-user-guide.md)
docs:
	@mkdir -p target/docs
	@printf '' > target/docs/sven-user-guide.md
	@for f in docs/00-introduction.md \
	           docs/01-installation.md \
	           docs/02-quickstart.md \
	           docs/03-user-guide.md \
	           docs/04-ci-pipeline.md \
	           docs/05-configuration.md \
	           docs/06-examples.md \
	           docs/07-troubleshooting.md; do \
		if [ -f "$$f" ]; then \
			cat "$$f" >> target/docs/sven-user-guide.md; \
			printf '\n---\n\n' >> target/docs/sven-user-guide.md; \
		fi; \
	done
	@echo "User guide written to target/docs/sven-user-guide.md"

## docs-pdf  – build PDF user guide (requires pandoc + a LaTeX distribution)
docs-pdf: docs
	@command -v pandoc >/dev/null 2>&1 || { \
		echo "Error: pandoc is not installed."; \
		echo "Install it with: sudo apt install pandoc texlive-xetex texlive-fonts-recommended"; \
		exit 1; \
	}
	pandoc target/docs/sven-user-guide.md \
		--metadata-file=docs/metadata.yaml \
		--pdf-engine=xelatex \
		--toc \
		--toc-depth=2 \
		--number-sections \
		--highlight-style=tango \
		-o target/docs/sven-user-guide.pdf
	@echo "PDF guide written to target/docs/sven-user-guide.pdf"

## fmt       – format all code
fmt:
	$(CARGO) fmt --all

## check     – lint without building
check:
	$(CARGO) clippy --all-targets $(CARGO_FLAGS) -- -D warnings

## relay     – build the sven-relay server (requires git-discovery feature)
relay:
	$(CARGO) build -p sven-p2p --bin sven-relay --features git-discovery $(CARGO_FLAGS)
	@echo "Binary: target/debug/sven-relay"
	@echo "Usage:  sven-relay --listen /ip4/0.0.0.0/tcp/4001 --repo /path/to/git/repo"

## relay-release – release-optimised relay binary
relay-release:
	$(CARGO) build -p sven-p2p --bin sven-relay --features git-discovery --release $(CARGO_FLAGS)
	@echo "Binary: target/release/sven-relay"

## p2p-client – build the sven-p2p-client TUI/chat client
p2p-client:
	$(CARGO) build -p sven-p2p --bin sven-p2p-client $(CARGO_FLAGS)
	@echo "Binary: target/debug/sven-p2p-client"
	@echo "Usage:  sven-p2p-client --repo . --room <room> --name <name>"
	@echo "        sven-p2p-client --repo . --room <room> --name <name> -m '@peer hello'"

## p2p-client-release – release-optimised client binary
p2p-client-release:
	$(CARGO) build -p sven-p2p --bin sven-p2p-client --release $(CARGO_FLAGS)
	@echo "Binary: target/release/sven-p2p-client"

## p2p      – build both relay and client debug binaries
p2p: relay p2p-client

## p2p-release – build both relay and client release binaries
p2p-release: relay-release p2p-client-release

## p2p-test – run sven-p2p unit and integration tests
p2p-test:
	$(CARGO) test -p sven-p2p $(CARGO_FLAGS)

## clean     – remove build artefacts
clean:
	$(CARGO) clean
	rm -rf target/debian target/debian-staging target/completions target/docs

## help      – show this message
help:
	@grep -E '^##' Makefile | sed 's/^## /  /'
