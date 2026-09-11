#!/usr/bin/env bats
# 08_trace_output.bats - CI trace output and multi-tier pipe chain tests.
#
# Covers:
#   • [sven:tool:call] includes id= field
#   • [sven:tool:result] replaces [sven:tool:ok] and includes id=, size=
#   • [sven:tokens] always emitted (input/output tokens)
#   • [sven:thinking] emitted at default verbosity (full content)
#   • [sven:thinking] emits full content at all verbosity levels
#   • Thinking does NOT appear in stdout (only stderr)
#   • Multi-tier pipe chains: history seeded correctly
#   • Conversation format detection: workflow headings not re-processed
#   • Pipe chain: each stage adds a new sven turn

load helpers

# ── Token usage ───────────────────────────────────────────────────────────────

@test "08.01 [sven:tokens] appears in stderr by default" {
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock'
    [[ "${STDERR_OUT}" == *"[sven:tokens]"* ]]
}

@test "08.02 [sven:tokens] reports input and output" {
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock'
    [[ "${STDERR_OUT}" == *"input="* ]]
    [[ "${STDERR_OUT}" == *"output="* ]]
}

@test "08.03 [sven:tokens] does not appear in stdout" {
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock'
    [[ "${STDOUT_OUT}" != *"[sven:tokens]"* ]]
}

# ── Tool call tracing ─────────────────────────────────────────────────────────

@test "08.04 [sven:tool:call] includes id= field" {
    run_split_output bash -c 'echo "write a file for me" | "$BIN" --headless --model mock'
    [[ "${STDERR_OUT}" == *'[sven:tool:call] id='* ]]
}

@test "08.05 [sven:tool:call] includes name= field" {
    run_split_output bash -c 'echo "write a file for me" | "$BIN" --headless --model mock'
    [[ "${STDERR_OUT}" == *'name="write_file"'* ]]
}

@test "08.06 [sven:tool:result] is emitted (not [sven:tool:ok])" {
    run_split_output bash -c 'echo "write a file for me" | "$BIN" --headless --model mock'
    [[ "${STDERR_OUT}" == *"[sven:tool:result]"* ]]
    [[ "${STDERR_OUT}" != *"[sven:tool:ok]"* ]]
}

@test "08.07 [sven:tool:result] includes id= matching the call" {
    run_split_output bash -c 'echo "write a file for me" | "$BIN" --headless --model mock'
    [[ "${STDERR_OUT}" == *'[sven:tool:result] id="tc-write"'* ]]
}

@test "08.08 [sven:tool:result] includes success= field" {
    run_split_output bash -c 'echo "write a file for me" | "$BIN" --headless --model mock'
    [[ "${STDERR_OUT}" == *"success="* ]]
}

@test "08.09 [sven:tool:result] output not in stdout" {
    run_split_output bash -c 'echo "write a file for me" | "$BIN" --headless --model mock'
    [[ "${STDOUT_OUT}" != *"[sven:tool:result]"* ]]
}

# ── Thinking trace ────────────────────────────────────────────────────────────

@test "08.10 [sven:thinking] appears in stderr at default verbosity" {
    run_split_output bash -c 'echo "think deeply about this" | "$BIN" --headless --model mock'
    [[ "${STDERR_OUT}" == *"[sven:thinking]"* ]]
}

@test "08.11 [sven:thinking] at default level shows full reasoning content" {
    run_split_output bash -c 'echo "think deeply about this" | "$BIN" --headless --model mock'
    # Full content is shown by default - thinking is valuable CI signal
    [[ "${STDERR_OUT}" == *"Let me carefully reason"* ]]
}

@test "08.12 [sven:thinking] at -v also shows full reasoning content" {
    run_split_output bash -c 'echo "think deeply about this" | "$BIN" --headless --model mock -v'
    [[ "${STDERR_OUT}" == *"Let me carefully reason"* ]]
}

@test "08.13 thinking content does NOT appear in stdout" {
    run_split_output bash -c 'echo "think deeply about this" | "$BIN" --headless --model mock'
    [[ "${STDOUT_OUT}" != *"[sven:thinking]"* ]]
    [[ "${STDOUT_OUT}" != *"Let me carefully reason"* ]]
}

