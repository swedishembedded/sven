#!/usr/bin/env bash
# build-deb.sh – Build a Debian package for sven without cargo-deb.
#
# Usage:
#   bash scripts/build-deb.sh [--out-dir DIR]
#
# The resulting .deb is written to the directory specified by --out-dir
# (default: target/debian/).
#
# Prerequisites: cargo (release build already done), dpkg-deb, strip.
set -euo pipefail

# ── Resolve paths ──────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
BINARY="${ROOT}/target/release/sven"

# ── Parse arguments ────────────────────────────────────────────────────────────
OUT_DIR="${ROOT}/target/debian"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --out-dir) OUT_DIR="$2"; shift 2 ;;
        *) echo "Unknown argument: $1" >&2; exit 1 ;;
    esac
done

# ── Sanity checks ──────────────────────────────────────────────────────────────
if [[ ! -x "${BINARY}" ]]; then
    echo "error: release binary not found at ${BINARY}" >&2
    echo "       run 'cargo build --release' first." >&2
    exit 1
fi

if ! command -v dpkg-deb &>/dev/null; then
    echo "error: dpkg-deb not found. Install the 'dpkg' package." >&2
    exit 1
fi

# ── Read metadata from Cargo.toml ─────────────────────────────────────────────
VERSION="$(grep '^version' "${ROOT}/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')"
ARCH="$(dpkg --print-architecture)"
PKG_NAME="sven_${VERSION}_${ARCH}"
STAGING="${ROOT}/target/debian-staging/${PKG_NAME}"

echo "Building ${PKG_NAME}.deb"

# ── Create staging tree ───────────────────────────────────────────────────────
rm -rf "${STAGING}"
install -d \
    "${STAGING}/DEBIAN" \
    "${STAGING}/usr/bin" \
    "${STAGING}/usr/share/bash-completion/completions" \
    "${STAGING}/usr/share/zsh/vendor-completions" \
    "${STAGING}/usr/share/fish/vendor_completions.d" \
    "${STAGING}/usr/share/doc/sven"

# ── Install binary ────────────────────────────────────────────────────────────
install -m 755 "${BINARY}" "${STAGING}/usr/bin/sven"

# ── Shell completions ─────────────────────────────────────────────────────────
COMP_DIR="${ROOT}/target/completions"
mkdir -p "${COMP_DIR}"

echo "  Generating shell completions..."
"${BINARY}" completions bash > "${COMP_DIR}/sven.bash"
"${BINARY}" completions zsh  > "${COMP_DIR}/_sven"
"${BINARY}" completions fish > "${COMP_DIR}/sven.fish"

install -m 644 "${COMP_DIR}/sven.bash" "${STAGING}/usr/share/bash-completion/completions/sven"
install -m 644 "${COMP_DIR}/_sven"     "${STAGING}/usr/share/zsh/vendor-completions/_sven"
install -m 644 "${COMP_DIR}/sven.fish" "${STAGING}/usr/share/fish/vendor_completions.d/sven.fish"

# ── Copyright / changelog ─────────────────────────────────────────────────────
cat > "${STAGING}/usr/share/doc/sven/copyright" <<EOF
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: sven
Source: https://github.com/example/sven

Files: *
Copyright: 2024 Sven Contributors
License: MIT
EOF

# ── Installed-size (in kB) ────────────────────────────────────────────────────
INST_SIZE="$(du -sk "${STAGING}" | cut -f1)"

# ── DEBIAN/control ────────────────────────────────────────────────────────────
cat > "${STAGING}/DEBIAN/control" <<EOF
Package: sven
Version: ${VERSION}
Architecture: ${ARCH}
Maintainer: Sven Team <team@example.com>
Installed-Size: ${INST_SIZE}
Depends: libc6 (>= 2.17)
Section: utils
Priority: optional
Homepage: https://github.com/example/sven
Description: An efficient AI coding agent for CLI and CI
 Sven is an efficient AI coding agent that works both as an interactive
 terminal UI (TUI) and as a headless CI pipeline tool.  It can read
 multi-step instructions from Markdown files or stdin, execute tool calls
 (shell, filesystem, glob search), and stream clean text to stdout so its
 output pipes directly into other agents or CI steps.
EOF

# ── DEBIAN/postinst ───────────────────────────────────────────────────────────
# Nothing strictly required; update-alternatives or similar could go here.
cat > "${STAGING}/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
# Rehash shell completions if the system provides a hook.
if [ -x /usr/lib/command-not-found ] && [ -d /usr/share/bash-completion ]; then
    true
fi
exit 0
EOF
chmod 755 "${STAGING}/DEBIAN/postinst"

# ── Build the package ─────────────────────────────────────────────────────────
mkdir -p "${OUT_DIR}"
DEB_PATH="${OUT_DIR}/${PKG_NAME}.deb"

dpkg-deb --build --root-owner-group "${STAGING}" "${DEB_PATH}"

echo ""
echo "Package built: ${DEB_PATH}"
echo ""
echo "To install:  sudo dpkg -i ${DEB_PATH}"
echo "To inspect:  dpkg-deb --info ${DEB_PATH}"
echo "             dpkg-deb --contents ${DEB_PATH}"
