// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Reading a secret from stdin, so it never appears on a command line or in
//! the shell's history.

use std::io::{BufRead, IsTerminal, Write};

/// One line from stdin; on a terminal, prompted on stderr and not echoed.
pub(crate) fn read_secret(prompt: &str) -> anyhow::Result<String> {
    let stdin = std::io::stdin();
    let interactive = stdin.is_terminal();
    if interactive {
        eprint!("{prompt}");
        std::io::stderr().flush()?;
    }
    let _echo = interactive.then(EchoOff::new).flatten();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    if interactive {
        eprintln!();
    }
    Ok(line)
}

/// Terminal echo turned off for as long as it lives.
struct EchoOff {
    #[cfg(unix)]
    saved: libc::termios,
}

impl EchoOff {
    #[cfg(unix)]
    fn new() -> Option<Self> {
        // SAFETY: tcgetattr/tcsetattr on stdin with a zeroed termios that
        // tcgetattr fills before it is read.
        unsafe {
            let mut saved: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut saved) != 0 {
                return None;
            }
            let mut quiet = saved;
            quiet.c_lflag &= !libc::ECHO;
            (libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &quiet) == 0)
                .then_some(Self { saved })
        }
    }

    #[cfg(not(unix))]
    fn new() -> Option<Self> {
        None
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: restores the attributes tcgetattr returned.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.saved);
        }
    }
}
