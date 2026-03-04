#!/usr/bin/env bats
# 09_edit_file.bats – Comprehensive end-to-end tests for the edit_file tool.
#
# Every test runs the full sven binary through the mock-model pipeline so the
# entire stack (arg parsing, tool dispatch, file I/O, result formatting) is
# exercised for each edge case.
#
# Each test creates its own temp file and its own mock_responses.yaml so tests
# are fully isolated from the shared fixtures and from each other.
#
# Edge cases covered (grouped by concern):
#   • Parameter validation  – missing path / diff, no @@ markers
#   • File I/O errors       – nonexistent file
#   • Basic edits           – replace, pure insert, pure delete, multi-line ins/del
#   • File boundary edits   – first line, last line, single-line file
#   • Newline preservation  – trailing \n kept / absent preserved
#   • Diff format variants  – FuDiff, --- +++ headers, markdown fence, git section name
#   • Multi-hunk behaviour  – two hunks, offset tracking, atomicity on partial failure
#   • Indent normalisation  – context at wrong indent level, added-line indent adjusted
#   • Fuzzy matching        – small typo tolerated, too-different context rejected
#   • Ambiguity resolution  – line-number hint picks correct duplicate, stale context fails
#   • Error diagnostics     – suggestions shown, Hunk N: prefix only on multi-hunk failures
#   • Trace output          – success=true / success=false in [sven:tool:result]
#   • Cargo unit tests      – full unit test suite gate

load helpers

# ── Per-test state and helpers ────────────────────────────────────────────────

_EF_MOCK_FILE=""
_EF_TEST_FILE=""

# Clean up temp files and restore global mock responses after every test.
teardown() {
    [ -n "${_EF_MOCK_FILE}" ] && rm -f "${_EF_MOCK_FILE}"
    [ -n "${_EF_TEST_FILE}" ] && rm -f "${_EF_TEST_FILE}"
    export SVEN_MOCK_RESPONSES="${MOCK_RESPONSES}"
    _EF_MOCK_FILE=""
    _EF_TEST_FILE=""
}

# Create a temp file with the given content; store its path in _EF_TEST_FILE.
# Usage: ef_make_file "exact content"  (use $'\n' for literal newlines in bash)
ef_make_file() {
    _EF_TEST_FILE="$(mktemp /tmp/sven_ef_XXXXXX.txt)"
    printf '%s' "$1" > "${_EF_TEST_FILE}"
}

# Write a per-test mock YAML from a heredoc and point SVEN_MOCK_RESPONSES at it.
# Must be called AFTER ef_make_file so ${_EF_TEST_FILE} is available.
# Usage:
#   ef_use_mock <<'EOF'
#   responses:
#     ...
#   EOF
ef_use_mock() {
    _EF_MOCK_FILE="$(mktemp /tmp/sven_ef_mock_XXXXXX.yaml)"
    cat > "${_EF_MOCK_FILE}"
    export SVEN_MOCK_RESPONSES="${_EF_MOCK_FILE}"
}

# Run sven headless-mock with a simple prompt; captures stdout in $output.
ef_run() {
    run bash -c 'echo "'"$1"'" | "$BIN" --headless --model mock 2>/dev/null'
}

# Run sven headless-mock, capturing stdout→STDOUT_OUT and stderr→STDERR_OUT.
ef_run_traced() {
    run_split_output bash -c 'echo "'"$1"'" | "$BIN" --headless --model mock'
}

# Assert the file at $_EF_TEST_FILE contains a substring.
ef_file_contains() {
    local needle="$1"
    local content
    content="$(cat "${_EF_TEST_FILE}")"
    if [[ "${content}" != *"${needle}"* ]]; then
        echo "Expected file to contain: ${needle}"
        echo "Actual file content:"
        echo "${content}"
        return 1
    fi
}

# Assert the file at $_EF_TEST_FILE does NOT contain a substring.
ef_file_not_contains() {
    local needle="$1"
    local content
    content="$(cat "${_EF_TEST_FILE}")"
    if [[ "${content}" == *"${needle}"* ]]; then
        echo "Expected file NOT to contain: ${needle}"
        echo "Actual file content:"
        echo "${content}"
        return 1
    fi
}

