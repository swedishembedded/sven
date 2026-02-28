//! Git-based `DiscoveryProvider`.
//!
//! Ref layout:
//!   refs/relay/<sha256hex-of-multiaddr>  →  blob("<full-multiaddr-with-/p2p-suffix>")
//!   refs/peers/<room>/<peer-id>          →  blob("<peer-id>|<relay-circuit-multiaddr>")
//!
//! Each relay listen address gets its own git ref named by the SHA-256 of the
//! multiaddr string.  This means:
//!   - Multiple relay servers can publish concurrently without conflicting writes.
//!   - A relay can delete exactly the refs it created on graceful shutdown by
//!     recomputing the SHA-256 of each address it published.
//!   - The client discovers all relays by scanning the `refs/relay/*` glob.
//!
//! Requires the `git-discovery` crate feature.

use std::{path::PathBuf, str::FromStr, sync::Mutex};

use git2::{CredentialType, FetchOptions, ObjectType, PushOptions, RemoteCallbacks, Repository};
use libp2p::{Multiaddr, PeerId};
use sha2::{Digest, Sha256};

use crate::error::P2pError;

use super::{DiscoveryProvider, PeerInfo};

// ── Thread-safety wrapper ─────────────────────────────────────────────────────

/// `git2::Repository` has raw pointers internally and is marked `!Send + !Sync`.
/// All operations on this struct are protected by a `Mutex`, so it is safe to
/// share across threads.
struct RepoGuard(Repository);

// SAFETY: access is serialised through the Mutex in GitDiscoveryProvider.
unsafe impl Send for RepoGuard {}
unsafe impl Sync for RepoGuard {}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn git_err(e: git2::Error) -> P2pError {
    P2pError::Discovery(e.to_string())
}

fn auth_callbacks<'a>() -> RemoteCallbacks<'a> {
    let mut cbs = RemoteCallbacks::new();
    let mut attempts = 0u8;
    cbs.credentials(move |_url, username, allowed| {
        attempts += 1;
        if attempts > 5 {
            return Err(git2::Error::from_str("too many auth attempts"));
        }
        let user = username.unwrap_or("git");
        if allowed.contains(CredentialType::SSH_KEY) {
            if let Ok(c) = git2::Cred::ssh_key_from_agent(user) {
                return Ok(c);
            }
            let home = std::env::var("HOME").unwrap_or_default();
            for name in &["id_ed25519", "id_rsa", "id_ecdsa"] {
                let key = PathBuf::from(&home).join(".ssh").join(name);
                if key.exists() {
                    if let Ok(c) = git2::Cred::ssh_key(user, None, &key, None) {
                        return Ok(c);
                    }
                }
            }
        }
        if allowed.contains(CredentialType::DEFAULT) {
            return git2::Cred::default();
        }
        Err(git2::Error::from_str("no suitable credentials"))
    });
    cbs
}

fn fetch_opts<'a>() -> FetchOptions<'a> {
    let mut opts = FetchOptions::new();
    opts.remote_callbacks(auth_callbacks());
    opts
}

fn push_opts<'a>() -> PushOptions<'a> {
    let mut opts = PushOptions::new();
    opts.remote_callbacks(auth_callbacks());
    opts
}

/// Compute the git ref name for a relay address.
///
/// The name is `refs/relay/<sha256hex>` where the hex string is the SHA-256
/// of the full multiaddr string (including the `/p2p/<peer-id>` component).
/// Using the address content as the key guarantees:
///   - Uniqueness: each (ip, port, peer-id) triple has its own ref.
///   - Determinism: the relay can always recompute the ref name from its known
///     addresses, so it can delete exactly its own refs on shutdown.
fn addr_ref_name(addr: &Multiaddr) -> String {
    let hash = Sha256::digest(addr.to_string().as_bytes());
    format!("refs/relay/{:x}", hash)
}

// ── GitDiscoveryProvider ──────────────────────────────────────────────────────

/// Production-grade `DiscoveryProvider` backed by a local Git repository with
/// an `origin` remote that all participants push/fetch from.
pub struct GitDiscoveryProvider {
    repo: Mutex<RepoGuard>,
}

impl GitDiscoveryProvider {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, P2pError> {
        let repo = Repository::open(path.into()).map_err(git_err)?;
        Ok(Self {
            repo: Mutex::new(RepoGuard(repo)),
        })
    }
}

impl DiscoveryProvider for GitDiscoveryProvider {
    /// Publish each relay listen address as its own git ref.
    ///
    /// The ref name is derived from the SHA-256 of the address string so
    /// concurrent pushes from different relay servers never conflict.
    fn publish_relay_addrs(&self, addrs: &[Multiaddr]) -> Result<(), P2pError> {
        if addrs.is_empty() {
            return Ok(());
        }

        let guard = self.repo.lock().unwrap();
        let repo = &guard.0;

        let mut ref_names: Vec<String> = Vec::new();
        for addr in addrs {
            let ref_name = addr_ref_name(addr);
            let data = addr.to_string();
            let oid = repo.blob(data.as_bytes()).map_err(git_err)?;
            repo.reference(&ref_name, oid, true, "relay addr publish")
                .map_err(git_err)?;
            ref_names.push(ref_name);
        }

        let refspecs: Vec<String> = ref_names.iter().map(|r| format!("+{r}:{r}")).collect();
        let refspecs_str: Vec<&str> = refspecs.iter().map(|s| s.as_str()).collect();
        let mut remote = repo.find_remote("origin").map_err(git_err)?;
        remote
            .push(&refspecs_str, Some(&mut push_opts()))
            .map_err(git_err)?;
        Ok(())
    }

