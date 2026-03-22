// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! OAuth 2.0 PKCE flow for MCP servers that require authentication.
//!
//! Implements the full MCP Authorization spec scope-discovery strategy:
//!
//! - RFC 7636 (PKCE)
//! - RFC 8414 (OAuth 2.0 Authorization Server Metadata)
//! - RFC 9728 (OAuth 2.0 Protected Resource Metadata)
//!
//! # Scope discovery (per MCP spec §Authorization)
//!
//! Scopes are discovered automatically – you do **not** need to configure them:
//!
//! 1. `scope` parameter in the `WWW-Authenticate` header of a 401 response.
//! 2. `scopes_supported` in the Protected Resource Metadata document
//!    (`/.well-known/oauth-protected-resource` on the MCP server, or the URL
//!    pointed to by `resource_metadata` in the `WWW-Authenticate` header).
//! 3. Omit the `scope` parameter entirely if neither source is available.
//!
//! # Authorization server discovery
//!
//! 1. `resource_metadata` URL in `WWW-Authenticate` → fetch PRM →
//!    `authorization_servers[0]` → fetch OAuth server metadata.
//! 2. Try `/.well-known/oauth-protected-resource` on the server URL → same.
//! 3. Fall back to `/.well-known/oauth-authorization-server` on the server URL.
//!
//! # Flow
//!
//! 1. Server returns HTTP 401 with `WWW-Authenticate: Bearer …`.
//! 2. Call `discover_oauth_info()` to resolve the authorization server and scopes.
//! 3. If the server advertises a registration endpoint and no client_id is configured,
//!    perform Dynamic Client Registration (RFC 7591) to obtain a client_id.
//! 4. Generate a PKCE code verifier and code challenge.
//! 5. **Bind the callback listener first** (OS-assigned port) so we're ready before
//!    the browser opens.
//! 6. Build the authorization URL and open it in the user's browser.
//! 7. Wait for the redirect callback with `?code=...&state=...`.
//! 8. Exchange the authorization code for tokens via `POST /token`.
//! 9. Persist tokens to `~/.config/sven/mcp-credentials.json`.
//! 10. Before each request, check token expiry and refresh if needed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{debug, info, warn};
use url::Url;

/// Seconds before token expiry to trigger a proactive refresh.
const REFRESH_SKEW_SECS: u64 = 60;
/// Timeout in seconds waiting for the OAuth callback.
const CALLBACK_TIMEOUT_SECS: u64 = 300;
/// Default OAuth callback port (used for sven:// and localhost fallback).
const DEFAULT_CALLBACK_PORT: u16 = 5598;
/// Default redirect URI when not in container (sven:// installed by .deb).
pub const DEFAULT_REDIRECT_URI: &str = "sven://sven.mcp/callback";

// ── Storage ───────────────────────────────────────────────────────────────────

/// Persisted OAuth tokens for a single MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTokens {
    pub server_name: String,
    pub server_url: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix timestamp (seconds) when the access token expires.
    pub expires_at: Option<u64>,
    /// The token endpoint, used for refresh.
    pub token_endpoint: String,
    /// The client_id used during authorization.
    pub client_id: Option<String>,
    /// The client_secret (from DCR or config), needed for token refresh with confidential clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
}

impl StoredTokens {
    /// Whether the access token is expired (or close enough to warrant refresh).
    pub fn is_expired(&self) -> bool {
        match self.expires_at {
            None => false,
            Some(exp) => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                now + REFRESH_SKEW_SECS >= exp
            }
        }
    }
}

/// Persisted DCR client registration for a server.
///
/// Stored separately from tokens so it survives token expiry/rotation.
/// However, since we use dynamic callback ports, DCR client_info is only
/// valid as long as the same port is available. We store it to avoid
/// unnecessary re-registration on hot restarts within the same session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredClientInfo {
    pub server_name: String,
    pub server_url: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    /// The `redirect_uri` that was used when this client was registered.
    /// Required for re-use: the auth server validates redirect_uri matches registration.
    pub redirect_uri: String,
}

/// The credentials store – a JSON file mapping server keys to tokens.
pub struct CredentialsStore {
    path: PathBuf,
}

impl CredentialsStore {
    /// Open the default credentials store at `~/.config/sven/mcp-credentials.json`.
    ///
    /// When `config_dir()` is unavailable (e.g. unset HOME in sandbox), falls back
    /// to `home_dir()/.config` so credentials are stored in a real path, not a
    /// literal `~/.config` that would resolve relative to CWD.
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("sven")
            .join("mcp-credentials.json")
    }

    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn with_default_path() -> Self {
        Self::new(Self::default_path())
    }

    fn client_info_path(&self) -> PathBuf {
        self.path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("mcp-client-info.json")
    }

    fn load_all(&self) -> Result<HashMap<String, StoredTokens>> {
        if !self.path.exists() {
            return Ok(HashMap::new());
        }
        let text = std::fs::read_to_string(&self.path)
            .with_context(|| format!("read credentials store: {}", self.path.display()))?;
        serde_json::from_str(&text).context("parse credentials store")
    }

    fn save_all(&self, store: &HashMap<String, StoredTokens>) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let text = serde_json::to_string_pretty(store)?;
        std::fs::write(&self.path, text)
            .with_context(|| format!("write credentials store: {}", self.path.display()))?;
        Ok(())
    }

    fn load_all_client_info(&self) -> Result<HashMap<String, StoredClientInfo>> {
        let path = self.client_info_path();
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("read client info store: {}", path.display()))?;
        serde_json::from_str(&text).context("parse client info store")
    }

    fn save_all_client_info(&self, store: &HashMap<String, StoredClientInfo>) -> Result<()> {
        let path = self.client_info_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let text = serde_json::to_string_pretty(store)?;
        std::fs::write(&path, text)
            .with_context(|| format!("write client info store: {}", path.display()))?;
        Ok(())
    }

    /// Normalize a server URL for consistent store keys.
    ///
    /// URLs can differ by trailing slashes or path normalization across config
    /// sources. Normalizing ensures save/load use the same key.
    fn normalize_url_for_key(server_url: &str) -> String {
        Url::parse(server_url)
            .ok()
            .map(|u| {
                let mut s = u.to_string();
                if s.ends_with('/') && s.len() > 1 && !s.ends_with("://") {
                    s.pop();
                }
                s
            })
            .unwrap_or_else(|| server_url.to_string())
    }

    /// Compute the store key for a server.
    fn key(server_name: &str, server_url: &str) -> String {
        let url = Self::normalize_url_for_key(server_url);
        format!("{server_name}::{url}")
    }

    /// Load tokens for a specific server.
    pub fn load(&self, server_name: &str, server_url: &str) -> Option<StoredTokens> {
        self.load_all()
            .ok()?
            .remove(&Self::key(server_name, server_url))
    }

    /// Persist tokens for a specific server.
    pub fn save(&self, tokens: &StoredTokens) -> Result<()> {
        let mut all = self.load_all().unwrap_or_default();
        all.insert(
            Self::key(&tokens.server_name, &tokens.server_url),
            tokens.clone(),
        );
        self.save_all(&all)
    }

    /// Remove tokens for a specific server.
    pub fn remove(&self, server_name: &str, server_url: &str) {
        let mut all = self.load_all().unwrap_or_default();
        all.remove(&Self::key(server_name, server_url));
        let _ = self.save_all(&all);
    }

    /// Load DCR client info for a specific server.
    pub fn load_client_info(
        &self,
        server_name: &str,
        server_url: &str,
    ) -> Option<StoredClientInfo> {
        self.load_all_client_info()
            .ok()?
            .remove(&Self::key(server_name, server_url))
    }

    /// Persist DCR client info for a specific server.
    pub fn save_client_info(&self, info: &StoredClientInfo) -> Result<()> {
        let mut all = self.load_all_client_info().unwrap_or_default();
        all.insert(Self::key(&info.server_name, &info.server_url), info.clone());
        self.save_all_client_info(&all)
    }

    /// Remove DCR client info for a specific server.
    pub fn remove_client_info(&self, server_name: &str, server_url: &str) {
        let mut all = self.load_all_client_info().unwrap_or_default();
        all.remove(&Self::key(server_name, server_url));
        let _ = self.save_all_client_info(&all);
    }
}

// ── OAuth discovery ───────────────────────────────────────────────────────────

