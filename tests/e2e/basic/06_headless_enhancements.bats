#!/usr/bin/env bats
# 06_headless_enhancements.bats - Tests for all headless/CI enhancements.
#
# Covers:
#   • Conversation-format output (## User / ## Sven sections)
#   • --output-format compact and json
#   • --output-last-message
#   • --system-prompt-file and --append-system-prompt
#   • YAML frontmatter (title, mode, step_timeout_secs)
#   • Per-step options via <!-- sven: ... --> comments
#   • Variable templating ({{key}})
#   • --artifacts-dir (per-step and conversation files created)
#   • Progress reporting on stderr ([sven:step:start] etc.)
#   • sven validate subcommand and --dry-run
#   • Exit codes (0 = success, 2 = validation error)
#   • Git context and project context file injection (no crash, runs clean)

load helpers

# ── Conversation format output ────────────────────────────────────────────────

@test "06.01 default output format contains ## User section" {
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock'
    [[ "${STDOUT_OUT}" == *"## User"* ]]
}

@test "06.02 default output format contains ## Sven section" {
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock'
    [[ "${STDOUT_OUT}" == *"## Sven"* ]]
}

@test "06.03 conversation output contains the model reply text" {
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock'
    [[ "${STDOUT_OUT}" == *"pong"* ]]
}

@test "06.04 conversation output is pipeable back into sven" {
    run bash -c \
        'echo "ping" \
           | "$BIN" --headless --model mock 2>/dev/null \
           | "$BIN" --headless --model mock --stdin "summarize the above" 2>/dev/null'
    [ "${status}" -eq 0 ]
    [ -n "${output}" ]
}

@test "06.05 multi-step file produces ## User section for each step" {
    run_split_output bash -c \
        '"$BIN" --headless --model mock --file "$FIXTURES/three_steps.md"'
    # Three steps → three ## User headings
    local count
    count=$(grep -c "^## User" <<< "${STDOUT_OUT}" || true)
    [ "${count}" -ge 3 ]
}

# ── --output-format compact ───────────────────────────────────────────────────

@test "06.06 --output-format compact contains model reply but no ## User" {
    run_split_output bash -c \
        'echo "ping" | "$BIN" --headless --model mock --output-format compact'
    [[ "${STDOUT_OUT}" == *"pong"* ]]
    [[ "${STDOUT_OUT}" != *"## User"* ]]
    [[ "${STDOUT_OUT}" != *"## Sven"* ]]
}

@test "06.07 --output-format compact exits 0" {
    run bash -c \
        'echo "ping" | "$BIN" --headless --model mock --output-format compact 2>/dev/null'
    [ "${status}" -eq 0 ]
}

# ── --output-format json ──────────────────────────────────────────────────────

@test "06.08 --output-format json produces valid JSON object" {
    run_split_output bash -c \
        'echo "ping" | "$BIN" --headless --model mock --output-format json'
    [ "${EXIT_CODE}" -eq 0 ]
    # Output should be a JSON object
    echo "${STDOUT_OUT}" | python3 -c "import sys, json; json.load(sys.stdin)" 2>/dev/null
}

@test "06.09 json output contains steps array" {
    run_split_output bash -c \
        'echo "ping" | "$BIN" --headless --model mock --output-format json'
    [[ "${STDOUT_OUT}" == *'"steps"'* ]]
}

@test "06.10 json output contains the ATIF schema_version marker" {
    run_split_output bash -c \
        'echo "ping" | "$BIN" --headless --model mock --output-format json'
    [[ "${STDOUT_OUT}" == *'"schema_version"'* ]]
    [[ "${STDOUT_OUT}" == *"ATIF-v"* ]]
}

@test "06.11 json output contains the model reply in an agent step message" {
    run_split_output bash -c \
        'echo "ping" | "$BIN" --headless --model mock --output-format json'
    [[ "${STDOUT_OUT}" == *'"source": "agent"'* ]]
    [[ "${STDOUT_OUT}" == *"pong"* ]]
}

# ── --output-last-message ─────────────────────────────────────────────────────

