// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Clipboard operations via xclip/xsel/pbcopy on the host system.

/// Copy `text` to the system clipboard using a subprocess (xclip, xsel, pbcopy).
/// Logs a warning on failure.
pub fn copy_to_clipboard(text: &str) {
    use std::io::Write;
    use std::process::{Command, Stdio};

    // Try xclip, then xsel, then pbcopy (macOS).
    let programs = [
        ("xclip", &["-selection", "clipboard"][..]),
        ("xsel", &["--clipboard", "--input"][..]),
        ("pbcopy", &[][..]),
    ];

    for (prog, args) in &programs {
        if let Ok(mut child) = Command::new(prog).args(*args).stdin(Stdio::piped()).spawn() {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
            return;
        }
    }

    tracing::warn!("clipboard: no clipboard program available (xclip/xsel/pbcopy)");
}
