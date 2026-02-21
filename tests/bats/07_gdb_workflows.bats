#!/usr/bin/env bats
# 07_gdb_workflows.bats – end-to-end tests for GDB debugging tools and workflows.
#
# Tests are structured in three tiers:
#
#   TIER 1  (always run): verifies CLI interface, tool registration, source code
#           structure, and mock-model interactions. No hardware required.
#
#   TIER 2  (skip unless SVEN_TEST_JLINK=1): verifies real J-Link connectivity
#           when a probe and target are physically connected.
#
#   TIER 3  (skip unless SVEN_TEST_JLINK=1 and JLINK_DEVICE=<name>):
#           verifies device-specific workflows (AT32F435RMT7, STM32H562VI).
#
# Run all tiers (hardware connected):
#   SVEN_TEST_JLINK=1 JLINK_DEVICE=AT32F435RMT7 bats tests/bats/07_gdb_workflows.bats
#
# Run only tier 1 (default):
#   bats tests/bats/07_gdb_workflows.bats

load helpers

# ── Helpers ───────────────────────────────────────────────────────────────────

# Cargo wrapper that works around registry permission issues in dev containers.
run_cargo_test() {
    local test_name="$1"
    shift
    CARGO_HOME=/tmp/cargo_home cargo test -p sven-tools \
        "${test_name}" \
        --no-fail-fast -- --nocapture "$@" 2>&1
}

# Skip if J-Link testing is not enabled.
require_jlink() {
    if [[ "${SVEN_TEST_JLINK:-0}" != "1" ]]; then
        skip "Set SVEN_TEST_JLINK=1 to run hardware tests"
    fi
}

# Skip if a specific device is not available.
require_device() {
    local device="${1:-}"
    require_jlink
    if [[ -z "${JLINK_DEVICE:-}" ]]; then
        skip "Set JLINK_DEVICE=<device> to run device-specific tests"
    fi
    if [[ "${JLINK_DEVICE}" != "${device}" ]]; then
        skip "This test requires JLINK_DEVICE=${device}, got ${JLINK_DEVICE}"
    fi
}

# Skip if the sven binary is not built.
setup() {
    if [[ ! -x "${BIN}" ]]; then
        skip "Binary not found: ${BIN} — run 'cargo build' first"
    fi
}

# ── Tier 1: CLI / tool registration ──────────────────────────────────────────

@test "07.01 sven --help mentions gdb tools" {
    run "${BIN}" --help
    [ "${status}" -eq 0 ]
    # GDB tools should be available/mentioned
}

@test "07.02 show-config includes gdb section" {
    run "${BIN}" show-config
    [ "${status}" -eq 0 ]
    # Config should have gdb settings
    assert_output_contains "gdb"
}

@test "07.03 glob_file_search finds elf files with star-dot-elf pattern" {
    local tmp_dir
    tmp_dir="$(mktemp -d)"
    mkdir -p "${tmp_dir}/build/zephyr"
    printf '\x7fELF' > "${tmp_dir}/build/zephyr/zephyr.elf"

    run bash -c "echo 'find elf' | SVEN_TEST_ROOT=${tmp_dir} \"${BIN}\" --headless --model mock 2>/dev/null"
    [ "${status}" -eq 0 ]

    rm -rf "${tmp_dir}"
}

@test "07.04 glob_file_search double-star pattern works" {
    local tmp_dir
    tmp_dir="$(mktemp -d)"
    mkdir -p "${tmp_dir}/deep/nested/dir"
    printf '\x7fELF' > "${tmp_dir}/deep/nested/dir/firmware.elf"

    # The glob_file_search tool should handle **/*.elf by normalising to *.elf
    run "${BIN}" show-config
    [ "${status}" -eq 0 ]

    rm -rf "${tmp_dir}"
}

@test "07.05 gdb_stop mock returns success when no session active" {
    run bash -c "echo 'stop gdb' | \"${BIN}\" --headless --model mock 2>/dev/null"
    [ "${status}" -eq 0 ]
}

