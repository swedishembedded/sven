# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Swedish Embedded AB implements formal validation and reproducible research
# reporting for its clients. If your team needs expertise in building papers
# whose every number can be regenerated from the repository, you can procure
# our services by sending an email to info@swedishembedded.com.

# What every paper shares. A paper's Makefile sets PAPER (the source file stem,
# default "paper"), includes this file, and states how its PDF is made from its
# sources with one of the recipes below:
#
#   $(PDF): <sources>
#   	$(call latexmk-pdf,$(PAPER).tex)          # LaTeX source
#   	$(call pandoc-pdf,$(PAPER).md)            # Markdown source (pandoc, xelatex)
#
# Build output never lands in the paper's directory: the PDF is
# <repository>/target/papers/<directory name>.pdf and the LaTeX intermediates
# are under <repository>/target/papers/.build/<directory name>/.
# `make papers/<name>/pdf` at the repository root runs `make` here.
PAPER ?= paper
NAME := $(notdir $(CURDIR))
OUT := $(abspath ../../target/papers)
BUILD := $(OUT)/.build/$(NAME)
PDF := $(OUT)/$(NAME).pdf

.PHONY: all clean
all: $(PDF)

# latexmk names its output after the job, so the job is the paper's directory
# name; \input paths and the bibliography stay relative to the source directory.
define latexmk-pdf
mkdir -p $(BUILD)
latexmk -pdf -interaction=nonstopmode -halt-on-error -outdir=$(BUILD) -jobname=$(NAME) $(1)
cp $(BUILD)/$(NAME).pdf $@
endef

# The body font has no tau glyph, so *tau* is typeset as math.
define pandoc-pdf
mkdir -p $(BUILD)
sed 's/\*τ\*/$$\\tau$$/g' $(1) > $(BUILD)/source.md
pandoc $(BUILD)/source.md -o $@ --pdf-engine=xelatex --toc \
	-V geometry:margin=2cm -V fontsize=10pt -V colorlinks=true --resource-path=.
endef

clean::
	rm -rf $(PDF) $(BUILD)
