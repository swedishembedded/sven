// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for tracing which context each
// decision of an LLM agent was made in, so training data only ever teaches
// what the model could see, for its clients. If your team needs expertise in
// agent trajectory provenance then you can procure our services by sending
// an email to info@swedishembedded.com.
//! Which context blocks a model request is made of, by name and digest.
//!
//! Read from the request's [`PromptManifest`]: the named sections of its
//! system prompt when the composer's account of them still describes the
//! text, else the system prompt as one block, then the tool definitions. Names come from where a block came from, never from
//! what it says (see [`ContextBlock`]).

use sven_model::{CompletionRequest, PromptManifest};
use sven_vocab::ContextBlock;

/// The name of the block a named system-prompt section is: `system/<id>`.
#[must_use]
pub fn section_block_name(section_id: &str) -> String {
    format!("{SYSTEM_BLOCK}/{section_id}")
}

/// The block name of a system prompt whose sections are not known.
pub const SYSTEM_BLOCK: &str = "system";
/// The block name of the offered tool definitions, taken together.
pub const TOOLS_BLOCK: &str = "tools";

/// The id of every section the composer can make a system prompt of
/// (`crate::prompts::compose_system_prompt`), in prompt order.
pub const SECTION_IDS: [&str; 14] = [
    "identity",
    "mode",
    "project",
    "git",
    "project_instructions",
    "skills",
    "agents",
    "knowledge",
    "knowledge_drift",
    "ci",
    "guidelines.working_method",
    "guidelines.tools",
    "custom",
    "append",
];

/// The system-prompt section a block name names (`system/append` is
/// `append`), when it names one a prompt can be composed without.
///
/// # Errors
///
/// A name that is not `system/<section>` for a section the composer makes,
/// with the names that are: the tool list is not a section, and the unsectioned `system` block is the whole prompt.
pub fn droppable_section(block: &str) -> Result<&str, String> {
    block
        .strip_prefix("system/")
        .filter(|id| SECTION_IDS.contains(id))
        .ok_or_else(|| {
            let names: Vec<String> = SECTION_IDS
                .iter()
                .map(|id| section_block_name(id))
                .collect();
            format!(
                "{block:?} is not a context block a session can be started without; \
                 the blocks that can be dropped are {}",
                names.join(", ")
            )
        })
}

/// The context blocks of `request`, in request order.
#[must_use]
pub fn context_blocks(request: &CompletionRequest) -> Vec<ContextBlock> {
    let described;
    let manifest = match &request.manifest {
        Some(manifest) => manifest,
        None => {
            described = PromptManifest::describe(&request.messages, None, &request.tools);
            match &described {
                Some(manifest) => manifest,
                None => return Vec::new(),
            }
        }
    };
    let block = |name: String, sha256: &str| ContextBlock {
        name,
        sha256: sha256.to_string(),
    };
    let mut blocks: Vec<ContextBlock> = if manifest.sections.is_empty() {
        manifest
            .system_sha256
            .iter()
            .map(|sha| block(SYSTEM_BLOCK.to_string(), sha))
            .collect()
    } else {
        manifest
            .sections
            .iter()
            .map(|section| block(section_block_name(&section.id), &section.sha256))
            .collect()
    };
    if !manifest.tools.is_empty() {
        blocks.push(block(TOOLS_BLOCK.to_string(), &manifest.tools_sha256));
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_model::manifest::{sha256_hex, PromptSections};
    use sven_model::{Message, ToolSchema};

    fn request(sections: Option<&PromptSections>, system: &str) -> CompletionRequest {
        let messages = vec![Message::system(system), Message::user("go")];
        let tools = vec![ToolSchema {
            name: "probe".into(),
            ..ToolSchema::default()
        }];
        CompletionRequest {
            manifest: PromptManifest::describe(&messages, sections, &tools),
            messages,
            tools,
            ..CompletionRequest::default()
        }
    }

    #[test]
    fn a_composed_prompt_is_listed_section_by_section_with_its_digests() {
        let text = "identity\n\nhint";
        let sections =
            PromptSections::new(text, &[("identity".into(), 0..8), ("append".into(), 8..14)]);
        let blocks = context_blocks(&request(Some(&sections), text));
        let names: Vec<&str> = blocks.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["system/identity", "system/append", "tools"]);
        assert_eq!(blocks[1].sha256, sha256_hex(b"\n\nhint"));
    }

    #[test]
    fn only_a_named_system_section_can_be_dropped() {
        assert_eq!(droppable_section("system/append"), Ok("append"));
        assert_eq!(droppable_section("system/knowledge"), Ok("knowledge"));
        for name in ["append", "system", "tools", "system/nope"] {
            let refused = droppable_section(name).unwrap_err();
            assert!(refused.contains("system/append"), "{refused}");
        }
    }

    #[test]
    fn a_system_prompt_of_unknown_sections_is_one_block() {
        let mut req = request(None, "whatever was composed");
        let blocks = context_blocks(&req);
        assert_eq!(blocks[0].name, "system");
        assert_eq!(blocks[0].sha256, sha256_hex(b"whatever was composed"));
        // A request built without a manifest is described from its messages.
        req.manifest = None;
        assert_eq!(context_blocks(&req), blocks);
    }
}
