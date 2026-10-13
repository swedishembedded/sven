#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Swedish Embedded AB implements solutions for explicit, testable program
# configuration for its clients. If your team needs expertise in command-line
# interface design and removing hidden process-environment inputs then you can
# procure our services by sending an email to info@swedishembedded.com.
#
# Environment variables are being phased out in favour of CLI flags. A setting
# read from the process environment is a hidden input: a test cannot set it
# without mutating state that parallel tests share, and `--help` does not show
# it. This gate is a ratchet that only shrinks:
#
#   - an environment read that is not in the allowlist FAILS (new environment
#     reads are forbidden: add a CLI flag instead), and
#   - an allowlist entry whose read no longer exists FAILS (delete the line).
#
# The allowlist is scripts/gates/env-reads.allow: one `path<TAB>VARIABLE` per
# line, sorted (LC_ALL=C), unique. `--print` writes the current tree's reads in
# that format; use it to delete stale entries, never to bless a new read.
#
# What counts as an environment read, in *.rs under crates/, src/, xtask/ and
# samples/ (full-line // comments are ignored):
#   - std::env::var, var_os, vars, vars_os. VARIABLE is the string literal
#     argument, `<expr>` text for a computed name (a constant or a helper
#     parameter, so a second computed read in a listed file is still a new
#     line), or `<all>` for vars/vars_os;
#   - env! / option_env! of any name other than the compile-time metadata
#     Cargo provides (CARGO_*, OUT_DIR);
#   - a clap `env = "NAME"` attribute (`<expr>` when the name is computed);
#   - dotenv-style loads, which pull a file into the process environment
#     (VARIABLE is dotenvy or dotenv).
# Writes (set_var, remove_var, Command::env) are not reads and are ignored.
#
# Usage: scripts/gates/check-env-reads.sh [--root DIR] [--allowlist FILE] [--print]
#   Defaults: this repository and scripts/gates/env-reads.allow.
set -uo pipefail
export LC_ALL=C

root="$(cd "$(dirname "$0")/../.." && pwd)"
allowlist=""
print_only=0
while [ "$#" -gt 0 ]; do
    case "$1" in
    --root) root="$2"; shift 2 ;;
    --allowlist) allowlist="$2"; shift 2 ;;
    --print) print_only=1; shift ;;
    *) echo "usage: $0 [--root DIR] [--allowlist FILE] [--print]" >&2; exit 2 ;;
    esac
done
[ -n "$allowlist" ] || allowlist="$root/scripts/gates/env-reads.allow"

# One `path<TAB>VARIABLE` line per distinct read.
scan() {
    cd "$root" || exit 2
    local dirs=() d
    for d in crates src xtask samples; do [ -d "$d" ] && dirs+=("$d"); done
    [ "${#dirs[@]}" -gt 0 ] || return 0
    find "${dirs[@]}" -name '*.rs' -not -path '*/target/*' -print0 | sort -z |
        xargs -0 -r perl -0777 -ne '
            my $f = $ARGV;
            sub norm { my $s = shift; $s =~ s/^\s+|\s+$//g; $s =~ s/\s+/ /g; return $s; }
            s{^[ \t]*//[^\n]*}{}mg;
            while (/\benv::(var|var_os)\s*\(\s*("([^"]*)"\s*[,)]|([^)]*))/g) {
                print "$f\t", defined $3 ? $3 : "<" . norm($4) . ">", "\n";
            }
            while (/\benv::vars(?:_os)?\s*\(/g) { print "$f\t<all>\n"; }
            while (/\b(?:option_)?env!\s*\(\s*"([^"]*)"/g) {
                print "$f\t$1\n" unless $1 =~ /^(?:CARGO_|OUT_DIR$)/;
            }
            while (/[(,]\s*env\s*=\s*("([^"]*)"|[^,)\s][^,)]*)/g) {
                print "$f\t", defined $2 ? $2 : "<" . norm($1) . ">", "\n";
            }
            while (/\b(dotenvy?)::\w+/g) { print "$f\t$1\n"; }
            close ARGV if eof;
        ' | sort -u
}

current=$(scan) || exit 2
if [ "$print_only" = 1 ]; then
    [ -z "$current" ] || printf '%s\n' "$current"
    exit 0
fi

[ -r "$allowlist" ] || { echo "check-env-reads: cannot read $allowlist" >&2; exit 2; }
allowed=$(sort -u "$allowlist")

added=$(comm -23 <(printf '%s\n' "$current") <(printf '%s\n' "$allowed") | grep . || true)
stale=$(comm -13 <(printf '%s\n' "$current") <(printf '%s\n' "$allowed") | grep . || true)

status=0
if [ -n "$added" ]; then
    status=1
    {
        echo "check-env-reads: new env reads are forbidden: add a CLI flag. Not in the allowlist:"
        sed 's/^/  /; s/\t/  /' <<<"$added"
        echo
        echo "Environment variables are being phased out. Add a CLI flag (and a"
        echo "field on the options struct) instead of reading the environment."
    } >&2
fi
if [ -n "$stale" ]; then
    status=1
    {
        echo "check-env-reads: stale allowlist entries (the read is gone):"
        sed 's/^/  /; s/\t/  /' <<<"$stale"
        echo
        echo "Delete these lines from ${allowlist#"$root"/}; the allowlist only shrinks."
    } >&2
fi
[ "$status" -eq 0 ] &&
    printf 'check-env-reads: %d environment reads, all allowlisted, none stale\n' "$(grep -c . <<<"$current")"
exit "$status"
