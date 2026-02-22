use tokio::process::Child;

/// Shared runtime state for an active GDB debugging session.
///
/// Created once in `build_registry()` and shared across all GDB tools
/// via `Arc<Mutex<GdbSessionState>>`.
pub struct GdbSessionState {
    /// GDB server process (e.g., JLinkGDBServer, OpenOCD).
    pub server: Option<Child>,
    /// Host:port address for the GDB server (e.g., "localhost:2331").
    pub server_addr: Option<String>,
    /// Process-group ID of the server (set to the server's PID when spawned
    /// with `process_group(0)`).  Used to kill the *entire* process tree,
    /// including JLinkGUIServerExe and similar sub-processes that would
    /// otherwise become orphans.
    pub server_pgid: Option<u32>,
    /// gdbmi client (gdb-multiarch driven via GDB/MI on its stdin/stdout).
    pub client: Option<gdbmi::Gdb>,
    /// PID of the gdb-multiarch process — stored separately because gdbmi::Gdb
    /// does not expose the child PID after construction.  Used by gdb_interrupt
    /// to send SIGINT for a reliable hardware halt.
    pub gdb_pid: Option<u32>,
    /// Whether the gdbmi client has successfully connected to the remote target.
    pub connected: bool,
}

#[allow(clippy::derivable_impls)]
impl Default for GdbSessionState {
    fn default() -> Self {
        Self {
            server: None,
            server_addr: None,
            server_pgid: None,
            client: None,
            gdb_pid: None,
            connected: false,
        }
    }
}

impl GdbSessionState {
    pub fn set_server(&mut self, child: Child, addr: String, pgid: Option<u32>) {
        self.server = Some(child);
        self.server_addr = Some(addr);
        self.server_pgid = pgid;
    }

    pub fn set_client(&mut self, gdb: gdbmi::Gdb, pid: Option<u32>) {
        self.client = Some(gdb);
        self.gdb_pid = pid;
        self.connected = true;
    }

    pub fn has_server(&self) -> bool { self.server.is_some() }
    pub fn has_client(&self) -> bool { self.client.is_some() }

    /// Kill and drop all live processes, reset all fields.
    ///
    /// Sends SIGTERM to the server's entire process group (to catch child
    /// processes like JLinkGUIServerExe), waits briefly, then SIGKILL, and
    /// finally drops the Child handle.
    pub async fn clear(&mut self) {
        // Drop the GDB client first – closing stdin causes gdb-multiarch to exit.
        let _ = self.client.take();
        self.gdb_pid = None;
        self.connected = false;

        // Send SIGTERM to the whole process group to cleanly terminate
        // JLinkGDBServer and all its child processes.
        if let Some(pgid) = self.server_pgid {
            unsafe { libc::kill(-(pgid as i32), libc::SIGTERM); }
        }

        tokio::time::sleep(std::time::Duration::from_millis(400)).await;

        // Kill the direct child (also triggers kill_on_drop cleanup path).
        if let Some(mut child) = self.server.take() {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                child.wait(),
            ).await;
        }

        // Final SIGKILL to the process group to catch any survivors.
        if let Some(pgid) = self.server_pgid.take() {
            unsafe { libc::kill(-(pgid as i32), libc::SIGKILL); }
        }

        self.server_addr = None;
    }
}