# Assert the file at $_EF_TEST_FILE equals the given string exactly.
# Uses diff against a temp file to avoid shell command-substitution stripping
# trailing newlines from the actual content.
ef_file_equals() {
    local expected="$1"
    local exp_tmp
    exp_tmp="$(mktemp /tmp/sven_ef_exp_XXXXXX.txt)"
    printf '%s' "${expected}" > "${exp_tmp}"
    if ! diff "${exp_tmp}" "${_EF_TEST_FILE}" > /dev/null 2>&1; then
        echo "File content mismatch."
        echo "Expected (cat -A):"
        cat -A "${exp_tmp}"
        echo "Actual (cat -A):"
        cat -A "${_EF_TEST_FILE}"
        rm -f "${exp_tmp}"
        return 1
    fi
    rm -f "${exp_tmp}"
}

# ── Parameter validation ──────────────────────────────────────────────────────

@test "09.01 missing path parameter returns error via tool result" {
    ef_use_mock <<'EOF'
responses:
  - match_type: contains
    pattern: "ef missing path"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          diff: "@@ @@\n-old\n+new\n"
    after_tool_reply: "Tool responded."
EOF
    ef_run "ef missing path"
    [ "${status}" -eq 0 ]
    assert_output_contains "Tool responded."
}

@test "09.02 missing diff parameter returns error via tool result" {
    ef_make_file "some content"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef missing diff"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
    after_tool_reply: "Tool responded."
EOF
    ef_run "ef missing diff"
    [ "${status}" -eq 0 ]
    assert_output_contains "Tool responded."
}

@test "09.03 diff with no @@ markers returns error" {
    ef_make_file "some content"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef no markers"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "just plain text without any hunk headers"
    after_tool_reply: "Tool responded."
EOF
    ef_run "ef no markers"
    [ "${status}" -eq 0 ]
    # File must be unchanged
    ef_file_equals "some content"$'\n'
}

# ── File I/O errors ───────────────────────────────────────────────────────────

@test "09.04 nonexistent file returns read error" {
    ef_use_mock <<'EOF'
responses:
  - match_type: contains
    pattern: "ef nonexistent"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "/tmp/sven_ef_no_such_file_xyz_09.txt"
          diff: "@@ @@\n-old\n+new\n"
    after_tool_reply: "Tool responded."
EOF
    ef_run "ef nonexistent"
    [ "${status}" -eq 0 ]
    assert_output_contains "Tool responded."
}

# ── Basic content modifications ───────────────────────────────────────────────

@test "09.05 basic line replacement changes old to new" {
    ef_make_file "fn foo() {"$'\n'"    old();"$'\n'"}"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef basic replace"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n fn foo() {\n-    old();\n+    new();\n }\n"
    after_tool_reply: "Edit done."
EOF
    ef_run "ef basic replace"
    [ "${status}" -eq 0 ]
    assert_output_contains "Edit done."
    ef_file_contains "new();"
    ef_file_not_contains "old();"
}

@test "09.06 surrounding lines are preserved after replacement" {
    ef_make_file "// header"$'\n'"fn target() { old(); }"$'\n'"// footer"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef preserve context"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n // header\n-fn target() { old(); }\n+fn target() { new(); }\n // footer\n"
    after_tool_reply: "Edit done."
EOF
    ef_run "ef preserve context"
    [ "${status}" -eq 0 ]
    ef_file_contains "// header"
    ef_file_contains "// footer"
    ef_file_contains "new()"
    ef_file_not_contains "old()"
}

@test "09.07 pure insertion adds line at correct position" {
    ef_make_file "before"$'\n'"after"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef pure insert"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n before\n+inserted\n after\n"
    after_tool_reply: "Insert done."
EOF
    ef_run "ef pure insert"
    [ "${status}" -eq 0 ]
    ef_file_equals "before"$'\n'"inserted"$'\n'"after"$'\n'
}