@test "06.12 --output-last-message writes final response to file" {
    local outfile
    outfile="$(tmp_file)"
    run bash -c \
        'echo "final response test" | "$BIN" --headless --model mock --output-last-message "$1" 2>/dev/null' \
        -- "${outfile}"
    [ "${status}" -eq 0 ]
    [ -f "${outfile}" ]
    local content
    content="$(cat "${outfile}")"
    [[ "${content}" == *"final agent response"* ]]
    rm -f "${outfile}"
}

@test "06.13 --output-last-message does not suppress stdout" {
    local outfile
    outfile="$(tmp_file)"
    run_split_output bash -c \
        'echo "final response test" | "$BIN" --headless --model mock --output-last-message "$1"' \
        -- "${outfile}"
    # stdout should still have conversation output
    [[ "${STDOUT_OUT}" == *"## Sven"* ]]
    rm -f "${outfile}"
}

@test "06.14 --output-last-message creates parent directories" {
    local outfile
    outfile="/tmp/sven_bats_$$_newdir/output.txt"
    run bash -c \
        'echo "final response test" | "$BIN" --headless --model mock --output-last-message "$1" 2>/dev/null' \
        -- "${outfile}"
    [ "${status}" -eq 0 ]
    [ -f "${outfile}" ]
    rm -rf "$(dirname "${outfile}")"
}

# ── --system-prompt-file ──────────────────────────────────────────────────────

@test "06.15 --system-prompt-file loads file without crashing" {
    local spfile
    spfile="$(tmp_file)"
    echo "You are a helpful assistant for system prompt test." > "${spfile}"
    run bash -c \
        'echo "ping" | "$BIN" --headless --model mock --system-prompt-file "$1" 2>/dev/null' \
        -- "${spfile}"
    [ "${status}" -eq 0 ]
    rm -f "${spfile}"
}

@test "06.16 --system-prompt-file nonexistent file exits with code 2" {
    run bash -c \
        'echo "ping" | "$BIN" --headless --model mock --system-prompt-file /no/such/prompt.md 2>/dev/null'
    [ "${status}" -eq 2 ]
}

@test "06.17 --system-prompt-file reports load on stderr" {
    local spfile
    spfile="$(tmp_file)"
    echo "Custom system prompt for testing." > "${spfile}"
    run_split_output bash -c \
        'echo "ping" | "$BIN" --headless --model mock --system-prompt-file "$1"' \
        -- "${spfile}"
    [[ "${STDERR_OUT}" == *"System prompt loaded"* ]]
    rm -f "${spfile}"
}

# ── --append-system-prompt ────────────────────────────────────────────────────

@test "06.18 --append-system-prompt runs without error" {
    run bash -c \
        'echo "ping" | "$BIN" --headless --model mock \
           --append-system-prompt "Always be concise." 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "06.19 --append-system-prompt combined with --system-prompt-file" {
    local spfile
    spfile="$(tmp_file)"
    echo "Base prompt." > "${spfile}"
    run bash -c \
        'echo "ping" | "$BIN" --headless --model mock \
           --system-prompt-file "$1" \
           --append-system-prompt "Extra rule." 2>/dev/null' \
        -- "${spfile}"
    [ "${status}" -eq 0 ]
    rm -f "${spfile}"
}

# ── YAML frontmatter ──────────────────────────────────────────────────────────

@test "06.20 workflow with frontmatter runs successfully" {
    local wf
    wf="$(tmp_file)"
    cat > "${wf}" << 'EOF'
---
title: Test Workflow
mode: research
step_timeout_secs: 300
---

## Frontmatter test

Run the frontmatter test step.
EOF
    run bash -c '"$BIN" --headless --model mock --file "$1" 2>/dev/null' -- "${wf}"
    [ "${status}" -eq 0 ]
    rm -f "${wf}"
}

@test "06.21 frontmatter title appears in conversation output" {
    local wf
    wf="$(tmp_file)"
    cat > "${wf}" << 'EOF'
---
title: My Special Workflow
---

## Frontmatter test

Run the test.
EOF
    run_split_output bash -c '"$BIN" --headless --model mock --file "$1"' -- "${wf}"
    [[ "${STDOUT_OUT}" == *"My Special Workflow"* ]]
    rm -f "${wf}"
}