@test "08.14 model reply appears in stdout even when thinking emitted" {
    run_split_output bash -c 'echo "think deeply about this" | "$BIN" --headless --model mock'
    [[ "${STDOUT_OUT}" == *"forty-two"* ]]
}

# ── Multi-tier pipe chain ─────────────────────────────────────────────────────
#
# Key invariant: when a sven stdout (conversation markdown) is piped into the
# next sven instance, the prior conversation is parsed as history - not as a
# new workflow.  The positional prompt becomes the new user turn.

@test "08.15 two-stage pipe succeeds" {
    run bash -c \
        'echo "make a plan" \
           | "$BIN" --headless --model mock 2>/dev/null \
           | "$BIN" --headless --model mock "summarize the above" 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "08.16 two-stage pipe produces non-empty output" {
    run bash -c \
        'echo "make a plan" \
           | "$BIN" --headless --model mock 2>/dev/null \
           | "$BIN" --headless --model mock "summarize the above" 2>/dev/null'
    [ -n "${output}" ]
}

@test "08.17 second stage output contains model reply text" {
    run bash -c \
        'echo "make a plan" \
           | "$BIN" --headless --model mock 2>/dev/null \
           | "$BIN" --headless --model mock "summarize the above" 2>/dev/null'
    assert_output_contains "Summary"
}

@test "08.18 three-stage pipe succeeds" {
    run bash -c \
        'echo "make a plan" \
           | "$BIN" --headless --model mock 2>/dev/null \
           | "$BIN" --headless --model mock "implement the plan above" 2>/dev/null \
           | "$BIN" --headless --model mock "summarize the above" 2>/dev/null'
    [ "${status}" -eq 0 ]
    assert_output_contains "Summary"
}

@test "08.19 piped conversation format detected - [sven:info] appears on stderr" {
    # The runner should log that it loaded prior messages from the pipe.
    run_split_output bash -c \
        'echo "make a plan" \
           | "$BIN" --headless --model mock 2>/dev/null \
           | "$BIN" --headless --model mock "summarize the above"'
    [[ "${STDERR_OUT}" == *"[sven:info] Loaded"* ]]
}

@test "08.20 piped conversation does not re-emit ## Sven as a step label" {
    # If the ## Sven heading were treated as a workflow step, the label would
    # appear in the step:start log line.
    run_split_output bash -c \
        'echo "make a plan" \
           | "$BIN" --headless --model mock 2>/dev/null \
           | "$BIN" --headless --model mock "summarize the above"'
    [[ "${STDERR_OUT}" != *'label="Sven"'* ]]
}

@test "08.21 stdout of pipe chain still contains conversation headings" {
    run_split_output bash -c \
        'echo "make a plan" \
           | "$BIN" --headless --model mock 2>/dev/null \
           | "$BIN" --headless --model mock "summarize the above" 2>/dev/null'
    [[ "${STDOUT_OUT}" == *"## User"* ]]
    [[ "${STDOUT_OUT}" == *"## Sven"* ]]
}

@test "08.22 four-stage pipe chain succeeds" {
    run bash -c \
        'echo "ping" \
           | "$BIN" --headless --model mock 2>/dev/null \
           | "$BIN" --headless --model mock "plan something" 2>/dev/null \
           | "$BIN" --headless --model mock "implement the plan above" 2>/dev/null \
           | "$BIN" --headless --model mock "summarize the above" 2>/dev/null'
    [ "${status}" -eq 0 ]
}

@test "08.23 pipe chain with set -e exits 0" {
    run bash -euc \
        'echo "make a plan" \
           | "$BIN" --headless --model mock 2>/dev/null \
           | "$BIN" --headless --model mock "summarize the above" 2>/dev/null \
           > /dev/null'
    [ "${status}" -eq 0 ]
}

# ── Tool call ↔ result correlation in CI trace ────────────────────────────────