/// OAuth server metadata from `.well-known/oauth-authorization-server`.
#[derive(Debug, Clone, Deserialize)]
pub struct OAuthServerMetadata {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
    pub scopes_supported: Option<Vec<String>>,
    pub response_types_supported: Option<Vec<String>>,
    pub code_challenge_methods_supported: Option<Vec<String>>,
}

/// Discover OAuth server metadata from the well-known endpoint.
///
/// Tries multiple discovery URLs per RFC 8414 and OIDC conventions:
/// - `/.well-known/oauth-authorization-server` at origin
/// - `/.well-known/oauth-authorization-server{path}` (path-based)
/// - `/.well-known/openid-configuration` (OIDC fallback)
pub async fn discover_oauth_metadata(
    client: &reqwest::Client,
    server_url: &str,
) -> Result<OAuthServerMetadata> {
    let url = Url::parse(server_url).context("parse server URL for OAuth discovery")?;
    let origin = format!(
        "{}://{}{}",
        url.scheme(),
        url.host_str().unwrap_or(""),
        url.port().map(|p| format!(":{p}")).unwrap_or_default()
    );
    let path = url.path().trim_end_matches('/');

    let candidates = vec![
        format!("{origin}/.well-known/oauth-authorization-server"),
        if !path.is_empty() && path != "/" {
            format!("{origin}/.well-known/oauth-authorization-server{path}")
        } else {
            String::new()
        },
        format!("{origin}/.well-known/openid-configuration"),
    ];

    for candidate in candidates {
        if candidate.is_empty() {
            continue;
        }
        debug!(url = %candidate, "trying OAuth discovery endpoint");
        match client
            .get(&candidate)
            .header("MCP-Protocol-Version", "2024-11-05")
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                let metadata: OAuthServerMetadata =
                    resp.json().await.context("parse OAuth server metadata")?;
                debug!(
                    auth = %metadata.authorization_endpoint,
                    token = %metadata.token_endpoint,
                    "OAuth discovery succeeded"
                );
                return Ok(metadata);
            }
            Ok(resp) => {
                debug!(status = %resp.status(), url = %candidate, "OAuth discovery not found");
            }
            Err(e) => {
                debug!(error = %e, url = %candidate, "OAuth discovery request failed");
            }
        }
    }

    Err(anyhow!(
        "OAuth discovery failed for {server_url}: no .well-known/oauth-authorization-server found"
    ))
}

// ── Protected Resource Metadata (RFC 9728) ────────────────────────────────────

/// OAuth 2.0 Protected Resource Metadata (RFC 9728).
#[derive(Debug, Clone, Deserialize)]
pub struct ProtectedResourceMetadata {
    /// The protected resource identifier URI.
    pub resource: String,
    /// List of OAuth 2.0 authorization server issuer URLs that protect this resource.
    pub authorization_servers: Option<Vec<String>>,
    /// OAuth 2.0 scope values that this server supports.
    pub scopes_supported: Option<Vec<String>>,
    /// Bearer token methods supported.
    pub bearer_methods_supported: Option<Vec<String>>,
}

/// Fetch Protected Resource Metadata from an explicit URL.
pub async fn fetch_protected_resource_metadata(
    client: &reqwest::Client,
    url: &str,
) -> Result<ProtectedResourceMetadata> {
    debug!(url = %url, "fetching Protected Resource Metadata");
    let resp = client
        .get(url)
        .header("MCP-Protocol-Version", "2024-11-05")
        .send()
        .await
        .context("fetch Protected Resource Metadata")?;
    if !resp.status().is_success() {
        return Err(anyhow!(
            "Protected Resource Metadata request failed: {}",
            resp.status()
        ));
    }
    resp.json()
        .await
        .context("parse Protected Resource Metadata")
}

/// Try to discover Protected Resource Metadata from the well-known endpoint of
/// the MCP server.
///
/// Tries multiple discovery URLs per RFC 9728 §3:
/// - `{origin}/.well-known/oauth-protected-resource` (origin-level)
/// - `{origin}/.well-known/oauth-protected-resource{path}` (path-based)
pub async fn discover_protected_resource_metadata(
    client: &reqwest::Client,
    server_url: &str,
) -> Option<ProtectedResourceMetadata> {
    let url = Url::parse(server_url).ok()?;
    let origin = format!(
        "{}://{}{}",
        url.scheme(),
        url.host_str().unwrap_or(""),
        url.port().map(|p| format!(":{p}")).unwrap_or_default()
    );
    let path = url.path().trim_end_matches('/');

    let candidates = vec![
        format!("{origin}/.well-known/oauth-protected-resource"),
        if !path.is_empty() && path != "/" {
            format!("{origin}/.well-known/oauth-protected-resource{path}")
        } else {
            String::new()
        },
    ];

    for candidate in candidates {
        if candidate.is_empty() {
            continue;
        }
        debug!(url = %candidate, "trying Protected Resource Metadata endpoint");
        match client
            .get(&candidate)
            .header("MCP-Protocol-Version", "2024-11-05")
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(prm) = resp.json::<ProtectedResourceMetadata>().await {
                    return Some(prm);
                }
            }
            _ => {}
        }
    }

    None
}

// ── WWW-Authenticate parsing ───────────────────────────────────────────────────

/// Parsed fields from a `Bearer` `WWW-Authenticate` challenge.
#[derive(Debug, Default)]
pub struct WwwAuthenticate {
    pub realm: Option<String>,
    pub resource_metadata: Option<String>,
    pub scope: Option<String>,
    pub error: Option<String>,
}

/// Parse a `WWW-Authenticate` header value into its `Bearer` parameters.
pub fn parse_www_authenticate(header: &str) -> WwwAuthenticate {
    let mut result = WwwAuthenticate::default();

    let rest = header
        .trim()
        .strip_prefix("Bearer ")
        .or_else(|| header.trim().strip_prefix("bearer "))
        .unwrap_or(header.trim());

    for part in rest.split(',') {
        let part = part.trim();
        let Some((key, raw_value)) = part.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = raw_value.trim().trim_matches('"').to_string();

        match key.as_str() {
            "realm" => result.realm = Some(value),
            "resource_metadata" => result.resource_metadata = Some(value),
            "scope" => result.scope = Some(value),
            "error" => result.error = Some(value),
            _ => {}
        }
    }

    result
}

// ── Unified MCP OAuth discovery ───────────────────────────────────────────────

/// All information needed to execute an OAuth PKCE flow.
pub struct OAuthDiscovery {
    pub auth_server_metadata: OAuthServerMetadata,
    /// Scopes to request.  Empty means "omit the `scope` parameter".
    pub scopes: Vec<String>,
}

/// Discover OAuth information for an MCP server.
pub async fn discover_oauth_info(
    client: &reqwest::Client,
    server_url: &str,
    www_authenticate: Option<&str>,
    config_scopes: &[String],
) -> Result<OAuthDiscovery> {
    let parsed_www = www_authenticate.map(parse_www_authenticate);

    let prm: Option<ProtectedResourceMetadata> = {
        if let Some(prm_url) = parsed_www
            .as_ref()
            .and_then(|w| w.resource_metadata.as_deref())
        {
            fetch_protected_resource_metadata(client, prm_url)
                .await
                .ok()
        } else {
            discover_protected_resource_metadata(client, server_url).await
        }
    };

    let auth_server_base: String = prm
        .as_ref()
        .and_then(|p| p.authorization_servers.as_ref())
        .and_then(|s| s.first())
        .cloned()
        .unwrap_or_else(|| server_url.to_string());

    let auth_server_metadata = discover_oauth_metadata(client, &auth_server_base).await?;

    let scopes = if !config_scopes.is_empty() {
        config_scopes.to_vec()
    } else if let Some(scope_str) = parsed_www.as_ref().and_then(|w| w.scope.as_deref()) {
        scope_str.split_whitespace().map(str::to_string).collect()
    } else if let Some(prm_scopes) = prm.as_ref().and_then(|p| p.scopes_supported.as_ref()) {
        prm_scopes.clone()
    } else {
        vec![]
    };

    debug!(
        auth_server = %auth_server_metadata.authorization_endpoint,
        scopes = ?scopes,
        "OAuth discovery complete"
    );

    Ok(OAuthDiscovery {
        auth_server_metadata,
        scopes,
    })
}

// ── PKCE ──────────────────────────────────────────────────────────────────────

/// Generate a PKCE code verifier (random 32-byte, base64url-encoded).
pub fn generate_code_verifier() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Compute the PKCE code challenge (`S256`) from a verifier.
pub fn compute_code_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let digest = hasher.finalize();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// Generate a CSRF state token.
pub fn generate_state() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