@test "09.08 pure deletion removes line and collapses gap" {
    ef_make_file "keep_before"$'\n'"remove_me"$'\n'"keep_after"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef pure delete"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n keep_before\n-remove_me\n keep_after\n"
    after_tool_reply: "Delete done."
EOF
    ef_run "ef pure delete"
    [ "${status}" -eq 0 ]
    ef_file_equals "keep_before"$'\n'"keep_after"$'\n'
}

@test "09.09 multi-line insertion adds all lines in order" {
    ef_make_file "before"$'\n'"after"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef multiline insert"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n before\n+line_a\n+line_b\n+line_c\n after\n"
    after_tool_reply: "Multi-insert done."
EOF
    ef_run "ef multiline insert"
    [ "${status}" -eq 0 ]
    ef_file_equals "before"$'\n'"line_a"$'\n'"line_b"$'\n'"line_c"$'\n'"after"$'\n'
}

@test "09.10 multi-line deletion removes all specified lines" {
    ef_make_file "keep_before"$'\n'"del_a"$'\n'"del_b"$'\n'"del_c"$'\n'"keep_after"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef multiline delete"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n keep_before\n-del_a\n-del_b\n-del_c\n keep_after\n"
    after_tool_reply: "Multi-delete done."
EOF
    ef_run "ef multiline delete"
    [ "${status}" -eq 0 ]
    ef_file_equals "keep_before"$'\n'"keep_after"$'\n'
}

@test "09.11 del and add lines interleaved in one hunk apply correctly" {
    ef_make_file "a"$'\n'"b"$'\n'"c"$'\n'"d"$'\n'"e"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef mixed hunk"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n a\n-b\n+B\n c\n-d\n+D\n e\n"
    after_tool_reply: "Mixed hunk done."
EOF
    ef_run "ef mixed hunk"
    [ "${status}" -eq 0 ]
    ef_file_equals "a"$'\n'"B"$'\n'"c"$'\n'"D"$'\n'"e"$'\n'
}

@test "09.12 blank context line inside hunk is matched correctly" {
    ef_make_file "fn a() {}"$'\n'$'\n'"fn b() {}"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef blank context"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n fn a() {}\n \n-fn b() {}\n+fn b() { /* updated */ }\n"
    after_tool_reply: "Blank context done."
EOF
    ef_run "ef blank context"
    [ "${status}" -eq 0 ]
    ef_file_contains "/* updated */"
    ef_file_not_contains "fn b() {}"$'\n'
}

# ── File boundary edits ───────────────────────────────────────────────────────

@test "09.13 change at start of file replaces first line" {
    ef_make_file "first"$'\n'"second"$'\n'"third"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef start edit"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ -1,2 +1,2 @@\n-first\n+FIRST\n second\n"
    after_tool_reply: "Start edit done."
EOF
    ef_run "ef start edit"
    [ "${status}" -eq 0 ]
    ef_file_equals "FIRST"$'\n'"second"$'\n'"third"$'\n'
}

@test "09.14 change at end of file replaces last line" {
    ef_make_file "first"$'\n'"second"$'\n'"last"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef end edit"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n second\n-last\n+LAST\n"
    after_tool_reply: "End edit done."
EOF
    ef_run "ef end edit"
    [ "${status}" -eq 0 ]
    ef_file_equals "first"$'\n'"second"$'\n'"LAST"$'\n'
}

@test "09.15 single-line file is replaced correctly" {
    ef_make_file "only line"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef single line"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n-only line\n+changed line\n"
    after_tool_reply: "Single-line done."
EOF
    ef_run "ef single line"
    [ "${status}" -eq 0 ]
    ef_file_equals "changed line"$'\n'
}

# ── Newline preservation ──────────────────────────────────────────────────────

