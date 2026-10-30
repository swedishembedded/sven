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

@test "skips a release whose binaries are not published yet, for the newest complete one" {
  # A release exists as soon as it is tagged; its binaries and checksums
  # arrive when its build finishes. Until then it is not installable.
  releases "v$MAJOR.2.0" "v$MAJOR.1.0"
  mkdir -p "$GH/download/v$MAJOR.2.0"
  publish "v$MAJOR.1.0" sven-linux-x86_64-musl
  run_install
  [ "$status" -eq 0 ]
  [ "$("$DEST/sven" --version)" = "sven $MAJOR.1.0" ]
}

@test "skips a release that does not build for this platform" {
  releases "v$MAJOR.2.0" "v$MAJOR.1.0"
  publish "v$MAJOR.2.0" sven-linux-aarch64-musl
  publish "v$MAJOR.1.0" sven-linux-x86_64-musl
  run_install
  [ "$status" -eq 0 ]
  [ "$("$DEST/sven" --version)" = "sven $MAJOR.1.0" ]
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

@test "creates the install directory when it does not exist yet" {
  releases "v${MAJOR}.0.1"
  publish "v${MAJOR}.0.1" sven-linux-x86_64-musl
  rmdir "$DEST"
  run_install
  [ "$status" -eq 0 ]
  [ -x "$DEST/sven" ]
}

# macOS: /usr/local/bin is often missing (Apple Silicon), only root may create
# it, and BSD `install` does not create directories: it writes a temporary
# INS@XXXXXX in the destination and fails with "No such file or directory".
@test "creates a missing directory with sudo before installing into it, as BSD install needs" {
  releases "v${MAJOR}.0.1"
  publish "v${MAJOR}.0.1" sven-linux-x86_64-musl
  local root="$BATS_TEST_TMPDIR/root"
  mkdir -p "$root" && chmod 0555 "$root"
  # sudo: records each command and runs it with the protected tree writable,
  # standing in for root.
  printf '#!/bin/sh\necho "$*" >>"%s/sudo.log"\nchmod u+w "%s"\n"$@"; s=$?\nchmod 0555 "%s"\nexit $s\n' \
    "$BATS_TEST_TMPDIR" "$root" "$root" >"$BIN/sudo"
  # install: BSD's, which never creates the destination directory.
  printf '#!/bin/sh\nfor last; do :; done\nd=$(dirname "$last")\n[ -d "$d" ] || { echo "install: $d/INS@Lr01hQ: No such file or directory" >&2; exit 71; }\ncp "$3" "$last" && chmod 0755 "$last"\n' >"$BIN/install"
  chmod +x "$BIN/sudo" "$BIN/install"
  run env PATH="$BIN:$PATH" SVEN_INSTALL_DIR="$root/usr/local/bin" \
    SVEN_RELEASES_API="file://$GH/releases.json" SVEN_DOWNLOAD_BASE="file://$GH/download" sh "$SCRIPT"
  chmod -R u+w "$root"
  [ "$status" -eq 0 ]
  [ -x "$root/usr/local/bin/sven" ]
  grep -q "mkdir -p $root/usr/local/bin" "$BATS_TEST_TMPDIR/sudo.log"
}

# run_default ENV...: the installer with no SVEN_INSTALL_DIR and no sudo on
# PATH, HOME a scratch home, and PATH limited to the fakes plus the system.
run_default() {
  HOME_DIR="$BATS_TEST_TMPDIR/home"
  mkdir -p "$HOME_DIR"
  run env -i HOME="$HOME_DIR" SHELL=/bin/zsh "$@" \
    SVEN_RELEASES_API="file://$GH/releases.json" SVEN_DOWNLOAD_BASE="file://$GH/download" \
    sh -c 'PATH="$BIN_PATH"; export PATH; exec sh "$0"' "$SCRIPT"
}

@test "installs without sudo into ~/.local/bin when it is already on PATH" {
  releases "v${MAJOR}.0.1"
  publish "v${MAJOR}.0.1" sven-linux-x86_64-musl
  printf '#!/bin/sh\necho "sudo must not be used" >&2; exit 1\n' >"$BIN/sudo" && chmod +x "$BIN/sudo"
  mkdir -p "$BATS_TEST_TMPDIR/home/.local/bin"
  run_default BIN_PATH="$BATS_TEST_TMPDIR/home/.local/bin:$BIN:/usr/bin:/bin"
  [ "$status" -eq 0 ]
  [ -x "$HOME_DIR/.local/bin/sven" ]
  [ ! -e "$HOME_DIR/.zshrc" ]
}


# forbidden_sudo: a sudo that fails the test if the installer touches it.
forbidden_sudo() {
  printf '#!/bin/sh\necho "sudo must not be used" >&2; exit 1\n' >"$BIN/sudo" && chmod +x "$BIN/sudo"
}

@test "a normal user installs into ~/.local/bin without sudo and gets it on PATH in the shell's startup file, once" {
  releases "v${MAJOR}.0.1"
  publish "v${MAJOR}.0.1" sven-linux-x86_64-musl
  forbidden_sudo
  run_default BIN_PATH="$BIN:/usr/bin:/bin"
  [ "$status" -eq 0 ]
  [[ "$output" != *"sudo must not be used"* ]]
  [ -x "$HOME_DIR/.local/bin/sven" ]
  grep -q '\.local/bin' "$HOME_DIR/.zshrc"
  run_default BIN_PATH="$BIN:/usr/bin:/bin"
  [ "$status" -eq 0 ]
  [ "$(grep -c 'added by the sven installer' "$HOME_DIR/.zshrc")" -eq 1 ]
}

@test "checks that a new terminal of the person's shell finds sven" {
  releases "v${MAJOR}.0.1"
  publish "v${MAJOR}.0.1" sven-linux-x86_64-musl
  forbidden_sudo
  HOME_DIR="$BATS_TEST_TMPDIR/home"
  mkdir -p "$HOME_DIR"
  run env -i HOME="$HOME_DIR" SHELL=/bin/bash \
    SVEN_RELEASES_API="file://$GH/releases.json" SVEN_DOWNLOAD_BASE="file://$GH/download" \
    sh -c 'PATH="$0"; export PATH; exec sh "$1"' "$BIN:/usr/bin:/bin" "$SCRIPT"
  [ "$status" -eq 0 ]
  grep -q 'added by the sven installer' "$HOME_DIR/.bashrc"
  [[ "$output" == *"A new terminal finds sven"* ]]
}

@test "fish gets the directory in its own configuration" {
  command -v fish >/dev/null || skip "fish is not installed here"
  releases "v${MAJOR}.0.1"
  publish "v${MAJOR}.0.1" sven-linux-x86_64-musl
  forbidden_sudo
  run_default BIN_PATH="$BIN:/usr/bin:/bin" SHELL="$(command -v fish)"
  [ "$status" -eq 0 ]
  grep -q '\.local/bin' "$HOME_DIR/.config/fish/conf.d/sven.fish"
  [ ! -e "$HOME_DIR/.zshrc" ]
}

@test "SVEN_NO_MODIFY_PATH=1 leaves the shell's startup files alone" {
  releases "v${MAJOR}.0.1"
  publish "v${MAJOR}.0.1" sven-linux-x86_64-musl
  forbidden_sudo
  run_default BIN_PATH="$BIN:/usr/bin:/bin" SVEN_NO_MODIFY_PATH=1
  [ "$status" -eq 0 ]
  [ -x "$HOME_DIR/.local/bin/sven" ]
  [ ! -e "$HOME_DIR/.zshrc" ]
  [[ "$output" == *'export PATH='* ]]
}