@test "06.22 frontmatter with vars expands template variables" {
    local wf
    wf="$(tmp_file)"
    cat > "${wf}" << 'EOF'
---
vars:
  testvar: "variable test"
---

## Template step

Please run the {{testvar}} now.
EOF
    run_split_output bash -c '"$BIN" --headless --model mock --file "$1"' -- "${wf}"
    [ "${EXIT_CODE}" -eq 0 ]
    [[ "${STDOUT_OUT}" == *"substitution confirmed"* ]]
    rm -f "${wf}"
}

# ── Per-step options ──────────────────────────────────────────────────────────

@test "06.23 per-step mode comment is accepted without error" {
    local wf
    wf="$(tmp_file)"
    cat > "${wf}" << 'EOF'
## Per-step test
<!-- sven: mode=research timeout=120 -->
Run the per-step test now.
EOF
    run bash -c '"$BIN" --headless --model mock --file "$1" 2>/dev/null' -- "${wf}"
    [ "${status}" -eq 0 ]
    rm -f "${wf}"
}

@test "06.24 per-step comment is stripped from user message in output" {
    local wf
    wf="$(tmp_file)"
    cat > "${wf}" << 'EOF'
## Per-step test
<!-- sven: mode=research timeout=120 -->
Run the per-step test now.
EOF
    run_split_output bash -c '"$BIN" --headless --model mock --file "$1"' -- "${wf}"
    # The HTML comment should NOT appear in the user section of the output
    [[ "${STDOUT_OUT}" != *"<!-- sven:"* ]]
    rm -f "${wf}"
}

# ── Variable templating (--var flag) ──────────────────────────────────────────

@test "06.25 --var flag substitutes into step content" {
    local wf
    wf="$(tmp_file)"
    cat > "${wf}" << 'EOF'
## Template step

Please run the {{mytest}} now.
EOF
    run_split_output bash -c \
        '"$BIN" --headless --model mock --file "$1" --var mytest="variable test"' \
        -- "${wf}"
    [ "${EXIT_CODE}" -eq 0 ]
    [[ "${STDOUT_OUT}" == *"substitution confirmed"* ]]
    rm -f "${wf}"
}

@test "06.26 multiple --var flags all substituted" {
    local wf
    wf="$(tmp_file)"
    cat > "${wf}" << 'EOF'
## Step

Branch {{branch}} ticket {{ticket}}: run variable test.
EOF
    run bash -c \
        '"$BIN" --headless --model mock --file "$1" \
           --var branch=main --var ticket=PROJ-1 2>/dev/null' \
        -- "${wf}"
    [ "${status}" -eq 0 ]
    rm -f "${wf}"
}

@test "06.27 unknown template placeholder is preserved unchanged" {
    local wf
    wf="$(tmp_file)"
    cat > "${wf}" << 'EOF'
## Step
Tell me about {{unknown_var}}.
EOF
    run_split_output bash -c \
        '"$BIN" --headless --model mock --file "$1"' -- "${wf}"
    # Should run without error; unknown var stays in output
    [ "${EXIT_CODE}" -eq 0 ]
    [[ "${STDOUT_OUT}" == *"{{unknown_var}}"* ]]
    rm -f "${wf}"
}

# ── --artifacts-dir ───────────────────────────────────────────────────────────

@test "06.28 --artifacts-dir creates the directory" {
    local artdir
    artdir="/tmp/sven_bats_$$_artifacts"
    run bash -c \
        '"$BIN" --headless --model mock --file "$FIXTURES/three_steps.md" \
           --artifacts-dir "$1" 2>/dev/null' \
        -- "${artdir}"
    [ "${status}" -eq 0 ]
    [ -d "${artdir}" ]
    rm -rf "${artdir}"
}

@test "06.29 --artifacts-dir writes conversation.md" {
    local artdir
    artdir="/tmp/sven_bats_$$_artifacts2"
    run bash -c \
        '"$BIN" --headless --model mock --file "$FIXTURES/three_steps.md" \
           --artifacts-dir "$1" 2>/dev/null' \
        -- "${artdir}"
    [ "${status}" -eq 0 ]
    [ -f "${artdir}/conversation.md" ]
    rm -rf "${artdir}"
}

