#!/usr/bin/env sh
# install.sh — Install sven from the latest GitHub release.
#
# One-liner usage:
#   curl -fsSL https://agentsven.com/install | sh
#
# Options (set as env vars before piping):
#   SVEN_VERSION   — install a specific version (e.g. "0.2.1"), default: latest
#   SVEN_INSTALL_DIR — install directory, default: /usr/local/bin
#   SVEN_NO_SUDO   — set to 1 to never use sudo (fail if dir not writable)
set -eu

REPO="swedishembedded/sven"
INSTALL_DIR="${SVEN_INSTALL_DIR:-/usr/local/bin}"
BIN="sven"

# ── Colours (only when stdout is a terminal) ──────────────────────────────────
if [ -t 1 ]; then
    BOLD="\033[1m"; GREEN="\033[32m"; YELLOW="\033[33m"; RED="\033[31m"; RESET="\033[0m"
else
    BOLD=""; GREEN=""; YELLOW=""; RED=""; RESET=""
fi

info()    { printf "${GREEN}▶${RESET}  %s\n" "$*"; }
warn()    { printf "${YELLOW}⚠${RESET}  %s\n" "$*" >&2; }
error()   { printf "${RED}✖${RESET}  %s\n" "$*" >&2; exit 1; }
section() { printf "\n${BOLD}%s${RESET}\n" "$*"; }

# ── Dependency checks ─────────────────────────────────────────────────────────
for cmd in curl uname; do
    command -v "$cmd" >/dev/null 2>&1 || error "Required command not found: $cmd"
done

# ── Detect OS and architecture ────────────────────────────────────────────────
section "Detecting platform..."

OS=$(uname -s)
ARCH=$(uname -m)

case "${OS}" in
    Linux)
        case "${ARCH}" in
            x86_64)          ASSET="sven-linux-x86_64" ;;
            aarch64|arm64)   ASSET="sven-linux-aarch64" ;;
            *)
                error "Unsupported Linux architecture: ${ARCH}
  Supported: x86_64, aarch64
  Visit https://github.com/${REPO}/releases for manual download."
                ;;
        esac
        ;;
    Darwin)
        ASSET="sven-darwin-universal"
        ;;
    *)
        error "Unsupported OS: ${OS}
  Supported: Linux, macOS
  Visit https://github.com/${REPO}/releases for manual download."
        ;;
esac

info "Platform: ${OS}/${ARCH} → ${ASSET}"

# ── Resolve version ───────────────────────────────────────────────────────────
section "Resolving version..."

if [ -n "${SVEN_VERSION:-}" ]; then
    VERSION="${SVEN_VERSION}"
    # Normalise: accept both "0.2.1" and "v0.2.1"
    case "${VERSION}" in
        v*) TAG="${VERSION}" ;;
        *)  TAG="v${VERSION}" ;;
    esac
    info "Requested version: ${TAG}"
else
    info "Fetching latest release..."
    TAG=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
        | grep '"tag_name"' \
        | sed 's/.*"tag_name": *"\(.*\)".*/\1/')
    [ -n "${TAG}" ] || error "Could not determine latest release. Check your internet connection."
    VERSION="${TAG#v}"
    info "Latest version: ${TAG}"
fi

# ── Download ──────────────────────────────────────────────────────────────────
section "Downloading sven ${TAG}..."

URL="https://github.com/${REPO}/releases/download/${TAG}/${ASSET}"
info "URL: ${URL}"

TMP=$(mktemp /tmp/sven.XXXXXX)
trap 'rm -f "${TMP}"' EXIT INT TERM

if ! curl -fsSL --progress-bar "${URL}" -o "${TMP}"; then
    error "Download failed.
  URL: ${URL}
  Check that version ${TAG} exists: https://github.com/${REPO}/releases"
fi

chmod +x "${TMP}"

# ── Verify the binary runs ────────────────────────────────────────────────────
if ! "${TMP}" --version >/dev/null 2>&1; then
    error "Downloaded binary failed to execute. This may be a platform compatibility issue."
fi

INSTALLED_VERSION=$("${TMP}" --version 2>/dev/null | head -1 || echo "unknown")
info "Binary OK: ${INSTALLED_VERSION}"

# ── Install ───────────────────────────────────────────────────────────────────
section "Installing to ${INSTALL_DIR}/${BIN}..."

if [ -w "${INSTALL_DIR}" ]; then
    mv "${TMP}" "${INSTALL_DIR}/${BIN}"
elif [ "${SVEN_NO_SUDO:-0}" = "1" ]; then
    error "Install directory ${INSTALL_DIR} is not writable and SVEN_NO_SUDO=1 is set."
elif command -v sudo >/dev/null 2>&1; then
    info "Using sudo to install (directory not writable by current user)..."
    sudo mv "${TMP}" "${INSTALL_DIR}/${BIN}"
    sudo chmod +x "${INSTALL_DIR}/${BIN}"
else
    # Fallback: install to ~/.local/bin
    LOCAL_BIN="${HOME}/.local/bin"
    warn "Cannot write to ${INSTALL_DIR} and sudo is not available."
    warn "Installing to ${LOCAL_BIN} instead."
    mkdir -p "${LOCAL_BIN}"
    mv "${TMP}" "${LOCAL_BIN}/${BIN}"
    INSTALL_DIR="${LOCAL_BIN}"
fi

# ── PATH check ────────────────────────────────────────────────────────────────
printf "\n${BOLD}${GREEN}✔  sven ${TAG} installed to ${INSTALL_DIR}/${BIN}${RESET}\n\n"

if ! command -v sven >/dev/null 2>&1; then
    warn "sven is not yet in your PATH."
    warn "Add this to your shell profile (~/.bashrc / ~/.zshrc):"
    printf "\n    export PATH=\"%s:\$PATH\"\n\n" "${INSTALL_DIR}"
fi

printf "Get started:\n"
printf "  sven --help\n"
printf "  sven --version\n\n"
printf "Documentation: https://agentsven.com/docs\n\n"
