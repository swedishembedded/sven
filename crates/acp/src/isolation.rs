// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for confining autonomous agents to
// networks its clients control. If your team needs expertise in running
// independent agents on isolated networks then you can procure our services
// by sending an email to info@swedishembedded.com.
//! The flags that make one process an independent actor: `--state-root`
//! (where its per-user state lives) and the `network` section of the
//! configuration (`--allow-host`, `--deny-egress`, `--egress-proxy`,
//! `--ca-bundle`, `--resolve`), for one run.
//!
//! The `sven` binary flattens them at its top level and `sven acp serve`
//! takes the same flags, which is how a sub-agent started by `task` inherits
//! its parent's confinement (`sven-bootstrap`'s `serve_args` writes them).

use std::net::IpAddr;
use std::path::PathBuf;

use clap::Args;

#[derive(Args, Debug, Default)]
pub struct IsolationFlags {
    /// Keep every file this process reads or writes for itself (its
    /// configuration, sessions, memory, audit log, worktrees, temporary
    /// files and the `~` of paths) under DIR, and read no `/etc/sven` or
    /// user configuration: two processes with different roots share nothing.
    /// Created private to the user. Put a `config/sven/config.yaml` in it to
    /// configure the agent.
    #[arg(long, value_name = "DIR")]
    pub state_root: Option<PathBuf>,

    /// A host the HTTP tools (web_fetch, web_search, http_request, HTTP MCP
    /// servers) may reach, added to `network.allow_hosts`. Repeatable. Giving
    /// any host makes the list exclusive: everything not listed is refused.
    /// Forms: `host`, `*.suffix`, an IP address, each with an optional
    /// `:port`. The model provider is configured separately and not affected.
    #[arg(long = "allow-host", value_name = "HOST[:PORT]")]
    pub allow_hosts: Vec<String>,

    /// Refuse every HTTP destination of the HTTP tools (an empty
    /// `network.allow_hosts`), unless `--allow-host` lists some.
    #[arg(long)]
    pub deny_egress: bool,

    /// Send every allowed HTTP(S) request through this proxy
    /// (`network.proxy`).
    #[arg(long, value_name = "URL")]
    pub egress_proxy: Option<String>,

    /// Trust the certificate authorities in this PEM file as well
    /// (`network.ca_bundle`), for a proxy that terminates TLS for emulated
    /// names.
    #[arg(long, value_name = "FILE")]
    pub ca_bundle: Option<PathBuf>,

    /// Resolve HOST to ADDR for direct connections (`network.resolve`), so a
    /// familiar name reaches a local emulator. Repeatable.
    #[arg(long = "resolve", value_name = "HOST=ADDR", value_parser = parse_resolve)]
    pub resolve: Vec<(String, IpAddr)>,
}

fn parse_resolve(text: &str) -> Result<(String, IpAddr), String> {
    let (host, addr) = text
        .split_once('=')
        .ok_or("expected HOST=ADDR, for example ntfy.sh=127.0.0.1")?;
    let addr = addr
        .parse::<IpAddr>()
        .map_err(|e| format!("`{addr}` is not an IP address: {e}"))?;
    Ok((host.to_ascii_lowercase(), addr))
}

impl IsolationFlags {
    /// Makes `--state-root` the process's root. Before the first
    /// configuration file is looked for.
    ///
    /// # Errors
    /// The root cannot be created, or a different one is already set.
    pub fn activate_state_root(&self) -> anyhow::Result<()> {
        match &self.state_root {
            Some(root) => sven_roots::set_state_root(root).map_err(anyhow::Error::msg),
            None => Ok(()),
        }
    }