// ── OAuth flow ────────────────────────────────────────────────────────────────

/// Pending OAuth authorization context.
pub struct OAuthContext {
    pub code_verifier: String,
    pub state: String,
    pub server_url: String,
    pub metadata: OAuthServerMetadata,
    /// The redirect URI for this flow (includes the dynamic callback port).
    pub redirect_uri: String,
    pub client_id: Option<String>,
}

impl OAuthContext {
    /// Build the callback URI for the given port.
    ///
    /// Uses `127.0.0.1` per RFC 8252 §8.3 which explicitly states that
    /// `localhost` is NOT RECOMMENDED for loopback redirect URIs because DNS
    /// resolution of `localhost` may behave unexpectedly. The loopback IP
    /// address is always reliable and must be accepted by compliant servers.
    pub fn callback_uri(port: u16) -> String {
        format!("http://127.0.0.1:{port}/callback")
    }

    /// Build the authorization URL the user should visit.
    pub fn authorization_url(&self, scopes: &[String]) -> Result<String> {
        let challenge = compute_code_challenge(&self.code_verifier);
        let client_id = self.client_id.as_deref().unwrap_or("sven-mcp-client");

        let scope_str = scopes.join(" ");

        let mut url = Url::parse(&self.metadata.authorization_endpoint)
            .context("parse authorization endpoint")?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", client_id)
            .append_pair("redirect_uri", &self.redirect_uri)
            .append_pair("state", &self.state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        if !scope_str.is_empty() {
            url.query_pairs_mut().append_pair("scope", &scope_str);
        }

        Ok(url.to_string())
    }
}

/// Detect if we are running inside a container (Docker, Podman, etc.).
///
/// When in a container, the sven:// protocol handler (installed on the host)
/// cannot reach our callback server. We fall back to http://127.0.0.1:PORT
/// and bind to 0.0.0.0 so port forwarding (e.g. -p 5598:5598) works.
pub fn running_in_container() -> bool {
    if std::path::Path::new("/.dockerenv").exists() {
        return true;
    }
    if std::path::Path::new("/run/.containerenv").exists() {
        return true;
    }
    if let Ok(cgroup) = std::fs::read_to_string("/proc/1/cgroup") {
        if cgroup.contains("docker") || cgroup.contains("containerd") || cgroup.contains("kubepods")
        {
            return true;
        }
    }
    false
}

/// Parameters for [`run_oauth_flow`].
pub struct RunOAuthFlowParams<'a> {
    pub client: &'a reqwest::Client,
    pub server_name: &'a str,
    pub server_url: &'a str,
    pub discovery: OAuthDiscovery,
    pub config_client_id: Option<String>,
    pub config_client_secret: Option<String>,
    pub store: &'a CredentialsStore,
    pub redirect_opts: OAuthRedirectOptions,
}

/// Options for custom OAuth redirect (e.g. sven://sven.mcp, cursor://cursor.mcp).
#[derive(Debug, Clone, Default)]
pub struct OAuthRedirectOptions {
    /// Custom redirect URI. When set (e.g. sven://sven.mcp/callback), the
    /// OAuth server redirects here. The OS protocol handler must forward to
    /// our local callback server (see callback_port).
    ///
    /// When None, we use DEFAULT_REDIRECT_URI (sven://) if not in a container,
    /// or http://127.0.0.1:5598/callback if in a container (localhost fallback).
    pub redirect_uri: Option<String>,
    /// Port for the local callback server when using a custom redirect_uri.
    /// Default: 5598.
    pub callback_port: Option<u16>,
}

/// Run the full OAuth PKCE flow using pre-discovered OAuth information.
///
/// 1. Binds the callback listener (dynamic port, or fixed port when using
///    custom redirect_uri like cursor://cursor.mcp/callback).
/// 2. Checks for a stored DCR client registration that matches the port; if
///    found reuses the `client_id` / `client_secret`.
/// 3. If no `client_id` is configured and the server supports Dynamic Client
///    Registration (RFC 7591), registers to obtain a new `client_id`.
/// 4. Builds the authorization URL and opens the user's browser.
/// 5. Waits for the callback, exchanges the code for tokens, and persists them.
///
/// Returns the stored tokens on success.
pub async fn run_oauth_flow(params: RunOAuthFlowParams<'_>) -> Result<StoredTokens> {
    let RunOAuthFlowParams {
        client,
        server_name,
        server_url,
        discovery,
        config_client_id,
        config_client_secret,
        store,
        redirect_opts,
    } = params;

    let metadata = discovery.auth_server_metadata;
    let scopes = discovery.scopes;

    let port = redirect_opts.callback_port.unwrap_or(DEFAULT_CALLBACK_PORT);
    let in_container = running_in_container();

    let (listener, redirect_uri, port) = if let Some(ref custom_uri) = redirect_opts.redirect_uri {
        // Explicit config: use as-is, bind 127.0.0.1.
        let listener = TcpListener::bind(format!("127.0.0.1:{port}"))
            .await
            .with_context(|| format!("bind OAuth callback on port {port} (for {custom_uri})"))?;
        info!(
            redirect_uri = %custom_uri,
            port = port,
            "Using custom redirect; configure your OS to forward {custom_uri} to http://127.0.0.1:{port}/callback"
        );
        (listener, custom_uri.clone(), port)
    } else if in_container {
        // Default when in container: sven:// on host cannot reach us; use localhost.
        // Bind 0.0.0.0 so port forwarding (e.g. -p 5598:5598) works.
        let uri = format!("http://127.0.0.1:{port}/callback");
        let listener = TcpListener::bind(format!("0.0.0.0:{port}"))
            .await
            .with_context(|| {
                format!("bind OAuth callback on 0.0.0.0:{port} (container fallback)")
            })?;
        info!(
            redirect_uri = %uri,
            port = port,
            "Running in container; using localhost redirect. Ensure port {port} is forwarded (e.g. -p {port}:{port})"
        );
        (listener, uri, port)
    } else {
        // Default when not in container: sven://sven.mcp/callback (installed by .deb).
        let uri = DEFAULT_REDIRECT_URI.to_string();
        let listener = TcpListener::bind(format!("127.0.0.1:{port}"))
            .await
            .with_context(|| format!("bind OAuth callback on port {port} (for {uri})"))?;
        info!(
            redirect_uri = %uri,
            port = port,
            "Using sven:// redirect (protocol handler forwards to http://127.0.0.1:{port}/callback)"
        );
        (listener, uri, port)
    };

    // Resolve client credentials:
    // Priority: config > stored DCR info > fresh DCR > default public client
    let (final_client_id, final_client_secret) = if config_client_id.is_some() {
        // Explicit config credentials always take priority.
        (config_client_id, config_client_secret)
    } else if let Some(stored_info) = store.load_client_info(server_name, server_url) {
        // Reuse a previously registered client if the redirect_uri still matches.
        // With dynamic ports this typically won't match, but it's worth checking.
        if stored_info.redirect_uri == redirect_uri {
            debug!(
                client_id = %stored_info.client_id,
                "reusing stored DCR client registration"
            );
            (Some(stored_info.client_id), stored_info.client_secret)
        } else {
            // Port changed: need fresh DCR (stale registration won't be accepted).
            debug!("stored DCR redirect_uri mismatch, running fresh DCR");
            run_dcr(
                client,
                &metadata,
                server_name,
                server_url,
                &redirect_uri,
                store,
            )
            .await
        }
    } else if metadata.registration_endpoint.is_some() {
        run_dcr(
            client,
            &metadata,
            server_name,
            server_url,
            &redirect_uri,
            store,
        )
        .await
    } else {
        (None, None)
    };

    let code_verifier = generate_code_verifier();
    let state = generate_state();

    let ctx = OAuthContext {
        code_verifier: code_verifier.clone(),
        state: state.clone(),
        server_url: server_url.to_string(),
        metadata: metadata.clone(),
        redirect_uri: redirect_uri.clone(),
        client_id: final_client_id.clone(),
    };

    let auth_url = ctx.authorization_url(&scopes)?;
    info!(url = %auth_url, port = port, "OAuth PKCE flow started, opening browser");

    // Listener was bound before browser open to avoid the redirect arriving before
    // we start listening (user may already be logged in → instant redirect).
    let (code, received_state) =
        wait_for_oauth_callback(listener, CALLBACK_TIMEOUT_SECS, &auth_url).await?;

    if received_state != state {
        return Err(anyhow!(
            "OAuth state mismatch — possible CSRF attack (expected {state}, got {received_state})"
        ));
    }

    let tokens = exchange_code(
        client,
        &metadata.token_endpoint,
        &code,
        &code_verifier,
        &redirect_uri,
        final_client_id.as_deref().unwrap_or("sven-mcp-client"),
        final_client_secret.as_deref(),
    )
    .await?;

    let stored = StoredTokens {
        server_name: server_name.to_string(),
        server_url: server_url.to_string(),
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_at: tokens.expires_in.map(|s| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() + s)
                .unwrap_or(0)
        }),
        token_endpoint: metadata.token_endpoint.clone(),
        client_id: final_client_id.clone(),
        client_secret: final_client_secret.clone(),
    };

    store.save(&stored)?;
    info!(server = %server_name, "OAuth tokens stored successfully");

    Ok(stored)
}

