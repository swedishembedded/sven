// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for measuring how a prompt change
// alters an LLM agent's next decision, in minutes instead of whole benchmark
// runs, for its clients. If your team needs expertise in prompt and tool
// definition tuning for coding agents then you can procure our services by
// sending an email to info@swedishembedded.com.
//! A variant of a recorded request: named prompt sections and tool
//! definitions swapped before the request is sent again.
//!
//! The file is YAML. Every key is optional:
//!
//! ```yaml
//! name: shorter-tools                # defaults to the file name
//! system_sections:
//!   - id: guidelines.tools           # a section id from `sven replay --list`
//!     replace: "### Tools\nUse them sparingly."
//!   - id: knowledge
//!     remove: true
//! tools:
//!   - name: grep
//!     description: "Search file contents."
//!     parameter_descriptions:
//!       pattern: "A regular expression."
//!   - name: find_file
//!     remove: true
//! append_message:
//!   role: user                       # or assistant
//!   text: "Continue."
//! ```
//!
//! Sections are found by the byte spans of the request's manifest. The text
//! of `replace` takes the place of the section's own text but keeps the
//! blank lines that separate it from the section before it, so a variant is
//! written as plain prose. `remove` takes the whole section, separator
//! included. Everything outside the named sections and tools stays
//! byte-identical.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context};
use serde::Deserialize;
use sven_model::manifest::sha256_hex;
use sven_model::{CompletionRequest, Message, MessageContent, Role};

/// What to change in a recorded request.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Variant {
    /// How the variant is called in the output.
    pub name: Option<String>,
    #[serde(default)]
    pub system_sections: Vec<SectionSwap>,
    #[serde(default)]
    pub tools: Vec<ToolSwap>,
    pub append_message: Option<AppendedMessage>,
}

/// Replace or remove one named section of the system prompt.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SectionSwap {
    pub id: String,
    pub replace: Option<String>,
    #[serde(default)]
    pub remove: bool,
}

/// Change or remove one tool definition.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolSwap {
    pub name: String,
    #[serde(default)]
    pub remove: bool,
    pub description: Option<String>,
    /// New `description` of top-level parameters, by parameter name.
    #[serde(default)]
    pub parameter_descriptions: BTreeMap<String, String>,
}

/// A message added to the end of the conversation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppendedMessage {
    pub role: AppendedRole,
    pub text: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AppendedRole {
    User,
    Assistant,
}

