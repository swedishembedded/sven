// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for giving signed-in users their
// account's models with no provider configuration at all, for its clients.
// If your team needs expertise in hosted LLM gateways and agent integration
// then you can procure our services by sending an email to
// info@swedishembedded.com.
//! The sven server this user signed in to, as the `sven` provider.
//!
//! `sven auth login` writes `<config dir>/sven/remote.yaml` (the sign-in
//! record, owned by the remote-control client). The `sven` provider reads
//! two of its fields: the server's OpenAI-compatible base URL and the key it
//! minted. So `sven/<model>` reaches whichever server the user signed in to
//! (the hosted service, or a development server), with that server's key,
//! and a signed-in user with no `model:` configured starts with the default
//! model of their account.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The driver id of the sven service.
pub const PROVIDER: &str = "sven";

/// The server a sign-in reaches, for the `sven` provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedIn {
    /// The server's OpenAI-compatible base URL, ending in `/v1`.
    pub base_url: String,
    /// The key the server minted at sign-in. A secret.
    pub key: String,
}

/// The fields of the sign-in record a model provider needs; the rest of the
/// record is the remote-control client's.
#[derive(Deserialize)]
struct Record {
    openai_base_url: String,
    token: String,
}

/// The sign-in record of this user.
#[must_use]
pub fn path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("sven").join("remote.yaml"))
}

/// The server this user signed in to, if they did. A record that cannot be
/// read is logged and treated as no sign-in: the `sven` provider then
/// reports the missing sign-in, which `sven auth login` fixes.
#[must_use]
pub fn signed_in() -> Option<SignedIn> {
    signed_in_from(&path()?)
}

/// [`signed_in`], from the record in `file`.
#[must_use]
pub fn signed_in_from(file: &Path) -> Option<SignedIn> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!("{}: {e}; ignoring the sign-in", file.display());
            return None;
        }
    };
    match serde_yaml::from_str::<Record>(&text) {
        Ok(record) => Some(SignedIn {
            base_url: record.openai_base_url,
            key: record.token,
        }),
        Err(e) => {
            tracing::warn!("{}: {e}; ignoring the sign-in", file.display());
            None
        }
    }
}

/// Every provider this user has a key for: those with a stored key
/// (`sven auth set`), and `sven` while signed in.
#[must_use]
pub fn keyed_providers() -> Vec<String> {
    let mut providers = crate::credentials::providers();
    if signed_in().is_some() && !providers.iter().any(|p| p == PROVIDER) {
        providers.push(PROVIDER.to_string());
        providers.sort();
    }
    providers
}

/// Whether `a` and `b` name the same base URL.
pub(crate) fn same_url(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sign_in_record_names_the_server_and_its_key() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("remote.yaml");
        std::fs::write(
            &file,
            "server: http://localhost:3000\ncontrol_url: http://localhost:3000/api/workspace\n\
             openai_base_url: http://localhost:3000/v1\ntoken: sk-minted\n\
             workspace_id: 00000000-0000-0000-0000-000000000001\nworkspace_name: box\naccount: me\n",
        )
        .unwrap();
        assert_eq!(
            signed_in_from(&file),
            Some(SignedIn {
                base_url: "http://localhost:3000/v1".into(),
                key: "sk-minted".into(),
            })
        );
    }

    #[test]
    fn no_record_or_a_broken_one_is_no_sign_in() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(signed_in_from(&dir.path().join("remote.yaml")), None);
        let broken = dir.path().join("broken.yaml");
        std::fs::write(&broken, "token: [").unwrap();
        assert_eq!(signed_in_from(&broken), None);
    }
}
