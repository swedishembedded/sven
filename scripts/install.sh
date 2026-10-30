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
#   SVEN_INSTALL_DIR      where to install. Default: your own ~/.local/bin
#                         (or ~/bin when only that is on PATH), with no sudo;
#                         /usr/local/bin when run as root
#   SVEN_NO_SUDO=1        never use sudo, even for a SVEN_INSTALL_DIR that
#                         needs it
#   SVEN_NO_MODIFY_PATH=1 never add the install directory to a shell's
#                         startup file; print the line to add instead
set -eu

SVEN_MAJOR=4

REPO="swedishembedded/sven"
RELEASES_API="${SVEN_RELEASES_API:-https://api.github.com/repos/${REPO}/releases?per_page=100}"
DOWNLOAD_BASE="${SVEN_DOWNLOAD_BASE:-https://github.com/${REPO}/releases/download}"
USER_DIR="$HOME/.local/bin"

on_path() {
	case ":$PATH:" in *":$1:"*) return 0 ;; *) return 1 ;; esac
}

# A person installs for themselves, into a directory they own: no sudo, ever,
# unless they name a directory that needs it. Root (a Dockerfile, a
# provisioning script) installs for everyone, into /usr/local/bin. Never
# /usr/bin: macOS protects it even from root, and on Linux it belongs to the
# package manager.
if [ -n "${SVEN_INSTALL_DIR:-}" ]; then
	INSTALL_DIR="$SVEN_INSTALL_DIR"
elif [ "$(id -u)" = 0 ]; then
	INSTALL_DIR=/usr/local/bin
elif ! on_path "$USER_DIR" && on_path "$HOME/bin"; then
	INSTALL_DIR="$HOME/bin"
else
	INSTALL_DIR="$USER_DIR"
fi

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
elif [ "${SVEN_NO_SUDO:-0}" = 1 ] || ! command -v sudo >/dev/null 2>&1; then
	fail "$INSTALL_DIR is not writable; choose another SVEN_INSTALL_DIR, or leave it unset to install into your own $USER_DIR"
else
	info "Installing to $INSTALL_DIR with sudo (it may ask for your password)"
	if ! { [ -d "$INSTALL_DIR" ] || sudo mkdir -p "$INSTALL_DIR"; } ||
		! sudo install -m 0755 "$TMP/sven" "$INSTALL_DIR/sven"; then
		fail "could not install into $INSTALL_DIR; choose another SVEN_INSTALL_DIR, or leave it unset to install into your own $USER_DIR"
	fi
fi

printf '\n%s%ssven %s is installed in %s%s\n\n' "$BOLD" "$GREEN" "${TAG#v}" "$INSTALL_DIR" "$RESET"
# The file a new terminal of the person's shell reads at start.
startup_file() {
	case "$(basename "${SHELL:-sh}")" in
	zsh) echo "${ZDOTDIR:-$HOME}/.zshrc" ;;
	# Terminals on macOS start login shells, which read .bash_profile.
	bash) if [ "$(uname -s)" = Darwin ]; then echo "$HOME/.bash_profile"; else echo "$HOME/.bashrc"; fi ;;
	fish) echo "${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d/sven.fish" ;;
	*) echo "$HOME/.profile" ;;
	esac
}

# Whether a new terminal finds sven: the person's own shell, started the way
# a terminal starts it, with nothing to read from and at most ten seconds to
# answer, is asked where `sven` is. Interactive; and a login shell too when
# the line went into a profile, which only login shells read (macOS
# terminals start bash as one).
new_terminal_finds_sven() {
	[ -n "${SHELL:-}" ] && [ -x "$SHELL" ] || return 1
	case "$file" in
	*profile) set -- -l -i ;;
	*) set -- -i ;;
	esac
	"$SHELL" "$@" -c 'command -v sven' </dev/null >"$TMP/found" 2>/dev/null &
	shell_pid=$!
	(sleep 10 && kill "$shell_pid" 2>/dev/null) &
	watchdog=$!
	wait "$shell_pid" 2>/dev/null || true
	kill "$watchdog" 2>/dev/null || true
	[ "$(tail -n 1 "$TMP/found" 2>/dev/null)" = "$INSTALL_DIR/sven" ]
}

# sven must start in the next terminal without the person doing anything: a
# directory not on PATH yet is added to their shell's startup file, once, and
# then checked the way they will use it.
if ! on_path "$INSTALL_DIR"; then
	file="$(startup_file)"
	# shellcheck disable=SC2016 # $PATH is written for the shell to expand
	case "$file" in
	*.fish) line="fish_add_path -g $INSTALL_DIR" ;;
	*) line="export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
	esac
	if [ "${SVEN_NO_MODIFY_PATH:-0}" = 1 ]; then
		printf 'Add it to your PATH:\n  %s\n\n' "$line"
	else
		if ! grep -qs 'added by the sven installer' "$file"; then
			mkdir -p "$(dirname "$file")"
			printf '\n# added by the sven installer\n%s\n' "$line" >>"$file"
			info "Added $INSTALL_DIR to your PATH in $file"
		fi
		if new_terminal_finds_sven; then
			info "A new terminal finds sven. In this one, run:  $line"
		else
			printf 'A new terminal did not find sven yet. Add this line to your shell'"'"'s startup file, then open a new terminal:\n  %s\n\n' "$line"
		fi
	fi
fi
printf 'Get started: sven --help\nDocumentation: https://swedishembedded.com/docs/sven\n'
