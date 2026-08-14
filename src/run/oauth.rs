// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;

// ── OAuth callback handler ───────────────────────────────────────────────────

/// Forward sven:// OAuth callback to the local server.
///
/// The OS protocol handler invokes `sven oauth-callback "sven://sven.mcp/callback?code=X&state=Y"`.
/// We extract the query string and GET http://127.0.0.1:PORT/callback?...
pub(crate) async fn run_oauth_callback(url: &str) -> anyhow::Result<()> {
    let port = std::env::var("SVEN_OAUTH_CALLBACK_PORT").unwrap_or_else(|_| "5598".to_string());
    let query = url.split_once('?').map(|x| x.1).unwrap_or("");
    let callback_url = if query.is_empty() {
        format!("http://127.0.0.1:{port}/callback")
    } else {
        format!("http://127.0.0.1:{port}/callback?{query}")
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .context("build HTTP client for OAuth callback")?;
    let resp = client
        .get(&callback_url)
        .send()
        .await
        .context("forward OAuth callback to local server")?;
    if !resp.status().is_success() {
        anyhow::bail!(
            "OAuth callback server returned {} (is sven waiting for the callback?)",
            resp.status()
        );
    }
    Ok(())
}