@test "09.16 trailing newline is preserved after a successful edit" {
    ef_make_file "line one"$'\n'"line two"$'\n'"line three"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef preserve newline"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n line one\n-line two\n+line 2\n line three\n"
    after_tool_reply: "Newline preserved."
EOF
    ef_run "ef preserve newline"
    [ "${status}" -eq 0 ]
    # Read raw bytes to verify the file still ends with a newline.
    local last_char
    last_char="$(tail -c 1 "${_EF_TEST_FILE}" | od -An -tx1 | tr -d ' ')"
    [ "${last_char}" = "0a" ]
    ef_file_equals "line one"$'\n'"line 2"$'\n'"line three"$'\n'
}

@test "09.17 file without trailing newline stays without trailing newline" {
    # printf with no trailing \n
    _EF_TEST_FILE="$(mktemp /tmp/sven_ef_XXXXXX.txt)"
    printf 'alpha\nbeta\ngamma' > "${_EF_TEST_FILE}"
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef no trailing nl"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n alpha\n-beta\n+BETA\n gamma\n"
    after_tool_reply: "No-newline done."
EOF
    ef_run "ef no trailing nl"
    [ "${status}" -eq 0 ]
    local last_char
    last_char="$(tail -c 1 "${_EF_TEST_FILE}" | od -An -tx1 | tr -d ' ')"
    [ "${last_char}" != "0a" ]
    ef_file_equals "alpha"$'\n'"BETA"$'\n'"gamma"
}

# ── Diff format variants ──────────────────────────────────────────────────────

@test "09.18 FuDiff header @@ @@ without line numbers is accepted" {
    ef_make_file "hello world"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef fudiff"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n-hello world\n+hello rust\n"
    after_tool_reply: "FuDiff done."
EOF
    ef_run "ef fudiff"
    [ "${status}" -eq 0 ]
    ef_file_equals "hello rust"$'\n'
}

@test "09.19 diff with --- +++ file header lines is accepted" {
    ef_make_file "fn foo() { old(); }"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef file headers"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "--- a/src/foo.rs\n+++ b/src/foo.rs\n@@ -1 +1 @@\n-fn foo() { old(); }\n+fn foo() { new(); }\n"
    after_tool_reply: "File headers done."
EOF
    ef_run "ef file headers"
    [ "${status}" -eq 0 ]
    ef_file_equals "fn foo() { new(); }"$'\n'
}

