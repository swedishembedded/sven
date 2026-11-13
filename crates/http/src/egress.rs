// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for confining autonomous agents to
// networks its clients control. If your team needs expertise in egress
// control for agent fleets then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Egress control: which hosts a process's HTTP clients may reach, through
//! which proxy, trusting which certificate authority, resolving which names
//! to which addresses.
//!
//! The policy is process-wide and installed once ([`install`]); every client
//! built through [`crate::client_builder`] enforces it at the connection
//! layer, so a tool, an MCP transport or a verifier cannot forget to check.
//! The model provider's client ([`crate::model_client_builder`]) is the one
//! exception: its endpoint is configured separately.
//!
//! Deny by default: with `allow_hosts` present, a destination that no entry
//! matches is never connected to, including IP literals and redirect targets.
//! A refused request fails with a message naming the host and the key that
//! would allow it. Enforcement reuses the proxy hook: a refused destination
//! is "proxied" to a name under `REFUSED_SUFFIX`, which the resolver of the
//! same client refuses with the reason. That is fail-closed for every URL
//! shape without a wrapper around the client.
//!
//! Shell commands and stdio MCP servers are processes sven starts, not HTTP
//! clients of sven, and are outside this policy: contain them with the
//! sandbox or network namespace sven runs in.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::{ClientBuilder, Proxy, Url};
use serde::{Deserialize, Serialize};

/// Names under this suffix are never resolved: they carry a refused
/// destination to the resolver, which turns it into the refusal message.
const REFUSED_SUFFIX: &str = ".egress-refused.invalid";

/// The `network` configuration section.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    /// The hosts HTTP(S) clients may reach. Absent: no restriction. Present
    /// (an empty list included): only these. An entry is a host name
    /// (`ntfy.sh`), a subdomain wildcard (`*.example.com`, not the apex), an
    /// IP literal, each optionally with a port (`api.test:8443`,
    /// `[::1]:9000`).
    pub allow_hosts: Option<Vec<String>>,
    /// An HTTP(S) proxy (`http://127.0.0.1:3128`) every allowed request goes
    /// through; system proxy variables are then ignored. The proxy resolves
    /// the destination names.
    pub proxy: Option<String>,
    /// A PEM file of certificate authorities trusted in addition to the
    /// built-in roots, for a proxy that terminates TLS for the names it
    /// emulates. Certificates are still verified.
    pub ca_bundle: Option<PathBuf>,
    /// Host name to address, for direct connections: a familiar name reaches
    /// a local emulator. The port is the URL's own.
    pub resolve: BTreeMap<String, IpAddr>,
}

/// A rule of `allow_hosts`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HostRule {
    host: String,
    subdomains_only: bool,
    port: Option<u16>,
}

impl HostRule {
    fn parse(entry: &str) -> Result<Self, String> {
        let text = entry.trim().to_ascii_lowercase();
        if text.contains('/') {
            return Err(format!(
                "`{entry}` is not a host name, `*.suffix` or IP address (no scheme or path)"
            ));
        }
        let (host, port) = if let Some(rest) = text.strip_prefix('[') {
            let (host, tail) = rest
                .split_once(']')
                .ok_or_else(|| format!("`{entry}`: unclosed `[`"))?;
            let port = tail.strip_prefix(':');
            (host.to_string(), port)
        } else if text.matches(':').count() == 1 {
            let (host, port) = text.split_once(':').unwrap_or_default();
            (host.to_string(), Some(port))
        } else {
            (text.clone(), None)
        };
        let port = port
            .map(|p| p.parse::<u16>().map_err(|_| format!("`{entry}`: bad port")))
            .transpose()?;
        let (host, subdomains_only) = match host.strip_prefix("*.") {
            Some(rest) => (rest.to_string(), true),
            None => (host, false),
        };
        let valid = !host.is_empty()
            && !host.contains('*')
            && !host.contains('/')
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':'));
        if !valid {
            return Err(format!(
                "`{entry}` is not a host name, `*.suffix` or IP address"
            ));
        }
        Ok(Self {
            host,
            subdomains_only,
            port,
        })
    }

    fn matches(&self, host: &str, port: Option<u16>) -> bool {
        if self.port.is_some() && self.port != port {
            return false;
        }
        if self.subdomains_only {
            host.strip_suffix(self.host.as_str())
                .is_some_and(|front| front.ends_with('.') && front.len() > 1)
        } else {
            host == self.host
        }
    }
}

/// A validated [`NetworkConfig`]: rules parsed, proxy and authorities read.
#[derive(Debug, Clone)]
pub struct EgressPolicy {
    config: NetworkConfig,
    allow: Option<Vec<HostRule>>,
    proxy: Option<Url>,
    authorities: Vec<reqwest::Certificate>,
}

impl PartialEq for EgressPolicy {
    fn eq(&self, other: &Self) -> bool {
        self.config == other.config
    }
}

