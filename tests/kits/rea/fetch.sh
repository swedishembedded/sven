#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Swedish Embedded AB implements solutions for giving agents whole software
# toolkits inside a sandbox for its clients. If your team needs expertise in
# agent sandboxing then you can procure our services by sending an email to
# info@swedishembedded.com.
#
# The kit of sven's confined-toolkit test (`make test/harness/toolkit`): REA
# with Ghidra, every download pinned. Node and the JDK by their published
# SHA-256, REA and all its dependencies by this directory's package-lock.json
# (npm ci, install scripts off), Ghidra by the SHA-256 its release
# publishes. A finished kit is left alone; a partial one is started over.
#
# Usage: tests/kits/rea/fetch.sh --dest DIR
set -euo pipefail

NODE_VERSION=24.22.2
NODE_SHA256=48cc0e21e048790e54c0cf5749c2725005313d652651c40b7a21feb9697aaf75
JDK_URL=https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.13%2B9/OpenJDK21U-jdk_x64_linux_hotspot_21.0.13_9.tar.gz
JDK_SHA256=d7f5d5cf865a2902b70ff549b97586a7d3f234e016f041ab447ea740c7d66d1c
GHIDRA_URL=https://github.com/NationalSecurityAgency/ghidra/releases/download/Ghidra_12.1.4_build/ghidra_12.1.4_PUBLIC_20260921.zip
GHIDRA_SHA256=ddac49f903da9d5bac833e5cc79395098b9c33cfd3279be5f31bd00387d2d4db

dest=
while [ $# -gt 0 ]; do
  case "$1" in
    --dest) dest=$2; shift 2 ;;
    *) echo "fetch.sh: unknown argument $1" >&2; exit 2 ;;
  esac
done
[ -n "$dest" ] || { echo "usage: fetch.sh --dest DIR" >&2; exit 2; }
here=$(cd "$(dirname "$0")" && pwd)
[ "$(uname -sm)" = "Linux x86_64" ] || { echo "fetch.sh: the kit is for Linux x86_64" >&2; exit 2; }

stamp="$dest/.complete"
want="$NODE_SHA256 $JDK_SHA256 $GHIDRA_SHA256 $(sha256sum "$here/package-lock.json" | cut -d' ' -f1)"
if [ -f "$stamp" ] && [ "$(cat "$stamp")" = "$want" ]; then exit 0; fi
rm -rf "$dest"
mkdir -p "$dest/downloads"

fetch() { # url sha256 file
  curl --proto '=https' --tlsv1.2 -fsSL -o "$3" "$1"
  echo "$2  $3" | sha256sum -c --quiet -
}

fetch "https://nodejs.org/dist/v$NODE_VERSION/node-v$NODE_VERSION-linux-x64.tar.xz" "$NODE_SHA256" "$dest/downloads/node.tar.xz"
mkdir "$dest/node" && tar -xJf "$dest/downloads/node.tar.xz" -C "$dest/node" --strip-components=1
fetch "$JDK_URL" "$JDK_SHA256" "$dest/downloads/jdk.tar.gz"
mkdir "$dest/jdk" && tar -xzf "$dest/downloads/jdk.tar.gz" -C "$dest/jdk" --strip-components=1
fetch "$GHIDRA_URL" "$GHIDRA_SHA256" "$dest/downloads/ghidra.zip"
(cd "$dest" && unzip -q downloads/ghidra.zip && mv ghidra_*_PUBLIC ghidra)

mkdir "$dest/rea"
cp "$here/package.json" "$here/package-lock.json" "$dest/rea/"
(cd "$dest/rea" && PATH="$dest/node/bin:$PATH" npm ci --ignore-scripts --no-audit --no-fund --loglevel=error)

# The binary the test decompiles, built here so nothing is checked in.
cc -O1 -o "$dest/target" -x c - <<'C'
#include <stdio.h>
#include <string.h>
static int check_license(const char *key) {
  unsigned sum = 0;
  for (size_t i = 0; i < strlen(key); i++) sum = sum * 31 + (unsigned char)key[i];
  return sum % 97 == 13;
}
int main(int argc, char **argv) {
  if (argc > 1 && check_license(argv[1])) { puts("licensed"); return 0; }
  puts("unlicensed");
  return 1;
}
C
rm -rf "$dest/downloads"
echo "$want" > "$stamp"