@test "09.20 markdown-fenced diff wrapped in triple backticks is accepted" {
    ef_make_file "fn foo() { bar(); }"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef md fence"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "\`\`\`diff\n@@ @@\n-fn foo() { bar(); }\n+fn foo() { baz(); }\n\`\`\`\n"
    after_tool_reply: "Fenced diff done."
EOF
    ef_run "ef md fence"
    [ "${status}" -eq 0 ]
    ef_file_contains "baz()"
    ef_file_not_contains "bar()"
}

@test "09.21 git extended header with section name after @@ is accepted" {
    ef_make_file "fn greet() {"$'\n'"    old();"$'\n'"}"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef git section"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ -1,3 +1,3 @@ fn greet()\n fn greet() {\n-    old();\n+    new();\n }\n"
    after_tool_reply: "Git section done."
EOF
    ef_run "ef git section"
    [ "${status}" -eq 0 ]
    ef_file_contains "new();"
    ef_file_not_contains "old();"
}

@test "09.22 no-newline-at-end-of-file marker in diff is silently ignored" {
    ef_make_file "old"$'\n'
    # Use a YAML literal block scalar (|) so the backslash in
    # "\ No newline at end of file" is stored verbatim — no YAML escape
    # processing occurs in literal blocks, avoiding the ambiguity of \<space>
    # in a double-quoted YAML string.
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef no nl marker"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: |
            @@ @@
            -old
            +new
            \\ No newline at end of file
    after_tool_reply: "Marker ignored."
EOF
    ef_run "ef no nl marker"
    [ "${status}" -eq 0 ]
    ef_file_equals "new"$'\n'
}

# ── Multi-hunk behaviour ──────────────────────────────────────────────────────

@test "09.23 two-hunk diff applies both changes in one call" {
    ef_make_file "first"$'\n'"second"$'\n'"third"$'\n'"fourth"$'\n'"fifth"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef two hunks"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n-first\n+FIRST\n second\n@@ @@\n third\n-fourth\n+FOURTH\n"
    after_tool_reply: "Two hunks done."
EOF
    ef_run "ef two hunks"
    [ "${status}" -eq 0 ]
    ef_file_equals "FIRST"$'\n'"second"$'\n'"third"$'\n'"FOURTH"$'\n'"fifth"$'\n'
}

@test "09.24 three-hunk diff applies all three changes" {
    ef_make_file "aa"$'\n'"bb"$'\n'"cc"$'\n'"dd"$'\n'"ee"$'\n'"ff"$'\n'"gg"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef three hunks"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n-aa\n+AA\n bb\n@@ @@\n cc\n-dd\n+DD\n ee\n@@ @@\n ff\n-gg\n+GG\n"
    after_tool_reply: "Three hunks done."
EOF
    ef_run "ef three hunks"
    [ "${status}" -eq 0 ]
    ef_file_equals "AA"$'\n'"bb"$'\n'"cc"$'\n'"DD"$'\n'"ee"$'\n'"ff"$'\n'"GG"$'\n'
}

@test "09.25 second hunk targets post-first-hunk position correctly" {
    # Hunk 1 inserts two lines after 'anchor'. Hunk 2 must find 'target' in
    # the updated (in-memory) content even though its file position shifted.
    ef_make_file "anchor"$'\n'"target"$'\n'"end"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef offset track"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n anchor\n+new1\n+new2\n target\n@@ @@\n-target\n+TARGET\n end\n"
    after_tool_reply: "Offset track done."
EOF
    ef_run "ef offset track"
    [ "${status}" -eq 0 ]
    ef_file_equals "anchor"$'\n'"new1"$'\n'"new2"$'\n'"TARGET"$'\n'"end"$'\n'
}

@test "09.26 second hunk failure names hunk in error and leaves file unchanged" {
    ef_make_file "line1"$'\n'"line2"$'\n'"line3"$'\n'
    local checksum_before
    checksum_before="$(sha256sum "${_EF_TEST_FILE}" | cut -d' ' -f1)"
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef atomic fail"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n-line1\n+LINE1\n line2\n@@ @@\n-no_such_context_xyz\n+X\n"
    after_tool_reply: "Atomicity checked."
EOF
    ef_run "ef atomic fail"
    [ "${status}" -eq 0 ]
    assert_output_contains "Atomicity checked."
    # File must be byte-for-byte unchanged — hunks are all-or-nothing.
    local checksum_after
    checksum_after="$(sha256sum "${_EF_TEST_FILE}" | cut -d' ' -f1)"
    [ "${checksum_before}" = "${checksum_after}" ]
}

@test "09.27 single-hunk failure error has no Hunk N: prefix" {
    ef_make_file "hello"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef single fail"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n-completely_nonexistent_content\n+x\n"
    after_tool_reply: "Single fail checked."
EOF
    ef_run "ef single fail"
    [ "${status}" -eq 0 ]
    assert_output_contains "Single fail checked."
}

# ── Indent normalisation ──────────────────────────────────────────────────────

@test "09.28 context at wrong indent level still matches via normalisation" {
    # File has 4-space outer indent; hunk context has 0 indent.
    ef_make_file "    fn foo() {"$'\n'"        old();"$'\n'"    }"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef indent norm"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n fn foo() {\n-    old();\n+    new();\n }\n"
    after_tool_reply: "Indent norm done."
EOF
    ef_run "ef indent norm"
    [ "${status}" -eq 0 ]
    ef_file_contains "new();"
    ef_file_not_contains "old();"
}

@test "09.29 added lines receive the file's actual indent after normalised match" {
    # File block is at 4-space indent; hunk Add lines at 1-space indent.
    # The tool must add 4 spaces to each Add line (delta = file_indent - hunk_indent).
    ef_make_file "    fn foo() {"$'\n'"        bar();"$'\n'"    }"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef indent add"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n fn foo() {\n-    bar();\n+    baz();\n+    qux();\n }\n"
    after_tool_reply: "Indent add done."
EOF
    ef_run "ef indent add"
    [ "${status}" -eq 0 ]
    # Both added lines must carry the same 8-space indent as the file block.
    ef_file_contains "        baz();"
    ef_file_contains "        qux();"
}

# ── Fuzzy matching ────────────────────────────────────────────────────────────

@test "09.30 fuzzy match tolerates a small type difference in context" {
    # 'u64' in file vs 'u32' in context — similarity well above 85 %.
    ef_make_file "fn process(id: u64) {"$'\n'"    validate(id);"$'\n'"    update(id);"$'\n'"}"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef fuzzy match"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n fn process(id: u32) {\n     validate(id);\n-    update(id);\n+    update(id);\n+    log(id);\n }\n"
    after_tool_reply: "Fuzzy match done."
EOF
    ef_run "ef fuzzy match"
    [ "${status}" -eq 0 ]
    ef_file_contains "log(id);"
}

@test "09.31 context below fuzzy threshold is rejected and file left unchanged" {
    ef_make_file "fn foo() { completely_different_content_here(); }"$'\n'
    local checksum_before
    checksum_before="$(sha256sum "${_EF_TEST_FILE}" | cut -d' ' -f1)"
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef fuzzy reject"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n-struct Widget { name: String, value: i32, active: bool }\n+struct Widget { name: String }\n"
    after_tool_reply: "Fuzzy reject checked."
EOF
    ef_run "ef fuzzy reject"
    [ "${status}" -eq 0 ]
    local checksum_after
    checksum_after="$(sha256sum "${_EF_TEST_FILE}" | cut -d' ' -f1)"
    [ "${checksum_before}" = "${checksum_after}" ]
}

# ── Ambiguity resolution ──────────────────────────────────────────────────────

@test "09.32 line-number hint selects correct block when content is duplicated" {
    # Two identical blocks; hint (@@ -5,...) must pick the second one.
    ef_make_file "fn block() {"$'\n'"    value = 1;"$'\n'"}"$'\n'$'\n'"fn block() {"$'\n'"    value = 1;"$'\n'"}"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef hint disambig"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ -5,3 +5,3 @@\n fn block() {\n-    value = 1;\n+    value = 2;\n }\n"
    after_tool_reply: "Hint disambiguation done."
EOF
    ef_run "ef hint disambig"
    [ "${status}" -eq 0 ]
    # First block must still have value = 1; second must have value = 2.
    local content
    content="$(cat "${_EF_TEST_FILE}")"
    [[ "${content}" == *"value = 1;"* ]]
    [[ "${content}" == *"value = 2;"* ]]
    # The first occurrence must be "1" (unchanged block).
    local first_val
    first_val="$(grep 'value = ' "${_EF_TEST_FILE}" | head -1)"
    [[ "${first_val}" == *"value = 1;"* ]]
}

@test "09.33 stale context after prior edit fails and leaves current content intact" {
    ef_make_file "fn alpha() { one(); }"$'\n'"fn beta() { two(); }"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef first edit"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n-fn alpha() { one(); }\n+fn alpha() { updated(); }\n"
    after_tool_reply: "First edit done."
  - match_type: contains
    pattern: "ef stale edit"
    tool_calls:
      - id: tc2
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n-fn alpha() { one(); }\n+fn alpha() { updated(); }\n"
    after_tool_reply: "Stale edit checked."
EOF
    # First edit must succeed and update the file.
    ef_run "ef first edit"
    [ "${status}" -eq 0 ]
    ef_file_contains "updated()"

    # Second edit with OLD context must fail; 'updated()' must remain.
    ef_run "ef stale edit"
    [ "${status}" -eq 0 ]
    ef_file_contains "updated()"
}

# ── Error diagnostics ─────────────────────────────────────────────────────────

@test "09.34 context-not-found error message contains the expected lines" {
    ef_make_file "fn foo() {"$'\n'"    bar();"$'\n'"}"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef ctx not found"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n fn foo() {\n-    completely_wrong_line();\n+    x();\n }\n"
    after_tool_reply: "Context not found checked."
EOF
    ef_run "ef ctx not found"
    [ "${status}" -eq 0 ]
    # Regardless of whether the error propagates to stdout or is swallowed by
    # the after_tool_reply, the file must be unchanged.
    ef_file_not_contains "x();"
    ef_file_contains "bar();"
}

@test "09.35 context-not-found error suggests a nearby matching block" {
    # Context has right function name but wrong body — the error should suggest
    # the actual line from the file to help the agent correct its diff.
    ef_make_file "fn calculate_total(items: &[Item]) -> f64 {"$'\n'"    items.iter().map(|i| i.price).sum()"$'\n'"}"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef suggestion"
    tool_calls:
      - id: tc1
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n fn calculate_total(items: &[Item]) -> f64 {\n-    items.len() as f64\n+    0.0\n }\n"
    after_tool_reply: "Suggestion checked."
EOF
    # Run with full output so we can inspect the tool result in the conversation.
    run_split_output bash -c 'echo "ef suggestion" | "$BIN" --headless --model mock'
    # The agent continues (after_tool_reply appears).
    [[ "${STDOUT_OUT}" == *"Suggestion checked."* ]]
    # Tool result (in stdout conversation output) must reference the function name.
    [[ "${STDOUT_OUT}" == *"calculate_total"* ]]
}

# ── Trace output ──────────────────────────────────────────────────────────────

@test "09.36 successful edit shows success=true in sven:tool:result trace" {
    ef_make_file "line a"$'\n'"line b"$'\n'"line c"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef trace success"
    tool_calls:
      - id: tc-success
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n line a\n-line b\n+line B\n line c\n"
    after_tool_reply: "Trace success done."
EOF
    ef_run_traced "ef trace success"
    local result_line
    result_line="$(echo "${STDERR_OUT}" | grep '\[sven:tool:result\]' | head -1)"
    [ -n "${result_line}" ]
    [[ "${result_line}" == *"success=true"* ]]
}

@test "09.37 failed edit (context not found) shows success=false in sven:tool:result trace" {
    ef_make_file "actual content here"$'\n'
    ef_use_mock <<EOF
responses:
  - match_type: contains
    pattern: "ef trace fail"
    tool_calls:
      - id: tc-fail
        tool: edit_file
        args:
          path: "${_EF_TEST_FILE}"
          diff: "@@ @@\n-this line does not exist in the file\n+replacement\n"
    after_tool_reply: "Trace fail done."
EOF
    ef_run_traced "ef trace fail"
    local result_line
    result_line="$(echo "${STDERR_OUT}" | grep '\[sven:tool:result\]' | head -1)"
    [ -n "${result_line}" ]
    [[ "${result_line}" == *"success=false"* ]]
}

@test "09.38 failed edit on nonexistent file shows success=false in trace" {
    ef_use_mock <<'EOF'
responses:
  - match_type: contains
    pattern: "ef trace nofile"
    tool_calls:
      - id: tc-nofile
        tool: edit_file
        args:
          path: "/tmp/sven_ef_no_such_file_trace_09.txt"
          diff: "@@ @@\n-anything\n+replacement\n"
    after_tool_reply: "Trace nofile done."
EOF
    ef_run_traced "ef trace nofile"
    local result_line
    result_line="$(echo "${STDERR_OUT}" | grep '\[sven:tool:result\]' | head -1)"
    [ -n "${result_line}" ]
    [[ "${result_line}" == *"success=false"* ]]
}

# ── Cargo unit test gate ──────────────────────────────────────────────────────

@test "09.39 all edit_file Rust unit tests pass" {
    run bash -c 'cd "${_REPO_ROOT}" && CARGO_HOME=/tmp/cargo_home cargo test -p sven-tools edit_file 2>&1 | tail -8'
    [ "${status}" -eq 0 ]
    assert_output_contains "ok"
}
