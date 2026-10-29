// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for attributing the behaviour of
// LLM agents to the exact prompt and tool definitions that produced it for
// its clients. If your team needs expertise in reproducible prompt
// engineering and prompt ablation then you can procure our services by
// sending an email to info@swedishembedded.com.
//! What a model request is made of: named system-prompt sections and tool
//! definitions, each with a content hash.
//!
//! A [`PromptManifest`] rides on a [`crate::CompletionRequest`] so a recorded
//! request says which part of it is which. An experiment can then replace one
//! section, found by its id and byte span, or one tool description, found by
//! its name, and send the request again knowing exactly what changed.
//!
//! The manifest describes a request; it is never part of one. No provider
//! sends it, and it is not hashed into any cache key.
//!
//! # Section convention
//!
//! The sections of a system prompt tile its text: the first starts at byte 0,
//! each starts where the previous ended, and the last ends at the end of the
//! text. A separator belongs to the section that follows it (as its leading
//! bytes), so replacing the bytes of a section by id never leaves a gap or a
//! doubled separator. [`PromptSections::new`] enforces the tiling.

use std::fmt::Write as _;
use std::ops::Range;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{Message, Role, ToolSchema};

/// The lowercase hexadecimal SHA-256 of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// One named section of a system prompt: where it is and what it hashes to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionSpan {
    /// Stable name of the section (`identity`, `guidelines.tool_usage`, ...).
    pub id: String,
    /// First byte of the section in the system text.
    pub start: usize,
    /// One past the last byte of the section.
    pub end: usize,
    /// SHA-256 of `text[start..end]`.
    pub sha256: String,
}

impl SectionSpan {
    /// The byte range of the section in the system text.
    #[must_use]
    pub fn range(&self) -> Range<usize> {
        self.start..self.end
    }
}

/// The sections a composer made a system prompt of, with the hash of the
/// whole text they tile. Kept beside the text it describes; a manifest only
/// uses it while the text still hashes to [`Self::sha256`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptSections {
    /// SHA-256 of the whole system text.
    pub sha256: String,
    /// Length of the whole system text in bytes.
    pub bytes: usize,
    /// The sections in text order, tiling `0..bytes`.
    pub sections: Vec<SectionSpan>,
}

impl PromptSections {
    /// The sections of `text`, which are `parts` (an id and the text of each)
    /// concatenated in order.
    ///
    /// # Panics
    ///
    /// When the parts do not concatenate to `text`: a composer that renders
    /// the text and the parts separately has a bug that would make every
    /// recorded span lie.
    #[must_use]
    pub fn new(text: &str, parts: &[(String, Range<usize>)]) -> Self {
        let mut at = 0;
        let sections = parts
            .iter()
            .map(|(id, range)| {
                assert_eq!(range.start, at, "section {id} leaves a gap or overlaps");
                at = range.end;
                SectionSpan {
                    id: id.clone(),
                    start: range.start,
                    end: range.end,
                    sha256: sha256_hex(&text.as_bytes()[range.clone()]),
                }
            })
            .collect();
        assert_eq!(at, text.len(), "sections stop short of the system text");
        Self {
            sha256: sha256_hex(text.as_bytes()),
            bytes: text.len(),
            sections,
        }
    }

    /// The section named `id`.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&SectionSpan> {
        self.sections.iter().find(|s| s.id == id)
    }
}

/// The digest of one tool definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDigest {
    pub name: String,
    /// SHA-256 of the canonical form of the name, description and parameter
    /// schema (see [`tool_digest`]).
    pub sha256: String,
}

/// What a request is made of. See the module documentation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PromptManifest {
    /// SHA-256 of the system message text; `None` when the request has none.
    pub system_sha256: Option<String>,
    /// Length of the system message text in bytes.
    pub system_bytes: usize,
    /// The named sections of the system text. Empty when the system text is
    /// not known to come from a composer.
    pub sections: Vec<SectionSpan>,
    /// The offered tool definitions in request order.
    pub tools: Vec<ToolDigest>,
    /// One digest over the ordered tool list: a reordering or an added or
    /// removed tool changes it as well as the tool's own digest.
    pub tools_sha256: String,
    /// SHA-256 of the volatile suffix appended after the cached system block.
    pub dynamic_suffix_sha256: Option<String>,
}