@test "08.24 tool call id matches tool result id in trace" {
    run_split_output bash -c 'echo "write a file for me" | "$BIN" --headless --model mock'
    # Both the call and result lines should have the same id value.
    local call_id result_id
    call_id=$(echo "${STDERR_OUT}" | grep '\[sven:tool:call\]' | grep -o 'id="[^"]*"' | head -1)
    result_id=$(echo "${STDERR_OUT}" | grep '\[sven:tool:result\]' | grep -o 'id="[^"]*"' | head -1)
    [ -n "${call_id}" ]
    [ "${call_id}" = "${result_id}" ]
}

@test "08.25 [sven:tool:result] reports success=false for error results" {
    # read_file on a non-existent path produces a guaranteed tool error.
    run_split_output bash -c 'echo "read nonexistent file" | "$BIN" --headless --model mock'
    local tool_result_line
    tool_result_line=$(echo "${STDERR_OUT}" | grep '\[sven:tool:result\]' | head -1)
    [[ "${tool_result_line}" == *"success=false"* ]]
    [[ "${tool_result_line}" != *"success=true"* ]]
}

# ── Verbose tool result output ────────────────────────────────────────────────

@test "08.26 at default verbosity [sven:tool:result] has no output= snippet" {
    run_split_output bash -c 'echo "run echo test" | "$BIN" --headless --model mock'
    # At trace_level 0, the tool:result line must NOT include output=.
    # (We grep for the specific tool:result line to avoid matching [sven:tokens]
    # which also contains "output=".)
    local tool_result_line
    tool_result_line=$(echo "${STDERR_OUT}" | grep '\[sven:tool:result\]' | head -1)
    [[ "${tool_result_line}" == *"success=true"* ]]   # it succeeded
    [[ "${tool_result_line}" != *"output="* ]]         # no output snippet at level 0
}

@test "08.27 at -v [sven:tool:result] includes output= snippet" {
    # run echo test triggers a shell command; the tool result should include output=
    run_split_output bash -c 'echo "run echo test" | "$BIN" --headless --model mock -v'
    local tool_result_line
    tool_result_line=$(echo "${STDERR_OUT}" | grep '\[sven:tool:result\]' | head -1)
    [[ "${tool_result_line}" == *"output="* ]]
}

# ── task-tool subagent trajectories embed into the parent --output-trace ────
#
# The `task` tool spawns a real `sven acp serve` child process over ACP (see
# `crates/bootstrap/src/task_tool.rs`); its conversation used to be entirely
# unrecorded — `AgentEvent::SubagentStarted`/`SubagentEvent` were silently
# dropped by the CI runner's event handler (`crates/ci/src/runner/event.rs`),
# and even before reaching there, `ToolEvent::SubagentStarted`/`SubagentEvent`
# never reached the `AgentEvent` stream at all (the receiving end of that
# channel was unconditionally dropped in `crates/bootstrap/src/
# runtime_builder.rs` — "all modes now use TurnExecutor"). Both gaps are now
# fixed, so the parent document's `subagent_trajectories` should contain the
# child's own turns, referenced from a resolvable `trajectory_id`.
#
# The mock fixture (`tests/fixtures/mock_responses.yaml`, "delegate a
# subtask" rule) delegates with prompt "ping"; the spawned child inherits
# SVEN_MOCK_RESPONSES and replies "pong" per this same file's "ping" rule.

@test "08.28 task tool delegation produces a non-empty subagent_trajectories" {
    local trace_file
    trace_file="$(tmp_file)"
    run bash -c 'echo "delegate a subtask to a subagent" | "$BIN" --headless --model mock --output-trace "$1"' -- "${trace_file}"
    [ "${status}" -eq 0 ]
    run python3 -c "import json,sys; d=json.load(open(sys.argv[1])); sys.exit(0 if d.get('subagent_trajectories') else 1)" "${trace_file}"
    [ "${status}" -eq 0 ]
    rm -f "${trace_file}"
}

