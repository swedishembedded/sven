// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for attributing the behaviour of
// LLM agents to the exact prompt that produced it for its clients. If your
// team needs expertise in reproducible prompt engineering and prompt
// ablation then you can procure our services by sending an email to
// info@swedishembedded.com.
//! The system prompt as an ordered list of named sections.
//!
//! [`Composer`] is the one place a system prompt is concatenated, so the text
//! and the account of which byte belongs to which section cannot disagree.
//! See [`sven_model::manifest`] for the tiling convention: a separator is the
//! leading bytes of the section that follows it, so the sections cover the
//! text exactly.

use std::ops::Range;

use sven_model::PromptSections;

/// A rendered system prompt and the sections it is made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposedPrompt {
    /// The text sent to the model as the system message.
    pub text: String,
    /// Where each named section sits in [`Self::text`], with its hash.
    pub sections: PromptSections,
}

/// Appends named sections to a growing system prompt.
#[derive(Debug, Default)]
pub(crate) struct Composer {
    text: String,
    parts: Vec<(String, Range<usize>)>,
}

impl Composer {
    /// Appends `piece` as section `id`, separator included. A piece with no
    /// text contributes nothing, so it has no section.
    pub(crate) fn push(&mut self, id: &str, piece: impl AsRef<str>) -> &mut Self {
        let piece = piece.as_ref();
        if !piece.is_empty() {
            let start = self.text.len();
            self.text.push_str(piece);
            self.parts.push((id.to_string(), start..self.text.len()));
        }
        self
    }

    pub(crate) fn finish(self) -> ComposedPrompt {
        let sections = PromptSections::new(&self.text, &self.parts);
        ComposedPrompt {
            text: self.text,
            sections,
        }
    }
}
