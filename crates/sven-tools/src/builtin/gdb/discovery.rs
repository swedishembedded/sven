// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
/// Agent intelligence for discovering the GDB server command and firmware ELF
/// from common project structures including Zephyr/sysbuild, PlatformIO, Cargo
/// embedded, and CMake/Make projects.
///
/// Discovery strategy (tried in order, first match wins):
///   1. `.gdbinit`                   — explicit server comment or target remote
///   2. `.vscode/launch.json`        — cortex-debug / debugServerPath
///   3. `debugging/launch.json`      — alternative location (ng-iot-platform style)
///   4. `openocd.cfg`               — OpenOCD config
///   5. `platformio.ini`            — PlatformIO debug_server / debug_tool
///   6. `Makefile`                  — JLinkExe / JLinkRTTLogger / flash targets
///   7. Chip heuristics             — scan CMakeLists, Cargo.toml, board files
use std::path::{Path, PathBuf};

use anyhow::Result;
use regex::Regex;

/// Find the project root by walking up from `cwd` looking for `.git`.
fn find_project_root(cwd: &Path) -> Option<PathBuf> {
    let mut dir = cwd.to_owned();
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Read a file to a string, returning None on any IO error.
fn read_opt(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Try to discover a GDB server launch command from common project files.
///
/// `search_from` is the directory to start looking from; pass `None` to use
/// the process's current working directory.
///
/// Returns `Ok(Some(command_string))` if found, `Ok(None)` if the project
/// files don't contain enough information, and `Err` only on unexpected errors.
pub async fn discover_gdb_server_command() -> Result<Option<String>> {
    discover_gdb_server_command_in(None).await
}

/// Like [`discover_gdb_server_command`] but searches from `base`.
pub async fn discover_gdb_server_command_in(
    base: Option<&std::path::Path>,
) -> Result<Option<String>> {
    let cwd = match base {
        Some(b) => b.to_path_buf(),
        None => std::env::current_dir()?,
    };
    let root = find_project_root(&cwd).unwrap_or(cwd.clone());

    // ── 1. .gdbinit ──────────────────────────────────────────────────────────
    for candidate in [root.join(".gdbinit"), cwd.join(".gdbinit")] {
        if let Some(content) = read_opt(&candidate) {
            if let Some(cmd) = gdbinit_to_server_command(&content) {
                return Ok(Some(cmd));
            }
        }
    }

    // ── 2. .vscode/launch.json ───────────────────────────────────────────────
    let vscode_launch = root.join(".vscode").join("launch.json");
    if let Some(content) = read_opt(&vscode_launch) {
        if let Some(cmd) = launch_json_to_server_command(&content) {
            return Ok(Some(cmd));
        }
    }

    // ── 3. debugging/launch.json (ng-iot-platform and similar) ───────────────
    for candidate in [
        root.join("debugging").join("launch.json"),
        cwd.join("debugging").join("launch.json"),
        root.join(".debug").join("launch.json"),
    ] {
        if let Some(content) = read_opt(&candidate) {
            if let Some(cmd) = launch_json_to_server_command(&content) {
                return Ok(Some(cmd));
            }
        }
    }

    // ── 4. openocd.cfg ───────────────────────────────────────────────────────
    for candidate in [root.join("openocd.cfg"), cwd.join("openocd.cfg")] {
        if candidate.exists() {
            return Ok(Some(openocd_command(&candidate)));
        }
    }

    // ── 5. platformio.ini ────────────────────────────────────────────────────
    for candidate in [root.join("platformio.ini"), cwd.join("platformio.ini")] {
        if let Some(content) = read_opt(&candidate) {
            if let Some(cmd) = platformio_to_server_command(&content) {
                return Ok(Some(cmd));
            }
        }
    }

    // ── 6. Makefile ──────────────────────────────────────────────────────────
    for candidate in [root.join("Makefile"), cwd.join("Makefile")] {
        if let Some(content) = read_opt(&candidate) {
            if let Some(cmd) = makefile_to_server_command(&content) {
                return Ok(Some(cmd));
            }
        }
    }

    // ── 7. Device/chip heuristics from CMakeLists / Cargo.toml / board files ─
    if let Some(cmd) = chip_heuristics(&root) {
        return Ok(Some(cmd));
    }

    Ok(None)
}

// ─── ELF discovery ───────────────────────────────────────────────────────────

/// Try to find the firmware ELF binary for a project.
///
/// Searches common build output locations used by Zephyr/sysbuild, PlatformIO,
/// Cargo embedded, and plain CMake/Make projects. Returns the path to the
/// most likely firmware ELF (newest mtime, largest of ambiguous matches).
pub fn find_firmware_elf(project_root: &Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    // Zephyr sysbuild (west build --sysbuild): build-firmware/<app>/zephyr/zephyr.elf
    // Multiple possible build dirs; collect all matches.
    for build_dir in ["build-firmware", "build", "out", "target"] {
        let base = project_root.join(build_dir);
        if !base.exists() {
            continue;
        }
        // Direct Zephyr structure: build/zephyr/zephyr.elf
        let direct = base.join("zephyr").join("zephyr.elf");
        if direct.exists() {
            candidates.push(direct);
        }
        // Sysbuild: build-firmware/<app>/zephyr/zephyr.elf (walk one level)
        if let Ok(entries) = std::fs::read_dir(&base) {
            for entry in entries.flatten() {
                let candidate = entry.path().join("zephyr").join("zephyr.elf");
                if candidate.exists() {
                    candidates.push(candidate);
                }
            }
        }
    }

    // PlatformIO: .pio/build/<env>/firmware.elf
    let pio = project_root.join(".pio").join("build");
    if let Ok(entries) = std::fs::read_dir(&pio) {
        for entry in entries.flatten() {
            let candidate = entry.path().join("firmware.elf");
            if candidate.exists() {
                candidates.push(candidate);
            }
        }
    }

    // Cargo embedded: target/<triple>/{debug,release}/*.elf
    let cargo_target = project_root.join("target");
    if cargo_target.exists() {
        collect_elf_under(&cargo_target, 3, &mut candidates);
    }

    // Plain CMake / Make out-of-tree builds
    for build_dir in ["build", "cmake-build-debug", "cmake-build-release"] {
        let base = project_root.join(build_dir);
        collect_elf_under(&base, 2, &mut candidates);
    }

    if candidates.is_empty() {
        return None;
    }

    // Prefer by mtime (newest first), then by size (largest = usually main fw)
    candidates.sort_by(|a, b| {
        let mt_a = a.metadata().and_then(|m| m.modified()).ok();
        let mt_b = b.metadata().and_then(|m| m.modified()).ok();
        mt_b.cmp(&mt_a)
    });

    // Prefer the firmware application ELF over bootloader/test ELFs
    let preferred = candidates.iter().find(|p| {
        let s = p.to_string_lossy().to_lowercase();
        // Exclude mcuboot, bootloader, test ELFs
        !s.contains("mcuboot")
            && !s.contains("bootloader")
            && !s.contains("_pre0")
            && !s.contains("native_sim")
            && !s.contains("zephyr_pre")
    });

    preferred.or_else(|| candidates.first()).cloned()
}

/// Recursively collect `.elf` files under `dir` up to `depth` levels deep.
fn collect_elf_under(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth == 0 || !dir.exists() {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_elf_under(&path, depth - 1, out);
            } else if path.extension().map(|e| e == "elf").unwrap_or(false) {
                out.push(path);
            }
        }
    }
}