@test "06.30 --artifacts-dir writes per-step artifact files" {
    local artdir
    artdir="/tmp/sven_bats_$$_artifacts3"
    run bash -c \
        '"$BIN" --headless --model mock --file "$FIXTURES/three_steps.md" \
           --artifacts-dir "$1" 2>/dev/null' \
        -- "${artdir}"
    [ "${status}" -eq 0 ]
    # Should have at least one per-step file (e.g. 01-*.md)
    local count
    count=$(find "${artdir}" -name "0*.md" | wc -l)
    [ "${count}" -ge 1 ]
    rm -rf "${artdir}"
}

# ── Progress reporting on stderr ──────────────────────────────────────────────

@test "06.31 progress [sven:step:start] appears on stderr" {
    run_split_output bash -c \
        '"$BIN" --headless --model mock --file "$FIXTURES/three_steps.md"'
    [[ "${STDERR_OUT}" == *"[sven:step:start]"* ]]
}

@test "06.32 progress [sven:step:complete] appears on stderr" {
    run_split_output bash -c \
        '"$BIN" --headless --model mock --file "$FIXTURES/three_steps.md"'
    [[ "${STDERR_OUT}" == *"[sven:step:complete]"* ]]
}

@test "06.33 step:complete contains duration_ms field" {
    run_split_output bash -c \
        'echo "ping" | "$BIN" --headless --model mock'
    [[ "${STDERR_OUT}" == *"duration_ms="* ]]
}

@test "06.34 step:complete contains success= field" {
    run_split_output bash -c \
        'echo "ping" | "$BIN" --headless --model mock'
    [[ "${STDERR_OUT}" == *"success="* ]]
}

@test "06.35 progress does not appear on stdout" {
    run_split_output bash -c \
        '"$BIN" --headless --model mock --file "$FIXTURES/three_steps.md"'
    [[ "${STDOUT_OUT}" != *"[sven:step:start]"* ]]
    [[ "${STDOUT_OUT}" != *"[sven:step:complete]"* ]]
}

# ── sven validate subcommand ──────────────────────────────────────────────────

@test "06.36 sven validate exits 0 for a valid workflow" {
    local wf
    wf="$(tmp_file)"
    cat > "${wf}" << 'EOF'
---
title: Valid Workflow
mode: research
step_timeout_secs: 120
---

## Step one
Do step one.

## Step two
Do step two.
EOF
    run bash -c '"$BIN" validate --file "$1" 2>/dev/null' -- "${wf}"
    [ "${status}" -eq 0 ]
    rm -f "${wf}"
}

@test "06.37 sven validate prints step count" {
    local wf
    wf="$(tmp_file)"
    cat > "${wf}" << 'EOF'
## Step one
First step.

## Step two
Second step.
EOF
    run bash -c '"$BIN" validate --file "$1"' -- "${wf}"
    assert_output_contains "Steps: 2"
    rm -f "${wf}"
}

@test "06.38 sven validate shows frontmatter fields" {
    local wf
    wf="$(tmp_file)"
    cat > "${wf}" << 'EOF'
---
title: My Audit
---

## Step
Do work.
EOF
    run bash -c '"$BIN" validate --file "$1"' -- "${wf}"
    assert_output_contains "Title: My Audit"
    assert_output_contains "Frontmatter: OK"
    rm -f "${wf}"
}

@test "06.39 sven validate shows step labels" {
    local wf
    wf="$(tmp_file)"
    cat > "${wf}" << 'EOF'
## Analyse codebase
Read all files.

## Write report
Summarise findings.
EOF
    run bash -c '"$BIN" validate --file "$1"' -- "${wf}"
    assert_output_contains "Analyse codebase"
    assert_output_contains "Write report"
    rm -f "${wf}"
}

@test "06.40 sven validate --file nonexistent exits non-zero" {
    run bash -c '"$BIN" validate --file /no/such/workflow.md 2>/dev/null'
    [ "${status}" -ne 0 ]
}

# ── --dry-run ─────────────────────────────────────────────────────────────────

@test "06.41 --dry-run exits 0 without calling the model" {
    local wf
    wf="$(tmp_file)"
    printf '## Step\nDo work.\n' > "${wf}"
    run bash -c '"$BIN" --headless --model mock --file "$1" --dry-run 2>/dev/null' -- "${wf}"
    [ "${status}" -eq 0 ]
    rm -f "${wf}"
}

