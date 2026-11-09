// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Files a tool made or names, carried by handle and never by bytes.
//!
//! Swedish Embedded AB implements solutions for passing generated images,
//! audio and video between agents and people without copying them through a
//! language model for its clients. If your team needs expertise in multimodal
//! agent orchestration then you can procure our services by sending an email
//! to info@swedishembedded.com.
//!
//! An [`ArtifactRef`] names one file by its handle: `mcp-file://files/<id>`
//! from a server that keeps it, a working-directory path otherwise. A tool
//! result carries them (`ToolOutput::artifacts`) and so does the session
//! event a client watches; [`ShownArtifacts`] bounds what one result may
//! put in an event, as [`ShownImages`](crate::ShownImages) does for images.

use serde::{Deserialize, Serialize};

/// Most artifacts carried with one tool result.
pub const MAX_ARTIFACTS: usize = 16;

/// Longest `uri` of a carried artifact, in bytes.
pub const MAX_ARTIFACT_URI_BYTES: usize = 2048;

/// Longest `name` of a carried artifact, in bytes.
pub const MAX_ARTIFACT_NAME_BYTES: usize = 255;

/// Longest `mimeType` of a carried artifact, in bytes.
pub const MAX_ARTIFACT_MIME_BYTES: usize = 255;

/// Text appended to a result's output when artifacts beyond the bounds were
/// left out.
pub const TOO_MANY_ARTIFACTS_NOTE: &str =
    "(more files than can be shown, or files with oversized names, were left out)";

/// One file, named by handle: the server's file value (MCP's file transfer
/// extension) without the bytes. Its JSON shape is the wire shape of an
/// attachment on an input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    /// The handle of the file, e.g. `mcp-file://files/<id>`.
    pub uri: String,
    pub name: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    pub size: u64,
}

impl ArtifactRef {
    /// One line of a file list for an agent: `place` (where the agent finds
    /// the file: a path, or the name), the type and size, then `detail`
    /// (e.g. `; it is also in this message as an image`).
    pub fn list_line(&self, place: &str, detail: &str) -> String {
        format!(
            "\n- `{place}` ({}, {} bytes{detail})",
            self.mime_type, self.size
        )
    }

    /// The reference list appended to the text of a message whose files are
    /// named by handle only, so the agent can pass them on without fetching
    /// anything. Empty when there are none.
    pub fn handles_note(files: &[ArtifactRef]) -> String {
        if files.is_empty() {
            return String::new();
        }
        let mut note = String::from("\n\n(Files of this message:");
        for file in files {
            note.push_str(&file.list_line(&file.name, ""));
            note.push_str(&format!(": {}", file.uri));
        }
        note.push(')');
        note
    }

    fn within_bounds(&self) -> bool {
        self.uri.len() <= MAX_ARTIFACT_URI_BYTES
            && self.name.len() <= MAX_ARTIFACT_NAME_BYTES
            && self.mime_type.len() <= MAX_ARTIFACT_MIME_BYTES
    }
}

/// The artifacts of a tool result that fit the bounds, and the note for what
/// was left out.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ShownArtifacts {
    pub artifacts: Vec<ArtifactRef>,
    /// Empty when nothing was left out; otherwise text to append to the
    /// result's output so a reader knows a file is missing.
    pub note: String,
}

impl ShownArtifacts {
    /// The first [`MAX_ARTIFACTS`] of `artifacts` whose fields are within
    /// their bounds; the rest are dropped and `note` says so.
    pub fn from_artifacts(artifacts: &[ArtifactRef]) -> Self {
        let mut shown = Self::default();
        let mut left_out = false;
        for artifact in artifacts {
            if artifact.within_bounds() && shown.artifacts.len() < MAX_ARTIFACTS {
                shown.artifacts.push(artifact.clone());
            } else {
                left_out = true;
            }
        }
        if left_out {
            shown.note = TOO_MANY_ARTIFACTS_NOTE.to_string();
        }
        shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(n: usize) -> ArtifactRef {
        ArtifactRef {
            uri: format!("mcp-file://files/{n}"),
            name: format!("{n}.png"),
            mime_type: "image/png".into(),
            size: 10,
        }
    }

    #[test]
    fn json_shape_is_uri_name_mime_type_size() {
        let json = serde_json::to_value(artifact(1)).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"uri": "mcp-file://files/1", "name": "1.png",
                "mimeType": "image/png", "size": 10})
        );
    }

    #[test]
    fn extras_beyond_the_count_bound_are_dropped_with_a_note() {
        let all: Vec<_> = (0..MAX_ARTIFACTS + 3).map(artifact).collect();
        let shown = ShownArtifacts::from_artifacts(&all);
        assert_eq!(shown.artifacts, all[..MAX_ARTIFACTS]);
        assert_eq!(shown.note, TOO_MANY_ARTIFACTS_NOTE);
    }

    #[test]
    fn an_entry_over_a_field_bound_is_dropped_and_the_rest_kept() {
        let mut long_uri = artifact(1);
        long_uri.uri = "u".repeat(MAX_ARTIFACT_URI_BYTES + 1);
        let mut long_name = artifact(2);
        long_name.name = "n".repeat(MAX_ARTIFACT_NAME_BYTES + 1);
        let mut long_mime = artifact(3);
        long_mime.mime_type = "m".repeat(MAX_ARTIFACT_MIME_BYTES + 1);
        let mut at_limit = artifact(4);
        at_limit.name = "n".repeat(MAX_ARTIFACT_NAME_BYTES);
        let shown =
            ShownArtifacts::from_artifacts(&[long_uri, long_name, long_mime, at_limit.clone()]);
        assert_eq!(shown.artifacts, vec![at_limit]);
        assert_eq!(shown.note, TOO_MANY_ARTIFACTS_NOTE);
    }

    #[test]
    fn handles_note_lists_each_file_by_name_type_size_and_handle() {
        assert_eq!(ArtifactRef::handles_note(&[]), "");
        assert_eq!(
            ArtifactRef::handles_note(&[artifact(1), artifact(2)]),
            "\n\n(Files of this message:\n- `1.png` (image/png, 10 bytes): mcp-file://files/1\n- `2.png` (image/png, 10 bytes): mcp-file://files/2)"
        );
    }

    #[test]
    fn nothing_dropped_means_no_note() {
        let shown = ShownArtifacts::from_artifacts(&[artifact(1)]);
        assert_eq!(shown.artifacts.len(), 1);
        assert_eq!(shown.note, "");
    }
}
