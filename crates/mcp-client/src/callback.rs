// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for signing agents in to OAuth
// protected services from a terminal for its clients. If your team needs
// expertise in OAuth for command-line tools then you can procure our services
// by sending an email to info@swedishembedded.com.
//! The local end of a browser sign-in: where the authorization server sends
//! the browser back to, and the pages the person sees there.
//!
//! sven listens on the loopback port the redirect lands on (directly, or
//! through the `sven://` handler that forwards to it), takes the first
//! well-formed `/callback` request, and hands its whole query back so the
//! OAuth layer checks state, issuer and code together.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{debug, info, warn};

/// How long a sign-in waits for the browser to come back.
pub const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);
/// The loopback port the redirect lands on by default.
pub const DEFAULT_CALLBACK_PORT: u16 = 5598;
/// The redirect a native install registers a handler for.
pub const DEFAULT_REDIRECT_URI: &str = "sven://sven.mcp/callback";

/// Where the browser is sent back to, and the listener that receives it.
pub struct Redirect {
    pub listener: TcpListener,
    pub redirect_uri: String,
}

/// Bind the callback listener for a sign-in: a configured redirect URI (its
/// handler forwards to `127.0.0.1:port`), `http://127.0.0.1:port/callback`
/// in a container (the host's `sven://` handler cannot reach it; the port is
/// bound on every interface so `docker -p` forwarding works, and the OAuth
/// `state` check refuses a callback that did not come from this sign-in),
/// or `sven://sven.mcp/callback` otherwise.
pub async fn bind(redirect_uri: Option<&str>, port: Option<u16>) -> Result<Redirect> {
    let port = port.unwrap_or(DEFAULT_CALLBACK_PORT);
    let (address, redirect_uri) = match redirect_uri {
        Some(custom) => (format!("127.0.0.1:{port}"), custom.to_string()),
        None if running_in_container() => (
            format!("0.0.0.0:{port}"),
            format!("http://127.0.0.1:{port}/callback"),
        ),
        None => (
            format!("127.0.0.1:{port}"),
            DEFAULT_REDIRECT_URI.to_string(),
        ),
    };
    let listener = TcpListener::bind(&address)
        .await
        .with_context(|| format!("bind the OAuth callback on {address} (for {redirect_uri})"))?;
    Ok(Redirect {
        listener,
        redirect_uri,
    })
}

/// Bind a listener on an ephemeral loopback port for a sign-in whose redirect
/// is `http://127.0.0.1:<port>/callback` (RFC 8252 section 7.3: the IP
/// literal, a port the operating system picks, nothing else can listen on it
/// meanwhile).
pub async fn bind_loopback() -> Result<Redirect> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind the sign-in callback on a loopback port")?;
    let port = listener.local_addr().context("loopback port")?.port();
    Ok(Redirect {
        listener,
        redirect_uri: format!("http://127.0.0.1:{port}/callback"),
    })
}

/// Show the person `auth_url` with `present`, then wait for the browser to
/// come back: to the listener, or as the address it ended on, pasted by the
/// person into `pasted` (a browser on another machine than sven cannot reach
/// the listener). Returns the callback's query string, e.g.
/// `code=...&state=...&iss=...`. The authorization server's own error (the
/// person declined) fails the sign-in with its description.
///
/// Any page the person visits can send their browser to the listener, so
/// when `auth_url` carries a `state`, a callback without that state (an
/// answer or an error alike) is refused and the wait goes on: it can
/// neither end nor fail this sign-in.
pub async fn wait(
    listener: TcpListener,
    auth_url: &str,
    present: &(dyn Fn(&str) + Send + Sync),
    pasted: Option<tokio::sync::mpsc::Receiver<String>>,
    timeout: Duration,
) -> Result<String> {
    wait_showing(
        listener,
        auth_url,
        present,
        pasted,
        timeout,
        &callback_success_html(),
    )
    .await
}

