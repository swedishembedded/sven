// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for signing agents in to OAuth
// protected tool servers safely for its clients. If your team needs expertise
// in OAuth 2.1 for MCP clients then you can procure our services by sending
// an email to info@swedishembedded.com.
//! Signing in to an OAuth-protected MCP server, and keeping the sign-in.
//!
//! The protocol work is `rmcp`'s, following the MCP 2026-07-28
//! authorization rules: discovery from the server's challenge and protected
//! resource metadata, PKCE, the `resource` parameter on every authorization
//! and token request (RFC 8707), the authorization response's issuer checked
//! (RFC 9207), client registration by a pre-registered client, a Client ID
//! Metadata Document or Dynamic Client Registration, and stored credentials
//! bound to the issuer that minted them. sven owns where the browser comes
//! back to (the `callback` module) and where the credentials are kept
//! ([`TokenStore`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use rmcp::transport::auth::{
    AuthClient, AuthError, AuthorizationManager, AuthorizationRequest, AuthorizationSession,
    CredentialRefreshGuard, CredentialStore, StoredCredentials,
};
use tracing::{info, warn};
use url::Url;

use crate::callback;
use crate::config::McpOAuthConfig;

/// The client id used when the authorization server offers no registration
/// and none is configured; some servers accept any public client.
const FALLBACK_CLIENT_ID: &str = "sven-mcp-client";

/// The files of the token store sven used before it signed in through
/// `rmcp`. Their tokens are not carried over (each server is signed in to
/// again); the files are removed so no stale secret stays on disk.
const RETIRED_FILES: &[&str] = &["mcp-credentials.json", "mcp-client-info.json"];

/// The signed-in MCP servers: one JSON file, readable by its owner only,
/// mapping a server (its configured name and URL) to its credentials.
pub struct TokenStore {
    path: PathBuf,
}

impl TokenStore {
    /// `~/.config/sven/mcp-oauth.json`.
    #[must_use]
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("sven")
            .join("mcp-oauth.json")
    }

    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The key of server `name` at `url`: a server is shared by every project
    /// that names it the same and uses the same URL.
    #[must_use]
    pub fn key(name: &str, url: &str) -> String {
        let url = Url::parse(url).map_or_else(
            |_| url.to_string(),
            |u| u.as_str().trim_end_matches('/').to_string(),
        );
        format!("{name}::{url}")
    }

    fn load_map(&self) -> Result<HashMap<String, StoredCredentials>> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("parse {}", self.path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(e) => Err(e).with_context(|| format!("read {}", self.path.display())),
        }
    }

    /// Change the file under an exclusive lock on `<file>.lock`, so sven
    /// processes running at once never lose each other's update, and
    /// replace it whole ([`sven_config::write_private`]).
    fn update(&self, change: impl FnOnce(&mut HashMap<String, StoredCredentials>)) -> Result<()> {
        let _lock = lock_file(&self.path.with_extension("lock"))?;
        let mut all = self.load_map()?;
        change(&mut all);
        sven_config::write_private(&self.path, &serde_json::to_string_pretty(&all)?)?;
        self.remove_retired_files();
        Ok(())
    }

    fn remove_retired_files(&self) {
        let Some(dir) = self.path.parent() else {
            return;
        };
        for name in RETIRED_FILES {
            let file = dir.join(name);
            match std::fs::remove_file(&file) {
                Ok(()) => info!(file = %file.display(), "removed the previous MCP token store"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    warn!(file = %file.display(), error = %e, "could not remove the previous MCP token store")
                }
            }
        }
    }

    /// The credentials kept for `key`.
    pub fn load(&self, key: &str) -> Result<Option<StoredCredentials>> {
        Ok(self.load_map()?.remove(key))
    }

    pub fn save(&self, key: &str, credentials: StoredCredentials) -> Result<()> {
        self.update(|all| {
            all.insert(key.to_string(), credentials);
        })
    }

    /// Forget the sign-in of `key`.
    pub fn remove(&self, key: &str) -> Result<()> {
        self.update(|all| {
            all.remove(key);
        })
    }

    /// The per-server lock that serialises token refreshes across processes.
    fn refresh_lock_path(&self, key: &str) -> PathBuf {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(key.as_bytes());
        let tag: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        self.path.with_extension(format!("refresh-{tag}.lock"))
    }
}