@test "08.29 embedded subagent trajectory contains the child's own turns" {
    local trace_file
    trace_file="$(tmp_file)"
    run bash -c 'echo "delegate a subtask to a subagent" | "$BIN" --headless --model mock --output-trace "$1"' -- "${trace_file}"
    run python3 -c "
import json, sys
d = json.load(open(sys.argv[1]))
child = d['subagent_trajectories'][0]
steps = child['steps']
assert any(s.get('message') == 'ping' for s in steps), steps
assert any(s.get('message') == 'pong' for s in steps), steps
" "${trace_file}"
    [ "${status}" -eq 0 ]
    rm -f "${trace_file}"
}

@test "08.30 parent step's subagent_trajectory_ref resolves to the embedded child's trajectory_id" {
    local trace_file
    trace_file="$(tmp_file)"
    run bash -c 'echo "delegate a subtask to a subagent" | "$BIN" --headless --model mock --output-trace "$1"' -- "${trace_file}"
    run python3 -c "
import json, sys
d = json.load(open(sys.argv[1]))
child_id = d['subagent_trajectories'][0]['trajectory_id']
assert child_id, 'child trajectory_id must be set'
refs = [
    r
    for step in d['steps']
    for r in (step.get('observation') or {}).get('results', [])
    if r.get('subagent_trajectory_ref')
]
assert refs, 'no subagent_trajectory_ref observation found in parent steps'
resolved = [ref['trajectory_id'] for r in refs for ref in r['subagent_trajectory_ref']]
assert child_id in resolved, (child_id, resolved)
" "${trace_file}"
    [ "${status}" -eq 0 ]
    rm -f "${trace_file}"
}

@test "08.31 [sven:subagent:started] and [sven:subagent:embedded] appear on stderr with --output-trace" {
    # --output-trace is one of the flags that routes to CiRunner (see
    # main.rs's runner-routing comment) - the [sven:subagent:embedded] token
    # only fires there, once the child trajectory is actually folded into
    # the parent document.
    local trace_file
    trace_file="$(tmp_file)"
    run_split_output bash -c 'echo "delegate a subtask to a subagent" | "$BIN" --headless --model mock --output-trace "$1"' -- "${trace_file}"
    [[ "${STDERR_OUT}" == *"[sven:subagent:started]"* ]]
    [[ "${STDERR_OUT}" == *"[sven:subagent:embedded]"* ]]
    rm -f "${trace_file}"
}

@test "08.32 [sven:subagent:started] still appears on stderr without --output-trace (RuntimeRunner path)" {
    # A plain `sven --headless` invocation with no workflow flags routes to
    # the *other* headless runner, `RuntimeRunner` (see main.rs's
    # runner-routing comment) - a separate implementation from `CiRunner`'s
    # event.rs that had the identical silently-dropped-subagent-events bug.
    # It has no ATIF trace document to embed into (RuntimeRunner never
    # writes --output-trace), so there is no [sven:subagent:embedded] token
    # here, but the lifecycle should still be visible on stderr rather than
    # silently dropped.
    run_split_output bash -c 'echo "delegate a subtask to a subagent" | "$BIN" --headless --model mock'
    [[ "${STDERR_OUT}" == *"[sven:subagent:started]"* ]]
    [[ "${STDERR_OUT}" == *"[sven:subagent:finished]"* ]]
}

# ── Reward stamping ───────────────────────────────────────────────────────────
# A concluded trajectory carries `final_metrics.extra.reward`, the per-session
# training weight an external trajectory consumer reads. Absent means "outcome
# unknown" and the trajectory is skipped, so these tests pin PRESENCE (and,
# just as importantly, ABSENCE) as much as value.
#
# A run the surface itself observed to fail (agent error, timeout, budget
# exhausted) is scored confidently - see 08.37 and 06.* below. A *claimed*
# success is different: with no verifier to back it up (none exists yet),
# `sven` cannot honestly tell a real success from an agent that just said
# "Done!" - so it stamps no reward at all, only a diagnostic reason. This is
# the actual point of the rework, not a regression: a claimed success used to
# score a confident 1.0 with nothing behind it.

@test "08.33 an unverified success stamps no reward, only the unknown reason" {
    local trace_file
    trace_file="$(tmp_file)"
    run bash -c 'echo "ping" | "$BIN" --headless --model mock --output-trace "$1"' -- "${trace_file}"
    run python3 -c "
import json, sys
d = json.load(open(sys.argv[1]))
extra = d['final_metrics']['extra']
assert 'reward' not in extra, extra
assert extra['sven_outcome_unknown_reason'] == 'no_verifier', extra
" "${trace_file}"
    [ "${status}" -eq 0 ]
    rm -f "${trace_file}"
}

