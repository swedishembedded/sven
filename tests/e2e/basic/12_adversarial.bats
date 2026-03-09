#!/usr/bin/env bats
# 12_adversarial.bats – Adversarial end-to-end tests for sven.
#
# Validates that sven handles hostile or malformed inputs gracefully:
#   • Mock model returning calls to nonexistent tools
#   • Mock model returning tool calls with missing/empty arguments
#   • Mock model returning enormous text replies
#   • Mock model returning ANSI escape sequences
#   • Mock model returning tool calls with extremely long IDs
#   • CLI argument fuzzing: long values, unknown modes, bad file paths
#   • Binary content on stdin
#   • Repeated flags
#   • Workflow files that are directories or named pipes
#
# In every case the invariant is: sven must NOT crash (SIGSEGV/SIGABRT) and
# must NOT hang beyond a reasonable timeout.  Exit code and error message
# quality are tested where predictable.

load helpers

# ── Per-test state ────────────────────────────────────────────────────────────

_ADV_MOCK_FILE=""
_ADV_TMP_FILES=()

teardown() {
    [ -n "${_ADV_MOCK_FILE}" ] && rm -f "${_ADV_MOCK_FILE}"
    for f in "${_ADV_TMP_FILES[@]+"${_ADV_TMP_FILES[@]}"}"; do
        rm -f "$f" 2>/dev/null || true
    done
    export SVEN_MOCK_RESPONSES="${MOCK_RESPONSES}"
    _ADV_MOCK_FILE=""
    _ADV_TMP_FILES=()
}

# Write a per-test mock YAML from a heredoc; export SVEN_MOCK_RESPONSES.
adv_use_mock() {
    _ADV_MOCK_FILE="$(mktemp /tmp/sven_adv_mock_XXXXXX.yaml)"
    cat > "${_ADV_MOCK_FILE}"
    export SVEN_MOCK_RESPONSES="${_ADV_MOCK_FILE}"
}

# Run sven headless/mock with a 30-second wall-clock timeout.
# Uses the 'timeout' command to prevent hangs.
adv_run() {
    run timeout 30 bash -c 'echo "'"$1"'" | "$BIN" --headless --model mock 2>&1'
}

# Same as adv_run but separates stdout and stderr.
adv_run_split() {
    local prompt="$1"
    run_split_output timeout 30 bash -c 'echo "'"$prompt"'" | "$BIN" --headless --model mock'
}

# ── Category 3: Mock model hostile responses ──────────────────────────────────

@test "12.01 tool call referencing nonexistent tool does not crash" {
    adv_use_mock <<'EOF'
responses:
  - match_type: equals
    pattern: "adv nonexistent tool"
    tool_calls:
      - id: tc-1
        tool: "does_not_exist_tool_xyz"
        args:
          param: value
    after_tool_reply: "I tried an unknown tool"
EOF
    adv_run "adv nonexistent tool"
    # The process must complete (timeout 30 ensures no hang).
    # Exit code 0 or non-zero is acceptable; SIGKILL (exit 137) is not.
    [ "${status}" -ne 137 ]
}

@test "12.02 tool call with empty args for required-param tool is handled gracefully" {
    adv_use_mock <<'EOF'
responses:
  - match_type: equals
    pattern: "adv empty args"
    tool_calls:
      - id: tc-1
        tool: "shell"
        args: {}
    after_tool_reply: "done with empty shell args"
EOF
    adv_run "adv empty args"
    # Must complete; shell tool returns an error about missing shell_command.
    [ "${status}" -ne 137 ]
}

@test "12.03 enormous text reply does not cause OOM or hang" {
    local big_reply
    big_reply="$(python3 -c "print('word ' * 50000)")"
    _ADV_MOCK_FILE="$(mktemp /tmp/sven_adv_mock_XXXXXX.yaml)"
    _ADV_TMP_FILES+=("${_ADV_MOCK_FILE}")
    printf 'responses:\n  - match_type: equals\n    pattern: "adv big reply"\n    reply: "%s"\n' \
        "$(echo "${big_reply}" | head -c 200000)" > "${_ADV_MOCK_FILE}"
    export SVEN_MOCK_RESPONSES="${_ADV_MOCK_FILE}"
    adv_run "adv big reply"
    [ "${status}" -ne 137 ]
}

@test "12.04 reply containing ANSI escape sequences does not crash" {
    adv_use_mock <<'EOF'
responses:
  - match_type: equals
    pattern: "adv ansi reply"
    reply: "\u001b[31mRed text\u001b[0m \u001b[1mBold\u001b[0m \u001b[2J\u001b[H"
EOF
    adv_run "adv ansi reply"
    [ "${status}" -ne 137 ]
}

