// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The files a turn request attaches to its instruction.
//!
//! A request's `attachments` name files (or directories of them) relative to
//! the session's project root; they become parts of the same user message as
//! the instruction, loaded by `sven_tools_fs::load_attachment` exactly as the
//! `attach_file` tool and the CLI's `--attach` load them. The machine that
//! asked only named the paths: reading the files is I/O, so it happens here.

use std::path::Path;

use sven_model::{ContentPart, ModelProvider};

/// The user message of a turn: `instruction` followed by every attached file.
///
/// A path that leaves the project root, a missing file, a directory with no
/// image or audio in it, and a file the model cannot take are all errors: a
/// turn that was meant to show the model a file never runs without it.
pub(super) async fn instruction_parts(
    instruction: &str,
    attachments: &[String],
    root: Option<&Path>,
    model: &dyn ModelProvider,
) -> Result<Vec<ContentPart>, String> {
    let root = root.ok_or("the session has no project root to resolve attachments against")?;
    let mut parts = vec![ContentPart::text(instruction)];
    for name in attachments {
        for path in expand(root, name)? {
            parts.extend(load(&path, model).await?);
        }
    }
    Ok(parts)
}

/// The files `name` stands for: itself, or the attachable files directly in
/// it (sorted, so a turn is the same on every machine).
fn expand(root: &Path, name: &str) -> Result<Vec<std::path::PathBuf>, String> {
    let canonical_root = root
        .canonicalize()
        .map_err(|e| format!("project root {}: {e}", root.display()))?;
    let path = canonical_root
        .join(name)
        .canonicalize()
        .map_err(|e| format!("attachment '{name}': {e}"))?;
    if !path.starts_with(&canonical_root) {
        return Err(format!("attachment '{name}' is outside the project root"));
    }
    if !path.is_dir() {
        return Ok(vec![path]);
    }
    let mut files: Vec<_> = std::fs::read_dir(&path)
        .map_err(|e| format!("attachment '{name}': {e}"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|p| p.is_file() && attachable(p))
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(format!(
            "attachment '{name}' is a directory with no image or audio file in it"
        ));
    }
    Ok(files)
}

#[cfg(feature = "media")]
fn attachable(path: &Path) -> bool {
    sven_tools_fs::classify_attachment(path).is_some()
}

#[cfg(not(feature = "media"))]
fn attachable(_path: &Path) -> bool {
    false
}

#[cfg(feature = "media")]
async fn load(path: &Path, model: &dyn ModelProvider) -> Result<Vec<ContentPart>, String> {
    let opts = sven_tools_fs::AttachOptions {
        supports_images: model.supports_images(),
        supports_audio: model.supports_audio(),
        ..sven_tools_fs::AttachOptions::default()
    };
    let label = format!("{}/{}", model.name(), model.model_name());
    sven_tools_fs::load_attachment(path, &opts, &label)
        .await
        .map(sven_tools_fs::LoadedAttachment::into_content_parts)
        .map_err(|e| format!("attachment {}: {e}", path.display()))
}

#[cfg(not(feature = "media"))]
async fn load(path: &Path, _model: &dyn ModelProvider) -> Result<Vec<ContentPart>, String> {
    Err(format!(
        "attachment {}: this build of sven cannot attach files (built without the `media` feature)",
        path.display()
    ))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::expand;

    #[test]
    fn an_attachment_outside_the_project_root_is_refused() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(outer.path().join("secret.png"), b"x").unwrap();
        let error = expand(&root, "../secret.png").unwrap_err();
        assert!(error.contains("outside the project root"), "{error}");
        let error = expand(&root, "missing.png").unwrap_err();
        assert!(error.contains("missing.png"), "{error}");
    }

    #[cfg(feature = "media")]
    #[test]
    fn a_directory_stands_for_its_images_in_name_order() {
        let root = tempfile::tempdir().unwrap();
        let drawings = root.path().join("drawings");
        std::fs::create_dir(&drawings).unwrap();
        for name in ["b.png", "a.jpg", "notes.txt"] {
            std::fs::write(drawings.join(name), b"x").unwrap();
        }
        let files = expand(root.path(), "drawings").unwrap();
        let names: Vec<_> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["a.jpg", "b.png"]);
        std::fs::remove_file(drawings.join("a.jpg")).unwrap();
        std::fs::remove_file(drawings.join("b.png")).unwrap();
        let error = expand(root.path(), "drawings").unwrap_err();
        assert!(error.contains("no image or audio file"), "{error}");
    }
}
