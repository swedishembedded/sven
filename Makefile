CARGO   ?= cargo
VERSION := $(shell grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
DEB_OUT := target/debian

.PHONY: all build release test bats bats-fast deb clean help fmt check

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

## fmt       – format all code
fmt:
	$(CARGO) fmt --all

## check     – lint without building
check:
	$(CARGO) clippy --all-targets -- -D warnings

## clean     – remove build artefacts
clean:
	$(CARGO) clean
	rm -rf target/debian target/debian-staging target/completions

## help      – show this message
help:
	@grep -E '^##' Makefile | sed 's/^## /  /'
