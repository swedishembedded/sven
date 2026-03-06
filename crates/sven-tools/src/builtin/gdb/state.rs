// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use tokio::process::Child;

// ── GdbServerProcess ─────────────────────────────────────────────────────────

/// Represents a GDB server that is listening on a known address.
///
/// The `child` field is `Some` when sven owns the process (spawned by
/// `gdb_start_server`), and `None` when the server was discovered as an
/// external process that was already running.
pub struct GdbServerProcess {
    /// Owned server child process, if sven started it.
    pub child: Option<Child>,
    /// Host:port the server is listening on (e.g., `"localhost:2331"`).
    pub addr: String,
    /// Process-group ID; used to kill the entire process tree (including
    /// sub-processes like JLinkGUIServerExe that would otherwise become orphans).
    /// Only set when sven owns the process.
    pub pgid: Option<u32>,
}

// ── GdbClientSession ─────────────────────────────────────────────────────────

/// Represents an active GDB/MI client connection to a remote target.
///
/// Invariant: both fields are populated together via [`GdbSessionState::set_client`].
pub struct GdbClientSession {
    /// The gdbmi handle (drives gdb-multiarch over stdin/stdout).
    pub gdb: gdbmi::Gdb,
    /// PID of the gdb-multiarch process; used by `gdb_interrupt` to send SIGINT.
    pub pid: Option<u32>,
}

// ── GdbSessionState ───────────────────────────────────────────────────────────

/// Shared runtime state for an active GDB debugging session.
///
/// Created once in `build_registry()` and shared across all GDB tools
/// via `Arc<Mutex<GdbSessionState>>`.
///
/// Server and client state are kept in separate `Option` sub-structs so that
/// illegal combinations (e.g. `connected = true` with `client = None`) are
/// unrepresentable by construction.
#[derive(Default)]
pub struct GdbSessionState {
    /// Running GDB server, if any.
    pub server: Option<GdbServerProcess>,
    /// Active GDB client connection, if any.
    pub client: Option<GdbClientSession>,
}

impl GdbSessionState {
    /// Record an owned server process started by sven.
    pub fn set_server(&mut self, child: Child, addr: String, pgid: Option<u32>) {
        self.server = Some(GdbServerProcess {
            child: Some(child),
            addr,
            pgid,
        });
    }

    /// Record a discovered external server that sven did not start.
    pub fn set_external_server(&mut self, addr: String) {
        self.server = Some(GdbServerProcess {
            child: None,
            addr,
            pgid: None,
        });
    }

    pub fn set_client(&mut self, gdb: gdbmi::Gdb, pid: Option<u32>) {
        self.client = Some(GdbClientSession { gdb, pid });
    }

    pub fn has_server(&self) -> bool {
        self.server.is_some()
    }

    pub fn has_client(&self) -> bool {
        self.client.is_some()
    }

    /// Whether the gdbmi client is connected.
    pub fn connected(&self) -> bool {
        self.client.is_some()
    }

    /// Kill and drop all live processes, reset all state.
    ///
    /// Sends SIGTERM to the server's entire process group (to catch child
    /// processes like JLinkGUIServerExe), waits briefly, then SIGKILL, and
    /// finally drops the Child handle.
    pub async fn clear(&mut self) {
        // Drop the GDB client first – closing stdin causes gdb-multiarch to exit.
        self.client = None;

        if let Some(server) = self.server.take() {
            // Only send signals / wait if sven owns the process.
            if server.child.is_some() || server.pgid.is_some() {
                // Send SIGTERM to the whole process group.
                if let Some(pgid) = server.pgid {
                    // SAFETY: `pgid` is a valid process-group ID obtained from
                    // `process_group(0)` on the spawned child.  Sending SIGTERM
                    // to `-pgid` terminates the group without crossing process
                    // boundaries in an undefined way.
                    unsafe {
                        libc::kill(-(pgid as i32), libc::SIGTERM);
                    }
                }

                tokio::time::sleep(std::time::Duration::from_millis(400)).await;

                // Kill the direct child (also triggers kill_on_drop cleanup).
                if let Some(mut child) = server.child {
                    let _ = child.start_kill();
                    let _ =
                        tokio::time::timeout(std::time::Duration::from_secs(2), child.wait()).await;
                }

                // Final SIGKILL to the process group to catch any survivors.
                if let Some(pgid) = server.pgid {
                    // SAFETY: same as above; idempotent for already-dead processes.
                    unsafe {
                        libc::kill(-(pgid as i32), libc::SIGKILL);
                    }
                }
            }
        }
    }
}
