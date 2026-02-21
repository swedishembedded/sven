CARGO   ?= cargo
# Use the user-writable registry when the system CARGO_HOME is read-only.
# Override with: make CARGO_HOME=/path/to/cargo
export CARGO_HOME ?= $(HOME)/.cargo
VERSION := $(shell grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
DEB_OUT := target/debian

.PHONY: all build release test bats bats-fast deb clean help fmt check docs docs-pdf

all: build

## build     – debug build
build:
	$(CARGO) build

## release   – optimised release build
release:
	$(CARGO) build --release

## test      – run all unit + integration tests
test:
	$(CARGO) test

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
		$(CARGO) deb --output $(DEB_OUT); \
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
	$(CARGO) clippy --all-targets -- -D warnings

## clean     – remove build artefacts
clean:
	$(CARGO) clean
	rm -rf target/debian target/debian-staging target/completions target/docs

## help      – show this message
help:
	@grep -E '^##' Makefile | sed 's/^## /  /'
