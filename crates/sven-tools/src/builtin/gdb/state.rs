use tokio::process::Child;

/// Shared runtime state for an active GDB debugging session.
///
/// Created once in `build_registry()` and shared across all five GDB tools
/// via `Arc<Mutex<GdbSessionState>>`.
pub struct GdbSessionState {
    /// GDB server process (e.g., JLinkGDBServer, OpenOCD).
    pub server: Option<Child>,
    /// Host:port address for the GDB server (e.g., "localhost:2331").
    pub server_addr: Option<String>,
    /// gdbmi client (gdb-multiarch driven via GDB/MI on its stdin/stdout).
    pub client: Option<gdbmi::Gdb>,
    /// Whether the gdbmi client has successfully connected to the remote target.
    pub connected: bool,
}

#[allow(clippy::derivable_impls)]
impl Default for GdbSessionState {
    fn default() -> Self {
        Self {
            server: None,
            server_addr: None,
            client: None,
            connected: false,
        }
    }
}

impl GdbSessionState {
    pub fn set_server(&mut self, child: Child, addr: String) {
        self.server = Some(child);
        self.server_addr = Some(addr);
    }

    pub fn set_client(&mut self, gdb: gdbmi::Gdb) {
        self.client = Some(gdb);
        self.connected = true;
    }

    pub fn has_server(&self) -> bool { self.server.is_some() }
    pub fn has_client(&self) -> bool { self.client.is_some() }

    /// Kill and drop all live processes, reset all fields.
    pub async fn clear(&mut self) {
        // Drop the client first – this closes stdin, which causes the
        // gdb-multiarch process to exit cleanly.
        let _ = self.client.take();
        self.connected = false;

        if let Some(mut child) = self.server.take() {
            let _ = child.start_kill();
            // Give it a moment to exit; we don't await indefinitely.
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                child.wait(),
            ).await;
        }
        self.server_addr = None;
    }
}
