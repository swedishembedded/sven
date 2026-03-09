#!/usr/bin/env bash
# Run cargo clippy only on the packages that contain the given files.
# Used by pre-commit: with a normal commit only staged files are passed, so
# only those packages are checked. With "pre-commit run --all-files" all Rust
# files are passed, so the whole workspace is checked.
set -euo pipefail

if [[ $# -eq 0 ]]; then
    exit 0
fi

# Derive package names from file paths:
#   crates/<name>/...  -> package <name>
#   else (e.g. src/...) -> root package "sven"
packages=()
for f in "$@"; do
    if [[ "$f" =~ ^crates/([^/]+)/ ]]; then
        packages+=("${BASH_REMATCH[1]}")
    else
        packages+=("sven")
    fi
done

# Deduplicate and sort
sorted=()
while IFS= read -r p; do sorted+=("$p"); done < <(printf '%s\n' "${packages[@]}" | sort -u)
packages=("${sorted[@]}")

# Build -p <pkg> args (each as separate arguments)
cli_args=()
for p in "${packages[@]}"; do
    cli_args+=(-p "$p")
done

# Run clippy only on those packages
exec env CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}" cargo clippy \
    "${cli_args[@]}" \
    --all-targets \
    --fix \
    --allow-dirty \
    --allow-staged \
    -- \
    -D warnings