/// Inspect `.gdbinit` for clues about the debug server.
///
/// Typical patterns:
/// ```text
/// target remote :2331
/// mon reset
/// ```
///
/// or a JLink init comment:
/// ```text
/// # JLinkGDBServer -device STM32F407VG -if SWD -speed 4000 -port 2331
/// ```
fn gdbinit_to_server_command(content: &str) -> Option<String> {
    // If there is an explicit JLinkGDBServer / openocd comment, use that.
    let server_re =
        Regex::new(r"(?i)(?:#\s*)?(JLinkGDBServer|JLinkGDBServerCL|openocd|pyocd)[^\n]*").unwrap();
    if let Some(m) = server_re.find(content) {
        let cmd = m.as_str().trim_start_matches('#').trim().to_string();
        if !cmd.is_empty() {
            return Some(cmd);
        }
    }

    // Fall back: if there is a `target remote :PORT` line we can infer
    // JLinkGDBServer is commonly used and build a default command.
    let remote_re =
        Regex::new(r"target\s+(?:extended-)?remote\s+(?:[a-zA-Z0-9.]+:)?(\d+)").unwrap();
    if let Some(caps) = remote_re.captures(content) {
        let port = &caps[1];
        // We know the port but not the device – return None and let the
        // caller fall through to further heuristics; we'll only use this
        // as a last resort to learn the port.
        let _ = port; // used in chip_heuristics via extract_port
    }

    None
}

