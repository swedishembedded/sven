#!/usr/bin/env sh
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Swedish Embedded AB implements solutions for shipping command-line tools
# that install cleanly everywhere for its clients. If your team needs
# expertise in release engineering then you can procure our services by
# sending an email to info@swedishembedded.com.
#
# install.sh - install sven from its GitHub releases.
#
#   curl -fsSL https://sven.swedishembedded.com/install.sh | sh
#
# Installs the newest release of sven ${SVEN_MAJOR}.x: patch and minor
# releases arrive with no change here, a new major version never does (it
# may break what you built on this one). Moving this script to the next
# major version is a deliberate edit of SVEN_MAJOR, made with that release.
#
# Linux on x86_64 and aarch64 gets the static build, which runs on any
# distribution (glibc or musl); macOS gets the universal build. The download
# is checked against the release's published SHA-256 before it is installed.
#
# Environment:
#   SVEN_VERSION      install exactly this version (e.g. 4.1.0)
#   SVEN_INSTALL_DIR  where to install (default /usr/local/bin)
#   SVEN_NO_SUDO=1    never use sudo
set -eu

SVEN_MAJOR=4

REPO="swedishembedded/sven"
RELEASES_API="${SVEN_RELEASES_API:-https://api.github.com/repos/${REPO}/releases?per_page=100}"
DOWNLOAD_BASE="${SVEN_DOWNLOAD_BASE:-https://github.com/${REPO}/releases/download}"
INSTALL_DIR="${SVEN_INSTALL_DIR:-/usr/local/bin}"

if [ -t 1 ]; then
	BOLD="$(printf '\033[1m')" GREEN="$(printf '\033[32m')" RED="$(printf '\033[31m')" RESET="$(printf '\033[0m')"
else
	BOLD="" GREEN="" RED="" RESET=""
fi
info() { printf '%s==>%s %s\n' "$GREEN" "$RESET" "$*"; }
fail() {
	printf '%serror:%s %s\n' "$RED" "$RESET" "$*" >&2
	exit 1
}

for cmd in curl uname; do
	command -v "$cmd" >/dev/null 2>&1 || fail "$cmd is required"
done
if command -v sha256sum >/dev/null 2>&1; then
	sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
	sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
	fail "sha256sum or shasum is required to verify the download"
fi

OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS/$ARCH" in
Linux/x86_64 | Linux/amd64) ASSET=sven-linux-x86_64-musl ;;
Linux/aarch64 | Linux/arm64) ASSET=sven-linux-aarch64-musl ;;
Darwin/x86_64 | Darwin/arm64) ASSET=sven-darwin-universal ;;
*)
	fail "$OS on $ARCH is not supported. sven runs on Linux (x86_64, aarch64) and macOS;
  other builds, if any, are at https://github.com/${REPO}/releases"
	;;
esac

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

# Whether release $1 is complete for this platform: it publishes its
# checksums and they list $ASSET. A release exists as soon as it is tagged;
# its binaries arrive when its build finishes.
release_complete() {
	curl -fsSL "${DOWNLOAD_BASE}/$1/checksums.txt" -o "$TMP/checksums.txt" 2>/dev/null &&
		awk -v a="$ASSET" '$2 == a || $2 == "*" a { found = 1 } END { exit !found }' "$TMP/checksums.txt"
}

if [ -n "${SVEN_VERSION:-}" ]; then
	TAG="v${SVEN_VERSION#v}"
	release_complete "$TAG" ||
		fail "sven ${TAG} publishes no ${ASSET} build (yet): https://github.com/${REPO}/releases/tag/${TAG}"
else
	# The vMAJOR.MINOR.PATCH releases, newest first; pre-releases (a suffix)
	# never match. The newest complete one of the ten newest is installed.
	CANDIDATES="$(curl -fsSL "$RELEASES_API" |
		sed -n 's/.*"tag_name": *"\(v[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\)".*/\1/p' |
		grep "^v${SVEN_MAJOR}\." |
		sort -t. -k1,1 -k2,2nr -k3,3nr | head -n 10)" ||
		fail "could not read sven's releases from GitHub"
	[ -n "$CANDIDATES" ] || fail "no release of sven v${SVEN_MAJOR} is published yet: https://github.com/${REPO}/releases"
	TAG=""
	for candidate in $CANDIDATES; do
		if release_complete "$candidate"; then
			TAG="$candidate"
			break
		fi
		info "sven ${candidate} has no ${ASSET} build yet; trying the release before it"
	done
	[ -n "$TAG" ] || fail "no release of sven v${SVEN_MAJOR} has a ${ASSET} build yet: https://github.com/${REPO}/releases"
fi
info "Installing sven ${TAG} (${ASSET})"

curl -fsSL "${DOWNLOAD_BASE}/${TAG}/${ASSET}" -o "$TMP/sven" ||
	fail "sven ${TAG} lists ${ASSET} but it could not be downloaded: https://github.com/${REPO}/releases/tag/${TAG}"

expected="$(awk -v a="$ASSET" '$2 == a || $2 == "*" a { print $1 }' "$TMP/checksums.txt")"
[ -n "$expected" ] || fail "sven ${TAG}'s checksums do not list ${ASSET}"
[ "$(sha256 "$TMP/sven")" = "$expected" ] ||
	fail "the download does not match its published checksum; nothing was installed"
chmod +x "$TMP/sven"
"$TMP/sven" --version >/dev/null 2>&1 || fail "the downloaded sven does not run on this machine"

# The directory may not exist yet: /usr/local/bin is missing on many Macs
# (Apple Silicon), and install(1) on macOS never creates its destination.
[ -d "$INSTALL_DIR" ] || mkdir -p "$INSTALL_DIR" 2>/dev/null || true
if [ -w "$INSTALL_DIR" ]; then
	mv "$TMP/sven" "$INSTALL_DIR/sven"
elif [ "${SVEN_NO_SUDO:-0}" = 1 ]; then
	fail "$INSTALL_DIR is not writable and SVEN_NO_SUDO=1 forbids sudo; set SVEN_INSTALL_DIR"
elif command -v sudo >/dev/null 2>&1; then
	info "Installing to $INSTALL_DIR with sudo"
	[ -d "$INSTALL_DIR" ] || sudo mkdir -p "$INSTALL_DIR"
	sudo install -m 0755 "$TMP/sven" "$INSTALL_DIR/sven"
else
	INSTALL_DIR="$HOME/.local/bin"
	info "Installing to $INSTALL_DIR (the default directory is not writable)"
	mkdir -p "$INSTALL_DIR"
	mv "$TMP/sven" "$INSTALL_DIR/sven"
fi

printf '\n%s%ssven %s is installed in %s%s\n\n' "$BOLD" "$GREEN" "${TAG#v}" "$INSTALL_DIR" "$RESET"
# shellcheck disable=SC2016 # $PATH is printed for the person to copy
case ":$PATH:" in
*":$INSTALL_DIR:"*) ;;
*) printf 'Add it to your PATH:\n  export PATH="%s:$PATH"\n\n' "$INSTALL_DIR" ;;
esac
printf 'Get started: sven --help\nDocumentation: https://swedishembedded.com/docs/sven\n'
