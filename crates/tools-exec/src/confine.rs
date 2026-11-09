// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Linux Landlock confinement, shared by the two places that need it: a
//! whole process (`sven rc --hosted`, in `sven-remote`) and the commands an
//! agent's tools start inside a trusted worker (`sven harness work`).
//!
//! # Confining a command
//!
//! A [`CommandConfinement`] names what a command may reach: read, write and
//! execute in `writable` (the workspace and a private tmp); read and
//! execute in `read_exec` (the directories of the programs it may run) and in
//! the system set a program needs to start; nothing else. Every TCP connect
//! and bind is refused, and so are signals to, and abstract unix sockets of,
//! processes outside the command's own tree. The worker that starts it stays
//! outside the domain, so its files, credentials and memory
//! ([`keep_memory_private`]) are out of the command's reach.
//!
//! The rule set is built in the parent, before the command is spawned: that
//! allocates and opens files, which a child of a multi-threaded process may
//! not do between `fork` and `exec`. The child only calls two
//! async-signal-safe system calls on the already built ruleset
//! (`prctl(PR_SET_NO_NEW_PRIVS)` and `landlock_restrict_self`); the ruleset
//! descriptor is close-on-exec, so the command never holds it.
//!
//! A kernel that cannot enforce the whole rule set (Landlock ABI 6, Linux
//! 6.12) makes [`CommandConfinement::restrict`] fail, and the command is not
//! started: a partly confined command is not offered. Other systems have no
//! such confinement and fail the same way.
//!
//! Swedish Embedded AB implements solutions for running untrusted agent
//! commands next to trusted workers for its clients. If your team needs
//! expertise in kernel-enforced sandboxing then you can procure our services
//! by sending an email to info@swedishembedded.com.

#[cfg(target_os = "linux")]
pub use linux::{require_abi, Prepared, ABI_REQUIRED, SYSTEM_READ_ONLY};

use std::path::PathBuf;

/// Ends `command`'s process when the thread that starts it ends, however it
/// ends: a sub-agent never outlives the session that ran it, also when that
/// session's host is killed outright. The thread that spawns must live as
/// long as the child is wanted (`task` spawns from the thread that drives
/// the child to its end). Linux only; elsewhere nothing changes.
pub fn die_with_parent(command: &mut tokio::process::Command) {
    #[cfg(target_os = "linux")]
    {
        // Read before the fork; the child compares, so a parent that died
        // between the fork and the call is noticed.
        let parent = std::process::id();
        // SAFETY: the closure runs in the forked child of a multi-threaded
        // parent, so it may only call async-signal-safe functions: two raw
        // system calls, and an error built from errno or a constant.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if u32::try_from(libc::getppid()).ok() != Some(parent) {
                    return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
                }
                Ok(())
            });
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = command;
}

/// Keeps this process's memory, which holds its sign-in key, from other
/// processes of the same user: its tool children (a shell, gdb, an MCP
/// server) and anything they start. Without it, where the kernel lets a
/// process trace its parent (`kernel.yama.ptrace_scope` 0), a child can
/// read the key from `/proc/<parent>/mem`. Children start dumpable again
/// (execve resets it), so they are unaffected. No-op off Linux.
///
/// # Errors
///
/// The kernel refused the setting.
pub fn keep_memory_private() -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: prctl(PR_SET_DUMPABLE, 0) takes no pointers.
        if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

/// What the commands of an agent's tools may reach. Set by sven itself for a
/// served run, never read from a configuration file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandConfinement {
    /// Directories a command may read, write, create in and execute from:
    /// the workspace, the private tmp, a scratch directory for the agent.
    pub writable: Vec<PathBuf>,
    /// Directories a command may only read and execute from, besides the
    /// system set: the package's `bin` (and whatever its programs read
    /// there) and the toolchain directories of the `PATH`. Missing ones are
    /// skipped.
    pub read_exec: Vec<PathBuf>,
    /// The `TMPDIR` of every command: one of `writable`, so `mktemp` and
    /// the like work without the shared `/tmp`.
    pub tmpdir: Option<PathBuf>,
}

