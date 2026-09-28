# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Swedish Embedded AB implements multi-workspace sample builds that keep brain
# out of sven's own build. If your team needs expertise in build systems, you
# can procure our services by sending an email to info@swedishembedded.com.

# samples/agent/ is a SEPARATE cargo workspace (see the root Cargo.toml's
# `exclude` and scripts/gates/check-no-brain-dependency.sh): the loop sample
# links both sven's SDK facade and brain directly, and must not put brain
# into sven's build. Same compensating control as samples/learning/: the
# targets skip, loudly and successfully, when there is no brain checkout to
# build against - a clone that legitimately does not have brain must not
# fail its checks over a sample it cannot build, but it must also never be
# left guessing why nothing ran, so the skip names the missing path.
#
# Every path here is repo-relative (make is invoked from the repository
# root; the top-level Makefile includes this file). Artifacts the loop
# sample itself writes go to target/, never into the sample's source tree.

BRAIN_DIR ?= ../edgeai/brain
AGENT_DIR = samples/agent
LOOP_BIN  = target/release/sample-agent-loop

CARGO   ?= cargo
CARGO_FLAGS ?= --jobs $(shell nproc 2>/dev/null || echo 4)

# The skip guard. As a prerequisite it stops the whole target when brain is
# absent (make's exit propagates); the `check` recipe runs it inline instead,
# because its steps are chained with && and a skipped dependency would not
# say why nothing after it ran.
_samples_agent_brain:
	@if [ ! -d "$(BRAIN_DIR)" ]; then \
		echo "samples/agent: SKIPPED - no brain checkout at $(BRAIN_DIR)"; \
		echo "  These samples link brain directly; set BRAIN_DIR=<path> to point at one."; \
		exit 1; \
	fi

## samples/agent/check - build + test the agent samples (skips without brain)
samples/agent/check:
	@if [ ! -d "$(BRAIN_DIR)" ]; then \
		echo "samples/agent: SKIPPED - no brain checkout at $(BRAIN_DIR)"; \
		echo "  These samples link brain directly; set BRAIN_DIR=<path> to point at one."; \
		exit 0; \
	fi; \
	echo "samples/agent: building and testing against $(BRAIN_DIR)"; \
	# Package-scoped: cargo fmt's --all follows path dependencies into the
	# brain checkout, which is not part of this workspace's formatting.
	$(CARGO) fmt --manifest-path $(AGENT_DIR)/Cargo.toml -p sample-agent-loop -- --check && \
	$(CARGO) clippy --manifest-path $(AGENT_DIR)/Cargo.toml --workspace --all-targets -- -D warnings && \
	$(CARGO) test --manifest-path $(AGENT_DIR)/Cargo.toml --workspace

## samples/agent/build - build the agent samples in release
samples/agent/build: _samples_agent_brain
	$(CARGO) build --release --manifest-path $(AGENT_DIR)/Cargo.toml --workspace

## samples/agent/audit - the loop sample's unit tests (no model, no GPU)
samples/agent/audit: _samples_agent_brain
	$(CARGO) test --manifest-path $(AGENT_DIR)/Cargo.toml -p sample-agent-loop --bin sample-agent-loop

## samples/agent/loop/build - build the loop sample (release)
samples/agent/loop/build: _samples_agent_brain
	$(CARGO) build --release --manifest-path $(AGENT_DIR)/Cargo.toml -p sample-agent-loop

## samples/agent/loop/run - run the loop sample (ARGS go to the binary)
samples/agent/loop/run: samples/agent/loop/build
	@echo "running $(LOOP_BIN) (built $$(date -r '$(LOOP_BIN)' '+%Y-%m-%d %H:%M:%S'))"
	exec $(LOOP_BIN) $(ARGS)

## samples/agent/loop/test - the loop sample's unit tests
samples/agent/loop/test: _samples_agent_brain
	$(CARGO) test --manifest-path $(AGENT_DIR)/Cargo.toml -p sample-agent-loop --bin sample-agent-loop

## samples/agent/loop/facts - learn a markdown fact sheet end to end
##   Document -> explored facts -> split -> gated LoRA train -> recall +
##   holdout scores. Artifacts land in $(FACTS_WORK); overrides:
##   FACTS_DOC FACTS_WORK FACTS_STEPS FACTS_RANK FACTS_FLAGS
FACTS_DOC  ?= $(AGENT_DIR)/loop/examples/stm32_datasheet.md
FACTS_WORK ?= target/facts
FACTS_STEPS ?= 40
FACTS_RANK  ?= 8
samples/agent/loop/facts: samples/agent/loop/build
	@mkdir -p $(FACTS_WORK)
	exec $(LOOP_BIN) facts \
		--file $(FACTS_DOC) \
		--work-dir $(FACTS_WORK) \
		--steps $(FACTS_STEPS) --rank $(FACTS_RANK) $(FACTS_FLAGS)

## samples/agent/loop/ask - one question to the promoted adapter (BASE=1: base model)
##   usage: make samples/agent/loop/ask Q='question' [BASE=1]
samples/agent/loop/ask: samples/agent/loop/build
	@test -n "$(Q)" || { echo "usage: make samples/agent/loop/ask Q='question' [BASE=1]"; exit 1; }
	@if [ -n "$(BASE)" ]; then \
		exec $(LOOP_BIN) ask --base --question "$(Q)"; \
	else \
		exec $(LOOP_BIN) ask --question "$(Q)"; \
	fi

.PHONY: samples/agent/check samples/agent/build samples/agent/audit \
	samples/agent/loop/build samples/agent/loop/run samples/agent/loop/test \
	samples/agent/loop/facts samples/agent/loop/ask _samples_agent_brain
