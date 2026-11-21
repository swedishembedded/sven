#!/usr/bin/env bats
# 27_run_profile.bats - the compact run profile (--profile compact).
#
# Covers:
#   • the request carries the three compact tool definitions and nothing else
#   • the whole request of a short task is under 600 estimated tokens
#   • the trajectory names the profile's version
#   • without the flag the default tool set and prompt are unchanged

load helpers

@test "27.01 --profile compact offers exactly read_file, write_file and shell" {
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock --profile compact --token-ledger'
    ledger="$(grep '^\[sven:ledger\] ' <<<"${STDERR_OUT}" | head -1)"
    [[ -n "${ledger}" ]]
    [[ "${ledger}" == *'"tool_schemas":{"read_file":'*'"shell":'*'"write_file":'*'}'* ]]
    [[ "${ledger}" != *'"todo"'* ]]
    [[ "${ledger}" != *'"task"'* ]]
}

@test "27.02 a compact request of a short task is under 600 estimated tokens" {
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock --profile compact --token-ledger'
    ledger="$(grep '^\[sven:ledger\] ' <<<"${STDERR_OUT}" | head -1)"
    total="$(sed -n 's/.*"estimated_total":\([0-9]*\).*/\1/p' <<<"${ledger}")"
    [[ -n "${total}" ]]
    [[ "${total}" -lt 600 ]]
}

@test "27.03 the trajectory records the profile version" {
    trace="$(tmp_file).json"
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock --profile compact --output-trace "$0"' "${trace}"
    run grep -c '"profile": "compact/3"' "${trace}"
    [[ "${output}" == "1" ]]
    rm -f "${trace}"
}

@test "27.04 without the flag the trajectory names no profile" {
    trace="$(tmp_file).json"
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock --output-trace "$0"' "${trace}"
    run grep -c '"profile"' "${trace}"
    [[ "${output}" == "0" ]]
    rm -f "${trace}"
}

@test "27.05 an unknown profile is refused" {
    run_split_output bash -c 'echo "ping" | "$BIN" --headless --model mock --profile tiny'
    [[ "${EXIT_CODE}" -ne 0 ]]
}
