# Debugging hardware with GDB

### GDB debugging tools

Sven is the **first AI agent with native GDB integration** for autonomous
embedded hardware debugging. Give it a plain-English task and it handles the
entire debug lifecycle - from starting the server and loading firmware through
setting breakpoints, inspecting state, and cleaning up - without any manual
intervention.

The five GDB tools form a lifecycle:

```
gdb_start_server → gdb_connect → gdb_command / gdb_interrupt → gdb_stop
```

The screenshots below show a real session: the user asks sven to find the
parameters passed to an nRF UART TX function, and the agent works through the
full debug cycle autonomously.

**Task start and target discovery** (ratatui TUI):

![sven GDB session - task start](../images/sven-gdb-1.png)

**Inspecting parameters and final summary** (ratatui TUI):

![sven GDB session - result](../images/sven-gdb-2.png)

**Same session in the embedded Neovim view**:

![sven GDB session - Neovim](../images/sven-gdb-nvim.png)

**Starting a server**

If you do not supply a command, sven searches your project for configuration
hints in this order:

1. `.gdbinit` - looks for `# JLinkGDBServer ...` comments or `target remote` lines
2. `.vscode/launch.json` - reads `debugServerPath`, `debugServerArgs`, and `servertype`
3. `openocd.cfg` - builds an OpenOCD command from the config file
4. `platformio.ini` - reads `debug_server` or `debug_tool`
5. `CMakeLists.txt` / `Cargo.toml` - matches MCU family names (STM32, AT32, NRF, ...)

If discovery fails, sven asks the user for the target device name.

**Example session**

```
User: Flash and debug my firmware. The device is an AT32F435RMT7.

Agent calls:
  gdb_start_server {"target": "AT32F435RMT7"}
  gdb_connect      {"executable": "build/firmware.elf"}
  gdb_command      {"command": "load"}
  gdb_command      {"command": "break main"}
  gdb_command      {"command": "continue"}
  gdb_command      {"command": "info registers"}
  gdb_stop
```
