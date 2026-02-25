//! Git-based `DiscoveryProvider`.
//!
//! Ref layout:
//!   refs/relay/server              →  blob("<addr1>\n<addr2>\n…")
//!   refs/peers/<room>/<peer-id>    →  blob("<peer-id>|<relay-circuit-multiaddr>")
//!
//! Requires the `git-discovery` crate feature.

use std::{
    path::PathBuf,
    str::FromStr,
    sync::Mutex,
};

use git2::{
    CredentialType, FetchOptions, ObjectType, PushOptions, RemoteCallbacks, Repository,
};
use libp2p::{Multiaddr, PeerId};

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

// ── GitDiscoveryProvider ──────────────────────────────────────────────────────

/// Production-grade `DiscoveryProvider` backed by a local Git repository with
/// an `origin` remote that all participants push/fetch from.
pub struct GitDiscoveryProvider {
    repo: Mutex<RepoGuard>,
}

impl GitDiscoveryProvider {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, P2pError> {
        let repo = Repository::open(path.into()).map_err(git_err)?;
        Ok(Self { repo: Mutex::new(RepoGuard(repo)) })
    }
}

impl DiscoveryProvider for GitDiscoveryProvider {
    fn publish_relay_addrs(&self, addrs: &[Multiaddr]) -> Result<(), P2pError> {
        let guard = self.repo.lock().unwrap();
        let repo = &guard.0;
        let data = addrs.iter().map(|a| a.to_string()).collect::<Vec<_>>().join("\n");
        let oid = repo.blob(data.as_bytes()).map_err(git_err)?;
        repo.reference("refs/relay/server", oid, true, "relay addrs").map_err(git_err)?;
        let mut remote = repo.find_remote("origin").map_err(git_err)?;
        remote
            .push(&["+refs/relay/server:refs/relay/server"], Some(&mut push_opts()))
            .map_err(git_err)?;
        Ok(())
    }

    fn fetch_relay_addrs(&self) -> Result<Vec<Multiaddr>, P2pError> {
        let guard = self.repo.lock().unwrap();
        let repo = &guard.0;
        let mut remote = repo.find_remote("origin").map_err(git_err)?;
        remote
            .fetch(&["+refs/relay/server:refs/relay/server"], Some(&mut fetch_opts()), None)
            .map_err(git_err)?;
        let reference = repo
            .find_reference("refs/relay/server")
            .map_err(|_| P2pError::NoRelayAddrs)?;
        let blob = reference
            .peel(ObjectType::Blob)
            .map_err(git_err)?
            .into_blob()
            .map_err(|_| P2pError::Discovery("not a blob".into()))?;
        let content = std::str::from_utf8(blob.content())
            .map_err(|e| P2pError::Discovery(e.to_string()))?;
        let addrs: Vec<Multiaddr> = content.lines().filter_map(|l| l.trim().parse().ok()).collect();
        if addrs.is_empty() { return Err(P2pError::NoRelayAddrs); }
        Ok(addrs)
    }

    fn publish_peer(&self, room: &str, peer_id: &PeerId, relay_addr: &Multiaddr) -> Result<(), P2pError> {
        let guard = self.repo.lock().unwrap();
        let repo = &guard.0;
        let data = format!("{}|{}", peer_id, relay_addr);
        let oid = repo.blob(data.as_bytes()).map_err(git_err)?;
        let ref_name = format!("refs/peers/{}/{}", room, peer_id);
        repo.reference(&ref_name, oid, true, "peer publish").map_err(git_err)?;
        let refspec = format!("+{ref_name}:{ref_name}");
        let mut remote = repo.find_remote("origin").map_err(git_err)?;
        remote.push(&[refspec.as_str()], Some(&mut push_opts())).map_err(git_err)?;
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
    Some(PeerInfo { peer_id, relay_addr })
}