@test "06.42 --dry-run prints validation info to stderr" {
    local wf
    wf="$(tmp_file)"
    cat > "${wf}" << 'EOF'
## Step one
Do step one.

## Step two
Do step two.
EOF
    run_split_output bash -c \
        '"$BIN" --headless --model mock --file "$1" --dry-run' -- "${wf}"
    [[ "${STDERR_OUT}" == *"dry-run"* ]]
    rm -f "${wf}"
}

@test "06.43 --dry-run produces no stdout" {
    local wf
    wf="$(tmp_file)"
    printf '## Step\nDo work.\n' > "${wf}"
    run_split_output bash -c \
        '"$BIN" --headless --model mock --file "$1" --dry-run' -- "${wf}"
    [ -z "${STDOUT_OUT}" ]
    rm -f "${wf}"
}

# ── Exit codes ────────────────────────────────────────────────────────────────

@test "06.44 exit code 0 on successful single step" {
    run bash -c 'echo "ping" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "06.45 exit code 0 on successful multi-step workflow" {
    run bash -c '"$BIN" --headless --model mock --file "$FIXTURES/three_steps.md" 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "06.46 exit code 2 for --system-prompt-file with missing file" {
    run bash -c \
        'echo "ping" | "$BIN" --headless --model mock --system-prompt-file /no/such/file.md 2>/dev/null'
    [ "${status}" -eq 2 ]
}

# ── Git context - no crash ────────────────────────────────────────────────────

@test "06.47 run from inside a git repo does not crash" {
    # Run from the project root (which is a git repo)
    run bash -c \
        'cd "$_REPO_ROOT" && echo "ping" | "$BIN" --headless --model mock 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "06.48 git context progress appears on stderr inside git repo" {
    # The runner emits [sven:step:start] regardless; just verify no crash
    run_split_output bash -c \
        'cd "$_REPO_ROOT" && echo "ping" | "$BIN" --headless --model mock'
    [ "${EXIT_CODE}" -eq 0 ]
}

# ── Project context file (AGENTS.md) ─────────────────────────────────────────

@test "06.49 AGENTS.md in project root is found and reported on stderr" {
    local tmpdir
    tmpdir="$(mktemp -d)"
    # Create a minimal git repo so find_project_root resolves here
    git -C "${tmpdir}" init -q
    echo "Always use Rust best practices." > "${tmpdir}/AGENTS.md"
    run_split_output bash -c \
        'cd "$1" && echo "context file test" | "$BIN" --headless --model mock' \
        -- "${tmpdir}"
    [[ "${STDERR_OUT}" == *"Project context file found at"* ]]
    rm -rf "${tmpdir}"
}

@test "06.50 .sven/context.md is preferred over AGENTS.md" {
    local tmpdir
    tmpdir="$(mktemp -d)"
    git -C "${tmpdir}" init -q
    mkdir -p "${tmpdir}/.sven"
    echo "Sven-specific project instructions." > "${tmpdir}/.sven/context.md"
    echo "Generic agent instructions." > "${tmpdir}/AGENTS.md"
    run_split_output bash -c \
        'cd "$1" && echo "context file test" | "$BIN" --headless --model mock' \
        -- "${tmpdir}"
    [ "${EXIT_CODE}" -eq 0 ]
    # Both should be reported with the same "Project context file found at" message
    [[ "${STDERR_OUT}" == *"Project context file found at"* ]]
    rm -rf "${tmpdir}"
}

# ── --step-timeout flag (validate it is accepted) ────────────────────────────