impl Variant {
    /// Reads and checks the variant file at `path`.
    pub(crate) fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading variant {}", path.display()))?;
        let mut variant: Self = serde_yaml::from_str(&text)
            .with_context(|| format!("parsing variant {}", path.display()))?;
        if variant.name.is_none() {
            variant.name = path.file_stem().map(|s| s.to_string_lossy().into_owned());
        }
        variant.check()?;
        Ok(variant)
    }

    /// The name shown in the output.
    pub(crate) fn label(&self) -> &str {
        self.name.as_deref().unwrap_or("variant")
    }

    fn check(&self) -> anyhow::Result<()> {
        let mut seen = std::collections::HashSet::new();
        for s in &self.system_sections {
            if s.replace.is_some() == s.remove {
                bail!(
                    "system section {:?} needs exactly one of `replace` and `remove: true`",
                    s.id
                );
            }
            if !seen.insert(s.id.as_str()) {
                bail!("system section {:?} is swapped twice", s.id);
            }
        }
        let mut seen = std::collections::HashSet::new();
        for t in &self.tools {
            let edits = t.description.is_some() || !t.parameter_descriptions.is_empty();
            if edits == t.remove {
                bail!(
                    "tool {:?} needs either `remove: true` or a new `description` / \
                     `parameter_descriptions`",
                    t.name
                );
            }
            if !seen.insert(t.name.as_str()) {
                bail!("tool {:?} is changed twice", t.name);
            }
        }
        Ok(())
    }

    /// Applies the swaps to `request`. The request's manifest no longer
    /// describes it afterwards, so it is cleared.
    pub(crate) fn apply(&self, request: &mut CompletionRequest) -> anyhow::Result<()> {
        self.swap_sections(request)?;
        self.swap_tools(request)?;
        if let Some(extra) = &self.append_message {
            request.messages.push(match extra.role {
                AppendedRole::User => Message::user(&extra.text),
                AppendedRole::Assistant => Message::assistant(&extra.text),
            });
        }
        request.manifest = None;
        Ok(())
    }

    fn swap_sections(&self, request: &mut CompletionRequest) -> anyhow::Result<()> {
        if self.system_sections.is_empty() {
            return Ok(());
        }
        let Some(manifest) = request.manifest.as_ref().filter(|m| !m.sections.is_empty()) else {
            bail!(
                "the request carries no section manifest (recorded before manifests, or its \
                 system prompt was rewritten by a compaction); pick another request"
            );
        };
        let system = request
            .messages
            .first()
            .filter(|m| m.role == Role::System)
            .and_then(Message::as_text)
            .context("the request has no plain-text system message")?;
        if manifest.system_sha256.as_deref() != Some(sha256_hex(system.as_bytes()).as_str()) {
            bail!("the system message does not match the hash its manifest recorded");
        }
        let mut edits = Vec::new();
        for swap in &self.system_sections {
            let Some(span) = manifest.sections.iter().find(|s| s.id == swap.id) else {
                let known: Vec<&str> = manifest.sections.iter().map(|s| s.id.as_str()).collect();
                bail!(
                    "this request has no section {:?}; it has: {}",
                    swap.id,
                    known.join(", ")
                );
            };
            if !system.is_char_boundary(span.start) || !system.is_char_boundary(span.end) {
                bail!("section {:?} does not lie on character boundaries", swap.id);
            }
            let old = &system[span.range()];
            let new = match &swap.replace {
                Some(text) => {
                    let separator = &old[..old.len() - old.trim_start().len()];
                    format!("{separator}{text}")
                }
                None => String::new(),
            };
            edits.push((span.range(), new));
        }
        // Back to front, so earlier spans keep their offsets.
        edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
        let mut text = system.to_string();
        for (range, new) in edits {
            text.replace_range(range, &new);
        }
        request.messages[0].content = MessageContent::Text(text);
        Ok(())
    }

    fn swap_tools(&self, request: &mut CompletionRequest) -> anyhow::Result<()> {
        for swap in &self.tools {
            let Some(at) = request.tools.iter().position(|t| t.name == swap.name) else {
                let known: Vec<&str> = request.tools.iter().map(|t| t.name.as_str()).collect();
                bail!(
                    "this request offers no tool {:?}; it offers: {}",
                    swap.name,
                    known.join(", ")
                );
            };
            if swap.remove {
                request.tools.remove(at);
                continue;
            }
            let tool = &mut request.tools[at];
            if let Some(description) = &swap.description {
                tool.description.clone_from(description);
            }
            for (parameter, description) in &swap.parameter_descriptions {
                let Some(property) = tool.parameters.pointer_mut(&format!(
                    "/properties/{}",
                    parameter.replace('~', "~0").replace('/', "~1")
                )) else {
                    bail!("tool {:?} has no parameter {parameter:?}", swap.name);
                };
                property["description"] = description.clone().into();
            }
        }
        request.core_tool_count = request.tools.iter().filter(|t| !t.is_mcp).count();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sven_model::manifest::{tool_digest, PromptSections};
    use sven_model::{PromptManifest, ToolSchema};

    use super::*;

    fn tool(name: &str) -> ToolSchema {
        ToolSchema {
            name: name.into(),
            description: format!("{name} does a thing"),
            parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            ..ToolSchema::default()
        }
    }

    fn request() -> CompletionRequest {
        let text = "identity\n\nmode text\n\nTools: use them.";
        let sections = PromptSections::new(
            text,
            &[
                ("identity".into(), 0..8),
                ("mode".into(), 8..19),
                ("tools".into(), 19..text.len()),
            ],
        );
        let messages = vec![Message::system(text), Message::user("go")];
        let tools = vec![tool("read"), tool("grep")];
        CompletionRequest {
            manifest: PromptManifest::describe(&messages, Some(&sections), &tools, None),
            messages,
            tools,
            ..CompletionRequest::default()
        }
    }

    fn variant(yaml: &str) -> Variant {
        let v: Variant = serde_yaml::from_str(yaml).unwrap();
        v.check().unwrap();
        v
    }

    fn system(r: &CompletionRequest) -> &str {
        r.messages[0].as_text().unwrap()
    }

    #[test]
    fn a_section_swap_changes_exactly_that_span() {
        let before = request();
        let mut after = before.clone();
        variant("system_sections: [{id: mode, replace: 'new mode'}]")
            .apply(&mut after)
            .unwrap();
        assert_eq!(
            system(&after),
            "identity\n\nnew mode\n\nTools: use them.",
            "the separator stays, the neighbours are untouched"
        );
        assert_eq!(after.tools.len(), 2);
        assert_eq!(after.messages.len(), 2);

        let mut removed = before.clone();
        variant("system_sections: [{id: mode, remove: true}]")
            .apply(&mut removed)
            .unwrap();
        assert_eq!(system(&removed), "identity\n\nTools: use them.");
    }

    #[test]
    fn two_section_swaps_do_not_shift_each_other() {
        let mut r = request();
        variant("system_sections: [{id: identity, replace: 'I'}, {id: tools, replace: 'T'}]")
            .apply(&mut r)
            .unwrap();
        assert_eq!(system(&r), "I\n\nmode text\n\nT");
    }

    #[test]
    fn a_tool_description_swap_changes_only_that_tool() {
        let before = request();
        let mut after = before.clone();
        variant(
            "tools: [{name: grep, description: 'Find text.', \
             parameter_descriptions: {path: 'Where.'}}]",
        )
        .apply(&mut after)
        .unwrap();
        assert_eq!(system(&after), system(&before));
        assert_eq!(tool_digest(&after.tools[0]), tool_digest(&before.tools[0]));
        assert_ne!(tool_digest(&after.tools[1]), tool_digest(&before.tools[1]));
        assert_eq!(after.tools[1].description, "Find text.");
        assert_eq!(
            after.tools[1].parameters["properties"]["path"],
            json!({"type": "string", "description": "Where."})
        );
    }

    #[test]
    fn a_removed_tool_and_an_appended_message_are_applied() {
        let mut r = request();
        variant(
            "tools: [{name: read, remove: true}]\n\
             append_message: {role: user, text: 'Continue.'}",
        )
        .apply(&mut r)
        .unwrap();
        assert_eq!(r.tools.len(), 1);
        assert_eq!(r.core_tool_count, 1);
        assert_eq!(r.messages.last().unwrap().as_text(), Some("Continue."));
    }

    #[test]
    fn unknown_targets_are_errors_that_name_the_choices() {
        let mut r = request();
        let e = variant("system_sections: [{id: nope, remove: true}]")
            .apply(&mut r)
            .unwrap_err();
        assert!(format!("{e}").contains("identity, mode, tools"), "{e}");
        let e = variant("tools: [{name: nope, remove: true}]")
            .apply(&mut r)
            .unwrap_err();
        assert!(format!("{e}").contains("read, grep"), "{e}");
    }

    #[test]
    fn a_request_without_a_manifest_cannot_swap_sections() {
        let mut r = request();
        r.manifest = None;
        let e = variant("system_sections: [{id: mode, remove: true}]")
            .apply(&mut r)
            .unwrap_err();
        assert!(format!("{e}").contains("no section manifest"), "{e}");
    }

    #[test]
    fn a_swap_without_exactly_one_action_is_refused() {
        for yaml in [
            "system_sections: [{id: mode}]",
            "system_sections: [{id: mode, replace: x, remove: true}]",
            "tools: [{name: grep}]",
            "tools: [{name: grep, description: x, remove: true}]",
        ] {
            let v: Variant = serde_yaml::from_str(yaml).unwrap();
            assert!(v.check().is_err(), "{yaml}");
        }
    }
}
