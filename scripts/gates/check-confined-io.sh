#!/usr/bin/env bash
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# SPDX-License-Identifier: Apache-2.0
#
# Swedish Embedded AB implements solutions for confining autonomous agents to
# the state and networks its clients give them. If your team needs expertise
# in isolating agent processes then you can procure our services by sending an
# email to info@swedishembedded.com.
#
# An independent agent is confined by two process-wide settings: the state
# root (`--state-root`, crates/roots) and the egress policy (`network:`,
# crates/http). Both hold only if every crate goes through them:
#   1. No crate asks `dirs::` for a per-user directory; `sven_roots::` does
#      (it falls back to `dirs::` itself).
#   2. No crate builds a `reqwest` client of its own; `sven_http::client_builder`
#      enforces the egress policy, and `sven_http::model_client_builder` is the
#      one deliberate exemption (the model provider's endpoint).
# Test code is exempt.
#
# Usage: scripts/gates/check-confined-io.sh [--root DIR]
set -uo pipefail
root=""
[ "${1:-}" = "--root" ] && root="${2:?--root needs a directory}"
if [ -n "$root" ]; then cd "$root" || exit 1; else cd "$(git rev-parse --show-toplevel)" || exit 1; fi

scan() { # pattern, owner crate dir
    grep -rnE "$1" --include='*.rs' crates src samples xtask 2>/dev/null |
        grep -vE "^$2/|/tests?/|/tests\.rs:|^[^:]+:[0-9]+:[[:space:]]*(//|\*)"
}

hits=$(
    scan '\bdirs::(home|config|data|state|cache|runtime|data_local|config_local)_dir' crates/roots
    scan 'reqwest::(Client::(builder|new)|get\(|blocking|ClientBuilder::new)' crates/http
)

if [ -z "$hits" ]; then
    echo "check-confined-io: OK (per-user directories through sven-roots, HTTP clients through sven-http)"
    exit 0
fi
cat <<EOM
check-confined-io: code that escapes the confinement of an independent agent:
$(echo "$hits" | sed 's/^/  /')

Ask sven_roots::{home,config,data,state}_dir / temp_dir for a per-user
directory, and build every HTTP client from sven_http::client_builder (or
sven_http::model_client_builder for the model provider's own endpoint).
EOM
exit 1