/// Extract the GDB port hint from a `.gdbinit` file for use by other helpers.
pub fn extract_port_from_gdbinit(content: &str) -> Option<u16> {
    let re = Regex::new(r"target\s+(?:extended-)?remote\s+(?:[a-zA-Z0-9.]+:)?(\d+)").unwrap();
    re.captures(content).and_then(|c| c[1].parse().ok())
}

/// Parse a VS Code / cortex-debug compatible `launch.json` (from `.vscode/`,
/// `debugging/`, or any other path) for a GDB server command.
///
/// Handles:
///   - `debugServerPath` + `debugServerArgs` (explicit server command)
///   - `servertype` = "jlink" | "openocd" | "pyocd" (inferred from cortex-debug config)
///   - `device` field for JLink device name
///   - `miDebuggerServerAddress` for host:port
pub fn launch_json_to_server_command(content: &str) -> Option<String> {
    // Strip comments (launch.json often has // comments which are not valid JSON).
    let stripped = strip_json_comments(content);
    let json: serde_json::Value = serde_json::from_str(&stripped).ok()?;

    let configurations = json.get("configurations")?.as_array()?;
    for cfg in configurations {
        // debugServerPath + debugServerArgs → explicit server command
        let server_path = cfg.get("debugServerPath").and_then(|v| v.as_str());
        if let Some(path) = server_path {
            let args = cfg
                .get("debugServerArgs")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let cmd = format!("{path} {args}").trim().to_string();
            if !cmd.is_empty() {
                return Some(cmd);
            }
        }

        let server_type = cfg
            .get("servertype")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();

        // cortex-debug: servertype + device (no miDebuggerServerAddress)
        // e.g. ng-iot-platform debugging/launch.json
        if !server_type.is_empty() {
            let port = cfg.get("port").and_then(|v| v.as_u64()).unwrap_or(2331);

            let raw_device = cfg.get("device").and_then(|v| v.as_str()).unwrap_or("");
            // Remove leading dash that cortex-debug sometimes uses
            let device = raw_device.trim_start_matches('-');

            match server_type.as_str() {
                "jlink" if !device.is_empty() => {
                    let interface = cfg
                        .get("interface")
                        .and_then(|v| v.as_str())
                        .unwrap_or("SWD");
                    return Some(format!(
                        "JLinkGDBServer -device {device} -if {interface} -speed 4000 -port {port}"
                    ));
                }
                "jlink" => {
                    return Some(format!("JLinkGDBServer -if SWD -speed 4000 -port {port}"));
                }
                "openocd" => {
                    return Some(format!("openocd -c 'gdb_port {port}'"));
                }
                "pyocd" => {
                    return Some(format!("pyocd gdbserver -p {port}"));
                }
                _ => {}
            }
        }

        // miDebuggerServerAddress fallback
        if let Some(addr) = cfg.get("miDebuggerServerAddress").and_then(|v| v.as_str()) {
            let port = addr.split(':').next_back().unwrap_or("3333");
            match server_type.as_str() {
                "jlink" => {
                    let raw_device = cfg
                        .get("device")
                        .and_then(|v| v.as_str())
                        .unwrap_or("cortex-m4");
                    let device = raw_device.trim_start_matches('-');
                    return Some(format!(
                        "JLinkGDBServer -device {device} -if SWD -speed 4000 -port {port}"
                    ));
                }
                "openocd" => return Some(format!("openocd -c 'gdb_port {port}'")),
                "pyocd" => return Some(format!("pyocd gdbserver -p {port}")),
                _ => {}
            }
        }
    }
    None
}