#[cfg(target_os = "linux")]
mod linux {
    use super::CommandConfinement;
    use std::os::fd::{AsRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    /// What a confined process may read, besides its own directory: no one's
    /// data, only what a program needs to run and reach the network, and the
    /// skills installed for everyone on the machine (the system skill
    /// directories `sven-workspace` discovers first). Missing ones are skipped.
    pub const SYSTEM_READ_ONLY: &[&str] = &[
        "/usr/share/sven/skills",
        "/usr/local/share/sven/skills",
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

    /// What any command needs, beyond `SYSTEM_READ_ONLY`, to find and start
    /// a program, resolve a user and run the usual tools: the system's
    /// program and library directories, the loader's cache, the alternatives
    /// links, user names, and the few `/proc` files that report the machine
    /// (never a process: `/proc/<pid>` stays out of reach).
    const SYSTEM_EXEC: &[&str] = &[
        "/usr",
        "/bin",
        "/sbin",
        "/lib32",
        "/etc/ld.so.cache",
        "/etc/ld.so.conf",
        "/etc/ld.so.conf.d",
        "/etc/alternatives",
        "/etc/passwd",
        "/etc/group",
        "/etc/gitconfig",
        "/etc/terminfo",
        "/dev/zero",
        "/proc/cpuinfo",
        "/proc/meminfo",
        "/proc/loadavg",
        "/proc/version",
    ];

    /// Device files a command may also write.
    const DEVICE_WRITABLE: &[&str] = &["/dev/null"];

    /// The Landlock ABI the rule set needs: 6 adds TCP bind/connect control
    /// (4) and signal and abstract unix socket scopes (6) to the file system
    /// rules. A kernel that enforces less cannot confine.
    pub const ABI_REQUIRED: landlock::ABI = landlock::ABI::V6;

    const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;

    /// Whether a kernel of Landlock ABI `version` (negative: Landlock is
    /// absent or disabled) enforces the whole rule set.
    ///
    /// # Errors
    ///
    /// The kernel is too old; the message names what is needed.
    pub fn require_abi(version: i64) -> Result<(), String> {
        if version < i64::from(ABI_REQUIRED as i32) {
            return Err(format!(
                "this kernel enforces Landlock ABI {version}, not the whole rule set; commands \
                 are not run unconfined (it needs Linux 6.12 or newer: Landlock ABI 6)"
            ));
        }
        Ok(())
    }

    fn kernel_abi() -> i64 {
        // SAFETY: with a null attribute, size 0 and this flag the call only
        // reports the ABI version; it touches no memory.
        unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::null::<libc::c_void>(),
                0_usize,
                LANDLOCK_CREATE_RULESET_VERSION,
            )
        }
    }

    /// A built rule set, ready to restrict one command.
    pub struct Prepared {
        ruleset: OwnedFd,
    }

