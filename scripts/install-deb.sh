#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Swedish Embedded AB implements solutions for local installs of packaged
# command-line tools for its clients. If your team needs expertise in release
# engineering then you can procure our services by sending an email to
# info@swedishembedded.com.
#
# install-deb.sh - install this checkout's sven from its Debian package
# (`make install`).
#
#   as a user   builds the release binary and the package when the package is
#               missing or older than the binary, then unpacks it under
#               --prefix (default ~/.local): no dpkg, no sudo.
#   as root     installs the package that is already built with `dpkg -i` and
#               builds nothing, so `sudo make install` never leaves root-owned
#               files in target/. With no package it stops and says to run
#               `make install` once as the user first.
#
# Running it again changes nothing once the package is current.
#
# Usage: install-deb.sh --deb-dir DIR --version V --binary PATH --make CMD
#                       [--prefix DIR]
set -euo pipefail

deb_dir="" version="" binary="" make_cmd="" prefix="${HOME:-}/.local"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --deb-dir) deb_dir="$2"; shift 2 ;;
        --version) version="$2"; shift 2 ;;
        --binary)  binary="$2";  shift 2 ;;
        --make)    make_cmd="$2"; shift 2 ;;
        --prefix)  prefix="$2";  shift 2 ;;
        *) echo "install-deb.sh: unknown argument: $1" >&2; exit 1 ;;
    esac
done
for v in deb_dir version binary make_cmd prefix; do
    [[ -n "${!v}" ]] || { echo "install-deb.sh: --${v//_/-} is required" >&2; exit 1; }
done

# The package of this version for this machine: cargo-deb names it
# sven_<v>-<revision>_<arch>.deb, build-deb.sh sven_<v>_<arch>.deb.
find_deb() {
    local arch f
    arch="$(dpkg --print-architecture 2>/dev/null || echo '*')"
    for f in "$deb_dir/sven_${version}_${arch}.deb" "$deb_dir/sven_${version}"-*_"${arch}".deb \
             "$deb_dir/sven_${version}_all.deb"; do
        [[ -f "$f" ]] && { echo "$f"; return; }
    done
    return 0
}

deb="$(find_deb)"

if [[ "$(id -u)" -eq 0 ]]; then
    if [[ -z "$deb" ]]; then
        echo "error: no sven $version package in $deb_dir to install." >&2
        echo "       run 'make install' once as your own user (it builds the package" >&2
        echo "       and installs under ~/.local), then 'sudo make install' for the system." >&2
        exit 1
    fi
    echo "Installing $deb system-wide"
    dpkg -i "$deb"
    exit 0
fi

# cargo is incremental, so this is a no-op when the binary is current.
"$make_cmd" build/release
if [[ -z "$deb" || "$deb" -ot "$binary" ]]; then
    "$make_cmd" package/deb/release
    deb="$(find_deb)"
    [[ -n "$deb" ]] || { echo "error: package build produced no sven $version package in $deb_dir" >&2; exit 1; }
fi

command -v dpkg-deb >/dev/null || { echo "error: dpkg-deb not found (install the 'dpkg' package)" >&2; exit 1; }

# Unpack beside the destination, then copy over it: --remove-destination
# replaces a running binary instead of failing with "text file busy".
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
dpkg-deb -x "$deb" "$stage"
mkdir -p "$prefix"
cp -a --remove-destination "$stage/usr/." "$prefix/"
echo "Installed $(basename "$deb") under $prefix"

case ":$PATH:" in
    *":$prefix/bin:"*) ;;
    *) echo "note: $prefix/bin is not on your PATH; add it to run 'sven' directly." ;;
esac