    /// The network flags over `network` of the configuration: lists extend
    /// it, a scalar replaces it.
    pub fn apply(&self, network: &mut sven_http::egress::NetworkConfig) {
        if !self.allow_hosts.is_empty() || self.deny_egress {
            let hosts = network.allow_hosts.get_or_insert_with(Vec::new);
            for host in &self.allow_hosts {
                if !hosts.contains(host) {
                    hosts.push(host.clone());
                }
            }
        }
        if let Some(proxy) = &self.egress_proxy {
            network.proxy = Some(proxy.clone());
        }
        if let Some(bundle) = &self.ca_bundle {
            network.ca_bundle = Some(bundle.clone());
        }
        for (host, addr) in &self.resolve {
            network.resolve.insert(host.clone(), *addr);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_http::egress::NetworkConfig;

    #[test]
    fn flags_extend_the_file_and_an_allow_list_becomes_exclusive() {
        let mut network = NetworkConfig::default();
        IsolationFlags::default().apply(&mut network);
        assert_eq!(network, NetworkConfig::default(), "no flag, no restriction");

        IsolationFlags {
            allow_hosts: vec!["ntfy.sh".into()],
            resolve: vec![("ntfy.sh".into(), "127.0.0.1".parse().unwrap())],
            ..IsolationFlags::default()
        }
        .apply(&mut network);
        assert_eq!(network.allow_hosts, Some(vec!["ntfy.sh".to_string()]));
        assert_eq!(network.resolve.len(), 1);

        let mut closed = NetworkConfig::default();
        IsolationFlags {
            deny_egress: true,
            ..IsolationFlags::default()
        }
        .apply(&mut closed);
        assert_eq!(closed.allow_hosts, Some(vec![]));
    }

    /// The flags a parent writes for its sub-agent (`confinement_flags`) are
    /// the flags the child reads: nothing of the confinement is lost on the
    /// way, an empty allow list (deny everything) included.
    #[test]
    fn a_childs_confinement_is_its_parents_exactly() {
        use clap::Parser;
        #[derive(Parser)]
        struct Child {
            #[command(flatten)]
            isolation: IsolationFlags,
        }
        let parents = [
            NetworkConfig {
                allow_hosts: Some(vec!["ntfy.sh".into(), "*.paste.test:8443".into()]),
                proxy: Some("http://127.0.0.1:3128".into()),
                ca_bundle: Some("/etc/emulation/ca.pem".into()),
                resolve: [("ntfy.sh".to_string(), "127.0.0.1".parse().unwrap())].into(),
            },
            NetworkConfig {
                allow_hosts: Some(vec![]),
                ..NetworkConfig::default()
            },
            NetworkConfig::default(),
        ];
        for parent in parents {
            let root = std::path::Path::new("/run/agents/a1");
            let flags = sven_bootstrap::task_tool::confinement_flags(Some(root), Some(&parent));
            let child = Child::try_parse_from(std::iter::once("sven".to_string()).chain(flags))
                .expect("the child accepts every flag its parent writes");
            assert_eq!(child.isolation.state_root.as_deref(), Some(root));
            let mut network = NetworkConfig::default();
            child.isolation.apply(&mut network);
            assert_eq!(network, parent);
        }
    }

    /// A sub-agent started with its parent's flags enforces the parent's egress
    /// policy: the host the parent allowed is reachable, any other is refused,
    /// and the child cannot hold a wider policy than it was handed.
    #[test]
    fn a_sub_agent_reaches_only_the_hosts_its_parent_allowed() {
        use clap::Parser;
        #[derive(Parser)]
        struct Child {
            #[command(flatten)]
            isolation: IsolationFlags,
        }
        let parent = NetworkConfig {
            allow_hosts: Some(vec!["ntfy.sh".into()]),
            ..NetworkConfig::default()
        };
        let flags = sven_bootstrap::task_tool::confinement_flags(None, Some(&parent));
        let child =
            Child::try_parse_from(std::iter::once("sven".to_string()).chain(flags)).unwrap();
        let mut network = NetworkConfig::default();
        child.isolation.apply(&mut network);
        let policy = sven_http::egress::EgressPolicy::from_config(&network).unwrap();
        let url = |s: &str| reqwest::Url::parse(s).unwrap();
        assert!(policy.check(&url("https://ntfy.sh/topic")).is_ok());
        for outside in [
            "https://example.com/",
            "http://169.254.169.254/",
            "http://127.0.0.1:8080/",
        ] {
            assert!(policy.check(&url(outside)).is_err(), "{outside}");
        }
    }

    #[test]
    fn a_resolve_flag_needs_host_and_address() {
        assert!(parse_resolve("ntfy.sh").is_err());
        assert!(parse_resolve("ntfy.sh=not-an-ip").is_err());
        assert_eq!(
            parse_resolve("NTFY.sh=::1").unwrap(),
            ("ntfy.sh".to_string(), "::1".parse().unwrap())
        );
    }
}