    impl Prepared {
        /// Makes `command` restrict itself with this rule set between
        /// `fork` and `exec`.
        pub fn apply(self, command: &mut Command) {
            let ruleset = self.ruleset;
            // SAFETY: the closure runs in the forked child of a
            // multi-threaded parent, so it may only call async-signal-safe
            // functions and must not allocate or lock. It makes two raw
            // system calls on an already open descriptor and builds an
            // error only from errno (`last_os_error` does not allocate).
            // The descriptor is close-on-exec: the command never holds it.
            unsafe {
                command.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::syscall(libc::SYS_landlock_restrict_self, ruleset.as_raw_fd(), 0_u32)
                        != 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
    }

    impl CommandConfinement {
        /// Whether this kernel can confine a command at all.
        ///
        /// # Errors
        ///
        /// The kernel does not enforce the whole rule set. A run that needs
        /// confinement must not start.
        pub fn require_kernel_support() -> Result<(), String> {
            require_abi(kernel_abi())
        }

        /// Builds the rule set in the calling thread; see the module docs
        /// for why not in the child.
        ///
        /// # Errors
        ///
        /// The kernel cannot enforce all of it, a `writable` directory does
        /// not exist, or the rules cannot be built.
        pub fn prepare(&self) -> Result<Prepared, String> {
            use landlock::{
                path_beneath_rules, Access, AccessFs, AccessNet, CompatLevel, Compatible, Ruleset,
                RulesetAttr, RulesetCreatedAttr, Scope,
            };
            Self::require_kernel_support()?;
            if let Some(missing) = self.writable.iter().find(|dir| !dir.is_dir()) {
                return Err(format!(
                    "{} is not a directory; it cannot be writable to a confined command",
                    missing.display()
                ));
            }
            let abi = ABI_REQUIRED;
            let read = AccessFs::from_read(abi);
            let build = || -> Result<Option<OwnedFd>, landlock::RulesetError> {
                // TCP connect and bind are handled and no port is allowed.
                let created = Ruleset::default()
                    .set_compatibility(CompatLevel::HardRequirement)
                    .handle_access(AccessFs::from_all(abi))?
                    .handle_access(AccessNet::from_all(abi))?
                    .scope(Scope::from_all(abi))?
                    .create()?
                    .add_rules(path_beneath_rules(&self.writable, AccessFs::from_all(abi)))?
                    .add_rules(path_beneath_rules(DEVICE_WRITABLE, AccessFs::from_all(abi)))?
                    .add_rules(path_beneath_rules(&self.read_exec, read))?
                    .add_rules(path_beneath_rules(SYSTEM_EXEC, read))?
                    .add_rules(path_beneath_rules(SYSTEM_READ_ONLY, read))?;
                Ok(created.into())
            };
            let ruleset = build()
                .map_err(|e| format!("cannot build the confinement: {e}"))?
                .ok_or_else(|| "the kernel returned no ruleset".to_string())?;
            Ok(Prepared { ruleset })
        }

        /// Whether these rules can confine a command here: they are built
        /// once and dropped, so a run can be refused before it starts.
        ///
        /// # Errors
        ///
        /// See [`Self::prepare`].
        pub fn check(&self) -> Result<(), String> {
            self.prepare().map(|_| ())
        }

        /// Confines `command` and everything it starts, and gives it
        /// `tmpdir` as its `TMPDIR`.
        ///
        /// # Errors
        ///
        /// See [`Self::prepare`]; the command must not be started.
        pub fn restrict(&self, command: &mut Command) -> Result<(), String> {
            self.prepare()?.apply(command);
            if let Some(tmp) = &self.tmpdir {
                command.env("TMPDIR", tmp);
            }
            Ok(())
        }
    }
}

/// Elsewhere there is no confinement a worker trusts.
#[cfg(not(target_os = "linux"))]
impl CommandConfinement {
    /// # Errors
    ///
    /// Always.
    pub fn require_kernel_support() -> Result<(), String> {
        Err("commands are confined only on Linux (Landlock)".into())
    }

    /// # Errors
    ///
    /// Always.
    pub fn check(&self) -> Result<(), String> {
        Self::require_kernel_support()
    }

    /// # Errors
    ///
    /// Always.
    pub fn restrict(&self, _command: &mut std::process::Command) -> Result<(), String> {
        Self::require_kernel_support()
    }
}

#[cfg(all(test, target_os = "linux"))]
mod die_with_parent_tests {
    /// A process started with `die_with_parent` is killed when the thread
    /// that started it ends, though nothing else stops it.
    #[test]
    fn a_child_ends_with_the_thread_that_started_it() {
        let pid = std::thread::spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let mut command = tokio::process::Command::new("sleep");
                command.arg("30");
                super::die_with_parent(&mut command);
                let child = command.spawn().unwrap();
                let pid = child.id().unwrap();
                // Not killed on drop: only the thread's end may end it.
                std::mem::forget(child);
                pid
            })
        })
        .join()
        .unwrap();
        let alive = || {
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .map(|stat| !stat.split(") ").nth(1).unwrap_or("").starts_with('Z'))
                .unwrap_or(false)
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while alive() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!alive(), "the child outlived the thread that started it");
    }
}
