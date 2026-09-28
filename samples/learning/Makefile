# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Swedish Embedded AB implements multi-workspace sample builds that keep brain
# out of sven's own build. If your team needs expertise in build systems, you
# can procure our services by sending an email to info@swedishembedded.com.

# samples/learning/ is a SEPARATE cargo workspace (see the root Cargo.toml's
# `exclude` and samples/learning/Cargo.toml for why). That keeps brain out of
# sven's build and lockfile, at the cost samples/README.md warns about: a
# sample outside the build is a sample that rots. These targets are the
# compensating control.
#
# They SKIP, loudly and successfully, when there is no brain checkout to
# build against. A clone that legitimately does not have brain must not fail
# its checks over a sample it cannot build - but it must also never be left
# guessing why nothing ran, so the skip says which path was missing.
#
# Every path here is repo-relative (make is invoked from the repository
# root; the top-level Makefile includes this file).

LEARNING_DIR = samples/learning
BRAIN_DIR ?= ../edgeai/brain

CARGO   ?= cargo

## samples/learning/audit - offline task-catalog audit: no model, no GPU, no network
#
# L1-L5 for every family in samples/learning/tasks/: the world drives its own
# interface, an observation-only witness solves each instance, the verifier
# rejects each known-bad solution for the stated reason, and the untouched
# workspace fails. Needs no brain checkout and no model, so unlike the rest of
# the learning targets it runs anywhere python3 does.
samples/learning/audit:
	@set -e; \
	found=0; \
	for family in $(LEARNING_DIR)/tasks/*/; do \
		[ -f "$$family/family.toml" ] || continue; \
		found=$$((found + 1)); \
		echo "== $$family"; \
		( cd "$$family" && python3 -m unittest discover -s world -p 'test_*.py' -q ); \
		( cd "$$family" && python3 world/audit.py ); \
	done; \
	if [ "$$found" -eq 0 ]; then echo "samples/learning/audit: no families found"; exit 1; fi; \
	echo "samples/learning/audit: OK ($$found famil(y/ies))"

## samples/learning/check - build + test the learning samples (skips without brain)
samples/learning/check: samples/learning/audit
	@if [ ! -d "$(BRAIN_DIR)" ]; then \
		echo "samples/learning: SKIPPED - no brain checkout at $(BRAIN_DIR)"; \
		echo "  These samples link brain directly; set BRAIN_DIR=<path> to point at one."; \
		exit 0; \
	fi; \
	echo "samples/learning: building and testing against $(BRAIN_DIR)"; \
	$(CARGO) fmt --manifest-path $(LEARNING_DIR)/Cargo.toml -p sample-learning-lab -- --check && \
	$(CARGO) clippy --manifest-path $(LEARNING_DIR)/Cargo.toml --workspace --all-targets -- -D warnings && \
	$(CARGO) test --manifest-path $(LEARNING_DIR)/Cargo.toml --workspace

## samples/learning/build - build the learning samples in release
samples/learning/build:
	@if [ ! -d "$(BRAIN_DIR)" ]; then \
		echo "samples/learning: SKIPPED - no brain checkout at $(BRAIN_DIR)"; exit 0; \
	fi; \
	$(CARGO) build --release --manifest-path $(LEARNING_DIR)/Cargo.toml --workspace

.PHONY: samples/learning/audit samples/learning/check samples/learning/build