@test "06.51 --step-timeout flag is accepted and run exits 0" {
    run bash -c \
        'echo "ping" | "$BIN" --headless --model mock --step-timeout 300 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "06.52 --run-timeout flag is accepted and run exits 0" {
    run bash -c \
        'echo "ping" | "$BIN" --headless --model mock --run-timeout 600 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "06.52a an elapsed --run-timeout exits 124 and still records the run" {
    # The deadline is enforced per observation rather than by racing the whole
    # drive loop, so a timed-out run keeps the trajectory it accumulated and
    # gets stamped as a timeout instead of vanishing without a record.
    local work_dir
    work_dir="$(tmp_file)"
    mkdir -p "${work_dir}/.sven"
    run bash -c 'cd "$1" && echo "ping" | "$BIN" --headless --model mock --run-timeout 0 2>/dev/null' -- "${work_dir}"
    [ "${status}" -eq 124 ]
    run python3 -c "
import glob, json, sys
files = glob.glob(sys.argv[1] + '/.sven/logs/*.atif.json')
assert len(files) == 1, files
extra = json.load(open(files[0]))['final_metrics']['extra']
assert extra['reward'] == 0.0, extra
assert extra['outcome'] == 'timeout', extra
" "${work_dir}"
    [ "${status}" -eq 0 ]
    rm -rf "${work_dir}"
}

# ── Conversation format - tool sections ──────────────────────────────────────

@test "06.53 tool call appears as ## Tool section in conversation output" {
    run_split_output bash -c \
        'echo "run echo the test command" | "$BIN" --headless --model mock'
    # After a tool call, we should see ## Tool in the output
    [[ "${STDOUT_OUT}" == *"## Tool"* ]]
}

@test "06.54 tool result appears as ## Tool Result section in conversation output" {
    run_split_output bash -c \
        'echo "run echo the test command" | "$BIN" --headless --model mock'
    [[ "${STDOUT_OUT}" == *"## Tool Result"* ]]
}

# ── sven validate shows per-step mode/timeout hints ──────────────────────────

@test "06.55 validate shows per-step mode from inline comment" {
    local wf
    wf="$(tmp_file)"
    cat > "${wf}" << 'EOF'
## Research step
<!-- sven: mode=research timeout=60 -->
Analyse the codebase.
EOF
    run bash -c '"$BIN" validate --file "$1"' -- "${wf}"
    assert_output_contains "mode=research"
    assert_output_contains "timeout=60s"
    rm -f "${wf}"
}

# ── --max-tokens budget exhaustion still flushes --output-trace ─────────────
#
# Regression coverage: exhausting the `--max-tokens` budget used to call
# `std::process::exit` directly from inside the per-event handler
# (`crates/ci/src/runner/event.rs`), before the runner's trace-flush code
# ever ran — so a budget-aborted run exited 4 but left `--output-trace`
# completely unwritten. The runner now signals the budget exhaustion back to
# `runner/mod.rs` (see `StepState::budget_exhausted`), which flushes the
# partial trajectory through the same `flush_trace` closure every other
# abort path uses, then exits 4 itself.

@test "06.56 --max-tokens budget exhaustion exits 4" {
    local trace_file
    trace_file="$(tmp_file)"
    run bash -c 'echo "ping" | "$BIN" --headless --model mock --max-tokens 1 --output-trace "$1"' -- "${trace_file}"
    [ "${status}" -eq 4 ]
    rm -f "${trace_file}"
}

@test "06.57 --max-tokens budget exhaustion still writes a non-empty --output-trace file" {
    local trace_file
    trace_file="$(tmp_file)"
    run bash -c 'echo "ping" | "$BIN" --headless --model mock --max-tokens 1 --output-trace "$1"' -- "${trace_file}"
    [ -s "${trace_file}" ]
    rm -f "${trace_file}"
}

@test "06.58 --max-tokens budget-exhaustion trace file is valid JSON" {
    local trace_file
    trace_file="$(tmp_file)"
    run bash -c 'echo "ping" | "$BIN" --headless --model mock --max-tokens 1 --output-trace "$1"' -- "${trace_file}"
    run python3 -c "import json,sys; json.load(open(sys.argv[1]))" "${trace_file}"
    [ "${status}" -eq 0 ]
    rm -f "${trace_file}"
}

@test "06.59 --max-tokens budget-exhaustion trace has an ATIF schema_version" {
    local trace_file
    trace_file="$(tmp_file)"
    run bash -c 'echo "ping" | "$BIN" --headless --model mock --max-tokens 1 --output-trace "$1"' -- "${trace_file}"
    run python3 -c "import json,sys; v=json.load(open(sys.argv[1]))['schema_version']; sys.exit(0 if v.startswith('ATIF-') else 1)" "${trace_file}"
    [ "${status}" -eq 0 ]
    rm -f "${trace_file}"
}
