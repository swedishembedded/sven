// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for agents that never send their
// long-lived keys where a short-lived token will do for its clients. If your
// team needs expertise in credential handling for agent tools then you can
// procure our services by sending an email to info@swedishembedded.com.
//! A server reached with short-lived tokens traded for a long-lived key.
//!
//! The key goes only to the server's token endpoint; each request to the
//! MCP endpoint carries a token it gave back, kept until shortly before it
//! expires and traded again then, or at once when the server refuses it. A
//! token caught in that traffic is useless within minutes, and the key
//! never travels with it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::stream::BoxStream;
use reqwest::header::{HeaderName, HeaderValue};
use rmcp::model::ClientJsonRpcMessage;
use rmcp::transport::streamable_http_client::{
    SseError, StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
};
use serde::Deserialize;
use sse_stream::Sse;
use tokio::sync::Mutex;

/// Where a server's tokens are traded for, and the key traded. Never
/// written to a configuration file: it carries the key.
#[derive(Clone)]
pub struct TokenExchange {
    pub url: String,
    pub key: String,
}

impl std::fmt::Debug for TokenExchange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenExchange")
            .field("url", &self.url)
            .field("key", &"[redacted]")
            .finish()
    }
}

/// Trade this long before a token expires, so one never lapses in flight.
const RENEW_BEFORE: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
struct Traded {
    access_token: String,
    expires_in: u64,
}

struct Source {
    exchange: TokenExchange,
    http: reqwest::Client,
    held: Mutex<Option<(String, Instant)>>,
}

impl Source {
    /// A token good for a while yet: the one held, or a newly traded one
    /// (always, with `fresh`).
    async fn token(&self, fresh: bool) -> Result<String, StreamableHttpError<reqwest::Error>> {
        let mut held = self.held.lock().await;
        if !fresh {
            if let Some((token, until)) = held.as_ref() {
                if Instant::now() + RENEW_BEFORE < *until {
                    return Ok(token.clone());
                }
            }
        }
        let traded = trade(&self.http, &self.exchange)
            .await
            .map_err(|why| StreamableHttpError::UnexpectedServerResponse(why.into()))?;
        let until = Instant::now() + Duration::from_secs(traded.expires_in.min(24 * 3600));
        *held = Some((traded.access_token.clone(), until));
        Ok(traded.access_token)
    }
}

/// Trade `exchange`'s key for a token at its token endpoint; the error says
/// why not, for the person to read.
async fn trade(http: &reqwest::Client, exchange: &TokenExchange) -> Result<Traded, String> {
    let response = http
        .post(&exchange.url)
        .bearer_auth(&exchange.key)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| format!("trading the key for a token: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "the server refused to trade this machine's key for a token ({}); sign in again",
            response.status()
        ));
    }
    response
        .json()
        .await
        .map_err(|e| format!("the token the server gave back: {e}"))
}

/// A connector added to the signed-in person's account, and the service's
/// sign-in page that connects their account of it.
#[derive(Debug, Deserialize)]
pub struct AccountConnection {
    pub connector: AccountConnector,
    #[serde(rename = "signInUrl")]
    pub sign_in_url: String,
}

#[derive(Debug, Deserialize)]
pub struct AccountConnector {
    pub id: String,
    pub name: String,
}

/// Add the MCP server at `url`, called `name`, to the account behind
/// `exchange` as a connector of the person's own, through the account's MCP
/// endpoint at `mcp_url` (its `/connect` beside it). The service's sign-in
/// happens in the person's browser, signed in to the account; its tokens
/// stay with the account, never on this machine.
///
/// # Errors
///
/// The key is refused, the account refuses the server (with its reason),
/// or the account cannot be reached.
pub async fn connect_account(
    exchange: &TokenExchange,
    mcp_url: &str,
    name: &str,
    url: &str,
) -> anyhow::Result<AccountConnection> {
    let http = sven_http::client();
    let traded = trade(&http, exchange).await.map_err(anyhow::Error::msg)?;
    let response = http
        .post(format!("{}/connect", mcp_url.trim_end_matches('/')))
        .bearer_auth(&traded.access_token)
        .json(&serde_json::json!({ "name": name, "url": url }))
        .timeout(Duration::from_secs(60))
        .send()
        .await?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        let server = reqwest::Url::parse(mcp_url).map_or_else(
            |_| mcp_url.to_string(),
            |u| u.origin().ascii_serialization(),
        );
        anyhow::bail!(
            "{server} does not offer adding servers to your account yet (it needs a newer \
             release). Add {name} to this machine instead: sven mcp add --local {name} {url}"
        );
    }
    if !status.is_success() {
        #[derive(Deserialize)]
        struct Refusal {
            error: String,
        }
        let why = response
            .json::<Refusal>()
            .await
            .map_or_else(|_| format!("the server answered {status}"), |r| r.error);
        anyhow::bail!("{name} was not added to your account: {why}");
    }
    Ok(response.json().await?)
}

/// An HTTP client of the MCP endpoint that presents a traded token on every
/// request, and trades again once when the server refuses it.
#[derive(Clone)]
pub struct ExchangingClient {
    http: reqwest::Client,
    source: Arc<Source>,
}

impl ExchangingClient {
    pub fn new(exchange: TokenExchange) -> Self {
        let http = sven_http::client();
        Self {
            source: Arc::new(Source {
                exchange,
                http: http.clone(),
                held: Mutex::new(None),
            }),
            http,
        }
    }

    /// Run `call` with a token; once more with a fresh one when the server
    /// says it needs authorization.
    async fn with_token<T, F, Fut>(&self, call: F) -> Result<T, StreamableHttpError<reqwest::Error>>
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = Result<T, StreamableHttpError<reqwest::Error>>>,
    {
        let token = self.source.token(false).await?;
        match call(token).await {
            Err(StreamableHttpError::AuthRequired(_)) => call(self.source.token(true).await?).await,
            other => other,
        }
    }
}

impl StreamableHttpClient for ExchangingClient {
    type Error = reqwest::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        _auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        self.with_token(|token| {
            self.http.post_message(
                uri.clone(),
                message.clone(),
                session_id.clone(),
                Some(token),
                custom_headers.clone(),
            )
        })
        .await
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        _auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        self.with_token(|token| {
            self.http.post_message_with_max_sse_event_size(
                uri.clone(),
                message.clone(),
                session_id.clone(),
                Some(token),
                custom_headers.clone(),
                max_sse_event_size,
            )
        })
        .await
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        _auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        self.with_token(|token| {
            self.http.delete_session(
                uri.clone(),
                session_id.clone(),
                Some(token),
                custom_headers.clone(),
            )
        })
        .await
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        _auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        self.with_token(|token| {
            self.http.get_stream(
                uri.clone(),
                session_id.clone(),
                last_event_id.clone(),
                Some(token),
                custom_headers.clone(),
            )
        })
        .await
    }
}