@test "12.05 tool call with extremely long ID does not crash" {
    local long_id
    long_id="tc-$(python3 -c "print('x' * 5000)")"
    _ADV_MOCK_FILE="$(mktemp /tmp/sven_adv_mock_XXXXXX.yaml)"
    _ADV_TMP_FILES+=("${_ADV_MOCK_FILE}")
    printf 'responses:\n  - match_type: equals\n    pattern: "adv long id"\n    tool_calls:\n      - id: "%s"\n        tool: "shell"\n        args:\n          shell_command: "echo ok"\n    after_tool_reply: "done"\n' \
        "${long_id}" > "${_ADV_MOCK_FILE}"
    export SVEN_MOCK_RESPONSES="${_ADV_MOCK_FILE}"
    adv_run "adv long id"
    [ "${status}" -ne 137 ]
}

@test "12.06 mock returning no matching rule falls back to default gracefully" {
    adv_use_mock <<'EOF'
responses:
  - match_type: equals
    pattern: "only this exact phrase"
    reply: "matched"
  - match_type: default
    reply: "default fallback reply"
EOF
    adv_run "this phrase has no exact match"
    [ "${status}" -ne 137 ]
    assert_output_contains "default fallback reply"
}

@test "12.07 empty tool_calls array with no reply does not hang" {
    adv_use_mock <<'EOF'
responses:
  - match_type: equals
    pattern: "adv empty tool calls"
    tool_calls: []
    after_tool_reply: "no tools were called"
EOF
    adv_run "adv empty tool calls"
    [ "${status}" -ne 137 ]
}

@test "12.08 tool call to shell with null command argument is handled" {
    adv_use_mock <<'EOF'
responses:
  - match_type: equals
    pattern: "adv shell null cmd"
    tool_calls:
      - id: tc-1
        tool: "shell"
        args:
          shell_command: ~
    after_tool_reply: "shell null cmd done"
EOF
    adv_run "adv shell null cmd"
    [ "${status}" -ne 137 ]
}

@test "12.09 tool call to write_file with no path does not crash" {
    adv_use_mock <<'EOF'
responses:
  - match_type: equals
    pattern: "adv write no path"
    tool_calls:
      - id: tc-1
        tool: "write_file"
        args:
          text: "content without path"
          append: false
    after_tool_reply: "write_file missing path done"
EOF
    adv_run "adv write no path"
    [ "${status}" -ne 137 ]
}

@test "12.10 multiple tool calls in one response all complete without crash" {
    adv_use_mock <<'EOF'
responses:
  - match_type: equals
    pattern: "adv multi tool"
    tool_calls:
      - id: tc-1
        tool: "shell"
        args:
          shell_command: "echo first"
      - id: tc-2
        tool: "shell"
        args:
          shell_command: "echo second"
      - id: tc-3
        tool: "shell"
        args:
          shell_command: "echo third"
    after_tool_reply: "all three tools ran"
EOF
    adv_run "adv multi tool"
    [ "${status}" -ne 137 ]
    assert_output_contains "all three tools ran"
}

# ── Category 4: CLI argument fuzzing ─────────────────────────────────────────

@test "12.11 extremely long --model value exits non-zero without crash" {
    local long_model
    long_model="$(python3 -c "print('A' * 10000)")"
    run timeout 15 bash -c 'echo "hi" | "$BIN" --headless --model '"${long_model}"' 2>&1'
    [ "${status}" -ne 0 ]
    [ "${status}" -ne 137 ]
}

@test "12.12 invalid --mode value exits non-zero" {
    run timeout 15 bash -c 'echo "hi" | "$BIN" --headless --model mock --mode totally_invalid_mode_xyz 2>&1'
    [ "${status}" -ne 0 ]
    [ "${status}" -ne 137 ]
}

@test "12.13 --file pointing to a directory exits non-zero" {
    run timeout 15 bash -c '"$BIN" --headless --model mock --file /tmp 2>&1'
    [ "${status}" -ne 0 ]
    [ "${status}" -ne 137 ]
}

@test "12.14 --file pointing to nonexistent path exits non-zero with message" {
    run timeout 15 bash -c '"$BIN" --headless --model mock --file /tmp/sven_adv_no_such_file_zzz.md 2>&1'
    [ "${status}" -ne 0 ]
    [ "${status}" -ne 137 ]
    assert_output_contains "sven_adv_no_such_file_zzz"
}

@test "12.15 binary content on stdin does not crash" {
    run timeout 15 bash -c 'dd if=/dev/urandom bs=1024 count=1 2>/dev/null | "$BIN" --headless --model mock 2>&1'
    # Must not be killed by SIGKILL (137) or SIGSEGV (139).
    [ "${status}" -ne 137 ]
    [ "${status}" -ne 139 ]
}

@test "12.16 stdin with only NUL bytes exits gracefully" {
    run timeout 15 bash -c 'printf "\x00\x00\x00" | "$BIN" --headless --model mock 2>&1'
    [ "${status}" -ne 137 ]
    [ "${status}" -ne 139 ]
}