@test "08.34 tool errors do not change an unverified success's unscored outcome" {
    # A claimed success is unscored regardless of how it went - the missing
    # verifier, not the tool-error count, is what makes it unknown.
    # Exit 3 is EXIT_TOOL_WARNINGS: a warning, not an agent failure.
    local trace_file
    trace_file="$(tmp_file)"
    run bash -c 'echo "read nonexistent file" | "$BIN" --headless --model mock --output-trace "$1"' -- "${trace_file}"
    [ "${status}" -eq 3 ]
    run python3 -c "
import json, sys
extra = json.load(open(sys.argv[1]))['final_metrics']['extra']
assert 'reward' not in extra, extra
assert extra['sven_outcome_unknown_reason'] == 'no_verifier', extra
" "${trace_file}"
    [ "${status}" -eq 0 ]
    rm -f "${trace_file}"
}

@test "08.35 the reward reads back through the exact consumer lookup path" {
    # Mirrors the consumer's reader byte-for-byte: four plain key lookups and a
    # float coercion, with no schema types involved. A renamed key or a
    # non-finite number (which serialises as null) fails here. Needs a
    # scenario that actually produces a Scored outcome - an observed failure,
    # since a claimed success alone (see 08.33) is never scored without a
    # verifier. `--max-tokens 1` reuses 08.37's reliable budget-exhausted path.
    local trace_file
    trace_file="$(tmp_file)"
    run bash -c 'echo "ping" | "$BIN" --headless --model mock --max-tokens 1 --output-trace "$1"' -- "${trace_file}"
    run python3 -c "
import json, sys
d = json.load(open(sys.argv[1]))
r = d.get('final_metrics', {}).get('extra', {}).get('reward')
sys.exit(0 if isinstance(r, float) else 1)
" "${trace_file}"
    [ "${status}" -eq 0 ]
    rm -f "${trace_file}"
}

@test "08.36 a plain headless run auto-logs an unscored trajectory into .sven/logs" {
    # The default headless path (no --output-trace, no workflow flags) routes
    # to RuntimeRunner. It writes the project auto-log, so ordinary runs are
    # recorded and ingestable, not just explicitly-traced ones - "recorded"
    # does not mean "scored": a plain claimed success has no verifier, so the
    # auto-logged trajectory carries the full step history but no reward.
    local work_dir
    work_dir="$(tmp_file)"
    mkdir -p "${work_dir}/.sven"
    run bash -c 'cd "$1" && echo "ping" | "$BIN" --headless --model mock' -- "${work_dir}"
    [ "${status}" -eq 0 ]
    run python3 -c "
import glob, json, sys
files = glob.glob(sys.argv[1] + '/.sven/logs/*.atif.json')
assert len(files) == 1, files
d = json.load(open(files[0]))
assert 'reward' not in d['final_metrics']['extra'], d['final_metrics']
assert any(s.get('message') == 'ping' for s in d['steps']), d['steps']
" "${work_dir}"
    [ "${status}" -eq 0 ]
    rm -rf "${work_dir}"
}

@test "08.37 an exhausted token budget stamps a zero reward" {
    local work_dir
    work_dir="$(tmp_file)"
    mkdir -p "${work_dir}/.sven"
    run bash -c 'cd "$1" && echo "ping" | "$BIN" --headless --model mock --max-tokens 1' -- "${work_dir}"
    [ "${status}" -eq 4 ]
    run python3 -c "
import glob, json, sys
files = glob.glob(sys.argv[1] + '/.sven/logs/*.atif.json')
assert len(files) == 1, files
extra = json.load(open(files[0]))['final_metrics']['extra']
assert extra['reward'] == 0.0, extra
assert extra['outcome'] == 'budget_exhausted', extra
" "${work_dir}"
    [ "${status}" -eq 0 ]
    rm -rf "${work_dir}"
}
