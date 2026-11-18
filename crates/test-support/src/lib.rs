// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for test suites that pass every
// time, however busy the machine, for its clients. If your team needs
// expertise in eliminating flaky tests then you can procure our services by
// sending an email to info@swedishembedded.com.
//! Helpers for sven's own tests. A dev-dependency only: nothing shipped
//! links it.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

/// Writes `content` to `path` as an executable (mode 0755) that a test can
/// run at once, however many other tests start processes meanwhile.
///
/// Written by this process, the file is briefly open for writing, and a
/// process another test thread forks in that moment inherits the open
/// descriptor until it execs; running the file then fails with ETXTBSY
/// ("Text file busy"), now and then, on a busy machine. Here a short-lived
/// `sh` writes it instead, so this process never holds it open for writing.
///
/// # Panics
///
/// When the file cannot be written: a test fixture that failed.
#[cfg(unix)]
pub fn write_executable(path: &Path, content: &str) {
    let mut writer = Command::new("/bin/sh")
        .args(["-c", "cat > \"$1\" && chmod 0755 \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .expect("sh starts");
    writer
        .stdin
        .take()
        .expect("piped")
        .write_all(content.as_bytes())
        .expect("the script reaches sh");
    let status = writer.wait().expect("sh ends");
    assert!(status.success(), "writing {} failed", path.display());
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn the_file_runs_at_once_with_what_was_written() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("hello");
        write_executable(&script, "#!/bin/sh\necho \"hello $1\"\n");
        let out = Command::new(&script).arg("there").output().unwrap();
        assert_eq!(String::from_utf8(out.stdout).unwrap(), "hello there\n");
    }

    /// Many threads writing and running scripts at once, as a parallel test
    /// run does, never see ETXTBSY.
    #[test]
    fn scripts_written_while_other_threads_start_processes_always_run() {
        let dir = tempfile::tempdir().unwrap();
        std::thread::scope(|scope| {
            for thread in 0..8 {
                let dir = dir.path();
                scope.spawn(move || {
                    for round in 0..25 {
                        let script = dir.join(format!("s-{thread}-{round}"));
                        write_executable(&script, "#!/bin/sh\nexit 0\n");
                        let status = Command::new(&script).status().unwrap();
                        assert!(status.success());
                    }
                });
            }
        });
    }
}