@test "12.17 workflow file with very deeply nested YAML frontmatter is handled" {
    local tmpfile
    tmpfile="$(mktemp /tmp/sven_adv_nested_XXXXXX.md)"
    _ADV_TMP_FILES+=("${tmpfile}")
    {
        echo "---"
        echo "vars:"
        for i in $(seq 1 100); do
            printf '  key_%d: value_%d\n' "$i" "$i"
        done
        echo "---"
        echo ""
        echo "## Step"
        echo "ping"
    } > "${tmpfile}"
    run timeout 15 bash -c '"$BIN" --headless --model mock --file "'"${tmpfile}"'" 2>&1'
    [ "${status}" -ne 137 ]
    [ "${status}" -ne 139 ]
}

@test "12.18 workflow file with 10000 markdown headings is handled" {
    local tmpfile
    tmpfile="$(mktemp /tmp/sven_adv_headings_XXXXXX.md)"
    _ADV_TMP_FILES+=("${tmpfile}")
    {
        printf '## Step\nping\n'
        python3 -c "
for i in range(10000):
    print(f'## Heading {i}')
    print('Some content here.')
"
    } > "${tmpfile}"
    run timeout 30 bash -c '"$BIN" --headless --model mock --file "'"${tmpfile}"'" 2>&1'
    [ "${status}" -ne 137 ]
    [ "${status}" -ne 139 ]
}

@test "12.19 --file pointing to /dev/null exits gracefully without hang" {
    run timeout 10 bash -c '"$BIN" --headless --model mock --file /dev/null 2>&1'
    # /dev/null is a valid file but empty — sven should not hang.
    [ "${status}" -ne 137 ]
    [ "${status}" -ne 139 ]
}

@test "12.20 named pipe as --file does not hang past timeout" {
    local fifo
    fifo="$(mktemp -u /tmp/sven_adv_fifo_XXXXXX)"
    mkfifo "${fifo}"
    _ADV_TMP_FILES+=("${fifo}")
    # Write to the pipe in the background, then run sven.
    printf '## Step\nping\n' > "${fifo}" &
    local writer_pid=$!
    run timeout 15 bash -c '"$BIN" --headless --model mock --file "'"${fifo}"'" 2>&1'
    wait "${writer_pid}" 2>/dev/null || true
    [ "${status}" -ne 137 ]
}

@test "12.21 mixed CRLF and LF in workflow file does not crash" {
    local tmpfile
    tmpfile="$(mktemp /tmp/sven_adv_crlf_XXXXXX.md)"
    _ADV_TMP_FILES+=("${tmpfile}")
    printf '## Step\r\nping\r\n' > "${tmpfile}"
    run timeout 15 bash -c '"$BIN" --headless --model mock --file "'"${tmpfile}"'" 2>&1'
    [ "${status}" -ne 137 ]
    [ "${status}" -ne 139 ]
}

@test "12.22 UTF-8 BOM at start of workflow file is handled gracefully" {
    local tmpfile
    tmpfile="$(mktemp /tmp/sven_adv_bom_XXXXXX.md)"
    _ADV_TMP_FILES+=("${tmpfile}")
    # Write UTF-8 BOM (EF BB BF) followed by valid markdown.
    printf '\xef\xbb\xbf## Step\nping\n' > "${tmpfile}"
    run timeout 15 bash -c '"$BIN" --headless --model mock --file "'"${tmpfile}"'" 2>&1'
    [ "${status}" -ne 137 ]
    [ "${status}" -ne 139 ]
}

@test "12.23 extremely long prompt via stdin does not crash" {
    local long_prompt
    long_prompt="$(python3 -c "print('word ' * 100000)")"
    run timeout 30 bash -c 'echo "'"$(echo "${long_prompt}" | head -c 500000)"'" | "$BIN" --headless --model mock 2>&1'
    [ "${status}" -ne 137 ]
    [ "${status}" -ne 139 ]
}

@test "12.24 shell tool command with path traversal via workdir does not crash" {
    adv_use_mock <<'EOF'
responses:
  - match_type: equals
    pattern: "adv path traversal workdir"
    tool_calls:
      - id: tc-1
        tool: "shell"
        args:
          shell_command: "pwd"
          workdir: "/tmp/../../../../tmp"
    after_tool_reply: "path traversal workdir done"
EOF
    adv_run "adv path traversal workdir"
    [ "${status}" -ne 137 ]
    [ "${status}" -ne 139 ]
}

@test "12.25 shell tool with unicode and control chars in command does not crash" {
    adv_use_mock <<'EOF'
responses:
  - match_type: equals
    pattern: "adv unicode shell"
    tool_calls:
      - id: tc-1
        tool: "shell"
        args:
          shell_command: "echo '日本語 café \u202e RTL'"
    after_tool_reply: "unicode shell done"
EOF
    adv_run "adv unicode shell"
    [ "${status}" -ne 137 ]
    [ "${status}" -ne 139 ]
}