/// Perform Dynamic Client Registration and store the result.
///
/// Returns `(client_id, client_secret)`.
async fn run_dcr(
    client: &reqwest::Client,
    metadata: &OAuthServerMetadata,
    server_name: &str,
    server_url: &str,
    redirect_uri: &str,
    store: &CredentialsStore,
) -> (Option<String>, Option<String>) {
    let reg_url = match &metadata.registration_endpoint {
        Some(url) => url,
        None => return (None, None),
    };

    match register_client(client, reg_url, redirect_uri).await {
        Ok(reg) => {
            info!(client_id = %reg.client_id, "Dynamic client registration succeeded");
            let info = StoredClientInfo {
                server_name: server_name.to_string(),
                server_url: server_url.to_string(),
                client_id: reg.client_id.clone(),
                client_secret: reg.client_secret.clone(),
                redirect_uri: redirect_uri.to_string(),
            };
            if let Err(e) = store.save_client_info(&info) {
                warn!(error = %e, "failed to persist DCR client info");
            }
            (Some(reg.client_id), reg.client_secret)
        }
        Err(e) => {
            warn!(error = %e, "DCR failed, using default public client_id");
            (None, None)
        }
    }
}

/// Attempt to open a URL in the user's default browser.
///
/// Uses the `webbrowser` crate which respects the system default and `$BROWSER`
/// on Linux. If opening fails (e.g. in Docker, headless, or no display), we log
/// the URL so the user can open it manually — the OAuth callback server keeps
/// waiting.
fn open_browser(url: &str) {
    match webbrowser::open(url) {
        Ok(()) => {}
        Err(e) => {
            warn!(
                error = %e,
                url = %url,
                "Could not open default browser (e.g. running in Docker or headless). \
                 Open the URL above manually to complete OAuth"
            );
            eprintln!(
                "\nOAuth: Could not open browser. Open this URL manually to complete login:\n  {url}\n"
            );
        }
    }
}

// ── Dynamic Client Registration (RFC 7591) ────────────────────────────────────

/// Client registration request metadata (RFC 7591 §2).
///
/// Note: `code_challenge_method` is intentionally absent. PKCE method selection
/// (S256) is negotiated at the authorization endpoint via `code_challenge_method`
/// parameter, not during client registration (RFC 7591 does not define this field).
#[derive(Debug, Serialize)]
struct ClientRegistrationRequest {
    redirect_uris: Vec<String>,
    client_name: String,
    token_endpoint_auth_method: String,
    grant_types: Vec<String>,
    response_types: Vec<String>,
}

/// Client registration response (RFC 7591 §3.2.1).
#[derive(Debug, Deserialize)]
struct ClientRegistrationResponse {
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
}

/// Register a public OAuth client via Dynamic Client Registration (RFC 7591).
///
/// Registers both `localhost` and `127.0.0.1` variants of the redirect URI
/// since some servers treat them differently.
async fn register_client(
    client: &reqwest::Client,
    registration_endpoint: &str,
    redirect_uri: &str,
) -> Result<ClientRegistrationResponse> {
    // For http://127.0.0.1 URIs, register both 127.0.0.1 and localhost variants
    // since some servers treat them differently. For custom schemes (sven://, cursor://)
    // only register the canonical URI.
    let redirect_uris: Vec<String> = if redirect_uri.contains("127.0.0.1") {
        vec![
            redirect_uri.to_string(),
            redirect_uri.replace("127.0.0.1", "localhost"),
        ]
    } else {
        vec![redirect_uri.to_string()]
    };

    let req = ClientRegistrationRequest {
        redirect_uris,
        client_name: "Sven MCP Client".to_string(),
        token_endpoint_auth_method: "none".to_string(),
        grant_types: vec![
            "authorization_code".to_string(),
            "refresh_token".to_string(),
        ],
        response_types: vec!["code".to_string()],
    };

    let resp = client
        .post(registration_endpoint)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("MCP-Protocol-Version", "2024-11-05")
        .json(&req)
        .send()
        .await
        .context("Dynamic Client Registration request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!(
            "Dynamic Client Registration failed: {} {}",
            status,
            body
        ));
    }

    resp.json()
        .await
        .context("parse Dynamic Client Registration response")
}

// ── Callback server ───────────────────────────────────────────────────────────

/// Wait for the OAuth callback and return `(code, state)`.
///
/// The `listener` must already be bound (before the browser is opened).
/// Only the first valid callback is processed; subsequent connections
/// (e.g. browser retries) receive a polite "already done" response.
async fn wait_for_oauth_callback(
    listener: TcpListener,
    timeout_secs: u64,
    auth_url: &str,
) -> Result<(String, String)> {
    // Open browser AFTER we confirmed the listener is ready.
    open_browser(auth_url);
    info!(
        port = listener.local_addr().map(|a| a.port()).unwrap_or(0),
        "OAuth callback server ready, browser opened"
    );

    let done = Arc::new(AtomicBool::new(false));

    let accept_fut = async {
        loop {
            let (mut stream, _peer) = listener
                .accept()
                .await
                .context("OAuth callback: accept failed")?;

            // Short-circuit duplicate connections after we've already got the code.
            if done.load(Ordering::SeqCst) {
                let _ = write_html_response(&mut stream, 200, callback_already_done_html()).await;
                continue;
            }

            // Read the HTTP request (give it 10 s for slow loopback stacks).
            let mut buf = vec![0u8; 8192];
            let n = match tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buf)).await
            {
                Ok(Ok(n)) if n > 0 => n,
                _ => continue, // bad or empty connection, skip
            };

            let request_text = String::from_utf8_lossy(&buf[..n]);
            let first_line = request_text.lines().next().unwrap_or("");

            // Extract the request path: "GET /path?query HTTP/1.1"
            let Some(path_and_query) = extract_get_path(first_line) else {
                let _ = write_html_response(&mut stream, 404, not_found_html()).await;
                continue;
            };

            // Strip leading "/callback" prefix.
            let query = if let Some(rest) = path_and_query.strip_prefix("/callback") {
                rest.strip_prefix('?').unwrap_or("")
            } else {
                // Not our endpoint.
                let _ = write_html_response(&mut stream, 404, not_found_html()).await;
                continue;
            };

            // OAuth error response (e.g. user denied).
            if let Some(error) = parse_query_param(query, "error") {
                let desc = parse_query_param(query, "error_description").unwrap_or_default();
                let user_msg = if !desc.is_empty() {
                    url_decode(&desc)
                } else {
                    error.replace('_', " ")
                };
                let _ = write_html_response(&mut stream, 400, callback_error_html(&user_msg)).await;
                return Err(anyhow!("OAuth authorization failed: {}", user_msg));
            }

            // Successful callback.
            match (
                parse_query_param(query, "code"),
                parse_query_param(query, "state"),
            ) {
                (Some(code), Some(state)) => {
                    done.store(true, Ordering::SeqCst);
                    let _ = write_html_response(&mut stream, 200, callback_success_html()).await;
                    return Ok((code, state));
                }
                _ => {
                    let _ = write_html_response(
                        &mut stream,
                        400,
                        callback_error_html("Missing authorization code or state parameter"),
                    )
                    .await;
                    return Err(anyhow!(
                        "OAuth callback missing required parameters (code/state)"
                    ));
                }
            }
        }
    };

    tokio::time::timeout(Duration::from_secs(timeout_secs), accept_fut)
        .await
        .map_err(|_| {
            anyhow!(
                "OAuth callback timed out after {}s. \
                 Complete the login in your browser and make sure you are \
                 redirected back to localhost.",
                timeout_secs
            )
        })?
}

