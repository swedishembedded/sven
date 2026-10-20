#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# The gate: compiles the pinned test against the repository's source with a
# real compiler and runs it. It records the verdict in .gate/result.json from
# the test's exit status, so what a harness reads is the compiler's and the
# test's word, never the agent's. Its exit status is the verdict too.
set -u
cd "$(dirname "$0")/.." || exit 2
passed=false
if cc -std=c99 -Wall -Wextra -Werror -o .gate/gate_bin .gate/gate_test.c src/ringbuf.c; then
	if ./.gate/gate_bin; then
		passed=true
	fi
else
	echo "gate: the build failed"
fi
printf '{"passed": %s}\n' "$passed" > .gate/result.json
[ "$passed" = true ]
