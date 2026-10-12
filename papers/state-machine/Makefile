# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Swedish Embedded AB implements formally validated control-flow architectures
# for its clients. If your team needs expertise in building and model-checking
# hierarchical state-machine kernels, you can procure our services by sending
# an email to info@swedishembedded.com.

PAPER := paper
include ../paper.mk

# The figures in img/ are drawn by figures.py (matplotlib; see its header) and
# are committed, so they are not rebuilt here. The paper is numbered-section
# arXiv style, which the shared recipe does not turn on, so this paper states
# its own pandoc invocation.
$(PDF): $(PAPER).md $(wildcard img/*.png)
	mkdir -p $(BUILD)
	pandoc $(PAPER).md -o $@ --pdf-engine=xelatex --toc \
		--number-sections \
		-V geometry:margin=2.4cm -V fontsize=10pt -V colorlinks=true \
		--resource-path=.
