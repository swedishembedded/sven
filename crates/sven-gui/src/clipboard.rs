// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Clipboard operations via platform-native programs or commands.

/// Copy `text` to the system clipboard.
///
/// Platform dispatch:
/// - Linux:   xclip → xsel (X11/Wayland bridges)
/// - macOS:   pbcopy
/// - Windows: clip.exe (built-in since Windows Vista)
///
/// Logs a warning if no clipboard program is available.
pub fn copy_to_clipboard(text: &str) {
    use std::io::Write;
    use std::process::{Command, Stdio};

    #[cfg(target_os = "macos")]
    let programs: &[(&str, &[&str])] = &[("pbcopy", &[])];

    #[cfg(target_os = "windows")]
    let programs: &[(&str, &[&str])] = &[("clip", &[])];

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let programs: &[(&str, &[&str])] = &[
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
        ("wl-copy", &[]),
    ];

    for (prog, args) in programs {
        if let Ok(mut child) = Command::new(prog).args(*args).stdin(Stdio::piped()).spawn() {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
            return;
        }
    }

    #[cfg(target_os = "macos")]
    tracing::warn!("clipboard: pbcopy not available");
    #[cfg(target_os = "windows")]
    tracing::warn!("clipboard: clip.exe not available");
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    tracing::warn!("clipboard: no clipboard program available (xclip/xsel/wl-copy)");
}