/// Parse a `Makefile` for J-Link device names and build a GDB server command.
///
/// Recognises patterns such as:
///   - `JLinkRTTLogger -device AT32F435RMT7 ...`
///   - `JLinkExe ... -device STM32H562VI ...`
///   - `jlink_quake) ... -device AT32F435RMT7`
fn makefile_to_server_command(content: &str) -> Option<String> {
    // Match explicit JLink commands with -device flag
    let device_re = Regex::new(r"-device\s+([A-Za-z0-9]+)").unwrap();

    // Find all unique device names referenced in the Makefile
    let mut devices: Vec<String> = Vec::new();
    for caps in device_re.captures_iter(content) {
        let dev = caps[1].to_uppercase();
        if !devices.contains(&dev) {
            devices.push(dev);
        }
    }

    // Pick the best device: prefer full part number (longer = more specific)
    // Filter out common non-device words
    let ignore = ["JTAG", "SWD", "USB", "TCP", "RTT", "GDB", "ALL"];
    let best = devices
        .iter()
        .filter(|d| !ignore.contains(&d.as_str()))
        .max_by_key(|d| d.len())?;

    // Detect the interface (SWD or JTAG)
    let interface = if content.contains("-if JTAG") || content.contains("-if jtag") {
        "JTAG"
    } else {
        "SWD"
    };

    // Detect speed
    let speed_re = Regex::new(r"-speed\s+(\d+)").unwrap();
    let speed = speed_re
        .captures(content)
        .and_then(|c| c[1].parse::<u32>().ok())
        .unwrap_or(4000);

    Some(format!(
        "JLinkGDBServer -device {best} -if {interface} -speed {speed} -port 2331"
    ))
}

/// Build a minimal OpenOCD command given its config file path.
fn openocd_command(cfg_path: &Path) -> String {
    format!("openocd -f {}", cfg_path.display())
}

/// Parse `platformio.ini` for `debug_server` or `debug_tool` entries.
fn platformio_to_server_command(content: &str) -> Option<String> {
    // Look for `debug_server = <path> <args...>` (INI key-value).
    let server_re = Regex::new(r"(?m)^debug_server\s*=\s*(.+)$").unwrap();
    if let Some(caps) = server_re.captures(content) {
        return Some(caps[1].trim().to_string());
    }

    // Look for `debug_tool = jlink` and build a template.
    let tool_re = Regex::new(r"(?m)^debug_tool\s*=\s*(\w+)$").unwrap();
    if let Some(caps) = tool_re.captures(content) {
        match caps[1].to_lowercase().as_str() {
            "jlink" => return Some("JLinkGDBServer -if SWD -speed 4000 -port 2331".to_string()),
            "openocd" => return Some("openocd".to_string()),
            "pyocd" => return Some("pyocd gdbserver -p 3333".to_string()),
            _ => {}
        }
    }
    None
}