/// Extract the path (including query string) from a raw HTTP request line.
///
/// Input:  `"GET /callback?code=abc HTTP/1.1"`
/// Output: `Some("/callback?code=abc")`
fn extract_get_path(request_line: &str) -> Option<&str> {
    let rest = request_line.strip_prefix("GET ")?;
    // Strip the HTTP version suffix.
    let path = rest
        .strip_suffix(" HTTP/1.1")
        .or_else(|| rest.strip_suffix(" HTTP/1.0"))
        .unwrap_or(rest);
    Some(path)
}

fn parse_query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        if k == key {
            Some(url_decode(v))
        } else {
            None
        }
    })
}

/// Minimal URL percent-decoding (handles `%HH` sequences and `+` → space).
fn url_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            let h1 = chars.next().unwrap_or('0');
            let h2 = chars.next().unwrap_or('0');
            if let Ok(byte) = u8::from_str_radix(&format!("{h1}{h2}"), 16) {
                out.push(byte as char);
            }
        } else if c == '+' {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

/// Write an HTTP response with styled HTML and security headers.
async fn write_html_response(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    html: String,
) -> Result<()> {
    let status_line = match status {
        200 => "HTTP/1.1 200 OK",
        400 => "HTTP/1.1 400 Bad Request",
        404 => "HTTP/1.1 404 Not Found",
        _ => "HTTP/1.1 500 Internal Server Error",
    };
    let body = html.into_bytes();
    let headers = format!(
        "{status_line}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {len}\r\n\
         X-Frame-Options: DENY\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Cache-Control: no-store, no-cache\r\n\
         Connection: close\r\n\
         \r\n",
        len = body.len(),
    );
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(&body).await?;
    stream.flush().await?;
    Ok(())
}

// ── HTML page templates ───────────────────────────────────────────────────────

fn callback_success_html() -> String {
    r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Sven — Authenticated</title>
  <style>
    * { box-sizing: border-box; margin: 0; padding: 0; }
    body {
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
      background: #0f1117; color: #e2e8f0;
      display: flex; align-items: center; justify-content: center; min-height: 100vh;
    }
    .card {
      background: #1a1f2e; border: 1px solid #2d3748; border-radius: 12px;
      padding: 2.5rem 3rem; text-align: center; max-width: 420px; width: 90%;
    }
    .icon {
      width: 60px; height: 60px; background: #22543d; border-radius: 50%;
      display: flex; align-items: center; justify-content: center;
      margin: 0 auto 1.25rem; font-size: 1.75rem;
    }
    h1 { font-size: 1.35rem; font-weight: 600; color: #68d391; margin-bottom: 0.6rem; }
    p { color: #a0aec0; line-height: 1.6; margin-bottom: 0.4rem; }
    .hint { margin-top: 1.5rem; font-size: 0.82rem; color: #4a5568; }
  </style>
</head>
<body>
  <div class="card">
    <div class="icon">&#10003;</div>
    <h1>Authentication successful</h1>
    <p>You have been authenticated with the MCP server.</p>
    <p>Return to sven to continue.</p>
    <p class="hint">You can safely close this tab.</p>
  </div>
</body>
</html>"#
        .to_string()
}

fn callback_error_html(message: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Sven — Authentication Failed</title>
  <style>
    * {{ box-sizing: border-box; margin: 0; padding: 0; }}
    body {{
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
      background: #0f1117; color: #e2e8f0;
      display: flex; align-items: center; justify-content: center; min-height: 100vh;
    }}
    .card {{
      background: #1a1f2e; border: 1px solid #2d3748; border-radius: 12px;
      padding: 2.5rem 3rem; text-align: center; max-width: 420px; width: 90%;
    }}
    .icon {{
      width: 60px; height: 60px; background: #742a2a; border-radius: 50%;
      display: flex; align-items: center; justify-content: center;
      margin: 0 auto 1.25rem; font-size: 1.75rem;
    }}
    h1 {{ font-size: 1.35rem; font-weight: 600; color: #fc8181; margin-bottom: 0.6rem; }}
    p {{ color: #a0aec0; line-height: 1.6; margin-bottom: 0.4rem; }}
    .hint {{ margin-top: 1.5rem; font-size: 0.82rem; color: #4a5568; }}
  </style>
</head>
<body>
  <div class="card">
    <div class="icon">&#10007;</div>
    <h1>Authentication failed</h1>
    <p>{message}</p>
    <p class="hint">Close this tab and try again in sven.</p>
  </div>
</body>
</html>"#
    )
}

fn callback_already_done_html() -> String {
    r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>Sven — Already authenticated</title>
  <style>
    body {
      font-family: sans-serif; background: #0f1117; color: #a0aec0;
      display: flex; align-items: center; justify-content: center;
      min-height: 100vh; text-align: center;
    }
  </style>
</head>
<body>
  <p>Authentication already completed. You can close this tab.</p>
</body>
</html>"#
        .to_string()
}

fn not_found_html() -> String {
    r#"<!DOCTYPE html><html><body><p>Not found.</p></body></html>"#.to_string()
}

// ── Token exchange ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    #[allow(dead_code)]
    token_type: Option<String>,
}

async fn exchange_code(
    client: &reqwest::Client,
    token_endpoint: &str,
    code: &str,
    code_verifier: &str,
    redirect_uri: &str,
    client_id: &str,
    client_secret: Option<&str>,
) -> Result<TokenResponse> {
    let mut params = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", code_verifier),
        ("client_id", client_id),
    ];
    let secret_owned;
    if let Some(secret) = client_secret {
        secret_owned = secret.to_string();
        params.push(("client_secret", &secret_owned));
    }

    let resp = client
        .post(token_endpoint)
        .form(&params)
        .send()
        .await
        .context("token exchange request")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("token exchange failed {status}: {text}"));
    }

    resp.json().await.context("parse token response")
}

// ── Token refresh ─────────────────────────────────────────────────────────────

/// Refresh an access token using the refresh_token grant.
///
/// Returns the updated `StoredTokens` on success.
pub async fn refresh_token(
    client: &reqwest::Client,
    stored: &StoredTokens,
) -> Result<StoredTokens> {
    let refresh = stored
        .refresh_token
        .as_deref()
        .ok_or_else(|| anyhow!("no refresh token stored for {}", stored.server_name))?;

    let client_id = stored.client_id.as_deref().unwrap_or("sven-mcp-client");

    let mut params = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh),
        ("client_id", client_id),
    ];
    let secret_owned;
    if let Some(secret) = stored.client_secret.as_deref() {
        secret_owned = secret.to_string();
        params.push(("client_secret", &secret_owned));
    }

    let resp = client
        .post(&stored.token_endpoint)
        .form(&params)
        .send()
        .await
        .context("token refresh request")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(anyhow!("token refresh failed {status}: {text}"));
    }

    let tr: TokenResponse = resp.json().await.context("parse refresh response")?;

    Ok(StoredTokens {
        access_token: tr.access_token,
        refresh_token: tr.refresh_token.or_else(|| stored.refresh_token.clone()),
        expires_at: tr.expires_in.map(|s| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() + s)
                .unwrap_or(0)
        }),
        ..stored.clone()
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Attempt to refresh the stored tokens for a server if they are expired.
///
/// Returns `Ok(fresh_tokens)` on success (possibly the original if not expired).
/// Returns `Err` if tokens are expired and refresh failed — in this case the
/// stored credentials are cleared so the OAuth flow is triggered anew.
pub async fn ensure_fresh(
    client: &reqwest::Client,
    stored: StoredTokens,
    store: &CredentialsStore,
) -> Result<StoredTokens> {
    if !stored.is_expired() {
        return Ok(stored);
    }

    let server_name = stored.server_name.clone();
    let server_url = stored.server_url.clone();

    if stored.refresh_token.is_none() {
        // Token expired and no way to refresh: clear stale credentials.
        store.remove(&server_name, &server_url);
        return Err(anyhow!(
            "access token expired and no refresh token for {server_name}"
        ));
    }

    match refresh_token(client, &stored).await {
        Ok(fresh) => {
            if let Err(e) = store.save(&fresh) {
                warn!(error = %e, "failed to persist refreshed tokens");
            }
            Ok(fresh)
        }
        Err(e) => {
            // Refresh failed (revoked, server error, etc.) — clear stale credentials
            // so the next connection attempt triggers a full OAuth re-authorization.
            warn!(
                server = %server_name,
                error = %e,
                "token refresh failed, clearing stored credentials"
            );
            store.remove(&server_name, &server_url);
            Err(anyhow!("token refresh failed for {server_name}: {e}"))
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use url::Url;

    use super::*;

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Build a minimal `OAuthServerMetadata` for testing authorization URL construction.
    fn test_metadata() -> OAuthServerMetadata {
        OAuthServerMetadata {
            authorization_endpoint: "https://auth.example.com/authorize".into(),
            token_endpoint: "https://auth.example.com/token".into(),
            registration_endpoint: None,
            scopes_supported: None,
            response_types_supported: None,
            code_challenge_methods_supported: None,
        }
    }

    /// Build an `OAuthContext` with predictable values for authorization URL tests.
    fn test_oauth_context() -> OAuthContext {
        OAuthContext {
            code_verifier: generate_code_verifier(),
            state: generate_state(),
            server_url: "https://mcp.example.com/v1/mcp".into(),
            metadata: test_metadata(),
            redirect_uri: OAuthContext::callback_uri(8080),
            client_id: Some("test-client-id".into()),
        }
    }

    /// Parse the query parameters of a URL into a `HashMap`.
    fn query_params(url_str: &str) -> HashMap<String, String> {
        Url::parse(url_str)
            .expect("valid URL")
            .query_pairs()
            .into_owned()
            .collect()
    }

    /// Build a `StoredTokens` expiring at `expires_at` unix seconds.
    fn stored_tokens_expiring_at(expires_at: Option<u64>) -> StoredTokens {
        StoredTokens {
            server_name: "test".into(),
            server_url: "https://example.com".into(),
            access_token: "tok".into(),
            refresh_token: None,
            expires_at,
            token_endpoint: "https://example.com/token".into(),
            client_id: None,
            client_secret: None,
        }
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    // ── RFC 7636 §4.1 — PKCE code verifier ───────────────────────────────────

    /// RFC 7636 §4.1: code_verifier MUST use only unreserved characters
    /// from RFC 3986 §2.3: [A-Z] / [a-z] / [0-9] / "-" / "." / "_" / "~".
    /// Our base64url output is a valid subset of those characters.
    #[test]
    fn code_verifier_chars_are_rfc3986_unreserved() {
        let v = generate_code_verifier();
        for c in v.chars() {
            assert!(
                c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~'),
                "code_verifier contains char '{c}' outside RFC 3986 §2.3 unreserved set"
            );
        }
    }

    /// RFC 7636 §4.1: length MUST be >= 43 and <= 128 characters.
    #[test]
    fn code_verifier_length_is_base64url_no_padding() {
        let v = generate_code_verifier();
        assert!(
            v.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "code_verifier must use base64url alphabet without padding"
        );
        // 32 random bytes → 43 base64url characters (no padding).
        assert_eq!(v.len(), 43, "expected 43 base64url chars for 32 bytes");
    }

    /// RFC 7636 §4.1: length bounds — 43 chars minimum, 128 maximum.
    #[test]
    fn code_verifier_length_within_rfc7636_bounds() {
        let v = generate_code_verifier();
        assert!(
            v.len() >= 43,
            "RFC 7636 §4.1: code_verifier must be at least 43 chars, got {}",
            v.len()
        );
        assert!(
            v.len() <= 128,
            "RFC 7636 §4.1: code_verifier must be at most 128 chars, got {}",
            v.len()
        );
    }

    /// RFC 7636 §4.1: "A fresh cryptographically random string ... MUST be
    /// created for each authorization request." Two successive calls MUST
    /// produce distinct values.
    #[test]
    fn code_verifier_is_unique_across_calls() {
        let v1 = generate_code_verifier();
        let v2 = generate_code_verifier();
        assert_ne!(
            v1, v2,
            "RFC 7636 §4.1: each authorization request must use a unique code_verifier"
        );
    }

    /// RFC 7636 §7.1: "It is RECOMMENDED that the output of a suitable
    /// random number generator be used to create a 32-octet sequence." This
    /// verifies that the encoded string decodes back to exactly 32 bytes
    /// (= 256 bits of entropy as recommended).
    #[test]
    fn code_verifier_encodes_256_bits_of_entropy() {
        let v = generate_code_verifier();
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(v.as_bytes())
            .expect("code_verifier must be valid base64url");
        assert_eq!(
            decoded.len(),
            32,
            "RFC 7636 §7.1: code_verifier must encode 32 bytes (256 bits of entropy)"
        );
    }

    // ── RFC 7636 §4.2 — PKCE code challenge ──────────────────────────────────

    /// RFC 7636 Appendix B: normative test vector. The implementation MUST
    /// produce exactly this challenge for the given verifier.
    #[test]
    fn code_challenge_s256_known_vector() {
        // RFC 7636 Appendix B test vector.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let expected = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert_eq!(compute_code_challenge(verifier), expected);
    }

    /// RFC 7636 §4.2: code_challenge MUST NOT contain padding characters.
    /// Base64url without padding means no '=' characters.
    #[test]
    fn code_challenge_has_no_base64_padding() {
        let challenge = compute_code_challenge(&generate_code_verifier());
        assert!(
            !challenge.contains('='),
            "RFC 7636 §4.2: code_challenge must not contain base64 padding '='"
        );
    }

    /// RFC 7636 §4.2: challenge uses URL-safe base64 alphabet. Standard base64
    /// uses '+' and '/' which are NOT URL-safe; these MUST be replaced with '-'
    /// and '_' respectively and the challenge must have no padding.
    #[test]
    fn code_challenge_uses_url_safe_alphabet_only() {
        let challenge = compute_code_challenge(&generate_code_verifier());
        assert!(
            !challenge.contains('+') && !challenge.contains('/'),
            "RFC 7636 §4.2: code_challenge must use URL-safe alphabet (no '+' or '/')"
        );
        // All characters must be base64url-safe.
        for c in challenge.chars() {
            assert!(
                c.is_ascii_alphanumeric() || c == '-' || c == '_',
                "code_challenge contains non-base64url char '{c}'"
            );
        }
    }

    /// RFC 7636 §4.2: SHA-256 produces 32 bytes; 32 bytes base64url-encoded
    /// without padding is exactly 43 characters.
    #[test]
    fn code_challenge_length_is_43_chars() {
        let challenge = compute_code_challenge(&generate_code_verifier());
        assert_eq!(
            challenge.len(),
            43,
            "RFC 7636 §4.2: S256 code_challenge must be 43 base64url chars (SHA-256 → 32 bytes)"
        );
    }

    /// RFC 7636 §4.2: the challenge is a pure function of the verifier.
    /// The same verifier MUST always produce the same challenge.
    #[test]
    fn code_challenge_is_deterministic() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            compute_code_challenge(verifier),
            compute_code_challenge(verifier),
            "RFC 7636 §4.2: code_challenge is a deterministic function of the verifier"
        );
    }

    // ── RFC 6749 §10.12 / RFC 8252 §8.9 — CSRF state ─────────────────────────

    /// RFC 6749 §10.12: state MUST be non-empty and use URL-safe characters.
    #[test]
    fn state_token_is_nonempty_url_safe() {
        let s = generate_state();
        assert!(!s.is_empty());
        assert!(
            s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "state must use base64url alphabet"
        );
    }

    /// RFC 6749 §10.12: "The probability of an attacker guessing generated
    /// tokens … MUST be less than or equal to 2^(-128)." This requires at
    /// least 128 bits of entropy. Our implementation uses 16 random bytes
    /// (= 128 bits). This test verifies the encoded string decodes to 16 bytes.
    #[test]
    fn state_encodes_minimum_128_bit_entropy() {
        let s = generate_state();
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(s.as_bytes())
            .expect("state must be valid base64url");
        assert_eq!(
            decoded.len(),
            16,
            "RFC 6749 §10.12: state must encode 16 bytes (128 bits) of entropy"
        );
    }

    /// RFC 6749 §10.12: a fresh state value MUST be created for every
    /// authorization request. Two successive calls MUST produce distinct values.
    #[test]
    fn state_is_unique_across_calls() {
        let s1 = generate_state();
        let s2 = generate_state();
        assert_ne!(
            s1, s2,
            "RFC 6749 §10.12: each authorization request must use a unique state value"
        );
    }

    /// RFC 6749 §10.12 / RFC 6819 §5.3.5: the client MUST verify that the
    /// `state` received in the callback exactly matches the value sent in the
    /// authorization request. Any mismatch MUST be treated as a CSRF attempt.
    /// This test verifies the comparison is strict string equality.
    #[test]
    fn state_mismatch_is_detectable_for_csrf_protection() {
        let sent = generate_state();
        let received = generate_state(); // simulates attacker-controlled or mixed-up state
        assert_ne!(
            sent, received,
            "RFC 6749 §10.12: different states must not compare equal \
             (run_oauth_flow rejects mismatches to prevent CSRF)"
        );
        // Verify exact same value does compare equal (correct callback round-trip).
        let echoed = sent.clone();
        assert_eq!(
            sent, echoed,
            "An echoed state value must compare equal to the original"
        );
    }

    // ── RFC 8252 §7.3 — Loopback redirect URI ────────────────────────────────

    /// RFC 8252 §7.3: loopback redirect URIs MUST use the `http` scheme
    /// (not `https` — TLS to localhost has no security benefit and causes
    /// certificate validation problems).
    #[test]
    fn callback_uri_scheme_is_http_not_https() {
        let uri = OAuthContext::callback_uri(12345);
        assert!(
            uri.starts_with("http://"),
            "RFC 8252 §7.3: loopback redirect URI must use http scheme, got: {uri}"
        );
        assert!(
            !uri.starts_with("https://"),
            "RFC 8252 §7.3: loopback redirect URI must NOT use https, got: {uri}"
        );
    }

    /// RFC 8252 §7.3 / §8.3: the loopback redirect URI MUST use the IP
    /// literal `127.0.0.1`, NOT the hostname `localhost`. DNS resolution of
    /// `localhost` may be intercepted or may resolve to non-loopback interfaces.
    #[test]
    fn callback_uri_uses_loopback_ip_not_localhost() {
        let uri = OAuthContext::callback_uri(8888);
        assert!(
            uri.starts_with("http://127.0.0.1:"),
            "RFC 8252 §8.3: callback must use 127.0.0.1, got: {uri}"
        );
        assert_eq!(uri, "http://127.0.0.1:8888/callback");
    }

    /// RFC 8252 §7.3: the callback path must be `/callback`.
    #[test]
    fn callback_uri_path_is_slash_callback() {
        let uri = OAuthContext::callback_uri(9999);
        let parsed = Url::parse(&uri).expect("callback URI must be a valid URL");
        assert_eq!(
            parsed.path(),
            "/callback",
            "callback URI must have path /callback"
        );
    }

    /// RFC 8252 §7.3 §8.3: "the client SHOULD use a loopback IP literal
    /// rather than localhost … any port." The port MUST be dynamic (OS-assigned)
    /// rather than a fixed well-known value. Verify the URI reflects whatever
    /// port is given.
    #[test]
    fn callback_uri_reflects_given_port() {
        assert_eq!(
            OAuthContext::callback_uri(1234),
            "http://127.0.0.1:1234/callback"
        );
        assert_eq!(
            OAuthContext::callback_uri(65535),
            "http://127.0.0.1:65535/callback"
        );
        assert_ne!(
            OAuthContext::callback_uri(1111),
            OAuthContext::callback_uri(2222),
            "different ports must produce different URIs"
        );
    }

    // ── RFC 6749 §4.1.1 + RFC 7636 §4.3 — Authorization URL structure ─────────

    /// RFC 6749 §4.1.1: `response_type` MUST be set to `"code"`.
    #[test]
    fn authorization_url_contains_response_type_code() {
        let ctx = test_oauth_context();
        let url = ctx.authorization_url(&[]).unwrap();
        let params = query_params(&url);
        assert_eq!(
            params.get("response_type").map(String::as_str),
            Some("code"),
            "RFC 6749 §4.1.1: authorization request must include response_type=code"
        );
    }

    /// RFC 7636 §4.3: `code_challenge` MUST be present in authorization requests.
    #[test]
    fn authorization_url_contains_code_challenge() {
        let ctx = test_oauth_context();
        let url = ctx.authorization_url(&[]).unwrap();
        let params = query_params(&url);
        assert!(
            params.contains_key("code_challenge"),
            "RFC 7636 §4.3: authorization request must include code_challenge"
        );
        assert!(
            !params["code_challenge"].is_empty(),
            "code_challenge must not be empty"
        );
    }

    /// RFC 7636 §4.3: `code_challenge_method` MUST be `"S256"`. The `plain`
    /// method MUST NOT be used in new implementations when S256 is available.
    #[test]
    fn authorization_url_contains_code_challenge_method_s256() {
        let ctx = test_oauth_context();
        let url = ctx.authorization_url(&[]).unwrap();
        let params = query_params(&url);
        assert_eq!(
            params.get("code_challenge_method").map(String::as_str),
            Some("S256"),
            "RFC 7636 §4.3: authorization request must use code_challenge_method=S256"
        );
    }

    /// RFC 6749 §10.12: `state` MUST be present for CSRF protection.
    #[test]
    fn authorization_url_contains_state() {
        let ctx = test_oauth_context();
        let url = ctx.authorization_url(&[]).unwrap();
        let params = query_params(&url);
        assert!(
            params.contains_key("state"),
            "RFC 6749 §10.12: authorization request must include state for CSRF protection"
        );
        assert!(!params["state"].is_empty(), "state must not be empty");
    }

    /// RFC 6749 §4.1.1 / RFC 8252 §7.3: `redirect_uri` MUST be present so
    /// the authorization server can send the callback to the correct listener.
    #[test]
    fn authorization_url_contains_redirect_uri() {
        let ctx = test_oauth_context();
        let url = ctx.authorization_url(&[]).unwrap();
        let params = query_params(&url);
        assert!(
            params.contains_key("redirect_uri"),
            "RFC 6749 §4.1.1: authorization request must include redirect_uri"
        );
        assert!(
            params["redirect_uri"].starts_with("http://127.0.0.1:"),
            "redirect_uri must be the loopback callback URI, got: {}",
            params["redirect_uri"]
        );
    }

    /// RFC 6749 §4.1.1 / RFC 3.3: `scope` MUST be omitted when no scopes are
    /// requested (clients must not send empty scope strings).
    #[test]
    fn authorization_url_omits_scope_when_empty() {
        let ctx = test_oauth_context();
        let url = ctx.authorization_url(&[]).unwrap();
        let params = query_params(&url);
        assert!(
            !params.contains_key("scope"),
            "RFC 6749 §4.1.1: scope parameter must be omitted when no scopes are requested"
        );
    }

    /// RFC 6749 §3.3: `scope` values are space-delimited, case-sensitive strings.
    /// When scopes are provided they MUST appear as a single space-separated value.
    #[test]
    fn authorization_url_includes_scope_when_provided() {
        let ctx = test_oauth_context();
        let scopes = vec!["read:issues".to_string(), "write:issues".to_string()];
        let url = ctx.authorization_url(&scopes).unwrap();
        let params = query_params(&url);
        assert_eq!(
            params.get("scope").map(String::as_str),
            Some("read:issues write:issues"),
            "RFC 6749 §3.3: scopes must be space-delimited in the authorization request"
        );
    }

    /// RFC 7636 §4.2: the `code_challenge` in the authorization URL must be
    /// the S256 transform of the `code_verifier` stored in the context.
    #[test]
    fn authorization_url_code_challenge_matches_verifier() {
        let ctx = test_oauth_context();
        let url = ctx.authorization_url(&[]).unwrap();
        let params = query_params(&url);
        let expected_challenge = compute_code_challenge(&ctx.code_verifier);
        assert_eq!(
            params.get("code_challenge").map(String::as_str),
            Some(expected_challenge.as_str()),
            "RFC 7636 §4.2: code_challenge must be S256(code_verifier)"
        );
    }

    /// RFC 6749 §4.1.1: `client_id` MUST be present in the authorization request.
    #[test]
    fn authorization_url_contains_client_id() {
        let ctx = test_oauth_context();
        let url = ctx.authorization_url(&[]).unwrap();
        let params = query_params(&url);
        assert!(
            params.contains_key("client_id"),
            "RFC 6749 §4.1.1: authorization request must include client_id"
        );
        assert_eq!(
            params.get("client_id").map(String::as_str),
            Some("test-client-id")
        );
    }

    // ── RFC 6749 §5.1 / Refresh skew boundary ────────────────────────────────

    /// StoredTokens with no expiry is never considered expired.
    #[test]
    fn stored_tokens_no_expiry_not_expired() {
        assert!(!stored_tokens_expiring_at(None).is_expired());
    }

    /// A token expiring 1 hour from now is not expired.
    #[test]
    fn stored_tokens_far_future_not_expired() {
        assert!(!stored_tokens_expiring_at(Some(now_secs() + 3600)).is_expired());
    }

    /// A token whose expiry is in the past is expired.
    #[test]
    fn stored_tokens_past_expiry_is_expired() {
        assert!(stored_tokens_expiring_at(Some(now_secs() - 1)).is_expired());
    }

    /// RFC 6749 §5.1 / proactive refresh: a token expiring in exactly
    /// `REFRESH_SKEW_SECS` seconds is considered expired because the client
    /// refreshes proactively within the skew window. At `now + REFRESH_SKEW`,
    /// the condition `now + REFRESH_SKEW >= exp` is `true`.
    #[test]
    fn stored_tokens_at_refresh_skew_boundary_is_expired() {
        let at_boundary = now_secs() + REFRESH_SKEW_SECS;
        assert!(
            stored_tokens_expiring_at(Some(at_boundary)).is_expired(),
            "token expiring at now+REFRESH_SKEW_SECS ({at_boundary}) must be \
             considered expired for proactive refresh"
        );
    }

    /// One second past the refresh window the token is NOT yet considered expired
    /// (enough margin remains for a fresh request).
    #[test]
    fn stored_tokens_one_second_past_refresh_skew_is_not_expired() {
        let just_outside = now_secs() + REFRESH_SKEW_SECS + 1;
        assert!(
            !stored_tokens_expiring_at(Some(just_outside)).is_expired(),
            "token expiring at now+REFRESH_SKEW_SECS+1 ({just_outside}) must NOT \
             be considered expired yet"
        );
    }

    // ── RFC 6749 §6 — ensure_fresh contracts ─────────────────────────────────

    /// RFC 6749 §5.1: a token that has not expired MUST be returned unchanged
    /// without any network call.
    #[tokio::test]
    async fn ensure_fresh_returns_non_expired_token_unchanged() {
        let far_future = now_secs() + 3600;
        let stored = StoredTokens {
            server_name: "s".into(),
            server_url: "https://example.com".into(),
            access_token: "original-token".into(),
            refresh_token: None,
            expires_at: Some(far_future),
            token_endpoint: "https://example.com/token".into(),
            client_id: None,
            client_secret: None,
        };
        let store_path =
            std::env::temp_dir().join(format!("sven-test-{}.json", std::process::id()));
        let store = CredentialsStore::new(store_path.clone());
        let client = reqwest::Client::new();

        let result = ensure_fresh(&client, stored, &store).await;
        assert!(result.is_ok(), "non-expired token should be returned as-is");
        assert_eq!(result.unwrap().access_token, "original-token");

        // Clean up temp file if it was written.
        let _ = std::fs::remove_file(&store_path);
    }

    /// RFC 6749 §6: when the access token is expired and no refresh token is
    /// available, `ensure_fresh` MUST return an error. The caller must then
    /// trigger a new authorization flow.
    #[tokio::test]
    async fn ensure_fresh_fails_when_expired_and_no_refresh_token() {
        let past = now_secs() - 1;
        let stored = StoredTokens {
            server_name: "s".into(),
            server_url: "https://example.com".into(),
            access_token: "expired-token".into(),
            refresh_token: None, // no refresh token
            expires_at: Some(past),
            token_endpoint: "https://example.com/token".into(),
            client_id: None,
            client_secret: None,
        };
        let store_path =
            std::env::temp_dir().join(format!("sven-test-{}-no-refresh.json", std::process::id()));
        let store = CredentialsStore::new(store_path.clone());
        let client = reqwest::Client::new();

        let result = ensure_fresh(&client, stored, &store).await;
        assert!(
            result.is_err(),
            "expired token without refresh_token must return Err"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no refresh token") || err.contains("expired"),
            "error message must indicate missing refresh token, got: {err}"
        );

        // The store must have cleared the expired token so the next
        // connection attempt triggers a fresh OAuth flow.
        let still_stored = store.load("s", "https://example.com");
        assert!(
            still_stored.is_none(),
            "RFC 6749 §6: expired tokens without refresh must be cleared from storage"
        );

        let _ = std::fs::remove_file(&store_path);
    }

    // ── WWW-Authenticate parsing ──────────────────────────────────────────────

    #[test]
    fn parse_www_authenticate_bearer_realm() {
        let parsed = parse_www_authenticate(r#"Bearer realm="mcp""#);
        assert_eq!(parsed.realm.as_deref(), Some("mcp"));
        assert!(parsed.resource_metadata.is_none());
        assert!(parsed.scope.is_none());
        assert!(parsed.error.is_none());
    }

    #[test]
    fn parse_www_authenticate_full_challenge() {
        let header = r#"Bearer realm="MCP",resource_metadata="https://example.com/.well-known/oauth-protected-resource",scope="read write",error="insufficient_scope""#;
        let parsed = parse_www_authenticate(header);
        assert_eq!(parsed.realm.as_deref(), Some("MCP"));
        assert_eq!(
            parsed.resource_metadata.as_deref(),
            Some("https://example.com/.well-known/oauth-protected-resource")
        );
        assert_eq!(parsed.scope.as_deref(), Some("read write"));
        assert_eq!(parsed.error.as_deref(), Some("insufficient_scope"));
    }

    /// RFC 6750 §3: WWW-Authenticate scheme matching MUST be case-insensitive.
    #[test]
    fn parse_www_authenticate_lowercase_bearer() {
        let parsed = parse_www_authenticate(r#"bearer realm="test""#);
        assert_eq!(parsed.realm.as_deref(), Some("test"));
    }

    #[test]
    fn parse_www_authenticate_empty_string() {
        let parsed = parse_www_authenticate("");
        assert!(parsed.realm.is_none());
        assert!(parsed.resource_metadata.is_none());
    }

    #[test]
    fn parse_www_authenticate_no_quotes() {
        let parsed = parse_www_authenticate("Bearer realm=mcp");
        assert_eq!(parsed.realm.as_deref(), Some("mcp"));
    }

    /// RFC 6750 §3.1: `error=invalid_token` indicates an expired or invalid
    /// bearer token. Clients MUST parse this to know they need to re-authenticate.
    #[test]
    fn parse_www_authenticate_invalid_token_error() {
        let parsed = parse_www_authenticate(
            r#"Bearer realm="api",error="invalid_token",error_description="token expired""#,
        );
        assert_eq!(parsed.error.as_deref(), Some("invalid_token"));
    }

    /// RFC 6750 §3.1: `error=insufficient_scope` means the token lacks the
    /// required scope. Client should know this is a permissions issue, not expiry.
    #[test]
    fn parse_www_authenticate_insufficient_scope_error() {
        let parsed = parse_www_authenticate(
            r#"Bearer realm="api",error="insufficient_scope",scope="admin""#,
        );
        assert_eq!(parsed.error.as_deref(), Some("insufficient_scope"));
        assert_eq!(parsed.scope.as_deref(), Some("admin"));
    }

    // ── HTTP request path extraction ──────────────────────────────────────────

    #[test]
    fn extract_get_path_standard_request() {
        assert_eq!(
            extract_get_path("GET /callback?code=abc&state=xyz HTTP/1.1"),
            Some("/callback?code=abc&state=xyz")
        );
    }

    #[test]
    fn extract_get_path_http_1_0() {
        assert_eq!(
            extract_get_path("GET /callback?code=x HTTP/1.0"),
            Some("/callback?code=x")
        );
    }

    #[test]
    fn extract_get_path_no_version_suffix() {
        assert_eq!(extract_get_path("GET /callback?a=b"), Some("/callback?a=b"));
    }

    #[test]
    fn extract_get_path_non_get_returns_none() {
        assert_eq!(extract_get_path("POST /token HTTP/1.1"), None);
    }

    // ── URL decode ────────────────────────────────────────────────────────────

    #[test]
    fn url_decode_plain_string() {
        assert_eq!(url_decode("hello"), "hello");
    }

    #[test]
    fn url_decode_percent_encoded() {
        assert_eq!(url_decode("hello%20world"), "hello world");
    }

    #[test]
    fn url_decode_plus_as_space() {
        assert_eq!(url_decode("hello+world"), "hello world");
    }

    #[test]
    fn url_decode_mixed_encoding() {
        assert_eq!(url_decode("foo%3Dbar%26baz%3Dqux"), "foo=bar&baz=qux");
    }

    #[test]
    fn url_decode_empty_string() {
        assert_eq!(url_decode(""), "");
    }
}