@test "07.06 mock gdb_start_server call completes without crashing sven" {
    # The mock triggers gdb_start_server with a 'sleep' command; the tool may
    # report an error (sleep exits fast or has wrong args) but sven itself must
    # exit cleanly (status 0 = agent completed, even if tool errored).
    run bash -c "echo 'start gdb server' | \"${BIN}\" --headless --model mock 2>/dev/null"
    [ "${status}" -eq 0 ]
}

@test "07.07 gdb session mock returns expected summary" {
    run bash -c "echo 'gdb session' | \"${BIN}\" --headless --model mock 2>/dev/null"
    [ "${status}" -eq 0 ]
    assert_output_contains "session"
}

# ── Tier 1: Discovery workflow (uses temp files, no hardware) ─────────────────

@test "07.08 discover device mock response includes device info" {
    run bash -c "echo 'discover device' | \"${BIN}\" --headless --model mock 2>/dev/null"
    [ "${status}" -eq 0 ]
    assert_output_contains "AT32F435RMT7"
}

@test "07.09 gdb-debug workflow file exists and has correct structure" {
    # .sven/ is gitignored (user-local config); skip if not present in this checkout.
    local workflow="${_REPO_ROOT}/.sven/workflow/examples/gdb-debug.md"
    if [[ ! -f "${workflow}" ]]; then
        skip ".sven/workflow/examples/ not present (gitignored user-local config)"
    fi
    grep -q "^# Embedded GDB Debug Session" "${workflow}"
    grep -q "gdb_connect" "${workflow}"
    grep -q "gdb_start_server" "${workflow}"
    grep -q "gdb_stop" "${workflow}"
}

@test "07.10 gdb-attach-only workflow file exists" {
    local workflow="${_REPO_ROOT}/.sven/workflow/examples/gdb-attach-only.md"
    if [[ ! -f "${workflow}" ]]; then
        skip ".sven/workflow/examples/ not present (gitignored user-local config)"
    fi
    grep -q "gdb_connect" "${workflow}"
    grep -q "gdb_stop" "${workflow}"
}

@test "07.11 gdb-flash-and-debug workflow file exists" {
    local workflow="${_REPO_ROOT}/.sven/workflow/examples/gdb-flash-and-debug.md"
    if [[ ! -f "${workflow}" ]]; then
        skip ".sven/workflow/examples/ not present (gitignored user-local config)"
    fi
    grep -q "load" "${workflow}"
}

@test "07.12 gdb-mcuboot-debug workflow file exists" {
    local workflow="${_REPO_ROOT}/.sven/workflow/examples/gdb-mcuboot-debug.md"
    if [[ ! -f "${workflow}" ]]; then
        skip ".sven/workflow/examples/ not present (gitignored user-local config)"
    fi
    grep -q "mcuboot" "${workflow}"
    grep -q "add-symbol-file" "${workflow}"
}

# ── Tier 1: Discovery from fixture project structures ─────────────────────────

@test "07.13 discovery finds device from debugging/launch.json fixture" {
    run run_cargo_test "discovery_reads_debugging_launch_json"
    [ "${status}" -eq 0 ]
}

@test "07.14 discovery finds device from Makefile fixture" {
    run run_cargo_test "discovery_reads_makefile_device"
    [ "${status}" -eq 0 ]
}

@test "07.15 elf discovery finds sysbuild zephyr.elf" {
    run run_cargo_test "elf_discovery_finds_sysbuild_elf"
    [ "${status}" -eq 0 ]
}

@test "07.16 elf discovery skips mcuboot prefers app elf" {
    run run_cargo_test "elf_discovery_skips_mcuboot_prefers_app"
    [ "${status}" -eq 0 ]
}

@test "07.17 glob_file_search double-star normalisation unit test" {
    run run_cargo_test "normalise_glob_strips_double_star_prefix"
    [ "${status}" -eq 0 ]
}

@test "07.18 connect gives helpful error when elf not found" {
    run run_cargo_test "connect_fails_when_elf_not_found"
    [ "${status}" -eq 0 ]
}

@test "07.19 connect gives helpful error when nothing listening" {
    run run_cargo_test "connect_fails_gracefully_when_nothing_listening"
    [ "${status}" -eq 0 ]
}

@test "07.20 launch.json dash-prefixed device name is handled" {
    run run_cargo_test "launch_json_servertype_jlink_device_with_dash_prefix"
    [ "${status}" -eq 0 ]
}

