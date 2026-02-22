---
title: Embedded GDB Debug Session
mode: agent
step_timeout_secs: 300
run_timeout_secs: 1800
vars:
  elf_path: ""
  device: ""
  port: "2331"
  interface: "SWD"
  speed: "4000"
---

# Embedded GDB Debug Session

A workflow for attaching a GDB server to an embedded target, inspecting
firmware state, and tearing down the session cleanly.

The firmware must already be flashed to the target before running this workflow.

## Discover target and ELF

<!-- step: mode=research timeout=90 -->

Gather the information needed to start a debug session.

### 1. Find the ELF binary

If `{{elf_path}}` is set, verify that file exists and use it directly.

Otherwise find it automatically. Use `run_terminal_command` to run:
```
find . -maxdepth 6 -name "*.elf" \
  -not -path "*mcuboot*" \
  -not -path "*bootloader*" \
  -not -path "*_pre0*" \
  -not -path "*native_sim*" \
  2>/dev/null | head -20
```

Priority rules for selecting among multiple matches:
- **Zephyr/sysbuild**: prefer `build-firmware/<app>/zephyr/zephyr.elf`
  (the application image, not mcuboot)
- **PlatformIO**: `.pio/build/<env>/firmware.elf`
- When multiple candidates remain, pick the newest file

Report the absolute ELF path (or "not found" — debugging without symbols is
still possible).

### 2. Find the MCU / device name

Check in order (stop at first hit):
1. `debugging/launch.json` — grep for `"device"`, strip the leading `-`
2. `.vscode/launch.json` — grep for `"device"`
3. `.gdbinit` — grep for `JLinkGDBServer`
4. `Makefile` — grep for `-device`

### 3. GDB server command

Use the explicit command from the project files above if found.
If not found, construct:
`JLinkGDBServer -device <DEVICE> -if {{interface}} -speed {{speed}} -port {{port}}`

**Report:**
- ELF path (absolute, or "not found")
- Device name
- Full GDB server command
- Port number

## Start GDB server

<!-- step: mode=agent timeout=90 -->

Start the GDB server using the command discovered in the previous step.

**Attempt 1 — auto-start:**
Call `gdb_start_server` with the discovered `command` string.

**If the server exits immediately with "Port N is already in use":**
This means a zombie JLink server from a previous session is still running.
Call `gdb_start_server` again with the same command **and** `force=true`.
The `force` flag kills the zombie process and starts fresh.

**If the server exits immediately for any other reason:**
1. Run `ss -tln | grep {{port}}` to check if port {{port}} is actually listening
   - If listening → the server may be running; proceed to the Connect step
2. Check device name is correct (exact part number, e.g. `AT32F435RMT7`)
3. Report what failed; do NOT proceed to gdb_connect if nothing is listening

**Success condition:** `gdb_start_server` reports "started successfully", OR
`ss -tln` confirms something is listening on port {{port}}.

## Connect and inspect

<!-- step: mode=agent timeout=90 -->

Connect gdb-multiarch to the running GDB server.

Call `gdb_connect` with:
- `port`: {{port}} (or port reported by gdb_start_server)
- `executable`: the absolute ELF path from the discovery step (omit if not found — GDB will connect without debug symbols)

The firmware is already on the target — do **NOT** run `load`.

After connecting, run these inspection commands with `gdb_command`:
1. `monitor reset halt`   — halt the CPU at a known state
2. `info registers`       — capture CPU registers
3. `x/16x $sp`            — dump top of stack
4. `info symbol $pc`      — identify current PC location

Report the register values and flag anything unexpected:
- Fault registers (CFSR, HFSR) non-zero → hard fault in progress
- Stack pointer outside SRAM range → stack corruption
- PC pointing to 0xDEADBEEF or similar → invalid reset handler

## Set breakpoints and run

<!-- step: mode=agent timeout=120 -->

Set a breakpoint and run to it.

1. `gdb_command` → `break arch_cpu_idle`
   (Use `arch_cpu_idle` for Zephyr targets — it is called continuously during
   idle and will be hit quickly. Alternatively use `main` for bare-metal.)
2. `gdb_command` → `continue`
3. Call `gdb_wait_stopped` to wait for the breakpoint to hit (up to 30 s).
   - If it returns "stopped at breakpoint": proceed to step 4.
   - If it times out: call `gdb_interrupt` to halt the CPU, then proceed to step 4.
4. `gdb_command` → `backtrace`
5. `gdb_command` → `info locals`

Report:
- Whether the breakpoint hit or the target was interrupted
- Call stack at that point
- Any local variable values of interest

## Stop session

<!-- step: mode=agent timeout=30 -->

Call `gdb_stop` to disconnect gdb-multiarch and kill the GDB server.

**Always call `gdb_stop` — even if previous steps failed.**

Summarise the debug session:
- ELF loaded successfully: yes/no
- Connected to target: yes/no
- Breakpoint hit: yes/no
- Any anomalies found (fault registers, bad stack, unexpected PC)
- Recommended next steps
