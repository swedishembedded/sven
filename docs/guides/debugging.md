<!--
SPDX-License-Identifier: Apache-2.0
Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
-->

# Debugging hardware with GDB

sven can drive a GDB session against embedded hardware. Give it a plain-English
task and the agent starts a debug server, loads your firmware, sets breakpoints,
inspects registers and memory, and cleans up, calling one tool: `gdb`.

The screenshots below show a session where the user asks sven to find the
parameters passed to an nRF UART TX function.

**Task start and target discovery** (ratatui TUI):

![sven GDB session - task start](../images/sven-gdb-1.png)

**Inspecting parameters and final summary** (ratatui TUI):

![sven GDB session - result](../images/sven-gdb-2.png)

**Same session in the embedded Neovim view**:

![sven GDB session - Neovim](../images/sven-gdb-nvim.png)

## Requirements

- A Unix system and a sven build with the `gdb` feature (on in the default
  build).
- `gdb-multiarch` on `PATH`, or another GDB set in `tools.gdb.gdb_path`.
- A GDB server for your probe (`JLinkGDBServer`, `openocd` or `pyocd`), unless
  one is already running.
- A firmware ELF with debug symbols, for symbolic debugging.

## When the `gdb` tool is offered

The tool is part of the full tool set, which a session gets when the project
root contains any of:

- `.gdbinit`, `openocd.cfg`, `openocd_board.cfg`, `pyocd.yaml`, `pyocd.yml`,
  `JLinkSettings.ini`, or a `debugging/` directory;
- `.vscode/launch.json` or `debugging/launch.json` mentioning `gdb`, `GDB` or
  `JLink`.

Elsewhere the tool is not offered. It is available in `agent` mode only, not in
`plan` or `research`, and is not available in `task` sub-agents. Under the
default deferred tool disclosure the model loads it first with the `tools` tool;
see [Tools and approvals](../reference/tools.md).

## The `gdb` tool

One tool with seven actions, normally used in this order:

```
start_server → connect → command (and interrupt / wait_stopped / status) → stop
```

| Action | What it does | Parameters |
|--------|--------------|------------|
| `start_server` | Starts the GDB server in the background (`sh -c`, own process group). If something already listens on the server's port it is reused; `force` kills it first. | `server_command`, `force` |
| `connect` | Starts GDB in MI mode, loads symbols and attaches with `target extended-remote host:port`. | `host` (default `localhost`), `port`, `executable`, `gdb_path` |
| `command` | Runs any GDB command (`break main`, `continue`, `info registers`, `x/10x 0x20000000`, `load`, `monitor reset halt`) and returns its output. | `command`, `timeout_secs` |
| `interrupt` | Halts a running target, like Ctrl+C. | `timeout_secs` (default 5) |
| `wait_stopped` | Blocks until the target halts after `continue` or a step and reports the stop reason, program counter, function, file and line. | `timeout_secs` (default 30) |
| `status` | Reports server, client and target state without interrupting. | none |
| `stop` | Disconnects GDB and kills the server. Always call it when done. | none |

`port` defaults to the port of the server `start_server` launched, else 2331.
`connect` fails with a clear error if `executable` does not exist or the server
cannot be reached.

### Finding the server command

When `start_server` is called without `server_command`, sven looks for one in
the project, in this order (first match wins):

1. `.gdbinit`: a `JLinkGDBServer`, `openocd` or `pyocd` command, usually in a comment.
2. `.vscode/launch.json`: `debugServerPath` with `debugServerArgs`, or `servertype`
   (`jlink`, `openocd`, `pyocd`) with `device`, `interface` and `port`.
3. `debugging/launch.json` and `.debug/launch.json`, read the same way.
4. `openocd.cfg`: `openocd -f <file>`.
5. `platformio.ini`: `debug_server`, else `debug_tool`.
6. `Makefile`: a J-Link `-device` name (and `-if`, `-speed`).
7. A chip name (STM32, AT32F, GD32, nRF, LPC, SAM, RP2040) found in `CMakeLists.txt`,
   `Cargo.toml` or `.cargo/config*`, which builds a `JLinkGDBServer` command.

If nothing is found the tool answers that the model should ask you which device
you are debugging, and the model then retries `start_server` with a device name
as `target` (letters, digits and `_ - . +` only), which builds a
`JLinkGDBServer -device <target> -if SWD -speed 4000 -port 2331` command.

### Example session

```
User: Flash and debug my firmware. The device is an AT32F435RMT7.

Agent calls (all through the gdb tool):
  start_server  {"server_command": "JLinkGDBServer -device AT32F435RMT7 -if SWD -speed 4000 -port 2331"}
  connect       {"executable": "build/firmware.elf"}
  command       {"command": "load", "timeout_secs": 120}
  command       {"command": "break main"}
  command       {"command": "continue"}
  wait_stopped  {}
  command       {"command": "info registers"}
  stop          {}
```

## Configuration

The `tools.gdb` section of the [configuration file](../reference/configuration.md):

| Key | Default | Description |
|-----|---------|-------------|
| `gdb_path` | `gdb-multiarch` | GDB executable |
| `command_timeout_secs` | `10` | Default timeout of a GDB command |
| `connect_timeout_secs` | `30` | Timeout of the `connect` handshake, which covers symbol loading; large ELFs can need 15 to 30 s |
| `server_startup_wait_ms` | `500` | Wait after spawning the server before checking it is alive |

```yaml
tools:
  gdb:
    gdb_path: /usr/bin/gdb-multiarch
    command_timeout_secs: 30
    connect_timeout_secs: 60
    server_startup_wait_ms: 1000
```

Slow operations (`load`, `monitor erase`) need a larger `timeout_secs` on the
`command` call.

## Permissions and safety

`gdb` is gated as shell execution: it runs a server command through `sh -c` and
GDB can run host commands. In `agent` mode it runs without asking under the
default `--approval auto`; under `--approval manual` every call is shown to you
first. See [Approval](../reference/tools.md#approval). Commands matching
`tools.deny_patterns` are refused, including the server command and the shell
side of GDB's `shell`, `!` and `pipe` commands. A server started by the tool is
killed by `stop` and when sven exits.
