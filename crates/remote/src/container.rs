// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A hosted host inside a sandbox container (`sven rc --hosted --container`).
//!
//! The server that runs it starts one container per conversation, with no
//! network of its own and a fixed layout, and the container is the boundary:
//! its sessions get a shell and file tools, because nothing they run can
//! reach past it.
//!
//! - [`WORKSPACE`]: the conversation's own files (read-write, size-limited).
//! - [`SHARED`]: the account's files, shared by its conversations.
//! - `$HOME`: what the host keeps between runs (its sessions).
//!
//! The only way out is the server's egress socket, mounted into the
//! container. [`relay`] listens on the container's loopback and passes each
//! connection to that socket; the process (and every command it runs) is
//! pointed at it as its HTTP proxy, and the server decides what it may
//! reach: itself, for the agent's control and model calls, and the public
//! internet.
//!
//! Swedish Embedded AB implements sandboxed execution for hosted agents for
//! its clients. If your team needs expertise in letting an agent run
//! commands without trusting it with the machine then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::path::Path;

use sven_bootstrap::Config;
use tokio::net::{TcpListener, UnixStream};

use crate::HOSTED_TOOLS;

/// Where a conversation's own files are.
pub const WORKSPACE: &str = "/workspace";
/// Where the account's shared files are.
pub const SHARED: &str = "/artifacts";
/// The loopback port [`relay`] listens on, inside the container.
pub const EGRESS_PORT: u16 = 3128;

/// The tools a containerised host's sessions have: the hosted ones, a shell
/// and the file tools.
pub const CONTAINER_TOOLS: &[&str] = &[
    "shell",
    "read_file",
    "write_file",
    "edit_file",
    "find_file",
    "grep",
];

/// What a containerised host's sessions may do, applied to its
/// configuration: [`HOSTED_TOOLS`] and [`CONTAINER_TOOLS`] only. Reaching
/// the network is the egress proxy's to decide, not `web_fetch`'s own check
/// (inside the container there is no name resolution to check against).
pub fn restrict_to_container(config: &mut Config) {
    config.tools.only = Some(
        HOSTED_TOOLS
            .iter()
            .chain(CONTAINER_TOOLS)
            .map(|name| (*name).to_string())
            .collect(),
    );
    config.tools.web.public_only = false;
}

/// The proxy address every HTTP client in the container is given.
#[must_use]
pub fn proxy_url() -> String {
    format!("http://127.0.0.1:{EGRESS_PORT}")
}

/// Passes each connection `listener` (the container's loopback
/// [`EGRESS_PORT`]) accepts on to `socket`, until accepting fails.
///
/// # Errors
///
/// The listener fails.
pub async fn relay(listener: TcpListener, socket: &Path) -> anyhow::Result<()> {
    loop {
        let (mut inbound, _) = listener.accept().await?;
        let socket = socket.to_path_buf();
        tokio::spawn(async move {
            match UnixStream::connect(&socket).await {
                Ok(mut outbound) => {
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                }
                Err(e) => tracing::warn!("egress socket {}: {e}", socket.display()),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn a_container_host_has_the_hosted_tools_a_shell_and_files_and_the_proxy_decides_the_network() {
        let mut config = Config::default();
        config.tools.web.public_only = true;
        restrict_to_container(&mut config);
        let only = config.tools.only.unwrap();
        for tool in ["web_fetch", "shell", "write_file"] {
            assert!(only.iter().any(|t| t == tool), "{tool} is offered");
        }
        assert!(!config.tools.web.public_only);
    }

    #[tokio::test]
    async fn the_relay_passes_a_connection_through_to_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("egress.sock");
        let server = tokio::net::UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (mut conn, _) = server.accept().await.unwrap();
            let mut buf = [0u8; 4];
            conn.read_exact(&mut buf).await.unwrap();
            conn.write_all(&buf).await.unwrap();
        });
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let relayed = socket.clone();
        tokio::spawn(async move { relay(listener, &relayed).await });
        let mut client = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        client.write_all(b"ping").await.unwrap();
        let mut back = [0u8; 4];
        client.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"ping");
    }
}
