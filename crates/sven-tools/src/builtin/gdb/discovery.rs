/// Agent intelligence for discovering the GDB server command from project files.
///
/// The discovery strategy tries each source in order, stopping at the first
/// match that yields a usable command string.
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
pub async fn discover_gdb_server_command_in(base: Option<&std::path::Path>) -> Result<Option<String>> {
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
    let launch_json = root.join(".vscode").join("launch.json");
    if let Some(content) = read_opt(&launch_json) {
        if let Some(cmd) = vscode_launch_to_server_command(&content) {
            return Ok(Some(cmd));
        }
    }

    // ── 3. openocd.cfg ───────────────────────────────────────────────────────
    for candidate in [root.join("openocd.cfg"), cwd.join("openocd.cfg")] {
        if candidate.exists() {
            return Ok(Some(openocd_command(&candidate)));
        }
    }

    // ── 4. platformio.ini ────────────────────────────────────────────────────
    for candidate in [root.join("platformio.ini"), cwd.join("platformio.ini")] {
        if let Some(content) = read_opt(&candidate) {
            if let Some(cmd) = platformio_to_server_command(&content) {
                return Ok(Some(cmd));
            }
        }
    }

    // ── 5. Device/chip heuristics from CMakeLists / Cargo.toml ───────────────
    if let Some(cmd) = chip_heuristics(&root) {
        return Ok(Some(cmd));
    }

    Ok(None)
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
    let server_re = Regex::new(
        r"(?i)(?:#\s*)?(JLinkGDBServer|JLinkGDBServerCL|openocd|pyocd)[^\n]*"
    ).unwrap();
    if let Some(m) = server_re.find(content) {
        let cmd = m.as_str().trim_start_matches('#').trim().to_string();
        if !cmd.is_empty() {
            return Some(cmd);
        }
    }

    // Fall back: if there is a `target remote :PORT` line we can infer
    // JLinkGDBServer is commonly used and build a default command.
    let remote_re = Regex::new(r"target\s+(?:extended-)?remote\s+(?:[a-zA-Z0-9.]+:)?(\d+)").unwrap();
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

/// Parse `.vscode/launch.json` for debugServerPath + debugServerArgs, or
/// miDebuggerServerAddress to infer a server command.
fn vscode_launch_to_server_command(content: &str) -> Option<String> {
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

        // miDebuggerServerAddress gives us host:port, but not the server binary.
        // Attempt to infer from the GDB server path field commonly used with
        // cortex-debug extension: `servertype`.
        let server_type = cfg
            .get("servertype")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if let Some(addr) = cfg.get("miDebuggerServerAddress").and_then(|v| v.as_str()) {
            let port = addr.split(':').last().unwrap_or("3333");
            match server_type.to_lowercase().as_str() {
                "jlink" => {
                    let device = cfg
                        .get("device")
                        .and_then(|v| v.as_str())
                        .unwrap_or("cortex-m4");
                    return Some(format!(
                        "JLinkGDBServer -device {device} -if SWD -speed 4000 -port {port}"
                    ));
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
    }
    None
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
            "jlink" => return Some(
                "JLinkGDBServer -if SWD -speed 4000 -port 2331".to_string()
            ),
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
                        if c == '\n' { result.push('\n'); break; }
                    }
                }
                Some('*') => {
                    // Block comment: consume until '*/'.
                    chars.next();
                    let mut prev = ' ';
                    for c in chars.by_ref() {
                        if prev == '*' && c == '/' { break; }
                        if c == '\n' { result.push('\n'); }
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
        assert_eq!(extract_port_from_command("JLinkGDBServer -device STM32F4"), None);
    }

    #[test]
    fn gdbinit_detects_jlink_comment() {
        let content = "# JLinkGDBServer -device AT32F435 -if SWD -speed 4000 -port 2331\ntarget remote :2331\n";
        let result = gdbinit_to_server_command(content);
        assert!(result.is_some());
        assert!(result.unwrap().contains("JLinkGDBServer"));
    }

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

    #[test]
    fn strip_json_line_comment() {
        let s = r#"{ "key": "value" // comment
}"#;
        let stripped = strip_json_comments(s);
        let v: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(v["key"], "value");
    }
}
