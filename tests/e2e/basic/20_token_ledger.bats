#!/usr/bin/env bats
# 20_token_ledger.bats - the per-request token ledger.
#
# Covers:
#   • --token-ledger prints one [sven:ledger] JSON line per model request
#   • the [sven:tokens] line is byte-identical with and without the ledger
#   • every tool of the default set is charged its own schema tokens
#   • a tool-using run attributes the tool call and its result to the tool
#   • the ledger is saved in the trajectory and `sven ledger` summarizes it
#   • a trajectory without a ledger is reported as such, not as zeros
#   • --tool-disclosure deferred charges the core tools only, and a deferred
#     tool is loaded before it is called; full is the legacy listing

load helpers

@test "20.01 no [sven:ledger] line without --token-ledger" {
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock'
    [[ "${STDERR_OUT}" != *"[sven:ledger]"* ]]
}

@test "20.02 --token-ledger prints a ledger line that attributes tool schemas" {
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock --token-ledger'
    ledger="$(grep '^\[sven:ledger\] ' <<<"${STDERR_OUT}")"
    [[ -n "${ledger}" ]]
    [[ "${ledger}" == *'"request":1'* ]]
    [[ "${ledger}" == *'"estimated_total":'* ]]
    [[ "${ledger}" == *'"tool_schemas":{'* ]]
    [[ "${ledger}" == *'"estimator":"bytes_div_4"'* ]]
}

@test "20.03 the [sven:tokens] line is byte-identical with and without the ledger" {
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock'
    plain="$(grep '^\[sven:tokens\]' <<<"${STDERR_OUT}")"
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock --token-ledger'
    with_ledger="$(grep '^\[sven:tokens\]' <<<"${STDERR_OUT}")"
    [[ -n "${plain}" ]]
    [[ "${plain}" == "${with_ledger}" ]]
}

@test "20.04 a tool call and its result are charged to the tool on the next request" {
    run_split_output bash -c 'echo "write a file for me" | "$BIN" --headless --model mock --token-ledger'
    second="$(grep '^\[sven:ledger\] ' <<<"${STDERR_OUT}" | sed -n 2p)"
    [[ -n "${second}" ]]
    [[ "${second}" == *'"request":2'* ]]
    [[ "${second}" == *'"tool_call_arguments":{"write_file":'* ]]
    [[ "${second}" == *'"tool_results":{"write_file":'* ]]
}

@test "20.05 the ledger is saved in the trajectory and sven ledger summarizes it" {
    trace="$(tmp_file).json"
    run_split_output bash -c 'echo "write a file for me" | "$BIN" --headless --model mock --output-trace "$0"' "${trace}"
    run "${BIN}" ledger "${trace}"
    [[ "${status}" -eq 0 ]]
    [[ "${output}" == *"requests: 2"* ]]
    [[ "${output}" == *"schema: "* ]]
    [[ "${output}" == *"result: write_file"* ]]
    run "${BIN}" ledger "${trace}" --json
    [[ "${status}" -eq 0 ]]
    [[ "${output}" == *'"requests": 2'* ]]
    rm -f "${trace}"
}

@test "20.06 a trajectory recorded without a ledger is reported as such" {
    trace="$(tmp_file).json"
    printf '%s' '{"schema_version":"ATIF-v1.7","agent":{"name":"sven","version":"1"},"steps":[{"step_id":1,"source":"agent","message":"hi","metrics":{"prompt_tokens":5,"completion_tokens":1}}]}' >"${trace}"
    run "${BIN}" ledger "${trace}"
    [[ "${status}" -eq 0 ]]
    [[ "${output}" == *"no token ledger"* ]]
    rm -f "${trace}"
}

@test "20.07 every core tool is charged its own schema tokens" {
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock --token-ledger'
    ledger="$(grep '^\[sven:ledger\] ' <<<"${STDERR_OUT}")"
    for tool in read_file write_file edit_file shell grep; do
        [[ "${ledger}" =~ \"${tool}\":[1-9][0-9]* ]]
    done
}

@test "20.08 --tool-disclosure deferred charges the core tools and the tools tool, not the rest" {
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock --token-ledger --tool-disclosure deferred'
    ledger="$(grep '^\[sven:ledger\] ' <<<"${STDERR_OUT}")"
    for tool in read_file write_file edit_file shell grep todo tools; do
        [[ "${ledger}" =~ \"${tool}\":[1-9][0-9]* ]]
    done
    for tool in find_file task system web_fetch web_search skill memory; do
        [[ ! "${ledger}" =~ \"tool_schemas\":\{[^\}]*\"${tool}\": ]]
    done
    # What is not loaded is described once, in the system prompt.
    [[ "${ledger}" == *'"Deferred Tools":'* ]]
}

@test "20.09 --tool-disclosure full is the legacy listing: no tools tool, no index" {
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock --token-ledger --tool-disclosure full'
    ledger="$(grep '^\[sven:ledger\] ' <<<"${STDERR_OUT}")"
    [[ "${ledger}" =~ \"task\":[1-9][0-9]* ]]
    [[ ! "${ledger}" =~ \"tool_schemas\":\{[^\}]*\"tools\": ]]
    [[ "${ledger}" != *'"Deferred Tools":'* ]]
}

@test "20.10 a deferred tool is loaded through the tools tool before it is called" {
    local home_dir
    home_dir="$(mktemp -d)"
    run_split_output env HOME="${home_dir}" bash -c \
        'echo "remember the launch code" | "$BIN" --headless --model mock --tool-disclosure deferred -v'
    [ "${EXIT_CODE}" -eq 0 ]
    load_line="$(grep -n '\[sven:tool:call\].* name="tools"' <<<"${STDERR_OUT}" | head -1 | cut -d: -f1)"
    call_line="$(grep -n '\[sven:tool:call\].* name="semantic_memory"' <<<"${STDERR_OUT}" | head -1 | cut -d: -f1)"
    [[ -n "${load_line}" && -n "${call_line}" ]]
    [[ "${load_line}" -lt "${call_line}" ]]
    rm -rf "${home_dir}"
}
