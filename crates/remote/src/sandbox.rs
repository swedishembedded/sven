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
//! The kernel also refuses every TCP bind and every TCP connect except to the
//! ports the host needs (name resolution, the web tools, the server it
//! serves), so a tool that is subverted cannot reach a service on the
//! machine's loopback or private network, and refuses signals to, and
//! abstract unix sockets of, processes outside the host (its siblings).
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

/// The TCP ports a host always may connect to: HTTPS and HTTP for the web
/// tools, and DNS (a resolver falls back to TCP for large answers).
const WEB_PORTS: &[u16] = &[443, 80, 53];

/// The Landlock ABI the rule set needs: 6 adds TCP bind/connect control
/// (4) and signal and abstract unix socket scopes (6) to the file system
/// rules. A kernel that enforces less fails [`confine`].
#[cfg(target_os = "linux")]
const ABI_REQUIRED: landlock::ABI = landlock::ABI::V6;

/// Confines this process, from now on, to `sandbox`: read, write and create
/// anything inside it; read the system paths in `SYSTEM_READ_ONLY`; connect
/// over TCP only to [`WEB_PORTS`] and `server_ports` (the server it serves),
/// never bind; signal and reach by abstract unix socket only itself.
/// Irreversible.
///
/// # Errors
///
/// The kernel cannot enforce all of it (no Landlock, or too old a one), or
/// the rules cannot be built. The caller must not go on unconfined.
#[cfg(target_os = "linux")]
pub fn confine(sandbox: &Path, server_ports: &[u16]) -> anyhow::Result<()> {
    use landlock::{
        path_beneath_rules, Access, AccessFs, AccessNet, CompatLevel, Compatible, NetPort, Ruleset,
        RulesetAttr, RulesetCreatedAttr, Scope,
    };
    let abi = ABI_REQUIRED;
    anyhow::ensure!(
        sandbox.is_dir(),
        "the sandbox {} is not a directory",
        sandbox.display()
    );
    let status = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(abi))?
        .handle_access(AccessNet::from_all(abi))?
        .scope(Scope::from_all(abi))?
        .create()?
        .add_rules(path_beneath_rules([sandbox], AccessFs::from_all(abi)))?
        .add_rules(path_beneath_rules(
            SYSTEM_READ_ONLY,
            AccessFs::from_read(abi),
        ))?
        .add_rules(WEB_PORTS.iter().chain(server_ports).map(|port| {
            Ok::<_, landlock::RulesetError>(NetPort::new(*port, AccessNet::ConnectTcp))
        }))?
        .restrict_self()?;
    require_full(status.ruleset, status.no_new_privs)
}

/// A partly enforced rule set is no confinement: the host must not run.
#[cfg(target_os = "linux")]
fn require_full(ruleset: landlock::RulesetStatus, no_new_privs: bool) -> anyhow::Result<()> {
    anyhow::ensure!(
        ruleset == landlock::RulesetStatus::FullyEnforced && no_new_privs,
        "the kernel did not confine this host fully ({ruleset:?}); a hosted host does not run unconfined \
         (it needs Linux 6.12 or newer: Landlock ABI 6)"
    );
    Ok(())
}

/// Elsewhere there is no confinement this host trusts.
///
/// # Errors
///
/// Always.
#[cfg(not(target_os = "linux"))]
pub fn confine(_sandbox: &Path, _server_ports: &[u16]) -> anyhow::Result<()> {
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
        super::confine(sandbox, &[]).expect("this kernel confines");
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

    #[test]
    fn a_partly_enforced_rule_set_is_refused() {
        use landlock::RulesetStatus::{FullyEnforced, NotEnforced, PartiallyEnforced};
        assert!(super::require_full(FullyEnforced, true).is_ok());
        assert!(super::require_full(PartiallyEnforced, true).is_err());
        assert!(super::require_full(NotEnforced, true).is_err());
        assert!(super::require_full(FullyEnforced, false).is_err());
    }

    /// The network and signal rules, on a thread of their own like the file
    /// ones: a listener the host may not reach, one it was told of, no bind,
    /// and no signal to a process outside the host.
    #[test]
    fn a_confined_host_reaches_only_its_ports_and_only_itself() {
        use std::net::{TcpListener, TcpStream};
        use std::os::linux::net::SocketAddrExt;
        use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
        let sandbox = tempfile::tempdir().unwrap();
        let blocked = TcpListener::bind("127.0.0.1:0").unwrap();
        let allowed = TcpListener::bind("127.0.0.1:0").unwrap();
        let (blocked_port, allowed_port) = (
            blocked.local_addr().unwrap().port(),
            allowed.local_addr().unwrap().port(),
        );
        let mut sibling = Command::new("sleep").arg("30").spawn().unwrap();
        let sibling_pid = i32::try_from(sibling.id()).unwrap();
        let abstract_name = SocketAddr::from_abstract_name(b"sven-sandbox-test").unwrap();
        let _abstract_listener = UnixListener::bind_addr(&abstract_name).unwrap();
        let dir = sandbox.path().to_path_buf();
        let confined = std::thread::spawn(move || {
            super::confine(&dir, &[allowed_port]).expect("this kernel confines");
            let denied = |error: std::io::Error, what: &str| {
                assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied, "{what}");
            };
            denied(
                TcpStream::connect(("127.0.0.1", blocked_port)).expect_err("blocked port"),
                "connecting to a local service",
            );
            TcpStream::connect(("127.0.0.1", allowed_port)).expect("the server's port");
            denied(
                TcpListener::bind("127.0.0.1:0").expect_err("bind"),
                "listening",
            );
            // A process started before the confinement is outside this host.
            // SAFETY: signal 0 only checks whether a signal could be sent.
            let sent = unsafe { libc::kill(sibling_pid, 0) };
            assert_eq!(sent, -1, "signalling a process outside the host");
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EPERM)
            );
            denied(
                UnixStream::connect_addr(&abstract_name).expect_err("abstract socket"),
                "connecting to a process's abstract unix socket",
            );
        })
        .join();
        sibling.kill().unwrap();
        sibling.wait().unwrap();
        confined.expect("the confined side holds");
    }
}
