//! `sven-relay` — standalone relay server binary.
//!
//! Usage:
//!   sven-relay --listen /ip4/0.0.0.0/tcp/4001 --repo /path/to/git/repo
//!
//! The server publishes its address to `refs/relay/server` in the git repo so
//! that agent nodes can discover it without manual configuration.

use std::{path::PathBuf, sync::Arc};

use clap::Parser;
use libp2p::Multiaddr;

use sven_p2p::{config::RelayConfig, discovery::git::GitDiscoveryProvider, relay};

#[derive(Parser, Debug)]
#[command(
    name = "sven-relay",
    about = "libp2p relay server for sven agent discovery"
)]
struct Args {
    /// TCP listen address (e.g. `/ip4/0.0.0.0/tcp/4001`).
    #[arg(long, default_value = "/ip4/0.0.0.0/tcp/4001")]
    listen: Multiaddr,

    /// Path to the git repository used for peer discovery.
    #[arg(long)]
    repo: PathBuf,

    /// File to store the relay's persistent Ed25519 keypair.
    /// Defaults to `<repo>/.relay-server-key`.
    #[arg(long)]
    keypair: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".parse().unwrap()),
        )
        .init();

    let args = Args::parse();

    let keypair_path = args
        .keypair
        .unwrap_or_else(|| args.repo.join(".relay-server-key"));

    let discovery = Arc::new(
        GitDiscoveryProvider::open(&args.repo)
            .map_err(|e| anyhow::anyhow!("failed to open git repo: {e}"))?,
    );

    let config = RelayConfig {
        listen_addr: args.listen,
        keypair_path,
        discovery,
    };

    relay::run(config).await.map_err(|e| anyhow::anyhow!("{e}"))
}