    /// Retrieve all relay addresses published by any relay server.
    ///
    /// Fetches the entire `refs/relay/*` namespace from origin before reading,
    /// so the returned list is always fresh.
    fn fetch_relay_addrs(&self) -> Result<Vec<Multiaddr>, P2pError> {
        let guard = self.repo.lock().unwrap();
        let repo = &guard.0;

        if let Ok(mut remote) = repo.find_remote("origin") {
            tracing::debug!("Fetching refs/relay/* from origin…");
            match remote.fetch(
                &["+refs/relay/*:refs/relay/*"],
                Some(&mut fetch_opts()),
                None,
            ) {
                Ok(()) => tracing::debug!("Fetched refs/relay/* successfully"),
                Err(e) => {
                    tracing::warn!("git fetch refs/relay/* failed, falling back to local refs: {e}")
                }
            }
        } else {
            tracing::warn!("No 'origin' remote configured, using local refs/relay/*");
        }

        let mut addrs: Vec<Multiaddr> = Vec::new();
        if let Ok(refs) = repo.references_glob("refs/relay/*") {
            for reference in refs.flatten() {
                let obj = match reference.peel(ObjectType::Blob) {
                    Ok(o) => o,
                    Err(_) => continue,
                };
                if let Some(blob) = obj.as_blob() {
                    if let Ok(content) = std::str::from_utf8(blob.content()) {
                        if let Ok(addr) = content.trim().parse::<Multiaddr>() {
                            addrs.push(addr);
                        }
                    }
                }
            }
        }

        if addrs.is_empty() {
            return Err(P2pError::NoRelayAddrs);
        }

        tracing::info!("Discovered {} relay address(es) from git", addrs.len());
        Ok(addrs)
    }

    /// Remove exactly the relay addresses that were previously published.
    ///
    /// Each address ref name is recomputed from its SHA-256, so only the refs
    /// created by this relay server are touched — other relays' refs are left
    /// intact.
    fn delete_relay_addrs(&self, addrs: &[Multiaddr]) -> Result<(), P2pError> {
        if addrs.is_empty() {
            return Ok(());
        }

        let guard = self.repo.lock().unwrap();
        let repo = &guard.0;

        let mut deletion_refspecs: Vec<String> = Vec::new();
        for addr in addrs {
            let ref_name = addr_ref_name(addr);
            if let Ok(mut r) = repo.find_reference(&ref_name) {
                tracing::info!("Removing relay addr ref {ref_name}");
                r.delete().map_err(git_err)?;
            }
            // Push deletion regardless — the remote ref may exist even if the
            // local one was already cleaned up in a prior run.
            deletion_refspecs.push(format!(":{ref_name}"));
        }

        let refspecs_str: Vec<&str> = deletion_refspecs.iter().map(|s| s.as_str()).collect();
        let mut remote = repo.find_remote("origin").map_err(git_err)?;
        // Ignore push errors for deletions (refs may not exist on remote).
        let _ = remote.push(&refspecs_str, Some(&mut push_opts()));
        Ok(())
    }

    fn publish_peer(
        &self,
        room: &str,
        peer_id: &PeerId,
        relay_addr: &Multiaddr,
    ) -> Result<(), P2pError> {
        let guard = self.repo.lock().unwrap();
        let repo = &guard.0;
        let data = format!("{}|{}", peer_id, relay_addr);
        let oid = repo.blob(data.as_bytes()).map_err(git_err)?;
        let ref_name = format!("refs/peers/{}/{}", room, peer_id);
        repo.reference(&ref_name, oid, true, "peer publish")
            .map_err(git_err)?;
        let refspec = format!("+{ref_name}:{ref_name}");
        let mut remote = repo.find_remote("origin").map_err(git_err)?;
        remote
            .push(&[refspec.as_str()], Some(&mut push_opts()))
            .map_err(git_err)?;
        Ok(())
    }

    fn fetch_peers(&self, room: &str) -> Result<Vec<PeerInfo>, P2pError> {
        let guard = self.repo.lock().unwrap();
        let repo = &guard.0;
        let refspec = format!("+refs/peers/{0}/*:refs/peers/{0}/*", room);
        let mut remote = repo.find_remote("origin").map_err(git_err)?;
        let _ = remote.fetch(&[refspec.as_str()], Some(&mut fetch_opts()), None);
        let glob = format!("refs/peers/{}/*", room);
        let mut peers = Vec::new();
        for reference in repo.references_glob(&glob).map_err(git_err)? {
            let reference = reference.map_err(git_err)?;
            let obj = match reference.peel(ObjectType::Blob) {
                Ok(o) => o,
                Err(_) => continue,
            };
            if let Some(blob) = obj.as_blob() {
                if let Ok(content) = std::str::from_utf8(blob.content()) {
                    if let Some(info) = parse_peer_record(content) {
                        peers.push(info);
                    }
                }
            }
        }
        Ok(peers)
    }

    fn delete_peer(&self, room: &str, peer_id: &PeerId) -> Result<(), P2pError> {
        let guard = self.repo.lock().unwrap();
        let repo = &guard.0;
        let ref_name = format!("refs/peers/{}/{}", room, peer_id);
        if let Ok(mut r) = repo.find_reference(&ref_name) {
            r.delete().map_err(git_err)?;
        }
        let refspec = format!(":refs/peers/{}/{}", room, peer_id);
        let mut remote = repo.find_remote("origin").map_err(git_err)?;
        let _ = remote.push(&[refspec.as_str()], Some(&mut push_opts()));
        Ok(())
    }
}

fn parse_peer_record(content: &str) -> Option<PeerInfo> {
    let s = content.trim();
    let mut parts = s.splitn(2, '|');
    let peer_id = PeerId::from_str(parts.next()?).ok()?;
    let relay_addr = Multiaddr::from_str(parts.next()?).ok()?;
    Some(PeerInfo {
        peer_id,
        relay_addr,
    })
}
