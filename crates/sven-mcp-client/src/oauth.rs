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
//! 5. **Start the local callback server first** (bind to 127.0.0.1:19876).
//! 6. Build the authorization URL and open it in the user's browser.
//! 7. Wait for the redirect callback with `?code=...&state=...`.
//! 8. Exchange the authorization code for tokens via `POST /token`.
//! 9. Persist tokens to `~/.config/sven/mcp-credentials.json`.
//! 10. Before each request, check token expiry and refresh if needed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{debug, info, warn};
use url::Url;

/// PKCE OAuth callback port.
const CALLBACK_PORT: u16 = 19876;
/// Seconds before token expiry to trigger a refresh.
const REFRESH_SKEW_SECS: u64 = 30;
/// Timeout in seconds waiting for the OAuth callback.
const CALLBACK_TIMEOUT_SECS: u64 = 300;

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

/// The credentials store – a JSON file mapping server keys to tokens.
pub struct CredentialsStore {
    path: PathBuf,
}

impl CredentialsStore {
    /// Open the default credentials store at `~/.config/sven/mcp-credentials.json`.
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join("sven")
            .join("mcp-credentials.json")
    }

    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn with_default_path() -> Self {
        Self::new(Self::default_path())
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

    /// Compute the store key for a server.
    fn key(server_name: &str, server_url: &str) -> String {
        format!("{server_name}::{server_url}")
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
/// Tries two paths per RFC 8414:
/// - `/.well-known/oauth-authorization-server` (root)
/// - `/.well-known/oauth-authorization-server/{base_path}` (path-based)
pub async fn discover_oauth_metadata(
    client: &reqwest::Client,
    server_url: &str,
) -> Result<OAuthServerMetadata> {
    let url = Url::parse(server_url).context("parse server URL for OAuth discovery")?;
    let base = format!("{}://{}", url.scheme(), url.host_str().unwrap_or(""));
    let port_suffix = url.port().map(|p| format!(":{p}")).unwrap_or_default();
    let base = format!("{base}{port_suffix}");

    let candidates = vec![
        format!("{base}/.well-known/oauth-authorization-server"),
        format!("{server_url}/.well-known/oauth-authorization-server"),
    ];

    for candidate in candidates {
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
///
/// Served at `/.well-known/oauth-protected-resource` on the MCP server (or at
/// the URL named in a `resource_metadata` parameter of a `WWW-Authenticate`
/// header).  The most relevant fields for MCP scope discovery are
/// `authorization_servers` and `scopes_supported`.
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
/// Tries `/.well-known/oauth-protected-resource` at the server's origin.
/// Returns `None` (not an error) when the document is simply absent.
pub async fn discover_protected_resource_metadata(
    client: &reqwest::Client,
    server_url: &str,
) -> Option<ProtectedResourceMetadata> {
    let url = Url::parse(server_url).ok()?;
    let base = format!("{}://{}", url.scheme(), url.host_str().unwrap_or(""));
    let port_suffix = url.port().map(|p| format!(":{p}")).unwrap_or_default();
    let well_known = format!("{base}{port_suffix}/.well-known/oauth-protected-resource");

    debug!(url = %well_known, "trying Protected Resource Metadata endpoint");
    let resp = client
        .get(&well_known)
        .header("MCP-Protocol-Version", "2024-11-05")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json().await.ok()
}

// ── WWW-Authenticate parsing ───────────────────────────────────────────────────

/// Parsed fields from a `Bearer` `WWW-Authenticate` challenge.
///
/// Example header:
/// ```text
/// WWW-Authenticate: Bearer realm="mcp",
///   resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource",
///   scope="read write"
/// ```
#[derive(Debug, Default)]
pub struct WwwAuthenticate {
    /// `realm` – usually the authorization server URL or a human-readable name.
    pub realm: Option<String>,
    /// `resource_metadata` – URL of the RFC 9728 Protected Resource Metadata document.
    pub resource_metadata: Option<String>,
    /// `scope` – space-separated required scopes.
    pub scope: Option<String>,
    /// `error` – set when the token was rejected.
    pub error: Option<String>,
}

/// Parse a `WWW-Authenticate` header value into its `Bearer` parameters.
///
/// Handles both quoted and unquoted parameter values; ignores unknown fields.
pub fn parse_www_authenticate(header: &str) -> WwwAuthenticate {
    let mut result = WwwAuthenticate::default();

    // Strip optional "Bearer " prefix (case-insensitive)
    let rest = header
        .trim()
        .strip_prefix("Bearer ")
        .or_else(|| header.trim().strip_prefix("bearer "))
        .unwrap_or(header.trim());

    // Split on commas, trim each token, parse key=value pairs.
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

/// All information needed to execute an OAuth PKCE flow, discovered from the
/// MCP server rather than requiring manual configuration.
pub struct OAuthDiscovery {
    /// OAuth authorization server metadata (endpoints, methods supported, …).
    pub auth_server_metadata: OAuthServerMetadata,
    /// Scopes to request.  Empty means "omit the `scope` parameter".
    pub scopes: Vec<String>,
}

/// Discover OAuth information for an MCP server using the full MCP
/// Authorization spec priority chain.
///
/// # Priority order
///
/// **Authorization server:**
/// 1. `resource_metadata` URL in `www_authenticate` → fetch PRM →
///    `authorization_servers[0]` → fetch OAuth server metadata.
/// 2. Try `/.well-known/oauth-protected-resource` on `server_url` → same.
/// 3. Fall back to `/.well-known/oauth-authorization-server` on `server_url`.
///
/// **Scopes** (per MCP spec §"Scope Selection Strategy"):
/// 1. `config_scopes` if non-empty (user explicitly configured them).
/// 2. `scope` from `www_authenticate` header.
/// 3. `scopes_supported` from Protected Resource Metadata.
/// 4. Omit scope parameter (empty Vec).
pub async fn discover_oauth_info(
    client: &reqwest::Client,
    server_url: &str,
    www_authenticate: Option<&str>,
    config_scopes: &[String],
) -> Result<OAuthDiscovery> {
    let parsed_www = www_authenticate.map(parse_www_authenticate);

    // Step 1: resolve the Protected Resource Metadata.
    let prm: Option<ProtectedResourceMetadata> = {
        if let Some(prm_url) = parsed_www
            .as_ref()
            .and_then(|w| w.resource_metadata.as_deref())
        {
            fetch_protected_resource_metadata(client, prm_url)
                .await
                .ok()
                .or_else(|| {
                    debug!(url = %prm_url, "PRM fetch failed, falling back");
                    None
                })
        } else {
            discover_protected_resource_metadata(client, server_url).await
        }
    };

    // Step 2: resolve the authorization server base URL.
    let auth_server_base: String = prm
        .as_ref()
        .and_then(|p| p.authorization_servers.as_ref())
        .and_then(|s| s.first())
        .cloned()
        .unwrap_or_else(|| server_url.to_string());

    // Step 3: fetch OAuth authorization server metadata.
    let auth_server_metadata = discover_oauth_metadata(client, &auth_server_base).await?;

    // Step 4: determine scopes.
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
    pub redirect_uri: String,
    pub client_id: Option<String>,
}

impl OAuthContext {
    /// The redirect URI for this callback.
    pub fn callback_uri() -> String {
        format!("http://127.0.0.1:{CALLBACK_PORT}/mcp/oauth/callback")
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

/// Run the full OAuth PKCE flow using pre-discovered OAuth information.
///
/// 1. If no client_id is configured and the server supports Dynamic Client
///    Registration (RFC 7591), registers to obtain a client_id.
/// 2. Builds the authorization URL from `discovery`.
/// 3. **Starts the callback server first** so it is listening before the user
///    is redirected (avoids race where redirect arrives before we're ready).
/// 4. Opens the user's browser (via `xdg-open` or `open`).
/// 5. Waits for the callback on `127.0.0.1:19876`.
/// 6. Exchanges the code for tokens.
/// 7. Persists tokens to the credential store.
///
/// Returns the stored tokens on success.
pub async fn run_oauth_flow(
    client: &reqwest::Client,
    server_name: &str,
    server_url: &str,
    discovery: OAuthDiscovery,
    client_id: Option<String>,
    store: &CredentialsStore,
) -> Result<StoredTokens> {
    let metadata = discovery.auth_server_metadata;
    let scopes = discovery.scopes;

    // Try Dynamic Client Registration when no client_id is configured and the
    // server advertises a registration endpoint (per MCP spec §Authorization).
    let client_id = if client_id.is_some() {
        client_id
    } else if let Some(ref reg_url) = metadata.registration_endpoint {
        match register_client(client, reg_url).await {
            Ok(reg) => {
                info!(client_id = %reg.client_id, "Dynamic client registration succeeded");
                Some(reg.client_id)
            }
            Err(e) => {
                warn!(error = %e, "Dynamic client registration failed, falling back to default client_id");
                None
            }
        }
    } else {
        None
    };

    let code_verifier = generate_code_verifier();
    let state = generate_state();
    let redirect_uri = OAuthContext::callback_uri();

    let ctx = OAuthContext {
        code_verifier: code_verifier.clone(),
        state: state.clone(),
        server_url: server_url.to_string(),
        metadata: metadata.clone(),
        redirect_uri: redirect_uri.clone(),
        client_id: client_id.clone(),
    };

    let auth_url = ctx.authorization_url(&scopes)?;
    info!(url = %auth_url, "Opening browser for MCP OAuth authentication");

    // Start callback server BEFORE opening browser so we're listening when the
    // auth server redirects (user may already be logged in and redirect immediately).
    let (code, received_state) =
        wait_for_callback_with_browser_open(CALLBACK_TIMEOUT_SECS, &auth_url).await?;

    if received_state != state {
        return Err(anyhow!(
            "OAuth state mismatch — possible CSRF attack (expected {state}, got {received_state})"
        ));
    }

    // Exchange code for tokens.
    let tokens = exchange_code(
        client,
        &metadata.token_endpoint,
        &code,
        &code_verifier,
        &redirect_uri,
        client_id.as_deref().unwrap_or("sven-mcp-client"),
        client_id.as_deref(),
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
        client_id,
    };

    store.save(&stored)?;
    info!(server = %server_name, "OAuth tokens stored");

    Ok(stored)
}

/// Attempt to open a URL in the user's browser.
fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(opener).arg(url).spawn();
}

// ── Dynamic Client Registration (RFC 7591) ────────────────────────────────────

/// Client registration request metadata (RFC 7591 §2).
#[derive(Debug, Serialize)]
struct ClientRegistrationRequest {
    redirect_uris: Vec<String>,
    client_name: String,
    token_endpoint_auth_method: String,
    grant_types: Vec<String>,
    response_types: Vec<String>,
    code_challenge_method: String,
}

/// Client registration response (RFC 7591 §3.2.1).
#[derive(Debug, Deserialize)]
struct ClientRegistrationResponse {
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
}

/// Register a public OAuth client via Dynamic Client Registration (RFC 7591).
async fn register_client(
    client: &reqwest::Client,
    registration_endpoint: &str,
) -> Result<ClientRegistrationResponse> {
    let redirect_uri = OAuthContext::callback_uri();
    // Register both 127.0.0.1 and localhost; some servers treat them differently.
    let redirect_uris = vec![
        redirect_uri.clone(),
        redirect_uri.replace("127.0.0.1", "localhost"),
    ];
    let req = ClientRegistrationRequest {
        redirect_uris,
        client_name: "Sven".to_string(),
        token_endpoint_auth_method: "none".to_string(),
        grant_types: vec![
            "authorization_code".to_string(),
            "refresh_token".to_string(),
        ],
        response_types: vec!["code".to_string()],
        code_challenge_method: "S256".to_string(),
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
/// Binds the callback server **before** opening the browser so we're listening
/// when the auth server redirects (avoids race if user is already logged in).
async fn wait_for_callback_with_browser_open(
    timeout_secs: u64,
    auth_url: &str,
) -> Result<(String, String)> {
    let listener = TcpListener::bind(format!("127.0.0.1:{CALLBACK_PORT}"))
        .await
        .with_context(|| format!("bind OAuth callback server on port {CALLBACK_PORT}"))?;

    info!("OAuth callback server listening on 127.0.0.1:{CALLBACK_PORT}, opening browser");

    // Open browser only after we're listening for the redirect.
    open_browser(auth_url);

    let accept_fut = async {
        loop {
            let (mut stream, _) = listener.accept().await?;
            let mut buf = vec![0u8; 4096];
            let n = stream.read(&mut buf).await?;
            let request = String::from_utf8_lossy(&buf[..n]);

            // Parse the GET request line.
            let first_line = request.lines().next().unwrap_or("");
            // "GET /mcp/oauth/callback?code=...&state=... HTTP/1.1"
            if let Some(path_with_query) = first_line.strip_prefix("GET ").and_then(|s| {
                s.strip_suffix(" HTTP/1.1")
                    .or_else(|| s.strip_suffix(" HTTP/1.0"))
            }) {
                if let Some(query) = path_with_query
                    .strip_prefix("/mcp/oauth/callback")
                    .and_then(|s| s.strip_prefix('?'))
                {
                    if let Some((code, state)) = parse_callback_query(query) {
                        // Send success response.
                        let resp = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n\
                            <html><body><h1>Authentication successful!</h1>\
                            <p>You can close this window and return to sven.</p></body></html>";
                        let _ = stream.write_all(resp.as_bytes()).await;
                        return Ok::<(String, String), anyhow::Error>((code, state));
                    } else if query.contains("error=") {
                        let error = parse_query_param(query, "error")
                            .unwrap_or_else(|| "unknown".to_string());
                        let desc =
                            parse_query_param(query, "error_description").unwrap_or_default();
                        let resp = format!(
                            "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\n\r\n\
                            <html><body><h1>Authentication failed</h1>\
                            <p>Error: {error}</p><p>{desc}</p></body></html>"
                        );
                        let _ = stream.write_all(resp.as_bytes()).await;
                        return Err(anyhow!("OAuth error {error}: {desc}"));
                    }
                }
            }

            // Not the callback path, send 404 and keep listening.
            let resp = "HTTP/1.1 404 Not Found\r\n\r\n";
            let _ = stream.write_all(resp.as_bytes()).await;
        }
    };

    tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), accept_fut)
        .await
        .map_err(|_| anyhow!("OAuth callback timed out after {timeout_secs}s. Complete the login in your browser and ensure you are redirected back to this application."))?
}

fn parse_callback_query(query: &str) -> Option<(String, String)> {
    let code = parse_query_param(query, "code")?;
    let state = parse_query_param(query, "state")?;
    Some((code, state))
}

fn parse_query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        if k == key {
            Some(urlencoding_decode(v))
        } else {
            None
        }
    })
}

/// Minimal URL percent-decoding.
fn urlencoding_decode(s: &str) -> String {
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

    let params = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh),
        ("client_id", client_id),
    ];

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
/// Returns the (possibly refreshed) tokens, or the original tokens if refresh
/// was not needed or not possible.
pub async fn ensure_fresh(
    client: &reqwest::Client,
    stored: StoredTokens,
    store: &CredentialsStore,
) -> StoredTokens {
    if !stored.is_expired() {
        return stored;
    }
    if stored.refresh_token.is_none() {
        warn!(
            server = %stored.server_name,
            "access token expired and no refresh token available"
        );
        return stored;
    }
    match refresh_token(client, &stored).await {
        Ok(fresh) => {
            if let Err(e) = store.save(&fresh) {
                warn!(error = %e, "failed to persist refreshed tokens");
            }
            fresh
        }
        Err(e) => {
            warn!(
                server = %stored.server_name,
                error = %e,
                "token refresh failed, using (possibly expired) access token"
            );
            stored
        }
    }
}