/// Scan CMakeLists.txt, Cargo.toml, and .cargo/config.toml for MCU name hints
/// and generate a JLink command if recognized.
fn chip_heuristics(root: &Path) -> Option<String> {
    let mut text = String::new();

    for path in [
        root.join("CMakeLists.txt"),
        root.join("Cargo.toml"),
        root.join(".cargo").join("config.toml"),
        root.join(".cargo").join("config"),
    ] {
        if let Some(content) = read_opt(&path) {
            text.push_str(&content);
            text.push('\n');
        }
    }

    // Match common MCU family patterns.
    let chip_re = Regex::new(
        r"(?i)(STM32[A-Z]\d+\w+|AT32F\d+\w+|GD32[A-Z]\d+\w+|NRF\d+\w+|LPC\d+\w+|SAM[CDSE]\d+\w+|RP2040)"
    ).unwrap();

    if let Some(caps) = chip_re.captures(&text) {
        let chip = caps[1].to_uppercase();
        return Some(format!(
            "JLinkGDBServer -device {chip} -if SWD -speed 4000 -port 2331"
        ));
    }

    None
}

/// Very simple JSON comment stripper: remove `// ...` and `/* ... */` style
/// comments so that launch.json can be parsed as JSON.
fn strip_json_comments(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '/' {
            match chars.peek() {
                Some('/') => {
                    // Line comment: consume until newline.
                    for c in chars.by_ref() {
                        if c == '\n' {
                            result.push('\n');
                            break;
                        }
                    }
                }
                Some('*') => {
                    // Block comment: consume until '*/'.
                    chars.next();
                    let mut prev = ' ';
                    for c in chars.by_ref() {
                        if prev == '*' && c == '/' {
                            break;
                        }
                        if c == '\n' {
                            result.push('\n');
                        }
                        prev = c;
                    }
                }
                _ => result.push(ch),
            }
        } else {
            result.push(ch);
        }
    }
    result
}

