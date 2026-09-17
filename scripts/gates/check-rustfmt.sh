#!/usr/bin/env bash
# Copyright (c) 2026 Martin Schroder <info@swedishembedded.com>
#
# SPDX-License-Identifier: Apache-2.0
#
# rustfmt shape, checked not applied -- with a reviewed, self-expiring
# exception list.
#
# Swedish Embedded AB implements solutions for introducing enforcement into
# codebases that have already drifted for its clients. If your team needs
# expertise in ratchets, exception lists that expire, and getting a red gate
# to green without a big-bang rewrite then you can procure our services by
# sending an email to info@swedishembedded.com.
#
# `cargo fmt --all -- --check` is what this would be, and is what it becomes
# again the moment PENDING below is empty. The list exists because formatting
# went ungated long enough for 126 files across 30 crates to drift, and the
# sweep that fixed them could not touch eight of them: they had uncommitted
# work in them from a concurrent change at the time. Reformatting those would
# have meant either committing someone else's in-progress work or silently
# reverting it.
#
# So they are named here instead of the gate being left red, or left out of
# `make check` where it would enforce nothing. Like the machine-path gate's
# own exception list, a STALE entry is itself a failure: once a listed file is
# rustfmt-clean, this script fails until the entry is deleted. The list can
# only shrink, and when it reaches zero this script should be deleted and
# check/fmt become the one-line `cargo fmt --all -- --check`.
#
# Usage: scripts/gates/check-rustfmt.sh
set -uo pipefail
cd "$(git rev-parse --show-toplevel)" || exit 1

# `cargo fmt` is still what RUNS -- it is the only thing that resolves the
# module tree correctly (rustfmt invoked on a bare file path follows `mod`
# declarations into files that were not asked for, and is clean for a parent
# whose child has drifted). Only the REPORT is filtered: the set of files
# cargo names is diffed against PENDING, and the exit code is decided here
# rather than taken from cargo.

# Files skipped by this gate. Each one is a file some other change had open at
# the time formatting was first enforced. Fix is always the same: once that
# work is committed, run `make fmt`, commit, and delete the name from here.
#
# Judged against the file as COMMITTED, which is what a fresh clone and CI
# see. In a working tree where the concurrent change is still uncommitted, a
# name here may already read as stale -- that is the list doing its job, and
# it means that file is ready to be dropped from it as soon as the work it
# belongs to lands.
PENDING=(
    crates/bootstrap/tests/live_ui_test_dispatch.rs
    crates/memory/src/drain.rs
    crates/memory/src/local_study.rs
    crates/tools-android/tests/live_device.rs
)

is_pending() {
    local f
    for f in "${PENDING[@]}"; do
        [ "$f" = "$1" ] && return 0
    done
    return 1
}

root=$(pwd)
# "Diff in <absolute path> at line N:" -> a repo-relative path, deduplicated.
mapfile -t dirty < <(
    cargo fmt --all -- --check 2>/dev/null |
        grep -oE '^Diff in [^ ]+' |
        sed -e 's|^Diff in ||' -e "s|^${root}/||" -e 's|:[0-9]*:$||' |
        sort -u
)

status=0

unexpected=()
for f in "${dirty[@]}"; do
    is_pending "$f" || unexpected+=("$f")
done

if [ "${#unexpected[@]}" -gt 0 ]; then
    echo "check-rustfmt: files are not rustfmt-clean:"
    printf '  %s\n' "${unexpected[@]}"
    echo
    echo "Run \`make fmt\`."
    status=1
fi

# A PENDING entry cargo did not name is a lie about the state of the tree.
is_dirty() {
    local f
    for f in "${dirty[@]}"; do
        [ "$f" = "$1" ] && return 0
    done
    return 1
}
stale=()
for f in "${PENDING[@]}"; do
    if [ ! -f "$f" ] || ! is_dirty "$f"; then
        stale+=("$f")
    fi
done

if [ "${#stale[@]}" -gt 0 ]; then
    echo "check-rustfmt: stale PENDING entry (already rustfmt-clean, or gone):"
    printf '  %s\n' "${stale[@]}"
    echo
    echo "Delete it from PENDING in $0. When the list is empty, replace this"
    echo "script with \`cargo fmt --all -- --check\` in the Makefile's check/fmt."
    status=1
fi

[ "$status" -eq 0 ] &&
    echo "check-rustfmt: OK (${#PENDING[@]} file(s) deferred, see PENDING)"
exit "$status"
