#!/usr/bin/env bats
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Swedish Embedded AB implements solutions for shipping command-line tools
# that install cleanly everywhere for its clients. If your team needs
# expertise in release engineering then you can procure our services by
# sending an email to info@swedishembedded.com.
#
# The spec of scripts/install.sh (served at https://sven.swedishembedded.com/install.sh),
# against a fake GitHub on the file system (curl reads file:// URLs) and a
# fake `uname`, so nothing leaves the machine:
#   - it installs the newest release of the major version it is written for,
#     never a newer major one, never a pre-release;
#   - it picks the static Linux build for x86_64 or aarch64 and says plainly
#     when a machine is neither;
#   - it installs nothing whose SHA-256 differs from the release's checksums.

setup() {
  SCRIPT="$(cd "$(dirname "$BATS_TEST_FILENAME")/../.." && pwd)/scripts/install.sh"
  MAJOR="$(sed -n 's/^SVEN_MAJOR=\([0-9]*\)$/\1/p' "$SCRIPT")"
  GH="$BATS_TEST_TMPDIR/github"
  BIN="$BATS_TEST_TMPDIR/bin"
  DEST="$BATS_TEST_TMPDIR/install"
  mkdir -p "$GH/download" "$BIN" "$DEST"
  fake_uname Linux x86_64
}

# fake_uname OS ARCH: what `uname -s` and `uname -m` report.
fake_uname() {
  printf '#!/bin/sh\ncase "$1" in -s) echo %s ;; -m) echo %s ;; *) echo %s ;; esac\n' "$1" "$2" "$1" >"$BIN/uname"
  chmod +x "$BIN/uname"
}

# releases TAG...: the releases listing, newest first as GitHub returns it.
releases() {
  {
    echo "["
    local first=1 tag
    for tag in "$@"; do
      [ $first -eq 1 ] || echo ","
      first=0
      printf '  {\n    "url": "x",\n    "tag_name": "%s",\n    "draft": false\n  }\n' "$tag"
    done
    echo "]"
  } >"$GH/releases.json"
}

# publish TAG ASSET...: a release with these assets, each a stand-in binary
# reporting the tag, and their checksums.
publish() {
  local tag="$1"; shift
  mkdir -p "$GH/download/$tag"
  local asset
  for asset in "$@"; do
    printf '#!/bin/sh\necho "sven %s"\n' "${tag#v}" >"$GH/download/$tag/$asset"
  done
  (cd "$GH/download/$tag" && sha256sum "$@" >checksums.txt)
}

run_install() {
  run env PATH="$BIN:$PATH" SVEN_INSTALL_DIR="$DEST" SVEN_NO_SUDO=1 \
    SVEN_RELEASES_API="file://$GH/releases.json" SVEN_DOWNLOAD_BASE="file://$GH/download" \
    "$@" sh "$SCRIPT"
}

@test "the script names the major version it installs" {
  [ -n "$MAJOR" ]
}

@test "installs the newest release of its major version, not a newer major or a pre-release" {
  next=$((MAJOR + 1))
  releases "v$next.0.0" "v$MAJOR.3.0-rc.1" "v$MAJOR.2.1" "v$MAJOR.10.0" "v$MAJOR.2.0" "v$((MAJOR - 1)).9.9"
  publish "v$MAJOR.10.0" sven-linux-x86_64-musl
  run_install
  [ "$status" -eq 0 ]
  [ "$("$DEST/sven" --version)" = "sven $MAJOR.10.0" ]
}

@test "picks the static aarch64 build on arm64" {
  fake_uname Linux aarch64
  releases "v$MAJOR.1.0"
  publish "v$MAJOR.1.0" sven-linux-x86_64-musl sven-linux-aarch64-musl
  run_install
  [ "$status" -eq 0 ]
  [[ "$output" == *"sven-linux-aarch64-musl"* ]]
  [ -x "$DEST/sven" ]
}

@test "says plainly that an unsupported architecture is not supported, and installs nothing" {
  fake_uname Linux riscv64
  releases "v$MAJOR.1.0"
  publish "v$MAJOR.1.0" sven-linux-x86_64-musl sven-linux-aarch64-musl
  run_install
  [ "$status" -ne 0 ]
  [[ "$output" == *"riscv64"* ]]
  [[ "$output" == *"not supported"* ]]
  [ ! -e "$DEST/sven" ]
}

@test "refuses a download whose checksum does not match" {
  releases "v$MAJOR.1.0"
  publish "v$MAJOR.1.0" sven-linux-x86_64-musl
  echo tampered >>"$GH/download/v$MAJOR.1.0/sven-linux-x86_64-musl"
  run_install
  [ "$status" -ne 0 ]
  [[ "$output" == *"checksum"* ]]
  [ ! -e "$DEST/sven" ]
}

@test "says so when no release of its major version exists yet" {
  releases "v$((MAJOR - 1)).9.9"
  run_install
  [ "$status" -ne 0 ]
  [[ "$output" == *"v$MAJOR"* ]]
  [ ! -e "$DEST/sven" ]
}

@test "SVEN_VERSION installs exactly that release" {
  releases "v$MAJOR.2.0" "v$MAJOR.1.0"
  publish "v$MAJOR.1.0" sven-linux-x86_64-musl
  run_install SVEN_VERSION="$MAJOR.1.0"
  [ "$status" -eq 0 ]
  [ "$("$DEST/sven" --version)" = "sven $MAJOR.1.0" ]
}
