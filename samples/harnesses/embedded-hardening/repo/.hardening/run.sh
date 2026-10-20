#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# build | test [module] | coverage | lint | layering, for the embedded fixture.
# The harness pins this file and .hardening/pins.sha256 by hash: they are the
# check, so the agent never edits them. `build` also verifies that the given
# tests, which are the specification, are the bytes in pins.sha256.
set -u
cd "$(dirname "$0")/.." || exit 2
CFLAGS="-std=c99 -Wall -Wextra -Werror -Iinclude -Isrc -Ihal"
OUT=.hardening/out
mkdir -p "$OUT"
MODULES="ringbuf crc8 uart_tx"

sources_of() {
	case "$1" in
	uart_tx) echo "src/uart_tx.c hal/hal_stub.c" ;;
	*) echo "src/$1.c" ;;
	esac
}

# A module's tests: the given tests/test_<m>.c and, when the agent adds them,
# tests/extra_<m>.c. Both must pass.
test_files() {
	echo "tests/test_$1.c"
	[ -f "tests/extra_$1.c" ] && echo "tests/extra_$1.c"
}

run_tests() { # extra compiler flags, then the modules
	flags=$1
	shift
	failed=0
	for m in "$@"; do
		for t in $(test_files "$m"); do
			# shellcheck disable=SC2086
			if ! cc $CFLAGS $flags -o "$OUT/$(basename "$t" .c)" "$t" $(sources_of "$m"); then
				echo "test build failed: $t"
				failed=1
				continue
			fi
			if ! "$OUT/$(basename "$t" .c)"; then
				echo "test failed: $t"
				failed=1
			fi
		done
	done
	return $failed
}

case "${1:-}" in
build)
	sha256sum --quiet -c .hardening/pins.sha256 || { echo "the given tests were changed"; exit 1; }
	for m in $MODULES; do
		for s in $(sources_of "$m"); do
			cc $CFLAGS -c "$s" -o "$OUT/$(basename "$s" .c).o" || exit 1
		done
	done
	;;
test)
	shift
	if [ $# -gt 0 ]; then run_tests "" "$@"; else run_tests "" $MODULES; fi
	;;
coverage)
	rm -rf "$OUT/cov" && mkdir -p "$OUT/cov"
	for m in $MODULES; do
		for t in $(test_files "$m"); do
			b=$(basename "$t" .c)
			# shellcheck disable=SC2086
			cc $CFLAGS --coverage -O0 -o "$OUT/cov/$b" "$t" $(sources_of "$m") || exit 1
			(cd "$OUT/cov" && "./$b" >/dev/null 2>&1) || true
		done
	done
	pct=$(gcovr --root . --object-directory "$OUT/cov" --filter 'src/' --json-summary 2>/dev/null |
		sed -n 's/.*"line_percent": *\([0-9.]*\).*/\1/p' | head -1)
	[ -n "$pct" ] || { echo "coverage: gcovr produced no figure"; exit 1; }
	printf '{"line_pct": %s}\n' "$pct" > .hardening/coverage.json
	;;
lint)
	# shellcheck disable=SC2046
	clang-tidy --quiet --warnings-as-errors='*' \
		--checks='-*,bugprone-*,clang-analyzer-*,-bugprone-easily-swappable-parameters' \
		$(ls src/*.c hal/*.c) -- $CFLAGS 2>&1
	;;
layering)
	# The HAL is reached from the modules that drive hardware only: no public
	# header names it, and only uart_tx.c includes it.
	bad=$( { grep -l 'hal\.h' src/*.h; grep -l 'hal\.h' src/*.c | grep -v '^src/uart_tx\.c$'; } 2>/dev/null)
	if [ -n "$bad" ]; then
		echo "layering: the HAL leaks into: $bad"
		exit 1
	fi
	;;
*)
	echo "usage: run.sh build | test [module] | coverage | lint | layering" >&2
	exit 2
	;;
esac