impl EgressPolicy {
    /// Validates `config`. A malformed entry, a proxy that is not an
    /// `http(s)` URL, an unreadable or empty CA bundle or a bad `resolve`
    /// name fails here, at startup, never silently at the first request.
    ///
    /// # Errors
    /// A message naming the offending key.
    pub fn from_config(config: &NetworkConfig) -> Result<Self, String> {
        let allow = config
            .allow_hosts
            .as_ref()
            .map(|hosts| {
                hosts
                    .iter()
                    .map(|h| HostRule::parse(h).map_err(|e| format!("network.allow_hosts: {e}")))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        let proxy = config
            .proxy
            .as_deref()
            .map(|p| {
                let url = Url::parse(p).map_err(|e| format!("network.proxy `{p}`: {e}"))?;
                if matches!(url.scheme(), "http" | "https") && url.host_str().is_some() {
                    Ok(url)
                } else {
                    Err(format!("network.proxy `{p}` must be an http(s) URL"))
                }
            })
            .transpose()?;
        let authorities = match &config.ca_bundle {
            Some(path) => read_authorities(path)?,
            None => Vec::new(),
        };
        for name in config.resolve.keys() {
            if HostRule::parse(name).is_err() || name.contains('*') {
                return Err(format!("network.resolve: `{name}` is not a host name"));
            }
        }
        Ok(Self {
            config: config.clone(),
            allow,
            proxy,
            authorities,
        })
    }

    /// The configuration this policy was made from.
    #[must_use]
    pub fn config(&self) -> &NetworkConfig {
        &self.config
    }

    /// Whether `url`'s destination is allowed.
    ///
    /// # Errors
    /// The refusal, naming the host and the key that would allow it.
    pub fn check(&self, url: &Url) -> Result<(), String> {
        let Some(rules) = &self.allow else {
            return Ok(());
        };
        let host = url
            .host_str()
            .map(|h| h.trim_matches(['[', ']']).to_ascii_lowercase())
            .ok_or_else(|| format!("network egress refused: `{url}` has no host"))?;
        let port = url.port_or_known_default();
        if rules.iter().any(|rule| rule.matches(&host, port)) {
            Ok(())
        } else {
            Err(refusal(&host, port))
        }
    }

    /// `builder`, enforcing this policy. `inner` resolves the names the
    /// policy does not map (the system resolver when `None`).
    pub fn apply(&self, builder: ClientBuilder, inner: Option<Arc<dyn Resolve>>) -> ClientBuilder {
        let policy = self.clone();
        let mut builder = builder
            .no_proxy()
            .dns_resolver(Arc::new(PolicyResolver {
                map: self.config.resolve.clone(),
                inner,
            }))
            .tls_certs_merge(self.authorities.clone());
        if self.allow.is_some() || self.proxy.is_some() {
            builder = builder.proxy(Proxy::custom(move |url| match policy.check(url) {
                Ok(()) => policy.proxy.clone(),
                Err(_) => Some(refused_destination(url)),
            }));
        }
        builder
    }
}

fn refusal(host: &str, port: Option<u16>) -> String {
    let at = port.map_or_else(|| host.to_string(), |p| format!("{host}:{p}"));
    format!(
        "network egress to `{at}` refused: not in `network.allow_hosts` \
         (--allow-host); this agent may reach only the hosts its operator listed"
    )
}

/// The pseudo-proxy that carries a refused destination to the resolver.
fn refused_destination(url: &Url) -> Url {
    let host: String = url
        .host_str()
        .unwrap_or("unknown")
        .chars()
        .map(|c| {
            if c == ':' || c == '[' || c == ']' {
                '-'
            } else {
                c
            }
        })
        .collect();
    let port = url.port_or_known_default().unwrap_or(0);
    Url::parse(&format!("http://{host}-port{port}{REFUSED_SUFFIX}:9"))
        .unwrap_or_else(|_| Url::parse("http://refused.egress-refused.invalid:9").expect("static"))
}

struct PolicyResolver {
    map: BTreeMap<String, IpAddr>,
    inner: Option<Arc<dyn Resolve>>,
}

impl Resolve for PolicyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_ascii_lowercase();
        if let Some(carried) = host.strip_suffix(REFUSED_SUFFIX) {
            let (host, port) = carried.rsplit_once("-port").unwrap_or((carried, "0"));
            let port = port.parse().ok().filter(|p| *p != 0);
            let message = refusal(host, port);
            return Box::pin(async move { Err(message.into()) });
        }
        if let Some(ip) = self.map.get(&host) {
            let addrs: Addrs = Box::new(vec![SocketAddr::new(*ip, 0)].into_iter());
            return Box::pin(async move { Ok(addrs) });
        }
        match &self.inner {
            Some(inner) => inner.resolve(name),
            None => Box::pin(async move {
                let found: Vec<SocketAddr> =
                    tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
                Ok(Box::new(found.into_iter()) as Addrs)
            }),
        }
    }
}

fn read_authorities(path: &Path) -> Result<Vec<reqwest::Certificate>, String> {
    let pem =
        std::fs::read(path).map_err(|e| format!("network.ca_bundle {}: {e}", path.display()))?;
    let certs = reqwest::Certificate::from_pem_bundle(&pem)
        .map_err(|e| format!("network.ca_bundle {}: {e}", path.display()))?;
    if certs.is_empty() {
        return Err(format!(
            "network.ca_bundle {} holds no certificate",
            path.display()
        ));
    }
    Ok(certs)
}

static INSTALLED: OnceLock<EgressPolicy> = OnceLock::new();

/// Makes `policy` the process's egress policy. Installing the same policy
/// again is fine (every surface may call it); a different one is refused,
/// because clients already built enforce the first.
///
/// # Errors
/// A different policy is already installed.
pub fn install(policy: EgressPolicy) -> Result<(), String> {
    let installed = INSTALLED.get_or_init(|| policy.clone());
    if *installed == policy {
        Ok(())
    } else {
        Err(
            "a different network policy is already installed in this process; \
             one process has one egress policy"
                .into(),
        )
    }
}

/// The installed policy, if any.
#[must_use]
pub fn installed() -> Option<&'static EgressPolicy> {
    INSTALLED.get()
}

/// Whether the installed policy (if any) lets a request to `url` out: the
/// early, readable refusal for code that has the URL before it sends.
///
/// # Errors
/// The refusal message.
pub fn check(url: &Url) -> Result<(), String> {
    installed().map_or(Ok(()), |policy| policy.check(url))
}

#[cfg(test)]
mod tests;