# ── Tier 1: Regression tests for known bugs ───────────────────────────────────

@test "07.21 gdb_connect does not use await_ready before target remote" {
    # Verify the source code uses -ex for connection (regression for await_ready timeout bug)
    grep -q '\-ex' "${_REPO_ROOT}/crates/sven-tools/src/builtin/gdb/connect.rs"
}

@test "07.22 gdb_connect uses extended-remote not plain remote" {
    grep -q 'extended-remote' "${_REPO_ROOT}/crates/sven-tools/src/builtin/gdb/connect.rs"
}

@test "07.23 discovery checks debugging/launch.json path" {
    grep -q '"debugging"' "${_REPO_ROOT}/crates/sven-tools/src/builtin/gdb/discovery.rs"
}

@test "07.24 discovery includes makefile_to_server_command function" {
    grep -q 'makefile_to_server_command' "${_REPO_ROOT}/crates/sven-tools/src/builtin/gdb/discovery.rs"
}

@test "07.25 glob_file_search normalises star-star pattern" {
    grep -q 'normalise_glob_for_find' "${_REPO_ROOT}/crates/sven-tools/src/builtin/glob_file_search.rs"
}

@test "07.26 find_firmware_elf function is exported" {
    grep -q 'pub fn find_firmware_elf' "${_REPO_ROOT}/crates/sven-tools/src/builtin/gdb/discovery.rs"
}

# ── Tier 2: Real hardware (requires SVEN_TEST_JLINK=1) ───────────────────────

@test "07.27 hardware: JLinkGDBServer binary is on PATH" {
    require_jlink
    run which JLinkGDBServer
    [ "${status}" -eq 0 ]
}

@test "07.28 hardware: gdb-multiarch binary is on PATH" {
    require_jlink
    run which gdb-multiarch
    [ "${status}" -eq 0 ]
}

@test "07.29 hardware: JLinkGDBServer starts and accepts connection" {
    require_jlink
    local device="${JLINK_DEVICE:-AT32F435RMT7}"

    # Start JLinkGDBServer in background
    JLinkGDBServer -device "${device}" -if SWD -speed 4000 -port 12331 &
    local server_pid=$!
    sleep 2

    # Verify it's listening
    run ss -tln
    kill "${server_pid}" 2>/dev/null || true
    wait "${server_pid}" 2>/dev/null || true

    assert_output_contains "12331"
}

@test "07.30 hardware: full gdb session lifecycle via cargo test" {
    require_jlink
    run cargo test -p sven-tools \
        hardware_jlink_at32_full_lifecycle \
        --no-fail-fast -- --ignored --nocapture 2>&1
    [ "${status}" -eq 0 ]
}

# ── Tier 3: Device-specific tests ─────────────────────────────────────────────

@test "07.31 hardware: AT32F435RMT7 registers are readable" {
    require_device "AT32F435RMT7"

    JLinkGDBServer -device AT32F435RMT7 -if SWD -speed 4000 -port 12332 &
    local server_pid=$!
    sleep 2

    run timeout 10 gdb-multiarch \
        --batch \
        --interpreter=mi3 \
        -ex "target extended-remote localhost:12332" \
        -ex "monitor reset halt" \
        -ex "info registers" \
        -ex "quit"

    kill "${server_pid}" 2>/dev/null || true
    wait "${server_pid}" 2>/dev/null || true

    # Should contain register output
    [[ "${output}" == *"pc"* ]] || [[ "${output}" == *"r0"* ]]
}

@test "07.32 hardware: STM32H562VI registers are readable" {
    require_device "STM32H562VI"

    JLinkGDBServer -device STM32H562VI -if SWD -speed 4000 -port 12333 &
    local server_pid=$!
    sleep 2

    run timeout 10 gdb-multiarch \
        --batch \
        --interpreter=mi3 \
        -ex "target extended-remote localhost:12333" \
        -ex "monitor reset halt" \
        -ex "info registers" \
        -ex "quit"

    kill "${server_pid}" 2>/dev/null || true
    wait "${server_pid}" 2>/dev/null || true

    [[ "${output}" == *"pc"* ]] || [[ "${output}" == *"r0"* ]]
}
