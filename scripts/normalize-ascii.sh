#!/usr/bin/env bash
# Normalize common non-ASCII punctuation to plain ASCII equivalents.
# Runs as a pre-commit hook; receives staged file paths as arguments.
set -euo pipefail

if [[ $# -eq 0 ]]; then
    exit 0
fi

# Map non-ASCII → ASCII
#   \xe2\x80\x94  (U+2014 EM DASH)           → -
#   \xe2\x80\x93  (U+2013 EN DASH)           → -
#   \xe2\x80\x98  (U+2018 LEFT SINGLE QUOT)  → '
#   \xe2\x80\x99  (U+2019 RIGHT SINGLE QUOT) → '
#   \xe2\x80\x9c  (U+201C LEFT DOUBLE QUOT)  → "
#   \xe2\x80\x9d  (U+201D RIGHT DOUBLE QUOT) → "
#   \xe2\x80\xa6  (U+2026 HORIZONTAL ELLIP)  → ...
#   \xc2\xa0      (U+00A0 NON-BREAKING SP)   → (plain space)

sed_args=(
    -e 's/\xe2\x80\x94/-/g'
    -e 's/\xe2\x80\x93/-/g'
    -e "s/\xe2\x80\x98/'/g"
    -e "s/\xe2\x80\x99/'/g"
    -e 's/\xe2\x80\x9c/"/g'
    -e 's/\xe2\x80\x9d/"/g'
    -e 's/\xe2\x80\xa6/.../g'
    -e 's/\xc2\xa0/ /g'
)

changed=0
for f in "$@"; do
    [[ -f "$f" ]] || continue
    new=$(sed "${sed_args[@]}" "$f")
    if [[ "$new" != "$(cat "$f")" ]]; then
        printf '%s\n' "$new" > "$f"
        git add "$f"
        changed=1
    fi
done

if [[ $changed -eq 1 ]]; then
    echo "normalize-ascii: non-ASCII punctuation replaced and files re-staged."
fi
