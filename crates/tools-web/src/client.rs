// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for confining autonomous agents to
// networks its clients control. If your team needs expertise in egress
// control for agent fleets then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The one HTTP client every web tool builds: the installed egress policy
//! (`network:`), the public-only rule when set, and errors that say why.

use std::sync::Arc;
use std::time::Duration;

use crate::public_net;

/// Redirects followed before a request fails.
pub(crate) const MAX_REDIRECTS: usize = 3;

pub(crate) const USER_AGENT: &str = "sven-agent/0.1";

/// Whether the public-only address rule applies. An installed egress policy
/// replaces it: the operator named the reachable hosts (and may map names to
/// private emulators), so that list is the one authority. The two are never
/// layered, which would silently refuse the emulated network the policy
/// exists to reach; `Config::apply_network` warns when both are set.
fn public_only_applies(requested: bool) -> bool {
    requested && sven_http::egress::installed().is_none()
}

/// A client with `timeout` that follows at most [`MAX_REDIRECTS`] redirects.
/// With `public_only` it reaches only the public internet; the egress policy,
/// when installed, applies in both cases (it decides which hosts, this which
/// addresses).
pub(crate) fn web_client(timeout: Duration, public_only: bool) -> anyhow::Result<reqwest::Client> {
    let public_only = public_only_applies(public_only);
    let builder = if public_only {
        sven_http::client_builder_resolving(Some(Arc::new(public_net::PublicOnly)))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() > MAX_REDIRECTS {
                    return attempt.error("too many redirects");
                }
                match public_net::refuse_literal(attempt.url()) {
                    Ok(()) => attempt.follow(),
                    Err(why) => attempt.error(why),
                }
            }))
    } else {
        sven_http::client_builder().redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
    };
    Ok(builder.timeout(timeout).user_agent(USER_AGENT).build()?)
}

/// A failed request as one readable error, with the cause reqwest's own text
/// hides (a destination the egress policy refused, a failed lookup).
pub(crate) fn request_error(error: &reqwest::Error) -> anyhow::Error {
    anyhow::anyhow!(sven_http::describe(error))
}

/// Parses `url` (http or https only) and applies the egress policy's early,
/// readable refusal and the public-only literal check.
pub(crate) fn checked_url(url: &str, public_only: bool) -> anyhow::Result<reqwest::Url> {
    let parsed = reqwest::Url::parse(url)?;
    if !matches!(parsed.scheme(), "http" | "https") {
        anyhow::bail!(
            "only http and https URLs are supported, not `{}`",
            parsed.scheme()
        );
    }
    sven_http::egress::check(&parsed).map_err(|why| anyhow::anyhow!(why))?;
    if public_only_applies(public_only) {
        public_net::refuse_literal(&parsed).map_err(|why| anyhow::anyhow!(why))?;
    }
    Ok(parsed)
}
