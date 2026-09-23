#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Fetches the TLA+ tools jar the model checker runs from, into the gitignored
# formal/tools/ directory.
#
# The jar is not committed. It is a 2 MB third-party binary under its own MIT
# licence, and `make check/gates` would be right to object; more to the point, a
# committed binary is a dependency nobody can see the provenance of. So it is
# pinned by RELEASE and verified by SHA-256 here, and a mismatch is a hard
# failure rather than a warning -- the whole value of a model checker is that
# you trust what it says, which means trusting what you ran.
#
# Idempotent: an already-correct jar is left alone, so `make formal` on a warm
# tree does no network I/O at all.
set -euo pipefail

RELEASE="v1.7.4"
SHA256="936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88"
URL="https://github.com/tlaplus/tlaplus/releases/download/${RELEASE}/tla2tools.jar"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
jar="${here}/tools/tla2tools.jar"

verify() {
    [ -f "$1" ] && [ "$(sha256sum "$1" | cut -d' ' -f1)" = "$SHA256" ]
}

if verify "$jar"; then
    echo "[formal] tla2tools ${RELEASE} already present"
    exit 0
fi

mkdir -p "${here}/tools"
echo "[formal] fetching tla2tools ${RELEASE}"
tmp="${jar}.tmp.$$"
trap 'rm -f "$tmp"' EXIT
curl -fsSL --retry 3 --retry-delay 2 -o "$tmp" "$URL"

if ! verify "$tmp"; then
    echo "[formal] SHA-256 mismatch for ${URL}" >&2
    echo "[formal]   expected ${SHA256}" >&2
    echo "[formal]   got      $(sha256sum "$tmp" | cut -d' ' -f1)" >&2
    exit 1
fi

mv "$tmp" "$jar"
trap - EXIT
echo "[formal] tla2tools ${RELEASE} verified at ${jar}"
