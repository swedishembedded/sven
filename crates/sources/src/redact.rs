// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Removing credentials from text before it is stored.

/// The mark a removed secret leaves.
pub const REDACTED: &str = "[redacted]";

/// Replaces what looks like a credential with `REDACTED`: provider API keys
/// (`sk-...`), cloud access key ids (`AKIA...`), PEM private key blocks and the
/// value of a `password`, `secret`, `token` or `api_key` assignment. Everything
/// else is returned as it was. This is a safety net for what an agent or a
/// document might carry into memory, not a guarantee that no secret remains.
#[must_use]
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_key_block = false;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let ending = &line[trimmed.len()..];
        if trimmed.contains("-----BEGIN") && trimmed.contains("PRIVATE KEY-----") {
            in_key_block = true;
        }
        if in_key_block {
            out.push_str(REDACTED);
            out.push_str(ending);
            if trimmed.contains("-----END") {
                in_key_block = false;
            }
            continue;
        }
        out.push_str(&redact_line(trimmed));
        out.push_str(ending);
    }
    out
}

fn redact_line(line: &str) -> String {
    for name in ["password", "passwd", "secret", "token", "api_key", "apikey"] {
        let lower = line.to_lowercase();
        if let Some(at) = lower.find(name) {
            let rest = &line[at + name.len()..];
            let trimmed = rest.trim_start();
            if let Some(value) = trimmed.strip_prefix(['=', ':']) {
                if !value.trim().is_empty() {
                    let kept = line.len() - value.len();
                    return format!("{}{REDACTED}", &line[..kept]);
                }
            }
        }
    }
    line.split(' ')
        .map(|word| {
            if is_secret_word(word) {
                REDACTED.to_owned()
            } else {
                word.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_secret_word(word: &str) -> bool {
    let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_');
    let long = |prefix: &str, min: usize| {
        word.strip_prefix(prefix).is_some_and(|rest| {
            rest.len() >= min
                && rest
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
    };
    long("sk-", 20) || long("AKIA", 16) || long("ghp_", 30)
}
