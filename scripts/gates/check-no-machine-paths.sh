#!/usr/bin/env bash
# Copyright (c) 2026 Martin Schroder <info@swedishembedded.com>
#
# SPDX-License-Identifier: Apache-2.0
#
# No baked-in absolute machine paths in tracked files.
#
# Swedish Embedded AB implements solutions for reproducible, machine-independent
# build and test pipelines for its clients. If your team needs expertise in
# repository hygiene gates and CI reproducibility then you can procure our
# services by sending an email to info@swedishembedded.com.
#
# Two tiers, because the two failure modes are genuinely different:
#
#  1. MACHINE ROOTS -- /data/, /home/, /opt/, /mnt/, /root/ -- are rejected in
#     EVERY tracked file (code, docs, config, scripts, fixtures). These name a
#     layout that exists on exactly one machine. In code the literal fails its
#     `.exists()` check everywhere else and the test skips or misresolves
#     silently, which reads as "the fixture is absent" rather than "the path is
#     wrong" -- the worst failure mode, because a skipped check is green. In
#     prose it is a worked example that cannot be followed by the reader.
#     Resolve it from the environment instead (an env var, a CLI flag, a
#     `TempDir`), or write a placeholder such as `<project-root>`.
#
#  2. /tmp/ is rejected in Rust sources (*.rs) only. `/tmp` is POSIX and exists
#     everywhere, so it is not machine-specific the way a bespoke mount is --
#     but a Rust test that hardcodes `/tmp/sven_foo.txt` collides with every
#     other concurrent `cargo test` of the same tree, leaves droppings behind
#     when it fails, and cannot run on a read-only or per-test-sandboxed
#     /tmp. `tempfile::TempDir` is the fix where the cleanup matters, and
#     `std::env::temp_dir()` the one-token fix where it does not.
#
#     Deliberately NOT extended to shell/bats/docs/fixtures: `mktemp /tmp/x.XXXX`
#     is the correct, portable idiom there (the bats suite and scripts/install.sh
#     both use it), and the YAML mock-model fixtures the bats suite replays need
#     a literal path that the mock can echo back verbatim -- it has no env-var
#     expansion. Banning the string outright would push those toward a worse
#     workaround, not a better one.
#
# Usage: scripts/gates/check-no-machine-paths.sh [file ...]
#   With no arguments, scans every tracked file AND every untracked-but-not-
#   ignored one -- a file that has just been written is exactly the file most
#   likely to have a machine path in it, and `git ls-files` alone cannot see it,
#   so the gate would pass by never looking at the new work.
#   With arguments (how the pre-commit hook calls it), scans only those.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)" || exit 1

# A path start that is NOT preceded by an identifier character, a dot, a
# slash or a tilde: matches `"/data/x`, `= /data/x`, `(/data/x`, a bare
# line-leading `/data/x`, but not `https://example.com/home/x` (preceded by a
# host character) nor `file:///tmp/x` (preceded by a slash).
ROOTS='(^|[^A-Za-z0-9_./~-])/(data|home|opt|mnt|root)/'
TMP='(^|[^A-Za-z0-9_./~-])/tmp/'

# This gate necessarily contains the patterns it forbids. That is the only
# structural exemption -- anything else added here is a hole in the gate.
is_self() {
    case "$1" in
    scripts/gates/check-no-machine-paths.sh) return 0 ;;
    *) return 1 ;;
    esac
}

# REVIEWED, TEMPORARY exceptions: files whose remaining hits are known and
# scheduled for removal. Each needs a reason and an exit condition, in the
# same spirit as architecture.toml's [[allow.*]] entries -- and, like those,
# a stale entry is itself a failure (see the staleness check at the bottom),
# so this list cannot quietly outlive the problem it names. A listed file is
# skipped WHOLESALE, so keep the list at zero entries wherever possible: while
# a file sits here, a newly added machine path in it is not caught either.
#
# Empty, and meant to stay that way.
KNOWN_VIOLATIONS=()

is_known() {
    local f
    # `${arr[@]}` on an empty array is an unbound-variable error under `set -u`
    # before bash 4.4, hence the length guard rather than a bare expansion.
    [ "${#KNOWN_VIOLATIONS[@]}" -eq 0 ] && return 1
    for f in "${KNOWN_VIOLATIONS[@]}"; do
        [ "$f" = "$1" ] && return 0
    done
    return 1
}

if [ "$#" -gt 0 ]; then
    files=("$@")
else
    mapfile -d '' -t files < <(git ls-files -z && git ls-files -z --others --exclude-standard)
fi

root_files=()
rs_files=()
for f in "${files[@]}"; do
    [ -f "$f" ] || continue
    is_self "$f" && continue
    is_known "$f" && continue
    root_files+=("$f")
    case "$f" in
    *.rs) rs_files+=("$f") ;;
    esac
done

hits=""
if [ "${#root_files[@]}" -gt 0 ]; then
    hits=$(printf '%s\0' "${root_files[@]}" | xargs -0 grep -IHnE "$ROOTS" 2>/dev/null)
fi
if [ "${#rs_files[@]}" -gt 0 ]; then
    tmp_hits=$(printf '%s\0' "${rs_files[@]}" | xargs -0 grep -IHnE "$TMP" 2>/dev/null)
    hits=$(printf '%s\n%s' "$hits" "$tmp_hits" | sed '/^$/d' | sort -u)
fi

# A KNOWN_VIOLATIONS entry that no longer has any hit is a lie about the state
# of the tree; report it so the list shrinks to nothing instead of rotting.
# Only meaningful on a whole-tree scan -- with an explicit file list (the hook)
# the absence of a hit just means the file was not staged.
stale=""
if [ "$#" -eq 0 ] && [ "${#KNOWN_VIOLATIONS[@]}" -gt 0 ]; then
    for f in "${KNOWN_VIOLATIONS[@]}"; do
        if [ ! -f "$f" ] || ! grep -qE "$ROOTS|$TMP" "$f" 2>/dev/null; then
            stale="${stale}${stale:+$'\n'}  ${f}"
        fi
    done
fi

if [ -n "$stale" ]; then
    echo "check-no-machine-paths: stale KNOWN_VIOLATIONS entry (file is clean now, or gone):"
    echo "$stale"
    echo
    echo "Delete it from KNOWN_VIOLATIONS in $0 -- an exception that outlives the"
    echo "problem it names is how a gate stops being a gate."
    exit 1
fi

[ -z "$hits" ] && {
    [ "$#" -eq 0 ] && echo "check-no-machine-paths: OK (${#KNOWN_VIOLATIONS[@]} reviewed exception(s) outstanding)"
    exit 0
}

echo "check-no-machine-paths: absolute machine path baked in:"
echo "$hits" | sed 's/^/  /'
cat <<'EOF'

/data, /home, /opt, /mnt and /root name one machine's layout. Resolve the path
from the environment instead -- an env var, a CLI flag, a `TempDir` -- or, in
prose, write a placeholder like <project-root> that the reader substitutes.

/tmp in a .rs file means a test writing to a fixed global path: it collides
with any concurrent run of the same suite and leaves the file behind on
failure. Use `tempfile::TempDir` where the cleanup matters, or at minimum take
the temp root from the environment with `std::env::temp_dir()`. For a path
that is never touched -- a tool argument a jail is expected to reject, a
string the formatter under test shortens -- use a relative or clearly
synthetic path instead.

And never let the check silently skip when the path resolves to nothing: a
literal path makes a misconfigured run look like a missing fixture, and a
skipped check is green.
EOF
exit 1