/// `wait`, answering the returning browser with `success_page` (a complete
/// HTML document) for callers whose sign-in is not an MCP server's.
pub async fn wait_showing(
    listener: TcpListener,
    auth_url: &str,
    present: &(dyn Fn(&str) + Send + Sync),
    pasted: Option<tokio::sync::mpsc::Receiver<String>>,
    timeout: Duration,
    success_page: &str,
) -> Result<String> {
    let expected_state = auth_url
        .split_once('?')
        .and_then(|(_, query)| parse_query_param(query, "state"));
    let ours = |query: &str| match &expected_state {
        Some(state) => parse_query_param(query, "state").as_deref() == Some(state.as_str()),
        None => true,
    };
    present(auth_url);
    info!(
        port = listener.local_addr().map(|a| a.port()).unwrap_or(0),
        "OAuth callback server ready, browser opened"
    );
    let done = Arc::new(AtomicBool::new(false));
    let accept = async {
        loop {
            let (mut stream, _peer) = listener
                .accept()
                .await
                .context("OAuth callback: accept failed")?;
            if done.load(Ordering::SeqCst) {
                let _ = write_html_response(&mut stream, 200, callback_already_done_html()).await;
                continue;
            }
            let mut buf = vec![0u8; 8192];
            let n = match tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buf)).await
            {
                Ok(Ok(n)) if n > 0 => n,
                _ => continue,
            };
            let request_text = String::from_utf8_lossy(&buf[..n]);
            let first_line = request_text.lines().next().unwrap_or("");
            let Some(query) = extract_get_path(first_line)
                .and_then(|path| path.strip_prefix("/callback"))
                .map(|rest| rest.strip_prefix('?').unwrap_or("").to_string())
            else {
                let _ = write_html_response(&mut stream, 404, not_found_html()).await;
                continue;
            };
            if !ours(&query) {
                let _ = write_html_response(
                    &mut stream,
                    400,
                    callback_error_html("This is not the sign-in sven is waiting for."),
                )
                .await;
                continue;
            }
            if let Some(error) = parse_query_param(&query, "error") {
                let desc = parse_query_param(&query, "error_description").unwrap_or_default();
                let message = if desc.is_empty() {
                    error.replace('_', " ")
                } else {
                    desc
                };
                let _ = write_html_response(&mut stream, 400, callback_error_html(&message)).await;
                return Err(anyhow!("OAuth authorization failed: {message}"));
            }
            if parse_query_param(&query, "code").is_none()
                || parse_query_param(&query, "state").is_none()
            {
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
            done.store(true, Ordering::SeqCst);
            let _ = write_html_response(&mut stream, 200, success_page.to_string()).await;
            return Ok(query);
        }
    };
    let paste = async {
        let Some(mut pasted) = pasted else {
            return std::future::pending().await;
        };
        loop {
            let Some(address) = pasted.recv().await else {
                return std::future::pending().await;
            };
            match query_of_pasted(&address) {
                Ok(query) if ours(&query) => return Ok(query),
                Ok(_) => warn!("that address is of another sign-in; paste this one's"),
                Err(e) => warn!(error = %e, "not the sign-in's return address; paste it again"),
            }
        }
    };
    let either = async {
        tokio::select! {
            result = accept => result,
            result = paste => result,
        }
    };
    tokio::time::timeout(timeout, either).await.map_err(|_| {
        anyhow!(
            "OAuth callback timed out after {}s. Complete the login in your browser and \
             make sure you are redirected back to localhost.",
            timeout.as_secs()
        )
    })?
}

