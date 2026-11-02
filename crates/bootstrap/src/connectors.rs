// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for giving agents the accounts
// their people connected, without handing them the sign-ins, for its clients.
// If your team needs expertise in agent tool integration then you can procure
// our services by sending an email to info@swedishembedded.com.
//! The `connectors` MCP server: the accounts a person connected on the
//! server sven is signed in to (`sven auth login`, `sven rc`), reached at the
//! MCP endpoint that server advertised, with the same key. To sven it is an
//! ordinary remote MCP server; the accounts' own sign-ins stay on the server.
//!
//! Added by the run modes that start sessions, never written to a file (it
//! carries the key). A configured `mcp_servers.connectors` entry takes its
//! place: `connectors: {enabled: false}` turns it off.

use crate::Config;

/// The server's name, which prefixes its tools (`connectors-<tool>`).
pub const SERVER: &str = "connectors";

/// Add the `connectors` server at `mcp_url`, unless the configuration names
/// one of its own. With `token_url`, every request carries a short-lived
/// token traded there for `key`, and `key` never travels to `mcp_url`;
/// without, `key` itself is the bearer token.
pub fn add(config: &mut Config, mcp_url: &str, token_url: Option<&str>, key: &str) {
    config
        .mcp_servers
        .entry(SERVER.to_string())
        .or_insert_with(|| match token_url {
            Some(token_url) => {
                sven_mcp_client::McpServerConfig::exchanged_http(mcp_url, token_url, key)
            }
            None => sven_mcp_client::McpServerConfig::bearer_http(mcp_url, key),
        });
}

/// [`add`] for the server this user signed in to, when it advertised an MCP
/// endpoint.
pub fn add_signed_in(config: &mut Config) {
    if let Some(signed_in) = sven_model_drivers::sven_service::signed_in() {
        if let Some(url) = &signed_in.mcp_url {
            add(
                config,
                url,
                signed_in.mcp_token_url.as_deref(),
                &signed_in.key,
            );
        }
    }
}

/// The address `/mcp add` names, when it is a server to add to the signed-in
/// person's account rather than to a file here: one `http(s)` address.
#[must_use]
pub fn account_target(target: &[String]) -> Option<&str> {
    match target {
        [url] if url.starts_with("https://") || url.starts_with("http://") => Some(url),
        _ => None,
    }
}

/// Add the server at `url`, called `name`, to the account this user signed
/// in to, as a connector of their own; its sign-in then happens in their
/// browser and its tokens stay with the account. None when not signed in to
/// a server that offers connectors.
pub async fn connect_through_account(
    name: &str,
    url: &str,
) -> Option<anyhow::Result<sven_mcp_client::AccountConnection>> {
    let signed_in = sven_model_drivers::sven_service::signed_in()?;
    let (mcp_url, token_url) = (signed_in.mcp_url?, signed_in.mcp_token_url?);
    let exchange = sven_mcp_client::TokenExchange {
        url: token_url,
        key: signed_in.key,
    };
    Some(sven_mcp_client::connect_account(&exchange, &mcp_url, name, url).await)
}

/// What to tell the person once a connector is added to their account: the
/// page that connects their account of it, and where its tools then appear.
#[must_use]
pub fn sign_in_notice(connection: &sven_mcp_client::AccountConnection) -> String {
    format!(
        "Added {} to your account. Connect your account of it on this page, signed in \
         to sven as yourself:\n\n  {}\n\nIts tools reach sven through the '{SERVER}' \
         server once you finish; your sign-in stays with your account.",
        connection.connector.name, connection.sign_in_url
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_web_address_goes_to_the_account() {
        let one = |s: &str| vec![s.to_string()];
        assert_eq!(
            account_target(&one("https://api.githubcopilot.com/mcp/")),
            Some("https://api.githubcopilot.com/mcp/")
        );
        assert_eq!(account_target(&one("npx")), None);
        assert_eq!(
            account_target(&["https://a.example/mcp".into(), "extra".into()]),
            None
        );
    }

    #[test]
    fn the_server_is_added_unless_configured() {
        let mut config = Config::default();
        add(&mut config, "https://sven.example/api/mcp", None, "key");
        let server = &config.mcp_servers[SERVER];
        assert!(matches!(
            &server.transport,
            sven_mcp_client::McpTransport::Http { url, headers }
                if url == "https://sven.example/api/mcp" && headers["Authorization"] == "Bearer key"
        ));

        let mut config: Config =
            serde_yaml::from_str("mcp_servers:\n  connectors: {enabled: false}\n").unwrap();
        add(&mut config, "https://sven.example/api/mcp", None, "key");
        assert!(!config.mcp_servers[SERVER].enabled);

        // With a token endpoint the key goes there alone: no header carries it.
        let mut config = Config::default();
        add(
            &mut config,
            "https://sven.example/api/mcp",
            Some("https://sven.example/api/mcp/token"),
            "key",
        );
        let server = &config.mcp_servers[SERVER];
        assert!(
            matches!(&server.transport, sven_mcp_client::McpTransport::Http { headers, .. } if headers.is_empty())
        );
        assert_eq!(
            server.token_exchange.as_ref().unwrap().url,
            "https://sven.example/api/mcp/token"
        );
        assert!(
            !format!("{server:?}").contains("key\""),
            "the key is never printed"
        );
    }
}
