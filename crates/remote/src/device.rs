// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for connecting developer machines
// to a hosted service without ever moving a long-lived secret through a
// browser page or a clipboard, for its clients. If your team needs expertise
// in device authorization and agent fleet access then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The client of the device authorization grant (RFC 8628) a remote-control
//! server offers for `sven auth login`.
//!
//! The terminal asks for a code ([`request`]), shows the person the user code
//! and the page to type it at, then polls until the person decides
//! ([`await_token`]). The device code that redeems the grant is a secret of
//! this process: it is sent to the token endpoint and never printed, logged,
//! or put in an error.

use std::future::Future;
use std::time::Duration;

use anyhow::Context as _;
use serde::Deserialize;

/// Extra wait added to the polling interval on `slow_down`
/// (RFC 8628 section 3.5), in [`Pacing::unit`]s.
const SLOW_DOWN_STEP: u32 = 5;

/// Consecutive failed polls (network errors, 5xx) tolerated before giving up.
const MAX_TRANSIENT_FAILURES: u32 = 5;

/// The longest wait between retries after a transient failure, in units.
const MAX_BACKOFF: u32 = 30;

/// How long one request may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The time scale of the protocol. The server speaks in seconds, so the unit
/// is one second; tests shrink it to keep a whole login fast.
#[derive(Debug, Clone, Copy)]
pub struct Pacing {
    pub unit: Duration,
}

impl Default for Pacing {
    fn default() -> Self {
        Self {
            unit: Duration::from_secs(1),
        }
    }
}

/// The server's answer to a device authorization request.
#[derive(Deserialize)]
pub struct DeviceAuthorization {
    device_code: String,
    /// What the person types at [`Self::verification_uri`].
    pub user_code: String,
    pub verification_uri: String,
    /// Seconds until the codes stop working.
    pub expires_in: u32,
    /// Seconds between polls.
    pub interval: u32,
}

impl std::fmt::Debug for DeviceAuthorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceAuthorization")
            .field("device_code", &"<redacted>")
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .field("expires_in", &self.expires_in)
            .field("interval", &self.interval)
            .finish()
    }
}

/// What an approved grant yields.
pub struct Granted {
    /// The key the host authenticates with. A secret.
    pub token: String,
    /// The account display name the person approved for.
    pub account: String,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

#[derive(Deserialize)]
struct TokenBody {
    token: String,
    account: String,
}

/// Seconds the server asked to wait, from a `Retry-After` header.
fn retry_after(response: &reqwest::Response) -> Option<u64> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Asks `url` for a device code for the machine called `name`.
///
/// # Errors
///
/// The server cannot be reached, rate limits the request (the error says how
/// long to wait), or answers something that is not a device authorization.
pub async fn request(
    http: &reqwest::Client,
    url: &str,
    name: &str,
) -> anyhow::Result<DeviceAuthorization> {
    let response = http
        .post(url)
        .timeout(REQUEST_TIMEOUT)
        .json(&serde_json::json!({ "name": name }))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("could not reach the server to start the sign-in: {e}"))?;
    let status = response.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(match retry_after(&response) {
            Some(seconds) => anyhow::anyhow!(
                "the server is limiting sign-in requests from this address; wait {seconds} seconds and run `sven auth login` again"
            ),
            None => anyhow::anyhow!(
                "the server is limiting sign-in requests from this address; wait a while and run `sven auth login` again"
            ),
        });
    }
    if status == reqwest::StatusCode::BAD_REQUEST {
        anyhow::bail!("the server refused the sign-in request (invalid request); update sven");
    }
    anyhow::ensure!(
        status.is_success(),
        "the server could not start the sign-in (it answered {status}); try again later"
    );
    let grant: DeviceAuthorization = response
        .json()
        .await
        .context("the server did not answer a device authorization")?;
    anyhow::ensure!(
        grant.interval > 0 && grant.expires_in > 0,
        "the server answered a device authorization with no usable timing"
    );
    Ok(grant)
}

enum Poll {
    Granted(Granted),
    Pending,
    SlowDown,
    /// The server could not be asked right now (network, 5xx).
    Transient(String),
}

async fn poll_once(http: &reqwest::Client, url: &str, device_code: &str) -> anyhow::Result<Poll> {
    let response = match http
        .post(url)
        .timeout(REQUEST_TIMEOUT)
        .json(&serde_json::json!({ "device_code": device_code }))
        .send()
        .await
    {
        Ok(response) => response,
        // reqwest errors name the URL, which holds no secret.
        Err(e) => return Ok(Poll::Transient(e.to_string())),
    };
    let status = response.status();
    if status.is_success() {
        let body: TokenBody = response
            .json()
            .await
            .context("the server approved the sign-in but did not answer a token")?;
        return Ok(Poll::Granted(Granted {
            token: body.token,
            account: body.account,
        }));
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Ok(Poll::SlowDown);
    }
    if status.is_server_error() {
        return Ok(Poll::Transient(format!("the server answered {status}")));
    }
    let body: ErrorBody = response
        .json()
        .await
        .map_err(|_| anyhow::anyhow!("the server answered {status} to the sign-in poll"))?;
    match body.error.as_str() {
        "authorization_pending" => Ok(Poll::Pending),
        "slow_down" => Ok(Poll::SlowDown),
        "access_denied" => Err(match body.error_description {
            Some(why) => anyhow::anyhow!("the connection was refused: {why}"),
            None => anyhow::anyhow!(
                "the connection was denied in the browser; run `sven auth login` again if that was a mistake"
            ),
        }),
        "expired_token" => Err(expired()),
        other => anyhow::bail!("the server refused the sign-in poll ({other}); update sven"),
    }
}

fn expired() -> anyhow::Error {
    anyhow::anyhow!("the code expired before it was approved; run `sven auth login` again")
}

/// Polls `url` for `grant` every `interval` until the person decides, the
/// grant expires or `cancel` completes.
///
/// # Errors
///
/// The person denied the connection, the code expired, `cancel` completed,
/// the server kept failing, or it answered something this sven cannot read.
pub async fn await_token(
    http: &reqwest::Client,
    url: &str,
    grant: &DeviceAuthorization,
    pacing: Pacing,
    cancel: impl Future<Output = ()>,
) -> anyhow::Result<Granted> {
    let deadline = tokio::time::Instant::now() + pacing.unit * grant.expires_in;
    let mut interval = grant.interval;
    let mut failures = 0u32;
    tokio::pin!(cancel);
    loop {
        let wait = if failures == 0 {
            pacing.unit * interval
        } else {
            pacing.unit * (1u32 << failures.min(5)).min(MAX_BACKOFF).max(interval)
        };
        let wake = tokio::time::Instant::now() + wait;
        anyhow::ensure!(wake < deadline, expired());
        tokio::select! {
            () = &mut cancel => anyhow::bail!("sign-in cancelled"),
            () = tokio::time::sleep_until(wake) => {}
        }
        let polled = tokio::select! {
            () = &mut cancel => anyhow::bail!("sign-in cancelled"),
            polled = poll_once(http, url, &grant.device_code) => polled?,
        };
        match polled {
            Poll::Granted(granted) => return Ok(granted),
            Poll::Pending => failures = 0,
            Poll::SlowDown => {
                failures = 0;
                interval += SLOW_DOWN_STEP;
            }
            Poll::Transient(why) => {
                failures += 1;
                anyhow::ensure!(
                    failures < MAX_TRANSIENT_FAILURES,
                    "lost the server while waiting for the approval ({why}); check the connection and run `sven auth login` again"
                );
            }
        }
    }
}