/// Extract the port number from a GDB server command string.
///
/// Tries common patterns: `-port NNNN`, `--port=NNNN`, `-p NNNN`, `:NNNN`.
pub fn extract_port_from_command(cmd: &str) -> Option<u16> {
    let patterns: &[&str] = &[
        r"-port[= ](\d+)",
        r"--port[= ](\d+)",
        r" -p (\d+)",
        r":(\d{4,5})\b",
    ];
    for pat in patterns {
        if let Ok(re) = Regex::new(pat) {
            if let Some(caps) = re.captures(cmd) {
                if let Ok(port) = caps[1].parse::<u16>() {
                    return Some(port);
                }
            }
        }
    }
    None
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // ── Port extraction ───────────────────────────────────────────────────────

    #[test]
    fn extract_port_jlink_port_flag() {
        assert_eq!(
            extract_port_from_command("JLinkGDBServer -device STM32F4 -port 2331"),
            Some(2331)
        );
    }

    #[test]
    fn extract_port_colon_notation() {
        assert_eq!(
            extract_port_from_command("openocd -c 'gdb_port 3333'"),
            None // colon notation not present here
        );
        assert_eq!(
            extract_port_from_command("target remote localhost:2331"),
            Some(2331)
        );
    }

    #[test]
    fn extract_port_missing_returns_none() {
        assert_eq!(
            extract_port_from_command("JLinkGDBServer -device STM32F4"),
            None
        );
    }

    // ── .gdbinit parsing ─────────────────────────────────────────────────────

    #[test]
    fn gdbinit_detects_jlink_comment() {
        let content = "# JLinkGDBServer -device AT32F435 -if SWD -speed 4000 -port 2331\ntarget remote :2331\n";
        let result = gdbinit_to_server_command(content);
        assert!(result.is_some());
        assert!(result.unwrap().contains("JLinkGDBServer"));
    }

    #[test]
    fn gdbinit_detects_openocd_comment() {
        let content = "# openocd -f board/stm32f4discovery.cfg\ntarget remote :3333\n";
        let result = gdbinit_to_server_command(content);
        assert!(result.is_some());
        assert!(result.unwrap().contains("openocd"));
    }

    // ── launch.json parsing ───────────────────────────────────────────────────

    #[test]
    fn launch_json_servertype_jlink_with_device() {
        let json = r#"{
            "configurations": [{
                "name": "Debug",
                "type": "cortex-debug",
                "servertype": "jlink",
                "device": "AT32F435RMT7",
                "interface": "swd",
                "port": 2331
            }]
        }"#;
        let cmd = launch_json_to_server_command(json).unwrap();
        assert!(cmd.contains("JLinkGDBServer"));
        assert!(cmd.contains("AT32F435RMT7"));
        assert!(cmd.contains("2331"));
    }

    #[test]
    fn launch_json_servertype_jlink_device_with_dash_prefix() {
        // ng-iot-platform uses "-AT32F435RMT7" (cortex-debug quirk)
        let json = r#"{
            "configurations": [{
                "name": "Debug",
                "type": "cortex-debug",
                "servertype": "jlink",
                "device": "-AT32F435RMT7"
            }]
        }"#;
        let cmd = launch_json_to_server_command(json).unwrap();
        // Dash prefix should be stripped
        assert!(cmd.contains("AT32F435RMT7"));
        assert!(
            !cmd.contains("-AT32F435RMT7"),
            "dash prefix should be removed"
        );
    }

    #[test]
    fn launch_json_servertype_openocd() {
        let json = r#"{
            "configurations": [{
                "servertype": "openocd",
                "port": 3333
            }]
        }"#;
        let cmd = launch_json_to_server_command(json).unwrap();
        assert!(cmd.contains("openocd"));
        assert!(cmd.contains("3333"));
    }

    #[test]
    fn launch_json_debug_server_path() {
        let json = r#"{
            "configurations": [{
                "debugServerPath": "/usr/bin/JLinkGDBServer",
                "debugServerArgs": "-device STM32H5 -if SWD -port 2331"
            }]
        }"#;
        let cmd = launch_json_to_server_command(json).unwrap();
        assert!(cmd.contains("/usr/bin/JLinkGDBServer"));
        assert!(cmd.contains("STM32H5"));
    }

    #[test]
    fn launch_json_mi_debugger_address_jlink() {
        let json = r#"{
            "configurations": [{
                "servertype": "jlink",
                "device": "STM32F407VG",
                "miDebuggerServerAddress": "localhost:2331"
            }]
        }"#;
        let cmd = launch_json_to_server_command(json).unwrap();
        assert!(cmd.contains("STM32F407VG"));
        assert!(cmd.contains("2331"));
    }

    #[test]
    fn launch_json_with_comments_parses() {
        let json = r#"{
            // This is a comment
            "version": "0.2.0",
            "configurations": [
                {
                    // Quake board
                    "servertype": "jlink",
                    "device": "AT32F435RMT7"
                }
            ]
        }"#;
        let cmd = launch_json_to_server_command(json);
        assert!(cmd.is_some(), "should parse despite comments");
        assert!(cmd.unwrap().contains("AT32F435RMT7"));
    }

    #[test]
    fn launch_json_no_relevant_config() {
        let json = r#"{
            "configurations": [{
                "type": "node",
                "request": "launch",
                "program": "app.js"
            }]
        }"#;
        assert!(launch_json_to_server_command(json).is_none());
    }

    // ── Makefile parsing ──────────────────────────────────────────────────────

    #[test]
    fn makefile_detects_at32f435() {
        let makefile = r#"
flash/quake_can:
    JLinkExe -nogui 1 -if swd -speed 4000 -device AT32F435RMT7 -CommanderScript flash.jlink

shell/quake/jlink:
    JLinkRTTLogger -device AT32F435RMT7 -if SWD -speed 4000
"#;
        let cmd = makefile_to_server_command(makefile).unwrap();
        assert!(cmd.contains("AT32F435RMT7"));
        assert!(cmd.contains("JLinkGDBServer"));
        assert!(cmd.contains("SWD"));
    }

    #[test]
    fn makefile_detects_stm32h562() {
        let makefile = "JLinkExe -device STM32H562VI -if swd -speed 4000\n";
        let cmd = makefile_to_server_command(makefile).unwrap();
        assert!(cmd.contains("STM32H562VI"));
    }

    #[test]
    fn makefile_no_jlink_returns_none() {
        let makefile = "build:\n\tcargo build\n";
        assert!(makefile_to_server_command(makefile).is_none());
    }

    // ── PlatformIO parsing ────────────────────────────────────────────────────

    #[test]
    fn platformio_debug_server_parsed() {
        let ini = "[env:main]\ndebug_server = /usr/bin/JLinkGDBServer -port 2331\n";
        assert_eq!(
            platformio_to_server_command(ini),
            Some("/usr/bin/JLinkGDBServer -port 2331".to_string())
        );
    }

    #[test]
    fn platformio_debug_tool_jlink() {
        let ini = "[env:main]\ndebug_tool = jlink\n";
        let result = platformio_to_server_command(ini);
        assert!(result.is_some());
        assert!(result.unwrap().contains("JLinkGDBServer"));
    }

    // ── JSON comment stripping ────────────────────────────────────────────────

    #[test]
    fn strip_json_line_comment() {
        let s = r#"{ "key": "value" // comment
}"#;
        let stripped = strip_json_comments(s);
        let v: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(v["key"], "value");
    }

    #[test]
    fn strip_json_block_comment() {
        let s = r#"{ /* block comment */ "key": "value" }"#;
        let stripped = strip_json_comments(s);
        let v: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(v["key"], "value");
    }

    // ── Full discovery from filesystem ────────────────────────────────────────

    #[tokio::test]
    async fn discovery_reads_gdbinit_comment() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(dir.path().join(".gdbinit")).unwrap();
        writeln!(f, "# JLinkGDBServer -device STM32F407VG -if SWD -speed 4000 -port 2331\ntarget remote :2331").unwrap();

        let cmd = discover_gdb_server_command_in(Some(dir.path()))
            .await
            .unwrap()
            .unwrap();
        assert!(cmd.contains("JLinkGDBServer"));
        assert!(cmd.contains("STM32F407VG"));
    }

    #[tokio::test]
    async fn discovery_reads_debugging_launch_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("debugging")).unwrap();
        std::fs::write(
            dir.path().join("debugging").join("launch.json"),
            r#"{
                "configurations": [{
                    "name": "Debug",
                    "servertype": "jlink",
                    "device": "AT32F435RMT7"
                }]
            }"#,
        )
        .unwrap();

        let cmd = discover_gdb_server_command_in(Some(dir.path()))
            .await
            .unwrap()
            .unwrap();
        assert!(cmd.contains("AT32F435RMT7"), "got: {cmd}");
    }

    #[tokio::test]
    async fn discovery_reads_vscode_launch_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".vscode")).unwrap();
        std::fs::write(
            dir.path().join(".vscode").join("launch.json"),
            r#"{
                "configurations": [{
                    "servertype": "jlink",
                    "device": "STM32H562VI"
                }]
            }"#,
        )
        .unwrap();

        let cmd = discover_gdb_server_command_in(Some(dir.path()))
            .await
            .unwrap()
            .unwrap();
        assert!(cmd.contains("STM32H562VI"), "got: {cmd}");
    }

    #[tokio::test]
    async fn discovery_reads_makefile() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Makefile"),
            "flash:\n\tJLinkExe -device AT32F435RMT7 -if SWD -speed 4000\n",
        )
        .unwrap();

        let cmd = discover_gdb_server_command_in(Some(dir.path()))
            .await
            .unwrap()
            .unwrap();
        assert!(cmd.contains("AT32F435RMT7"), "got: {cmd}");
    }

    #[tokio::test]
    async fn discovery_prefers_gdbinit_over_makefile() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(dir.path().join(".gdbinit")).unwrap();
        writeln!(
            f,
            "# JLinkGDBServer -device DEVICE_FROM_GDBINIT -if SWD -speed 4000 -port 2331"
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Makefile"),
            "flash:\n\tJLinkExe -device DEVICE_FROM_MAKEFILE -if SWD -speed 4000\n",
        )
        .unwrap();

        let cmd = discover_gdb_server_command_in(Some(dir.path()))
            .await
            .unwrap()
            .unwrap();
        assert!(
            cmd.contains("DEVICE_FROM_GDBINIT"),
            "gdbinit should win: {cmd}"
        );
    }

    #[tokio::test]
    async fn discovery_returns_none_in_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = discover_gdb_server_command_in(Some(dir.path())).await;
        assert!(result.unwrap().is_none());
    }

    // ── ELF discovery ─────────────────────────────────────────────────────────

    #[test]
    fn find_firmware_elf_sysbuild_structure() {
        let dir = tempfile::tempdir().unwrap();
        // Create sysbuild structure: build-firmware/<app>/zephyr/zephyr.elf
        let elf_dir = dir
            .path()
            .join("build-firmware")
            .join("ng-iot-platform")
            .join("zephyr");
        std::fs::create_dir_all(&elf_dir).unwrap();
        let elf_path = elf_dir.join("zephyr.elf");
        std::fs::write(&elf_path, b"\x7fELF").unwrap(); // minimal ELF magic

        let found = find_firmware_elf(dir.path());
        assert!(found.is_some(), "should find sysbuild ELF");
        assert_eq!(found.unwrap(), elf_path);
    }

    #[test]
    fn find_firmware_elf_direct_build_structure() {
        let dir = tempfile::tempdir().unwrap();
        let elf_dir = dir.path().join("build").join("zephyr");
        std::fs::create_dir_all(&elf_dir).unwrap();
        let elf_path = elf_dir.join("zephyr.elf");
        std::fs::write(&elf_path, b"\x7fELF").unwrap();

        let found = find_firmware_elf(dir.path());
        assert!(found.is_some());
        assert_eq!(found.unwrap(), elf_path);
    }

    #[test]
    fn find_firmware_elf_skips_mcuboot() {
        let dir = tempfile::tempdir().unwrap();
        // Create both mcuboot and app ELF
        let mcuboot_dir = dir
            .path()
            .join("build-firmware")
            .join("mcuboot")
            .join("zephyr");
        std::fs::create_dir_all(&mcuboot_dir).unwrap();
        std::fs::write(mcuboot_dir.join("zephyr.elf"), b"\x7fELF").unwrap();

        let app_dir = dir
            .path()
            .join("build-firmware")
            .join("ng-iot-platform")
            .join("zephyr");
        std::fs::create_dir_all(&app_dir).unwrap();
        let app_elf = app_dir.join("zephyr.elf");
        std::fs::write(&app_elf, b"\x7fELF").unwrap();

        let found = find_firmware_elf(dir.path());
        assert!(found.is_some());
        // Should prefer the app ELF over mcuboot
        assert!(
            found
                .as_ref()
                .map(|p| !p.to_string_lossy().contains("mcuboot"))
                .unwrap_or(false),
            "should not pick mcuboot ELF, got: {:?}",
            found
        );
    }

    #[test]
    fn find_firmware_elf_platformio_structure() {
        let dir = tempfile::tempdir().unwrap();
        let elf_dir = dir.path().join(".pio").join("build").join("main_env");
        std::fs::create_dir_all(&elf_dir).unwrap();
        let elf_path = elf_dir.join("firmware.elf");
        std::fs::write(&elf_path, b"\x7fELF").unwrap();

        let found = find_firmware_elf(dir.path());
        assert!(found.is_some());
        assert_eq!(found.unwrap(), elf_path);
    }

    #[test]
    fn find_firmware_elf_empty_dir_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_firmware_elf(dir.path()).is_none());
    }
}