impl PromptManifest {
    /// The manifest of a request made of `messages`, `tools` and
    /// `dynamic_suffix`. `sections` is what the composer reported for the
    /// system text it rendered, if the caller has it.
    ///
    /// `None` when `sections` is given but the first message no longer is
    /// that text (a compaction rewrote it): a manifest is dropped rather than
    /// reported against text it does not describe.
    #[must_use]
    pub fn describe(
        messages: &[Message],
        sections: Option<&PromptSections>,
        tools: &[ToolSchema],
        dynamic_suffix: Option<&str>,
    ) -> Option<Self> {
        let system = messages
            .first()
            .filter(|m| m.role == Role::System)
            .and_then(Message::as_text);
        let system_sha256 = system.map(|t| sha256_hex(t.as_bytes()));
        let sections = match (sections, &system_sha256) {
            (Some(known), Some(now)) if known.sha256 == *now => known.sections.clone(),
            (Some(_), _) => return None,
            (None, _) => Vec::new(),
        };
        let tools: Vec<ToolDigest> = tools
            .iter()
            .map(|t| ToolDigest {
                name: t.name.clone(),
                sha256: tool_digest(t),
            })
            .collect();
        let list: String = tools.iter().map(|t| format!("{}\n", t.sha256)).collect();
        Some(Self {
            system_bytes: system.map_or(0, str::len),
            system_sha256,
            sections,
            tools_sha256: sha256_hex(list.as_bytes()),
            tools,
            dynamic_suffix_sha256: dynamic_suffix.map(|s| sha256_hex(s.as_bytes())),
        })
    }
}

/// The digest of a tool definition: SHA-256 of the canonical JSON of its
/// name, description and parameter schema. Canonical means object keys in
/// sorted order and no insignificant whitespace, so the digest depends on the
/// definition and not on how a map happened to be ordered. `is_mcp` is not
/// part of the definition a model sees and is not hashed.
#[must_use]
pub fn tool_digest(tool: &ToolSchema) -> String {
    let definition = serde_json::json!({
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.parameters,
    });
    let mut canonical = String::new();
    write_canonical(&definition, &mut canonical);
    sha256_hex(canonical.as_bytes())
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str, parameters: Value) -> ToolSchema {
        ToolSchema {
            name: name.into(),
            description: format!("{name} does a thing"),
            parameters,
            ..ToolSchema::default()
        }
    }

    fn parts(text: &str, cuts: &[(&str, usize)]) -> PromptSections {
        let mut at = 0;
        let parts: Vec<_> = cuts
            .iter()
            .map(|(id, end)| {
                let part = ((*id).to_string(), at..*end);
                at = *end;
                part
            })
            .collect();
        PromptSections::new(text, &parts)
    }

    #[test]
    fn sections_tile_the_text_and_hash_their_bytes() {
        let sections = parts("abcdef", &[("a", 2), ("b", 6)]);
        assert_eq!(sections.get("b").unwrap().range(), 2..6);
        assert_eq!(sections.get("b").unwrap().sha256, sha256_hex(b"cdef"));
        assert_eq!(sections.sha256, sha256_hex(b"abcdef"));
    }

    #[test]
    #[should_panic(expected = "gap or overlaps")]
    fn a_gap_between_sections_is_a_bug() {
        let _ = PromptSections::new("abcdef", &[("a".into(), 0..2), ("b".into(), 3..6)]);
    }

    #[test]
    fn tool_digest_ignores_key_order_and_changes_with_content() {
        let a = tool(
            "t",
            json!({"type": "object", "required": ["x"], "properties": {}}),
        );
        let b = tool(
            "t",
            json!({"properties": {}, "required": ["x"], "type": "object"}),
        );
        assert_eq!(tool_digest(&a), tool_digest(&b));
        let mut c = a.clone();
        c.description.push('!');
        assert_ne!(tool_digest(&a), tool_digest(&c));
    }

    #[test]
    fn tool_list_digest_follows_the_order() {
        let (a, b) = (tool("a", json!({})), tool("b", json!({})));
        let ab = PromptManifest::describe(&[], None, &[a.clone(), b.clone()], None).unwrap();
        let ba = PromptManifest::describe(&[], None, &[b, a], None).unwrap();
        assert_ne!(ab.tools_sha256, ba.tools_sha256);
        assert_eq!(ab.tools[0].name, "a");
    }

    #[test]
    fn sections_are_kept_while_the_system_text_is_what_was_composed() {
        let sections = parts("abcdef", &[("a", 2), ("b", 6)]);
        let messages = [Message::system("abcdef"), Message::user("hi")];
        let m = PromptManifest::describe(&messages, Some(&sections), &[], Some("tail")).unwrap();
        assert_eq!(m.system_sha256.as_deref(), Some(sections.sha256.as_str()));
        assert_eq!(m.sections.len(), 2);
        assert_eq!(m.dynamic_suffix_sha256, Some(sha256_hex(b"tail")));
    }

    #[test]
    fn the_manifest_is_dropped_when_the_system_text_changed() {
        let sections = parts("abcdef", &[("a", 2), ("b", 6)]);
        for messages in [
            vec![Message::system("summary of abcdef")],
            vec![Message::user("no system message any more")],
        ] {
            assert!(PromptManifest::describe(&messages, Some(&sections), &[], None).is_none());
        }
    }

    #[test]
    fn a_system_text_of_unknown_origin_has_a_hash_and_no_sections() {
        let m = PromptManifest::describe(&[Message::system("x")], None, &[], None).unwrap();
        assert!(m.system_sha256.is_some() && m.sections.is_empty());
    }
}
