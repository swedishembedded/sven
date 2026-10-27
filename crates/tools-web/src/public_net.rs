// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Reaching only the public internet (`tools.web.public_only`).
//!
//! A host that runs on a server (`sven rc --hosted`) must not let a prompt
//! point `web_fetch` at the server's own network: its loopback services, its
//! database, other machines on its private network, or a cloud metadata
//! service. Checking the URL's text is not enough, because a public name can
//! resolve to a private address and a public page can redirect to one; so the
//! check is where addresses are chosen:
//!
//! - names are resolved by [`PublicOnly`], which hands the connection only
//!   the public addresses a name has (none: the fetch fails);
//! - an address written in the URL itself (which skips resolution) is
//!   checked by [`refuse_literal`], first and on every redirect;
//! - no proxy is used, so the address checked is the one connected to.
//!
//! Swedish Embedded AB implements network egress control for agents running
//! on shared servers for its clients. If your team needs expertise in
//! keeping tool calls away from internal infrastructure then you can procure
//! our services by sending an email to info@swedishembedded.com.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};

/// Whether `ip` is an address on the public internet: not this machine, not
/// a private or shared network, not link-local (where cloud metadata lives),
/// not multicast, documentation or reserved space.
#[must_use]
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        // Shared address space (carrier-grade NAT), 100.64.0.0/10.
        || (a == 100 && (b & 0xc0) == 64)
        // IETF protocol assignments, 192.0.0.0/24.
        || (a == 192 && b == 0 && c == 0)
        // Benchmarking, 198.18.0.0/15.
        || (a == 198 && (b & 0xfe) == 18)
        // Reserved, 240.0.0.0/4.
        || a >= 240)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let first = ip.segments()[0];
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        // Unique local, fc00::/7.
        || (first & 0xfe00) == 0xfc00
        // Link-local and the old site-local, fe80::/10 and fec0::/10.
        || (first & 0xffc0) == 0xfe80
        || (first & 0xffc0) == 0xfec0
        // Documentation, 2001:db8::/32.
        || (first == 0x2001 && ip.segments()[1] == 0x0db8)
        // Addresses that carry an IPv4 address inside (NAT64, 6to4, the old
        // IPv4-compatible form): refused rather than unwrapped.
        || (first == 0x0064 && ip.segments()[1] == 0xff9b)
        || first == 0x2002
        || ip.segments()[..6].iter().all(|s| *s == 0))
}

/// A resolver that gives the connection only a name's public addresses.
#[derive(Debug, Clone, Copy, Default)]
pub struct PublicOnly;

impl Resolve for PublicOnly {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let host = name.as_str().to_string();
            let public: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|addr| is_public(addr.ip()))
                .collect();
            if public.is_empty() {
                return Err(format!(
                    "{host} has no public address; only the public internet is reachable"
                )
                .into());
            }
            Ok(Box::new(public.into_iter()) as Addrs)
        })
    }
}

/// An error when `url` names a non-public address directly (which no
/// resolver sees).
///
/// # Errors
///
/// The URL's host is an IP address that is not public.
pub fn refuse_literal(url: &reqwest::Url) -> Result<(), String> {
    // The parser has already normalised an address host (`2130706433`
    // reads as `127.0.0.1`; IPv6 comes bracketed), so text that parses as an
    // address is one.
    let Some(host) = url.host_str() else {
        return Ok(());
    };
    let Ok(ip) = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
    else {
        return Ok(());
    };
    if is_public(ip) {
        Ok(())
    } else {
        Err(format!(
            "{ip} is not a public address; only the public internet is reachable"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn the_servers_own_networks_are_not_public() {
        for refused in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "240.0.0.1",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "64:ff9b::a9fe:a9fe",
            "2002:a9fe:a9fe::1",
            "::127.0.0.1",
        ] {
            assert!(
                !is_public(refused.parse().unwrap()),
                "{refused} counted as public"
            );
        }
        for public in [
            "1.1.1.1",
            "93.184.216.34",
            "2606:4700:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            assert!(is_public(public.parse().unwrap()), "{public} refused");
        }
    }

    #[test]
    fn an_address_written_in_the_url_is_checked() {
        let url = |s: &str| reqwest::Url::parse(s).unwrap();
        assert!(refuse_literal(&url("http://169.254.169.254/latest/meta-data/")).is_err());
        assert!(refuse_literal(&url("http://[::1]:5432/")).is_err());
        assert!(
            refuse_literal(&url("http://2130706433/")).is_err(),
            "127.0.0.1 written as a number"
        );
        assert!(refuse_literal(&url("https://example.com/")).is_ok());
        assert!(refuse_literal(&url("http://1.1.1.1/")).is_ok());
    }

    #[tokio::test]
    async fn a_name_with_only_private_addresses_resolves_to_nothing() {
        let refused = PublicOnly
            .resolve(Name::from_str("localhost").unwrap())
            .await;
        assert!(refused.is_err());
    }
}
