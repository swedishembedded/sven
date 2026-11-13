// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for delegating work to sub-agents
// that stay inside the confinement of their parent, for its clients. If your
// team needs expertise in least-privilege agent delegation then you can
// procure our services by sending an email to info@swedishembedded.com.
//! A sub-agent of a confined session runs under the confinement of its
//! parent's commands.
//!
//! The child is a whole `sven` process. It is started inside a Landlock
//! domain built from the parent's [`CommandConfinement`] (same directory
//! tree, no other path, no network), widened by exactly what a sven process
//! needs to do its job:
//!
//! - its own executable (the file, to read and execute);
//! - the working directory it was given, when that is not already in the tree
//!   (an isolated child's worktree);
//! - a state root of its own inside the parent's private tmp (its sessions,
//!   memory and audit log), and nothing else writable;
//! - TCP connections to the ports of its model endpoint and of the hosts the
//!   egress policy allows. The egress policy and the state root are also
//!   handed to it as flags.
//!
//! A Landlock domain is inherited by everything the child starts and can only
//! be narrowed, so the child's shell, its grandchildren and its tools stay
//! inside, and none can loosen it.

use std::path::{Path, PathBuf};

use sven_tools_exec::confine::CommandConfinement;

/// What a confined child is started with.
pub(super) struct ConfinedChild {
    pub(super) confinement: CommandConfinement,
    /// The child's `--state-root`: private to it, removed when it is dropped.
    pub(super) state_root: PathBuf,
}

impl Drop for ConfinedChild {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.state_root);
    }
}

/// The ports a child may connect to: its model endpoint's and those of the
/// hosts the installed egress policy allows (an entry without a port allows
/// 80 and 443; the proxy's own port when there is one).
pub(super) fn connect_ports(
    model_base_url: Option<&str>,
    network: Option<&sven_http::egress::NetworkConfig>,
) -> Vec<u16> {
    let mut ports = Vec::new();
    ports.extend(model_base_url.and_then(url_port));
    if let Some(network) = network {
        ports.extend(network.proxy.as_deref().and_then(url_port));
        for entry in network.allow_hosts.iter().flatten() {
            ports.extend(entry_ports(entry));
        }
    }
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// The port of an `http(s)` URL, its scheme's default when it names none.
fn url_port(url: &str) -> Option<u16> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    let explicit = authority
        .rsplit_once(':')
        .filter(|(host, _)| !host.ends_with(':') && !host.is_empty())
        .and_then(|(_, port)| port.parse().ok());
    explicit.or(match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    })
}

/// The ports an `allow_hosts` entry names: its own, else 80 and 443.
fn entry_ports(entry: &str) -> Vec<u16> {
    let port = match entry.strip_prefix('[') {
        Some(bracketed) => bracketed.split_once("]:").map(|(_, port)| port),
        None if entry.matches(':').count() == 1 => entry.split_once(':').map(|(_, port)| port),
        None => None,
    };
    port.and_then(|p| p.parse().ok())
        .map_or_else(|| vec![80, 443], |p| vec![p])
}

/// The confinement of a child of a session confined as `parent`.
///
/// # Errors
///
/// The child's state directory cannot be made.
pub(super) fn confine_child(
    parent: &CommandConfinement,
    exe: &Path,
    workdir: &Path,
    handle: &str,
    connect_ports: Vec<u16>,
) -> Result<ConfinedChild, String> {
    let mut confinement = parent.clone();
    if !confinement
        .writable
        .iter()
        .any(|dir| workdir.starts_with(dir))
    {
        confinement.writable.push(workdir.to_path_buf());
    }
    confinement.read_exec.push(exe.to_path_buf());
    let home = parent
        .tmpdir
        .clone()
        .or_else(|| parent.writable.first().cloned())
        .ok_or("the confinement has no writable directory for a sub-agent's state")?;
    let state_root = home.join(format!("sven-child-{handle}"));
    std::fs::create_dir_all(&state_root)
        .map_err(|e| format!("sub-agent state {}: {e}", state_root.display()))?;
    confinement.tmpdir = Some(state_root.join("tmp"));
    std::fs::create_dir_all(state_root.join("tmp"))
        .map_err(|e| format!("sub-agent state {}: {e}", state_root.display()))?;
    confinement.connect_ports = connect_ports;
    Ok(ConfinedChild {
        confinement,
        state_root,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_child_may_connect_to_its_model_and_the_hosts_its_policy_allows_only() {
        let network = sven_http::egress::NetworkConfig {
            allow_hosts: Some(vec![
                "ntfy.sh".into(),
                "api.test:8443".into(),
                "[::1]:9000".into(),
            ]),
            proxy: Some("http://127.0.0.1:3128".into()),
            ..Default::default()
        };
        assert_eq!(
            connect_ports(Some("http://127.0.0.1:8799/v1"), Some(&network)),
            [80, 443, 3128, 8443, 8799, 9000]
        );
        assert_eq!(connect_ports(Some("https://m.test/v1"), None), [443]);
        assert!(connect_ports(None, None).is_empty());
    }

    #[test]
    fn a_child_keeps_the_parents_tree_and_gets_a_private_state_root_inside_its_tmp() {
        let root = tempfile::tempdir().unwrap();
        let tree = root.path().join("tree");
        let tmp = tree.join(".tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        let parent = CommandConfinement {
            writable: vec![tree.clone()],
            read_exec: Vec::new(),
            tmpdir: Some(tmp.clone()),
            connect_ports: Vec::new(),
        };
        let worktree = root.path().join("worktree");
        let child = confine_child(&parent, Path::new("/x/sven"), &worktree, "h1", vec![1]).unwrap();
        assert_eq!(child.confinement.writable, [tree, worktree]);
        assert_eq!(child.confinement.read_exec, [PathBuf::from("/x/sven")]);
        assert_eq!(child.state_root, tmp.join("sven-child-h1"));
        assert!(child.state_root.join("tmp").is_dir());
        let gone = child.state_root.clone();
        drop(child);
        assert!(!gone.exists());
    }
}
