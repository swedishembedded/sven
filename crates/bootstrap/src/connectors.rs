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
/// endpoint. It joins the servers the person configured on this machine and
/// never replaces one of them.
pub fn add_signed_in(config: &mut Config) {
    if let Some(signed_in) = sven_model_drivers::sven_service::signed_in() {
        if let Some(url) = &signed_in.mcp_url {
            if signed_in.mcp_token_url.is_none() {
                tracing::warn!(
                    "the sven sign-in at {} is from an older release and has no token \
                     endpoint, so its long-lived key is sent to the connectors server as a \
                     bearer token; run `sven auth login` again",
                    origin_of(url)
                );
            }
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

/// `scheme://host[:port]` of `url`: where a server lives, for saying so.
#[must_use]
pub fn origin_of(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("https", url));
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    format!("{scheme}://{authority}")
}

/// The sven server this person is signed in to, as far as adding a
/// connector to their account needs it. Holds the key: never printed.
pub struct Account {
    /// Where the server lives, for saying so.
    pub origin: String,
    mcp_url: String,
    exchange: sven_mcp_client::TokenExchange,
}

/// Where `/mcp add` puts a server.
pub enum Placement {
    /// On the signed-in person's account: usable from every session and
    /// computer, its sign-in kept with the account.
    Account(Account),
    /// In a configuration file on this machine, signed in to from here.
    Local,
    /// The person is signed in, but with a record from a release that
    /// predates adding servers to an account; no silent fallback to a file.
    OutdatedSignIn { origin: String },
}

/// Decide where `/mcp add` puts a server. A plain web address goes to the
/// account of a signed-in person (one connection reused everywhere, tokens
/// off this machine) unless they ask for `local`. `plain` is false when the
/// request carries headers, environment or an OAuth client of its own, which
/// only a file here can hold.
#[must_use]
pub fn placement(
    target: &[String],
    plain: bool,
    local: bool,
    signed_in: Option<sven_model_drivers::sven_service::SignedIn>,
) -> Placement {
    if local || !plain || account_target(target).is_none() {
        return Placement::Local;
    }
    let Some(signed_in) = signed_in else {
        return Placement::Local;
    };
    match (signed_in.mcp_url, signed_in.mcp_token_url) {
        (Some(mcp_url), Some(token_url)) => Placement::Account(Account {
            origin: origin_of(&mcp_url),
            mcp_url,
            exchange: sven_mcp_client::TokenExchange {
                url: token_url,
                key: signed_in.key,
            },
        }),
        (Some(mcp_url), None) => Placement::OutdatedSignIn {
            origin: origin_of(&mcp_url),
        },
        (None, _) => Placement::Local,
    }
}

/// [`placement`] for the sign-in of this machine's user.
#[must_use]
pub fn current_placement(target: &[String], plain: bool, local: bool) -> Placement {
    placement(
        target,
        plain,
        local,
        sven_model_drivers::sven_service::signed_in(),
    )
}

/// Where the account this user signed in to lives, when it offers
/// connectors: what a list of servers names the account by.
#[must_use]
pub fn signed_in_origin() -> Option<String> {
    sven_model_drivers::sven_service::signed_in()?
        .mcp_url
        .map(|url| origin_of(&url))
}

/// The one line that says where `name` is being added, before anything is
/// done.
#[must_use]
pub fn adding_notice(name: &str, placement: &Placement, config_file: &std::path::Path) -> String {
    match placement {
        Placement::Account(account) => format!(
            "Adding {name} to your sven account at {} (usable from all your sessions; \
             use --local to keep it on this machine only) ...",
            account.origin
        ),
        _ => format!(
            "Adding {name} to {} on this machine ...",
            config_file.display()
        ),
    }
}

/// What to say when the sign-in record predates adding servers to an
/// account.
#[must_use]
pub fn outdated_sign_in_notice(origin: &str) -> String {
    format!(
        "Your sven sign-in at {origin} is from an older release; run `sven auth login` again \
         to add servers to your account, or use `--local`."
    )
}

/// Add the server at `url`, called `name`, to `account`, as a connector of
/// their own; its sign-in then happens in their browser and its tokens stay
/// with the account.
pub async fn connect_through_account(
    account: &Account,
    name: &str,
    url: &str,
) -> anyhow::Result<sven_mcp_client::AccountConnection> {
    sven_mcp_client::connect_account(&account.exchange, &account.mcp_url, name, url).await
}

/// What to tell the person once a connector is added to their account: the
/// page that connects their account of it (alone on its own line, to copy),
/// and where its tools then appear.
#[must_use]
pub fn sign_in_notice(connection: &sven_mcp_client::AccountConnection) -> String {
    let name = &connection.connector.name;
    format!(
        "Added {name} to your account.\n\n{}\n\nIts tools reach sven through the '{SERVER}' \
         server once you finish; your sign-in stays with your account.",
        sven_mcp_client::sign_in_link(name, &connection.sign_in_url),
    )
}

/// The services and accounts behind the `connectors` server's tools, from
/// the tool names (`connectors-<service>_<account>__<tool>`): what a list
/// of the account's servers shows.
#[must_use]
pub fn account_groups<'a>(tool_names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let prefix = format!("{SERVER}-");
    let mut groups: Vec<String> = tool_names
        .into_iter()
        .filter_map(|n| n.strip_prefix(&prefix))
        .map(|n| n.split("__").next().unwrap_or(n).to_string())
        .collect();
    groups.sort();
    groups.dedup();
    groups
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

    fn signed_in(token_url: Option<&str>) -> Option<sven_model_drivers::sven_service::SignedIn> {
        Some(sven_model_drivers::sven_service::SignedIn {
            base_url: "https://sven.example/v1".into(),
            key: "key".into(),
            mcp_url: Some("https://sven.example/api/mcp".into()),
            mcp_token_url: token_url.map(str::to_string),
        })
    }

    fn url() -> Vec<String> {
        vec!["https://mcp.example.com/mcp".to_string()]
    }

    #[test]
    fn a_plain_address_goes_to_the_account_unless_asked_otherwise() {
        let token = Some("https://sven.example/api/mcp/token");
        let account =
            |p: Placement| matches!(p, Placement::Account(a) if a.origin == "https://sven.example");
        assert!(account(placement(&url(), true, false, signed_in(token))));
        // --local, headers/env/a client of one's own, a command, no sign-in.
        assert!(matches!(
            placement(&url(), true, true, signed_in(token)),
            Placement::Local
        ));
        assert!(matches!(
            placement(&url(), false, false, signed_in(token)),
            Placement::Local
        ));
        let command = vec!["npx".to_string()];
        assert!(matches!(
            placement(&command, true, false, signed_in(token)),
            Placement::Local
        ));
        assert!(matches!(
            placement(&url(), true, false, None),
            Placement::Local
        ));
    }

    #[test]
    fn an_older_sign_in_is_refused_not_silently_replaced_by_a_file() {
        let Placement::OutdatedSignIn { origin } = placement(&url(), true, false, signed_in(None))
        else {
            panic!("an older sign-in record must be refused");
        };
        assert_eq!(
            outdated_sign_in_notice(&origin),
            "Your sven sign-in at https://sven.example is from an older release; run \
             `sven auth login` again to add servers to your account, or use `--local`."
        );
        // --local still works with it.
        assert!(matches!(
            placement(&url(), true, true, signed_in(None)),
            Placement::Local
        ));
    }

    #[test]
    fn where_a_server_goes_is_said_in_one_line() {
        let account = placement(
            &url(),
            true,
            false,
            signed_in(Some("https://sven.example/t")),
        );
        let file = std::path::Path::new("config/sven/config.yaml");
        assert_eq!(
            adding_notice("docs", &account, file),
            "Adding docs to your sven account at https://sven.example (usable from all your \
             sessions; use --local to keep it on this machine only) ..."
        );
        assert_eq!(
            adding_notice("docs", &Placement::Local, file),
            "Adding docs to config/sven/config.yaml on this machine ..."
        );
    }

    #[test]
    fn the_sign_in_address_stands_alone_in_the_account_notice() {
        let connection = sven_mcp_client::AccountConnection {
            connector: sven_mcp_client::AccountConnector {
                id: "c1".into(),
                name: "docs".into(),
            },
            sign_in_url: "https://sven.example/connect/abc".into(),
        };
        let text = sign_in_notice(&connection);
        assert!(
            text.lines()
                .any(|l| l == "https://sven.example/connect/abc"),
            "{text}"
        );
        assert!(text.contains("Open this link in a browser to sign in to docs:"));
    }

    #[test]
    fn the_accounts_servers_are_named_by_their_tools() {
        let names = [
            "connectors-github_me__search",
            "connectors-github_me__read",
            "connectors-linear_me__list",
            "web_fetch",
        ];
        assert_eq!(account_groups(names), vec!["github_me", "linear_me"]);
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
