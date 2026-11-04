// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Skills a server offers: resources whose URI is `skill://<path>`, each a
//! file of a skill directory (`skill://case-manager/SKILL.md`,
//! `skill://case-manager/references/intake.md`). sven writes them into a
//! directory of their own per server, which the session then reads as a
//! skills root of the lowest precedence: a skill the person has locally of
//! the same name wins. A path must be plain names, a file is bounded and so
//! is the whole set; what the server stopped offering is removed.
//!
//! Swedish Embedded AB implements solutions for distributing agent skills
//! from a service to every agent of its users for its clients. If your team
//! needs expertise in MCP integrations then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::path::Path;

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use rmcp::model::{ReadResourceRequestParams, ResourceContents};

use crate::connection::Connection;

/// The URI scheme of a served skill file.
pub const SKILL_SCHEME: &str = "skill://";
/// The most files a server's skills hold.
pub const MAX_FILES: usize = 512;
/// The most bytes a server's skills hold together.
pub const MAX_BYTES: usize = 16 * 1024 * 1024;

/// The relative path `uri` names, when it is a served skill file whose path
/// is plain names.
#[must_use]
pub fn skill_path(uri: &str) -> Option<&str> {
    let path = uri.strip_prefix(SKILL_SCHEME)?;
    let plain = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    (path.split('/').count() >= 2 && path.split('/').all(plain)).then_some(path)
}

/// Writes every skill file the server behind `connection` offers into
/// `dest`, replacing what was there. Answers how many files it wrote.
pub async fn sync(connection: &Connection, dest: &Path) -> Result<usize> {
    let resources = connection.list_resources().await?;
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut total = 0usize;
    for resource in resources {
        let Some(path) = skill_path(&resource.uri) else {
            continue;
        };
        if files.len() >= MAX_FILES {
            bail!("the server offers more than {MAX_FILES} skill files");
        }
        let read = connection
            .read_resource(ReadResourceRequestParams::new(resource.uri.clone()))
            .await
            .with_context(|| format!("reading {}", resource.uri))?;
        for contents in read.contents {
            let bytes = match contents {
                ResourceContents::TextResourceContents { text, .. } => text.into_bytes(),
                ResourceContents::BlobResourceContents { blob, .. } => {
                    base64::engine::general_purpose::STANDARD
                        .decode(blob.as_bytes())
                        .with_context(|| format!("{} is not base64", resource.uri))?
                }
                _ => continue,
            };
            total += bytes.len();
            if total > MAX_BYTES {
                bail!("the server's skills are larger than {MAX_BYTES} bytes");
            }
            files.push((path.to_string(), bytes));
        }
    }
    // Written beside the old set and swapped in, so a session reading the
    // directory meanwhile sees one set or the other, never half of each.
    let staging = dest.with_extension("new");
    let _ = std::fs::remove_dir_all(&staging);
    for (path, bytes) in &files {
        let target = staging.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, bytes)?;
    }
    std::fs::create_dir_all(&staging)?;
    let _ = std::fs::remove_dir_all(dest);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(&staging, dest)?;
    Ok(files.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_skill_file_is_plain_names_under_a_skill() {
        assert_eq!(
            skill_path("skill://case-manager/SKILL.md"),
            Some("case-manager/SKILL.md")
        );
        assert_eq!(
            skill_path("skill://a/references/b.md"),
            Some("a/references/b.md")
        );
        for bad in [
            "skill://SKILL.md",
            "skill://../etc/passwd",
            "skill://a/../b",
            "skill:///abs/SKILL.md",
            "skill://a//b",
            "file:///a/SKILL.md",
            "skill://a b/SKILL.md",
        ] {
            assert_eq!(skill_path(bad), None, "{bad}");
        }
    }
}