/// An exclusive advisory lock on `path`, held until the file is dropped.
fn lock_file(path: &Path) -> Result<std::fs::File> {
    use fs4::fs_std::FileExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .with_context(|| format!("locking {}", path.display()))?;
    file.lock_exclusive()
        .with_context(|| format!("locking {}", path.display()))?;
    Ok(file)
}

/// One server's slot in the [`TokenStore`], as `rmcp` reads and writes it.
struct ServerCredentials {
    store: Arc<TokenStore>,
    key: String,
}

fn store_error(e: anyhow::Error) -> AuthError {
    AuthError::CredentialStoreError(format!("{e:#}"))
}

#[async_trait]
impl CredentialStore for ServerCredentials {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let (store, key) = (Arc::clone(&self.store), self.key.clone());
        tokio::task::spawn_blocking(move || store.load(&key))
            .await
            .map_err(|e| store_error(e.into()))?
            .map_err(store_error)
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        let (store, key) = (Arc::clone(&self.store), self.key.clone());
        tokio::task::spawn_blocking(move || store.save(&key, credentials))
            .await
            .map_err(|e| store_error(e.into()))?
            .map_err(store_error)
    }

    async fn clear(&self) -> Result<(), AuthError> {
        let (store, key) = (Arc::clone(&self.store), self.key.clone());
        tokio::task::spawn_blocking(move || store.remove(&key))
            .await
            .map_err(|e| store_error(e.into()))?
            .map_err(store_error)
    }

    /// Two sven processes whose tokens expire together refresh one at a
    /// time; the second sees the first's new tokens instead of spending a
    /// rotated refresh token twice.
    async fn acquire_refresh_guard(&self) -> Result<Option<CredentialRefreshGuard>, AuthError> {
        let path = self.store.refresh_lock_path(&self.key);
        let file = tokio::task::spawn_blocking(move || lock_file(&path))
            .await
            .map_err(|e| store_error(e.into()))?
            .map_err(store_error)?;
        Ok(Some(CredentialRefreshGuard::new(file)))
    }
}

/// An authorization manager for the server at `url`, its credentials in
/// `store` under `key`.
async fn manager(store: &Arc<TokenStore>, key: &str, url: &str) -> Result<AuthorizationManager> {
    let mut manager = AuthorizationManager::new(url)
        .await
        .with_context(|| format!("OAuth for {url}"))?;
    manager.set_credential_store(ServerCredentials {
        store: Arc::clone(store),
        key: key.to_string(),
    });
    Ok(manager)
}

/// An HTTP client that signs every request to the server with its stored
/// sign-in and refreshes the token when it expires, or `None` when the
/// server has never been signed in to (or its authorization server changed,
/// which invalidates the old sign-in).
pub async fn signed_in_client(
    store: &Arc<TokenStore>,
    key: &str,
    url: &str,
) -> Result<Option<AuthClient<reqwest::Client>>> {
    if store.load(key)?.is_none() {
        return Ok(None);
    }
    let mut manager = manager(store, key, url).await?;
    if !manager
        .initialize_from_store()
        .await
        .context("reading the stored MCP sign-in")?
    {
        return Ok(None);
    }
    Ok(Some(AuthClient::new(reqwest::Client::new(), manager)))
}