/// Detect if we are running inside a container (Docker, Podman, etc.).
///
/// When in a container, the sven:// protocol handler (installed on the host)
/// cannot reach our callback server. We fall back to http://127.0.0.1:PORT
/// and bind to 0.0.0.0 so port forwarding (e.g. -p 5598:5598) works.
///
/// Container detection relies on Linux-specific files (`/.dockerenv`,
/// `/proc/1/cgroup`).  On macOS and Windows, Docker containers run a Linux VM
/// underneath, so from the host's perspective there are no container markers;
/// this function always returns `false` on non-Linux platforms.
pub fn running_in_container() -> bool {
    #[cfg(target_os = "linux")]
    {
        if std::path::Path::new("/.dockerenv").exists() {
            return true;
        }
        if std::path::Path::new("/run/.containerenv").exists() {
            return true;
        }
        if let Ok(cgroup) = std::fs::read_to_string("/proc/1/cgroup") {
            if cgroup.contains("docker")
                || cgroup.contains("containerd")
                || cgroup.contains("kubepods")
            {
                return true;
            }
        }
        false
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Try to open `url` in the user's default browser; whether one was started.
///
/// It prints nothing: a caller always shows the address itself
/// ([`sign_in_link`]), because opening can "succeed" with nothing appearing
/// (a headless box, a container, a remote shell) and a terminal frontend
/// cannot show stderr. Only a web page is opened: the address comes from the
/// server's metadata, so a local file or another program's handler is
/// refused.
pub fn open_browser(url: &str) -> bool {
    if !openable(url) {
        warn!(url = %url, "not opening a sign-in address that is not a web page");
        return false;
    }
    match webbrowser::open(url) {
        Ok(()) => true,
        Err(e) => {
            // Expected on a headless machine: the caller shows the address.
            debug!(error = %e, "could not open a browser (headless, or no display)");
            false
        }
    }
}

/// What to show a person who has to sign in to `server` on `url`: one line
/// saying what to do, then the address alone on its own line, so it can be
/// copied as one string. Every frontend shows it next to the attempt to open
/// a browser, so nobody depends on the browser opening.
///
/// An address that is not a web page is still shown (the person decides
/// whether to trust it), under a line saying sven does not open it.
#[must_use]
pub fn sign_in_link(server: &str, url: &str) -> String {
    if openable(url) {
        format!("Open this link in a browser to sign in to {server}:\n\n{url}")
    } else {
        format!(
            "The sign-in address for {server} is not a web page, so sven does not open it. \
             Open it yourself only if you trust it:\n\n{url}"
        )
    }
}

/// The query of an address the browser ended on, as pasted by the person
/// (`http://127.0.0.1:5598/callback?code=...&state=...`, or the `sven://`
/// address the browser could not open).
fn query_of_pasted(address: &str) -> Result<String> {
    let query = address
        .trim()
        .split_once('?')
        .map(|(_, q)| q.split('#').next().unwrap_or(q).to_string())
        .ok_or_else(|| anyhow!("the address has no `?code=...&state=...` part"))?;
    if let Some(error) = parse_query_param(&query, "error") {
        return Err(anyhow!(
            "OAuth authorization failed: {}",
            error.replace('_', " ")
        ));
    }
    if parse_query_param(&query, "code").is_none() || parse_query_param(&query, "state").is_none() {
        return Err(anyhow!("the address carries no code and state"));
    }
    Ok(query)
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

/// Percent-decoding of a query value (`%HH` sequences and `+` as space),
/// as UTF-8: an error description in another language reads as written. A
/// `%` not followed by two hex digits stays as it is.
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes[i] == b'%')
            .then(|| bytes.get(i + 1..i + 3))
            .flatten()
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (escaped, bytes[i]) {
            (Some(byte), _) => {
                out.push(byte);
                i += 3;
            }
            (None, b'+') => {
                out.push(b' ');
                i += 1;
            }
            (None, other) => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether `url` is a web page a browser may be sent to.
fn openable(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

/// `text` safe to place in an HTML page as text.
fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
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
         Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\n\
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
  <title>Sven - Authenticated</title>
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
    let message = escape_html(message);
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Sven - Authentication Failed</title>
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
  <title>Sven - Already authenticated</title>
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sign_in_address_is_alone_on_its_own_line() {
        let url = "https://auth.example/authorize?client_id=a&state=b";
        let text = sign_in_link("docs", url);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "Open this link in a browser to sign in to docs:");
        assert_eq!(lines.last(), Some(&url), "{text}");
        assert_eq!(lines.iter().filter(|l| l.contains(url)).count(), 1);
    }

    #[test]
    fn an_address_that_is_not_a_web_page_is_shown_but_not_offered_for_opening() {
        let text = sign_in_link("docs", "file:///etc/passwd");
        assert!(text.contains("does not open it"), "{text}");
        assert!(text.ends_with("file:///etc/passwd"));
        assert!(!open_browser("file:///etc/passwd"));
    }

    #[test]
    fn a_query_value_decodes_as_utf8() {
        assert_eq!(
            url_decode("Zugriff+verweigert%3A+%C3%BC"),
            "Zugriff verweigert: ü"
        );
        assert_eq!(url_decode("100%"), "100%");
        assert_eq!(url_decode("%zz"), "%zz");
    }

    #[test]
    fn a_pasted_return_address_gives_its_query() {
        assert_eq!(
            query_of_pasted("  sven://sven.mcp/callback?code=a&state=b&iss=x#frag \n").unwrap(),
            "code=a&state=b&iss=x"
        );
        assert!(query_of_pasted("https://example.com/").is_err());
        assert!(query_of_pasted("http://h/callback?state=b").is_err());
    }

    /// A sign-in whose browser cannot reach the listener completes from the
    /// address the person pastes.
    #[tokio::test]
    async fn a_sign_in_completes_from_a_pasted_address() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        tx.send("not it".into()).await.unwrap();
        tx.send("http://127.0.0.1:5598/callback?code=c&state=s".into())
            .await
            .unwrap();
        let query = wait(
            listener,
            "https://auth.example/x",
            &|_| {},
            Some(rx),
            CALLBACK_TIMEOUT,
        )
        .await
        .unwrap();
        assert_eq!(query, "code=c&state=s");
    }

    #[test]
    fn only_the_callback_path_is_ours() {
        assert_eq!(
            extract_get_path("GET /callback?code=a&state=b HTTP/1.1"),
            Some("/callback?code=a&state=b")
        );
        assert_eq!(
            parse_query_param("code=a&state=b%20c", "state").as_deref(),
            Some("b c")
        );
    }

    async fn get(port: u16, path: &str) -> String {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        stream
            .write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut answer = String::new();
        let _ = stream.read_to_string(&mut answer).await;
        answer
    }

    /// Any page the person visits can send their browser to the listener:
    /// what does not carry this sign-in's state neither ends nor fails it,
    /// and nothing it says is run as a page's script.
    #[tokio::test]
    async fn only_this_sign_ins_callback_ends_the_wait() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let waiting = tokio::spawn(async move {
            wait(
                listener,
                "https://auth.example/authorize?client_id=c&state=the-state&code_challenge=x",
                &|_| {},
                None,
                CALLBACK_TIMEOUT,
            )
            .await
        });
        let forged = get(
            port,
            "/callback?error=x&error_description=%3Cscript%3Ealert(1)%3C%2Fscript%3E",
        )
        .await;
        assert!(forged.starts_with("HTTP/1.1 400"), "{forged}");
        assert!(!forged.contains("<script>"), "{forged}");
        assert!(
            forged.contains("Content-Security-Policy: default-src 'none'"),
            "{forged}"
        );
        let other = get(port, "/callback?code=c&state=another").await;
        assert!(other.starts_with("HTTP/1.1 400"), "{other}");
        assert!(!waiting.is_finished());
        let ours = get(port, "/callback?code=c&state=the-state").await;
        assert!(ours.starts_with("HTTP/1.1 200"), "{ours}");
        assert_eq!(waiting.await.unwrap().unwrap(), "code=c&state=the-state");
    }

    #[test]
    fn only_a_web_address_is_opened_in_a_browser() {
        assert!(openable("https://auth.example/authorize?x=1"));
        assert!(openable("http://127.0.0.1:5598/x"));
        assert!(!openable("file:///etc/passwd"));
        assert!(!openable("javascript:alert(1)"));
        assert!(!openable("smb://host/share"));
    }
}
