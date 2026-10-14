// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Parsing of a `SKILL.md` file into its frontmatter fields and body.

use serde::Deserialize;

use crate::skills::SvenSkillMeta;

#[derive(Debug, Deserialize)]
struct RawFrontmatter {
    /// Optional display name; falls back to the directory name when omitted.
    #[serde(default)]
    name: Option<String>,
    description: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    sven: Option<SvenSkillMeta>,
}

// ── Parsing ───────────────────────────────────────────────────────────────────

/// Parsed frontmatter fields plus the SKILL.md body.
pub struct ParsedSkill {
    /// Optional display name from frontmatter.
    pub name: Option<String>,
    pub description: String,
    pub version: Option<String>,
    pub sven_meta: Option<SvenSkillMeta>,
    /// Everything after the closing `---` fence, with leading whitespace trimmed.
    pub body: String,
}

/// Parse a raw SKILL.md string into its frontmatter fields and body.
///
/// The `description` field is required.  The `name` field is optional; callers
/// should fall back to the directory name when it is absent.
///
/// Returns `None` when the frontmatter is missing, malformed, or lacks a
/// non-empty `description`.
#[must_use]
pub fn parse_skill_file(raw: &str) -> Option<ParsedSkill> {
    parse_skill_frontmatter(raw).ok()
}

/// [`parse_skill_file`] with the reason a file is refused: the YAML error
/// names the offending key, for example an unknown `activation` key.
///
/// # Errors
///
/// Returns the reason when the frontmatter is missing, malformed, lacks a
/// non-empty `description` or carries an invalid `activation` block.
pub fn parse_skill_frontmatter(raw: &str) -> Result<ParsedSkill, String> {
    let rest = raw.trim_start_matches('\n');

    // ── Frontmatter path ─────────────────────────────────────────────────────
    if let Some(after_open) = rest.strip_prefix("---") {
        let close = after_open
            .find("\n---")
            .ok_or("the frontmatter fence is not closed")?;
        let yaml_block = &after_open[..close];
        // Body starts after "\n---" (4 bytes). Strip one leading newline if present.
        let body = after_open[close + 4..].trim_start_matches('\n').to_string();

        let fm: RawFrontmatter = serde_yaml::from_str(yaml_block).map_err(|e| e.to_string())?;

        if fm.description.trim().is_empty() {
            return Err("the description is empty".to_string());
        }
        if let Some(activation) = fm.sven.as_ref().and_then(|m| m.activation.as_ref()) {
            activation.validate()?;
        }

        return Ok(ParsedSkill {
            name: fm.name.filter(|n| !n.trim().is_empty()),
            description: fm.description,
            version: fm.version,
            sven_meta: fm.sven,
            body,
        });
    }

    // ── Frontmatter-free path ─────────────────────────────────────────────────
    // A SKILL.md with no `---` fence is accepted as a plain-markdown skill.
    // The description is synthesised from the first non-empty, non-heading line
    // (or from the text of the first `# Heading` if no plain line comes first).
    // The entire file becomes the body.
    if rest.trim().is_empty() {
        return Err("the file is empty".to_string());
    }

    let description = rest
        .lines()
        .find_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            // Strip a leading `#` heading marker so "# Sven" → "Sven".
            let text = line.trim_start_matches('#').trim();
            if text.is_empty() {
                None
            } else {
                Some(text.to_string())
            }
        })
        .unwrap_or_else(|| "Skill".to_string());

    Ok(ParsedSkill {
        name: None,
        description,
        version: None,
        sven_meta: None,
        body: rest.to_string(),
    })
}