/// What a sign-in needs.
pub struct SignIn<'a> {
    pub key: &'a str,
    pub url: &'a str,
    pub oauth: Option<&'a McpOAuthConfig>,
    /// The `WWW-Authenticate` challenge of the 401 that asked for it.
    pub challenge: Option<&'a str>,
    /// Shows the person the page to sign in on (opens the browser, prints
    /// the link, or hands it to a frontend).
    pub present: &'a (dyn Fn(&str) + Send + Sync),
    /// Addresses the person pastes when the browser cannot reach sven.
    pub pasted: Option<tokio::sync::mpsc::Receiver<String>>,
}

/// Sign in to an MCP server in the browser and keep the result in `store`.
/// Waits until the browser comes back (at most five minutes).
pub async fn sign_in(store: &Arc<TokenStore>, params: SignIn<'_>) -> Result<()> {
    let oauth = params.oauth.cloned().unwrap_or_default();
    let redirect = callback::bind(oauth.redirect_uri.as_deref(), oauth.callback_port).await?;

    let mut manager = manager(store, params.key, params.url).await?;
    let resolution = manager
        .resolve_metadata_from_challenge(params.challenge)
        .await
        .context("discovering the MCP server's authorization server")?;
    let can_register = resolution.metadata.registration_endpoint.is_some()
        || resolution
            .metadata
            .additional_fields
            .get("client_id_metadata_document_supported")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
            && oauth.client_metadata_url.is_some();
    manager.set_metadata(resolution.metadata);

    let mut request = AuthorizationRequest::new(redirect.redirect_uri.clone())
        .with_client_name("sven")
        .with_scopes(oauth.scopes.iter().cloned());
    if let Some(url) = &oauth.client_metadata_url {
        request = request.with_client_metadata_url(url.clone());
    }
    match (&oauth.client_id, can_register) {
        (Some(id), _) => {
            request = request.with_preregistered_client(id.clone());
            if let Some(secret) = &oauth.client_secret {
                request = request.with_client_secret(secret.clone());
            }
        }
        (None, false) => request = request.with_preregistered_client(FALLBACK_CLIENT_ID),
        (None, true) => {}
    }
    let session = AuthorizationSession::new(manager, request)
        .await
        .map_err(|(_, e)| e)
        .context("starting the sign-in")?;

    let query = callback::wait(
        redirect.listener,
        session.get_authorization_url(),
        params.present,
        params.pasted,
        callback::CALLBACK_TIMEOUT,
    )
    .await?;
    // The state, issuer and code are checked together, against this
    // session's own record; the code is exchanged only when all hold.
    session
        .handle_callback_url(&format!("http://127.0.0.1/callback?{query}"))
        .await
        .context("completing the sign-in")?;
    info!(server = %params.key, "MCP server signed in");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credentials(client: &str) -> StoredCredentials {
        StoredCredentials::new(client.to_string(), None, vec![], None)
            .with_issuer(Some("https://auth.example.com".into()))
    }

    /// The store is its owner's alone, survives writers racing each other,
    /// and retires the files of the previous store.
    #[test]
    fn signed_in_servers_are_kept_owner_only_without_lost_updates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sven/mcp-oauth.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path.with_file_name("mcp-credentials.json"), "{}").unwrap();
        let store = Arc::new(TokenStore::new(path.clone()));
        let writers: Vec<_> = (0..8)
            .map(|i| {
                let store = Arc::clone(&store);
                std::thread::spawn(move || {
                    let key = TokenStore::key(&format!("s{i}"), "https://mcp.example.com/");
                    store.save(&key, credentials(&format!("c{i}"))).unwrap();
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        for i in 0..8 {
            let key = TokenStore::key(&format!("s{i}"), "https://mcp.example.com");
            let kept = store.load(&key).unwrap().expect("kept");
            assert_eq!(kept.client_id, format!("c{i}"));
            assert_eq!(kept.issuer.as_deref(), Some("https://auth.example.com"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert!(!path.with_file_name("mcp-credentials.json").exists());
        let key = TokenStore::key("s0", "https://mcp.example.com");
        store.remove(&key).unwrap();
        assert!(store.load(&key).unwrap().is_none());
    }
}
