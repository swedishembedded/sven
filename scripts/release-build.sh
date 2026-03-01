#!/usr/bin/env bash
# release-build.sh – Build release artifacts for the current platform.
#
# Outputs binaries and (on Linux) .deb packages into dist/.
# On Linux this requires dpkg-deb. cargo-deb is used when available, otherwise
# scripts/build-deb.sh is used as a fallback.
#
# Usage:
#   bash scripts/release-build.sh [--out-dir DIR]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# ── Colours ───────────────────────────────────────────────────────────────────
BOLD="\033[1m"; GREEN="\033[32m"; YELLOW="\033[33m"; RESET="\033[0m"
info()    { printf "${GREEN}▶${RESET}  %s\n" "$*"; }
warn()    { printf "${YELLOW}⚠${RESET}  %s\n" "$*"; }
section() { printf "\n${BOLD}%s${RESET}\n" "$*"; }

# ── Args ──────────────────────────────────────────────────────────────────────
OUT_DIR="${ROOT}/dist"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --out-dir) OUT_DIR="$2"; shift 2 ;;
        *) echo "Unknown argument: $1" >&2; exit 1 ;;
    esac
done

mkdir -p "${OUT_DIR}"

# ── Detect platform ───────────────────────────────────────────────────────────
OS="$(uname -s)"
ARCH="$(uname -m)"

case "${OS}/${ARCH}" in
    Linux/x86_64)   ASSET_NAME="sven-linux-x86_64" ;;
    Linux/aarch64)  ASSET_NAME="sven-linux-aarch64" ;;
    Darwin/x86_64)  ASSET_NAME="sven-darwin-x86_64" ;;
    Darwin/arm64)   ASSET_NAME="sven-darwin-aarch64" ;;
    *)              ASSET_NAME="sven-${OS,,}-${ARCH}" ;;
esac

# ── Read version ──────────────────────────────────────────────────────────────
VERSION="$(grep '^version' "${ROOT}/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/')"
TAG="v${VERSION}"

section "Building sven ${TAG} for ${OS}/${ARCH}"

# ── Cargo release build ───────────────────────────────────────────────────────
section "Compiling..."
cd "${ROOT}"
cargo build --release

BINARY="${ROOT}/target/release/sven"

# ── Generate shell completions ────────────────────────────────────────────────
section "Generating shell completions..."
mkdir -p "${ROOT}/target/completions"
"${BINARY}" completions bash > "${ROOT}/target/completions/sven.bash"
"${BINARY}" completions zsh  > "${ROOT}/target/completions/_sven"
"${BINARY}" completions fish > "${ROOT}/target/completions/sven.fish"
info "Completions generated in target/completions/"

# ── Copy binary ───────────────────────────────────────────────────────────────
section "Staging artifacts..."
cp "${BINARY}" "${OUT_DIR}/${ASSET_NAME}"
info "Binary: ${OUT_DIR}/${ASSET_NAME}"

# ── Build .deb on Linux ───────────────────────────────────────────────────────
if [[ "${OS}" == "Linux" ]]; then
    if command -v dpkg-deb &>/dev/null; then
        section "Building .deb package..."
        if command -v cargo-deb &>/dev/null; then
            info "Using cargo-deb..."
            cargo deb --output "${OUT_DIR}/"
        else
            warn "cargo-deb not found, using scripts/build-deb.sh..."
            bash "${SCRIPT_DIR}/build-deb.sh" --out-dir "${OUT_DIR}"
        fi
        DEB_FILE="$(ls "${OUT_DIR}"/sven_*.deb 2>/dev/null | head -1 || true)"
        if [[ -n "${DEB_FILE}" ]]; then
            # Rename to canonical release name for consistency
            ARCH_SHORT="$(dpkg --print-architecture)"
            CANONICAL="${OUT_DIR}/sven-linux-${ARCH_SHORT/amd64/x86_64}.deb"
            # Keep both the versioned deb and the canonical name
            cp "${DEB_FILE}" "${CANONICAL}" 2>/dev/null || true
            info "Deb package: ${DEB_FILE}"
        fi
    else
        warn "dpkg-deb not found; skipping .deb build."
    fi
fi

# ── Checksums ─────────────────────────────────────────────────────────────────
section "Generating checksums..."
cd "${OUT_DIR}"
sha256sum sven-* > checksums.txt 2>/dev/null || shasum -a 256 sven-* > checksums.txt
info "checksums.txt written"

# ── Summary ───────────────────────────────────────────────────────────────────
printf "\n${BOLD}${GREEN}✔  Build complete — sven ${TAG}${RESET}\n\n"
ls -lh "${OUT_DIR}"/
printf "\nNext steps:\n"
printf "  make release/tag      — tag ${TAG} and push (triggers CI multi-arch release)\n"
printf "  make release/publish  — publish this platform's artifacts to GitHub Releases\n\n"
