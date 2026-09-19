// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The fixed instance set, and the prompt each instance asks.
//!
//! Fixed rather than generated: two runs of this sample must be comparable,
//! and an instance set drawn from a seed is one more thing that can silently
//! differ between the arm that failed and the arm that passed.

use crate::svf::{self, Artifact};

/// One graded task.
pub struct Instance {
    /// Stable identifier, used in the report and as the per-instance answer file.
    pub id: String,
    /// Which artifact the agent is asked to produce.
    pub artifact: Artifact,
    /// The payload it is produced from.
    pub payload: Vec<u8>,
}

impl Instance {
    /// The hex string a correct answer contains.
    #[must_use]
    pub fn expected(&self) -> String {
        svf::hex(&svf::encode(self.artifact, &self.payload))
    }

    /// What the agent is asked to do. Names the artifact and the payload, and
    /// nothing about how either is encoded.
    #[must_use]
    pub fn prompt(&self) -> String {
        let subject = match self.artifact {
            // A header depends only on the payload's length, so stating the
            // length rather than 300 bytes of hex keeps the prompt short
            // without giving anything away.
            Artifact::Header => format!("a payload of {} bytes", self.payload.len()),
            _ => format!(
                "this payload, {} bytes, lowercase hex:\n\n    {}",
                self.payload.len(),
                svf::hex(&self.payload)
            ),
        };
        format!(
            "Read `SVF.md` in this directory - it specifies the SVF container format. \
             Read every other file in this directory too; one of them may carry values \
             `SVF.md` refers to but does not contain.\n\n\
             Produce the SVF **{}** for {}.\n\n\
             Write your answer to `{}` as a single JSON object and nothing else:\n\n\
             \x20   {{\"hex\": \"<the artifact, lowercase hex, no separators>\"}}\n\n\
             Do not explain your answer. If a value you need is not written down \
             anywhere in this directory, say so in the file instead: \
             {{\"hex\": \"\", \"missing\": \"<what you could not find>\"}}.",
            self.artifact.as_str(),
            subject,
            self.answer_file(),
        )
    }

    /// Where this instance's answer is written, inside the arm workspace.
    #[must_use]
    pub fn answer_file(&self) -> String {
        format!("out-{}.json", self.id)
    }
}

/// The twenty instances, in tier order.
///
/// Each tier needs strictly more of the errata than the one above it, so the
/// per-tier totals say *which* piece of knowledge landed rather than only how
/// much - a model that scores 6/20 learned the signature and the length bias
/// and nothing else.
#[must_use]
pub fn all() -> Vec<Instance> {
    let mut out = Vec::new();

    for (i, len) in [0usize, 1, 7, 64, 255, 300].into_iter().enumerate() {
        out.push(Instance {
            id: format!("header-{i}"),
            artifact: Artifact::Header,
            payload: (0..len).map(|b| (b % 251) as u8).collect(),
        });
    }

    let bodies: [&[u8]; 6] = [
        b"a",
        b"sven",
        b"\x00\x01\x02\x03",
        b"the quick brown fox",
        b"\xff\xfe\xfd\xfc\xfb",
        b"0123456789abcdef",
    ];
    for (i, payload) in bodies.into_iter().enumerate() {
        out.push(Instance {
            id: format!("preamble-{i}"),
            artifact: Artifact::Preamble,
            payload: payload.to_vec(),
        });
    }

    let frames: [&[u8]; 8] = [
        b"",
        b"x",
        b"\x00",
        b"\xff",
        b"swedish embedded",
        b"\x10\x20\x30\x40\x50",
        b"SVF",
        b"\x7f\x80\x81",
    ];
    for (i, payload) in frames.into_iter().enumerate() {
        out.push(Instance {
            id: format!("frame-{i}"),
            artifact: Artifact::Frame,
            payload: payload.to_vec(),
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_instance_set_is_twenty_in_three_tiers() {
        let all = all();
        assert_eq!(all.len(), 20);
        let count = |a: Artifact| all.iter().filter(|i| i.artifact == a).count();
        assert_eq!(count(Artifact::Header), 6);
        assert_eq!(count(Artifact::Preamble), 6);
        assert_eq!(count(Artifact::Frame), 8);
    }

    #[test]
    fn no_two_instances_in_a_tier_share_an_answer() {
        // A repeated answer would mean a tier scores several points for one
        // piece of knowledge, which makes the staircase read wrong.
        for tier in [Artifact::Header, Artifact::Preamble, Artifact::Frame] {
            let mut answers: Vec<String> = all()
                .iter()
                .filter(|i| i.artifact == tier)
                .map(Instance::expected)
                .collect();
            let before = answers.len();
            answers.sort();
            answers.dedup();
            assert_eq!(answers.len(), before, "{tier:?} has a duplicated answer");
        }
    }

    #[test]
    fn a_prompt_never_contains_its_own_answer_or_any_secret() {
        for instance in all() {
            let prompt = instance.prompt();
            assert!(
                !prompt.contains(&instance.expected()),
                "{} leaks its answer",
                instance.id
            );
            for secret in svf::secret_renderings() {
                assert!(
                    !prompt.to_lowercase().contains(&secret.to_lowercase()),
                    "{} leaks the secret {secret}",
                    instance.id
                );
            }
        }
    }
}
