// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Reading a one-time code from the person at the terminal without showing
//! it: as they paste or type, the line shows its first and last
//! [`SHOWN_AT_EACH_END`] characters and masks the rest, the same way the
//! server's page shows it. Elsewhere than on a Unix terminal (input piped
//! in, or another system) the line is read as it is.

use std::io::{BufRead, Write};

/// Characters shown at each end of a code being entered.
pub(super) const SHOWN_AT_EACH_END: usize = 10;

/// `code` as the terminal shows it: its first and last
/// [`SHOWN_AT_EACH_END`] characters with masks between them, or all masks
/// while it is too short for that to hide anything.
pub(super) fn masked(code: &str) -> String {
    let chars: Vec<char> = code.chars().collect();
    if chars.len() <= SHOWN_AT_EACH_END * 2 {
        return "*".repeat(chars.len());
    }
    let start: String = chars[..SHOWN_AT_EACH_END].iter().collect();
    let end: String = chars[chars.len() - SHOWN_AT_EACH_END..].iter().collect();
    format!(
        "{start}{}{end}",
        "*".repeat(chars.len() - SHOWN_AT_EACH_END * 2)
    )
}

/// One line from standard input, trimmed; masked as it is entered when
/// standard input is a terminal.
///
/// # Errors
///
/// Standard input ended before a line, or the person pressed Ctrl-C.
pub(super) fn read_code() -> anyhow::Result<String> {
    #[cfg(unix)]
    if let Some(echo_off) = EchoOff::on_stdin() {
        return read_masked(&echo_off);
    }
    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line)?;
    anyhow::ensure!(read > 0, "no code was entered (end of input)");
    Ok(line.trim().to_string())
}

/// Reads keys one at a time and redraws the masked code after each.
#[cfg(unix)]
fn read_masked(_echo_off: &EchoOff) -> anyhow::Result<String> {
    use std::io::Read;
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout();
    let mut code = String::new();
    let mut byte = [0u8; 1];
    loop {
        anyhow::ensure!(
            stdin.read(&mut byte)? > 0,
            "no code was entered (end of input)"
        );
        match byte[0] {
            b'\r' | b'\n' => break,
            // Ctrl-C and Ctrl-D: the terminal does not turn them into a
            // signal or end of input while it is in this mode.
            0x03 => {
                writeln!(stdout)?;
                anyhow::bail!("sign-in cancelled");
            }
            0x04 if code.is_empty() => anyhow::bail!("no code was entered (end of input)"),
            0x7f | 0x08 => {
                code.pop();
            }
            // Ctrl-U clears the line.
            0x15 => code.clear(),
            // A code is URL-safe characters; anything else (escape
            // sequences of a bracketed paste, stray control keys) is not
            // part of it.
            b if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' => code.push(char::from(b)),
            _ => {}
        }
        write!(stdout, "\r\x1b[2K{}", masked(&code))?;
        stdout.flush()?;
    }
    writeln!(stdout)?;
    Ok(code)
}

/// Standard input with echo and line editing off while this lives, so keys
/// arrive one at a time and are not shown; restored when dropped, Ctrl-C
/// and errors included.
#[cfg(unix)]
struct EchoOff {
    saved: libc::termios,
}

#[cfg(unix)]
impl EchoOff {
    /// None when standard input is not a terminal.
    fn on_stdin() -> Option<Self> {
        // SAFETY: tcgetattr writes a termios for a valid descriptor and
        // fails, writing nothing used, for any other.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut saved) } != 0 {
            return None;
        }
        let mut raw = saved;
        raw.c_lflag &= !(libc::ECHO | libc::ICANON | libc::ISIG);
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        // SAFETY: as above, with a termios read from the same descriptor.
        if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) } != 0 {
            return None;
        }
        Some(Self { saved })
    }
}

#[cfg(unix)]
impl Drop for EchoOff {
    fn drop(&mut self) {
        // SAFETY: restores the termios this read from the same descriptor.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.saved);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::masked;

    #[test]
    fn a_code_shows_its_first_and_last_ten_characters() {
        let code = "yuk8AICTa5bgRTSCm2SEZr0sLtpYO_KiHG2verSGIgw";
        let shown = masked(code);
        assert_eq!(shown.len(), code.len());
        assert!(
            shown.starts_with("yuk8AICTa5") && shown.ends_with("G2verSGIgw"),
            "{shown}"
        );
        assert!(!shown.contains("SEZr0sLtpYO"), "{shown}");
    }

    #[test]
    fn a_code_too_short_to_show_both_ends_of_is_all_masks() {
        assert_eq!(masked("abc"), "***");
        assert_eq!(masked(&"x".repeat(20)), "*".repeat(20));
    }
}
