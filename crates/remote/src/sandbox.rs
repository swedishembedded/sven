// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Confining a hosted host to its own directory (`sven rc --hosted`).
//!
//! A server runs one hosted host per account, several on one machine, each
//! answering that account's conversations. The tools a hosted session has
//! never touch the machine (see `HOSTED_TOOLS`); this is the second wall,
//! for the case that one of them, or sven itself, does: the kernel refuses
//! every access to the file system outside the host's sandbox directory,
//! except reading what any program needs to run and reach the network (TLS
//! roots, name resolution, time zones, system libraries) - none of which
//! holds anyone's data.
//!
//! Linux Landlock does the confining, for the calling thread and everything
//! it starts afterwards, so [`confine`] must run while the process has one
//! thread, before any runtime starts. It fails - and the host must not run -
//! unless the kernel enforces the whole rule set; a partly confined host
//! is not offered. On other systems a hosted host does not start.

use std::path::Path;

/// What a confined host may read, besides its sandbox: no one's data, only
/// what a program needs to run and reach the network. Missing ones are
/// skipped.
const SYSTEM_READ_ONLY: &[&str] = &[
    "/etc/ssl",
    "/etc/pki",
    "/etc/ca-certificates",
    "/usr/share/ca-certificates",
    "/etc/resolv.conf",
    "/etc/hosts",
    "/etc/nsswitch.conf",
    "/etc/gai.conf",
    "/etc/host.conf",
    "/etc/localtime",
    "/usr/share/zoneinfo",
    "/lib",
    "/lib64",
    "/usr/lib",
    "/usr/lib64",
    "/dev/null",
    "/dev/urandom",
    "/dev/random",
];

/// Confines this process, from now on, to `sandbox`: read, write and create
/// anything inside it; read the system paths in `SYSTEM_READ_ONLY`; nothing else. Irreversible.
///
/// # Errors
///
/// The kernel cannot enforce all of it (no Landlock, or too old a one), or
/// the rules cannot be built. The caller must not go on unconfined.
#[cfg(target_os = "linux")]
pub fn confine(sandbox: &Path) -> anyhow::Result<()> {
    use landlock::{
        path_beneath_rules, Access, AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr,
        RulesetCreatedAttr, RulesetStatus, ABI,
    };
    let abi = ABI::V3;
    anyhow::ensure!(
        sandbox.is_dir(),
        "the sandbox {} is not a directory",
        sandbox.display()
    );
    let status = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(abi))?
        .create()?
        .add_rules(path_beneath_rules([sandbox], AccessFs::from_all(abi)))?
        .add_rules(path_beneath_rules(
            SYSTEM_READ_ONLY,
            AccessFs::from_read(abi),
        ))?
        .restrict_self()?;
    anyhow::ensure!(
        status.ruleset == RulesetStatus::FullyEnforced && status.no_new_privs,
        "the kernel did not confine this host fully ({:?}); a hosted host does not run unconfined",
        status.ruleset
    );
    Ok(())
}

/// Elsewhere there is no confinement this host trusts.
///
/// # Errors
///
/// Always.
#[cfg(not(target_os = "linux"))]
pub fn confine(_sandbox: &Path) -> anyhow::Result<()> {
    anyhow::bail!("a hosted host runs only on Linux, which can confine it to its sandbox")
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::path::Path;
    use std::process::Command;

    /// Landlock confines the thread that asks and what it starts, so the
    /// confined side runs on a thread of its own and the rest of the test
    /// process stays as it was.
    #[test]
    fn a_confined_process_reaches_its_sandbox_and_nothing_else_of_anyones() {
        let root = tempfile::tempdir().unwrap();
        let sandbox = root.path().join("account-a");
        let neighbour = root.path().join("account-b");
        std::fs::create_dir_all(&sandbox).unwrap();
        std::fs::create_dir_all(&neighbour).unwrap();
        let outside = neighbour.join("secret.txt");
        std::fs::write(&outside, "account b's data").unwrap();

        let (inside, beyond) = (sandbox.clone(), outside.clone());
        std::thread::spawn(move || confined(&inside, &beyond))
            .join()
            .expect("the confined side holds");
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "account b's data",
            "the neighbour's file is untouched"
        );
    }

    fn confined(sandbox: &Path, outside: &Path) {
        super::confine(sandbox).expect("this kernel confines");
        let denied = |result: std::io::Result<()>, what: &str| {
            let error = result.expect_err(what);
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied,
                "{what}: {error}"
            );
        };

        // Its own directory: everything.
        let mine = sandbox.join("work").join("notes.txt");
        std::fs::create_dir_all(mine.parent().unwrap()).unwrap();
        std::fs::write(&mine, "mine").unwrap();
        assert_eq!(std::fs::read_to_string(&mine).unwrap(), "mine");
        std::fs::remove_file(&mine).unwrap();

        // Another account's directory, and anything else: nothing.
        denied(
            std::fs::read_to_string(outside).map(drop),
            "reading another account's file",
        );
        denied(
            std::fs::write(outside, "x"),
            "writing another account's file",
        );
        denied(
            std::fs::read_dir(outside.parent().unwrap()).map(drop),
            "listing another account's directory",
        );
        denied(
            std::fs::read_dir(sandbox.parent().unwrap()).map(drop),
            "listing the directory above its own",
        );
        if let Some(home) = dirs::home_dir() {
            denied(
                std::fs::read_dir(home).map(drop),
                "listing the home directory",
            );
        }
        denied(
            std::fs::read_to_string("/etc/passwd").map(drop),
            "reading /etc/passwd",
        );
        denied(
            std::fs::write(std::env::temp_dir().join("sven-sandbox-escape"), "x"),
            "writing the shared temp directory",
        );

        // A program it starts is confined too.
        match Command::new("/bin/cat").arg(outside).output() {
            Ok(output) => assert!(
                !output.status.success(),
                "a program it started read another account's file"
            ),
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied),
        }
    }
}
