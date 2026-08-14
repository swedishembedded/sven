// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `/share` command - expose THIS running session so a remote consultant can
//! steer it ("local brain, remote steer"), one-tap.
//!
//! When a broker is configured via the environment (`SVEN_SHARE_URL`,
//! `SVEN_SHARE_TOKEN`, `SVEN_SHARE_TENANT`), `/share [share-id]` emits an
//! [`ImmediateAction::ShareSession`]: the frontend forwards it to the live
//! session's agent task, which starts the **in-process** share bridge against
//! the already-running kernel — no copy-paste, no second process. The TUI
//! prints "Session shared as <id>" once the broker accepts the registration.
//!
//! With `--print`, or when no broker is configured, it falls back to the
//! [`ImmediateAction::ShareInstructions`] handoff that shows the fully
//! scriptable `sven share` invocation.

use std::path::PathBuf;

use crate::{
    CommandContext, CommandResult, CompletionItem, FrontendShareOptions, ImmediateAction,
    SlashCommand,
};

pub struct ShareCommand;

impl SlashCommand for ShareCommand {
    fn name(&self) -> &str {
        "share"
    }

    fn description(&self) -> &str {
        "Share this running session so a remote consultant can steer it"
    }

    fn complete(
        &self,
        _arg_index: usize,
        _partial: &str,
        _ctx: &CommandContext,
    ) -> Vec<CompletionItem> {
        vec![]
    }

    fn execute(&self, args: Vec<String>) -> CommandResult {
        let action = build_share_action(&args, |k| std::env::var(k).ok());
        CommandResult {
            immediate_action: Some(action),
            ..Default::default()
        }
    }
}

/// Resolve `/share` into an [`ImmediateAction`], reading broker settings via the
/// `get_env` lookup (injected so this is unit-testable without touching the
/// process environment).
///
/// One-tap when a broker is configured and `--print` was not passed; otherwise
/// the scriptable-handoff instructions.
fn build_share_action(
    args: &[String],
    get_env: impl Fn(&str) -> Option<String>,
) -> ImmediateAction {
    let print_only = args.iter().any(|a| a == "--print");
    // The first non-flag positional is the optional share id.
    let share_id = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(default_share_id);

    let env = |k: &str| get_env(k).filter(|v| !v.trim().is_empty());
    let url = env("SVEN_SHARE_URL");
    let token = env("SVEN_SHARE_TOKEN");
    let tenant = env("SVEN_SHARE_TENANT");

    if let (false, Some(broker_url), Some(token), Some(tenant_id)) =
        (print_only, url, token, tenant)
    {
        let insecure_dev = env("SVEN_SHARE_INSECURE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let ca_pem = env("SVEN_SHARE_CA").map(PathBuf::from);
        return ImmediateAction::ShareSession {
            options: Box::new(FrontendShareOptions {
                broker_url,
                token,
                share_id,
                tenant_id,
                title: "shared sven session".to_string(),
                ca_pem,
                insecure_dev,
            }),
        };
    }

    // Fallback: the scriptable handoff (also reached explicitly with --print).
    ImmediateAction::ShareInstructions {
        text: handoff_instructions(),
    }
}

/// A stable-ish default share id derived from the wall clock.
fn default_share_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("share-{nanos:x}")
}

fn handoff_instructions() -> String {
    "One-tap sharing needs a broker configured in the environment:\n\
     \n    export SVEN_SHARE_URL=<broker-url>\n    \
     export SVEN_SHARE_TOKEN=\"$(cat tenant.token)\"\n    \
     export SVEN_SHARE_TENANT=<tenant>\n\
     \nThen `/share [share-id]` exposes THIS session in-process.\n\
     \nOr run the fully-scriptable bridge in another terminal:\n\
     \n    sven -c <config> share --url <broker-url> \\\n      \
     --token \"$(cat tenant.token)\" --tenant-id <tenant> --title \"session\"\n\
     \nThe consultant then attaches with:\n\
     \n    sven cloud session attach --url <broker-url> \\\n      \
     --share-id <printed-id> --token \"$(cat operator.token)\" --prompt \"…\"\n\
     \nTools run locally; credentials never leave your machine."
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No broker configured → the scriptable-handoff instructions.
    #[test]
    fn no_broker_env_falls_back_to_instructions() {
        let action = build_share_action(&[], |_| None);
        match action {
            ImmediateAction::ShareInstructions { text } => {
                assert!(text.contains("share --url"), "shows the share command: {text}");
                assert!(text.contains("sven cloud session attach"));
            }
            other => panic!("expected ShareInstructions, got {other:?}"),
        }
    }

    /// `--print` forces the handoff even when a broker is configured.
    #[test]
    fn print_flag_forces_instructions() {
        let action = build_share_action(&["--print".to_string()], |k| match k {
            "SVEN_SHARE_URL" => Some("https://broker".into()),
            "SVEN_SHARE_TOKEN" => Some("tok".into()),
            "SVEN_SHARE_TENANT" => Some("acme".into()),
            _ => None,
        });
        assert!(matches!(action, ImmediateAction::ShareInstructions { .. }));
    }

    /// Broker configured → one-tap `ShareSession` with the resolved options and
    /// the positional argument threaded in as the share id.
    #[test]
    fn broker_env_yields_one_tap_share_session() {
        let action = build_share_action(&["my-share".to_string()], |k| match k {
            "SVEN_SHARE_URL" => Some("https://broker:8443".into()),
            "SVEN_SHARE_TOKEN" => Some("secret".into()),
            "SVEN_SHARE_TENANT" => Some("acme".into()),
            "SVEN_SHARE_INSECURE" => Some("true".into()),
            _ => None,
        });
        match action {
            ImmediateAction::ShareSession { options } => {
                assert_eq!(options.broker_url, "https://broker:8443");
                assert_eq!(options.token, "secret");
                assert_eq!(options.tenant_id, "acme");
                assert_eq!(options.share_id, "my-share");
                assert!(options.insecure_dev);
            }
            other => panic!("expected ShareSession, got {other:?}"),
        }
    }

    /// A missing tenant (but present url/token) still falls back rather than
    /// producing a half-configured share.
    #[test]
    fn missing_tenant_falls_back_to_instructions() {
        let action = build_share_action(&[], |k| match k {
            "SVEN_SHARE_URL" => Some("https://broker".into()),
            "SVEN_SHARE_TOKEN" => Some("tok".into()),
            _ => None,
        });
        assert!(matches!(action, ImmediateAction::ShareInstructions { .. }));
    }
}
