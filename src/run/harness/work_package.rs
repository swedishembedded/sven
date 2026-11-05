// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The tar package of a served harness run ([`sven_control::work`]): unpacked
//! into the run's private directory, and named by the run's inputs.
//!
//! The archive comes from a server, so it is untrusted data. Only regular
//! files and directories are ever created, and only below the destination;
//! the first entry that is anything else, or that would land elsewhere,
//! refuses the whole archive.
//!
//! Swedish Embedded AB implements solutions for running third-party agent
//! workflow packages safely for its clients. If your team needs expertise in
//! sandboxing and supply-chain checks for agent tooling then you can procure
//! our services by sending an email to info@swedishembedded.com.

use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use sven_control::work::{MAX_TAR_BYTES, MAX_TAR_ENTRIES};

/// `path` as the names below the archive's root, refused when it is empty,
/// absolute, or climbs out. A leading `./` is the archive's own root.
fn relative_parts(path: &Path) -> Option<Vec<&std::ffi::OsStr>> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => parts.push(name),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(parts)
}

/// Unpacks `archive` (a tar) into `dest`, which must not exist yet, and
/// makes everything in it read-only.
///
/// Only regular files and directories are created; an entry that is a link,
/// a device or anything else, a path that is absolute or has a `..`
/// component, a duplicate, or more than [`MAX_TAR_ENTRIES`] entries or
/// [`MAX_TAR_BYTES`] bytes of content refuses the archive. Entries are
/// created below `dest` only and no link is ever created, so no earlier
/// entry can redirect a later one.
///
/// # Errors
///
/// The entry that was refused (by its name), or an unreadable archive or
/// unwritable `dest`. What was written before stays under `dest`.
pub(super) fn unpack_tar(archive: &[u8], dest: &Path) -> Result<()> {
    if dest.exists() {
        bail!("{} already exists", dest.display());
    }
    std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
    let (mut entries, mut bytes) = (0usize, 0u64);
    for entry in tar::Archive::new(archive)
        .entries()
        .context("the package is not a tar archive")?
    {
        let mut entry = entry.context("reading the package archive")?;
        let name = entry.path().context("an entry name")?.into_owned();
        let shown = name.display();
        let kind = entry.header().entry_type();
        if kind == tar::EntryType::XGlobalHeader {
            continue;
        }
        entries += 1;
        if entries > MAX_TAR_ENTRIES {
            bail!("the package has more than {MAX_TAR_ENTRIES} entries");
        }
        let parts = relative_parts(&name)
            .ok_or_else(|| anyhow!("{shown} is not a path inside the package"))?;
        let target = parts.iter().fold(dest.to_path_buf(), |dir, p| dir.join(p));
        match kind {
            tar::EntryType::Directory => {
                std::fs::create_dir_all(&target).with_context(|| format!("creating {shown}"))?
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                if parts.is_empty() {
                    bail!("{shown} is not a path inside the package");
                }
                bytes = bytes.saturating_add(entry.size());
                if bytes > MAX_TAR_BYTES {
                    bail!("the package holds more than {MAX_TAR_BYTES} bytes");
                }
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).with_context(|| format!("creating {shown}"))?;
                }
                let executable = entry.header().mode().unwrap_or(0) & 0o111 != 0;
                write_new(&target, &mut entry, executable)
                    .with_context(|| format!("unpacking {shown}"))?;
            }
            other => {
                bail!("{shown} is a {other:?} entry; a package holds only files and directories")
            }
        }
    }
    seal(dest)
}

/// Creates `target`, which must not exist, with `content`.
fn write_new(target: &Path, content: &mut impl std::io::Read, executable: bool) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(if executable { 0o755 } else { 0o644 });
    }
    #[cfg(not(unix))]
    let _ = executable;
    let mut file = options.open(target)?;
    std::io::copy(content, &mut file)?;
    Ok(())
}

/// Removes write permission from `path` and everything below it, children
/// first so a directory is still searchable while it is walked.
fn seal(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() {
        for child in std::fs::read_dir(path)? {
            seal(&child?.path())?;
        }
    }
    let mut permissions = meta.permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(path, permissions)
        .with_context(|| format!("sealing {}", path.display()))
}

/// The absolute path of `relative` in the package rooted at `root`.
///
/// # Errors
///
/// The path is absolute, empty or leaves the package, or names nothing.
pub(super) fn package_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let parts = relative_parts(Path::new(relative))
        .filter(|parts| !parts.is_empty())
        .ok_or_else(|| anyhow!("{relative:?} is not a path inside the package"))?;
    let path = parts.iter().fold(root.to_path_buf(), |dir, p| dir.join(p));
    let found = std::fs::canonicalize(&path)
        .map_err(|_| anyhow!("{relative:?} names nothing in the package"))?;
    if !found.starts_with(std::fs::canonicalize(root)?) {
        bail!("{relative:?} is not a path inside the package");
    }
    Ok(found)
}

/// Replaces every `{"package": "<path>"}` in `inputs` (an object with that
/// one key, at any depth) by the absolute path of that entry of the package
/// rooted at `root`.
///
/// # Errors
///
/// A path [`package_path`] refuses.
pub(super) fn resolve_inputs(inputs: &mut Value, root: &Path) -> Result<()> {
    match inputs {
        Value::Object(map)
            if map.len() == 1 && map.get("package").is_some_and(Value::is_string) =>
        {
            let relative = map["package"].as_str().unwrap_or_default();
            *inputs = Value::String(package_path(root, relative)?.display().to_string());
        }
        Value::Object(map) => {
            for value in map.values_mut() {
                resolve_inputs(value, root)?;
            }
        }
        Value::Array(items) => {
            for value in items {
                resolve_inputs(value, root)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Copies what is in directory `from` into `to` (created): files and
/// directories only, the copies writable.
///
/// # Errors
///
/// An unreadable source, an entry that is neither file nor directory, or an
/// unwritable target.
pub(super) fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
            let mut permissions = std::fs::metadata(&target)?.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            std::fs::set_permissions(&target, permissions)?;
        } else {
            bail!("{} is not a file or directory", entry.path().display());
        }
    }
    Ok(())
}

/// Makes a sealed tree removable again (tests clean up with it).
#[cfg(test)]
pub(super) fn restore_write(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::symlink_metadata(root) {
        if meta.is_dir() {
            let _ = std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o755));
            for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
                restore_write(&entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Spec: an archive unpacks to its regular files and directories, below
    //! the destination and read-only; anything else refuses it before a byte
    //! lands outside the destination.

    use super::*;

    fn raw(builder: &mut tar::Builder<Vec<u8>>, name: &str, kind: tar::EntryType, link: &str) {
        let mut header = tar::Header::new_gnu();
        let gnu = header.as_gnu_mut().unwrap();
        gnu.name[..name.len()].copy_from_slice(name.as_bytes());
        gnu.linkname[..link.len()].copy_from_slice(link.as_bytes());
        header.set_entry_type(kind);
        header.set_mode(0o644);
        header.set_size(if kind == tar::EntryType::Regular {
            1
        } else {
            0
        });
        header.set_cksum();
        let body: &[u8] = if kind == tar::EntryType::Regular {
            b"x"
        } else {
            b""
        };
        builder.append(&header, body).unwrap();
    }

    fn archive(entries: &[(&str, tar::EntryType, &str)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, kind, link) in entries {
            raw(&mut builder, name, *kind, link);
        }
        builder.into_inner().unwrap()
    }

    /// The scratch directory holds `package/` (the destination) and a
    /// sibling a traversal would reach.
    fn refused(entries: &[(&str, tar::EntryType, &str)]) -> String {
        let scratch = tempfile::tempdir().unwrap();
        let dest = scratch.path().join("package");
        let error = unpack_tar(&archive(entries), &dest).expect_err("refused");
        let outside: Vec<_> = std::fs::read_dir(scratch.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(
            outside.len() <= 1,
            "nothing is written beside the destination: {outside:?}"
        );
        restore_write(&dest);
        format!("{error:#}")
    }

    #[test]
    fn a_traversing_path_is_refused() {
        assert!(refused(&[("../x", tar::EntryType::Regular, "")]).contains("../x"));
        assert!(refused(&[("a/../../x", tar::EntryType::Regular, "")]).contains("a/../../x"));
    }

    #[test]
    fn an_absolute_path_is_refused() {
        assert!(refused(&[("/outside/x", tar::EntryType::Regular, "")]).contains("/outside/x"));
    }

    #[test]
    fn links_and_devices_are_refused() {
        for kind in [
            tar::EntryType::Symlink,
            tar::EntryType::Link,
            tar::EntryType::Char,
            tar::EntryType::Block,
            tar::EntryType::Fifo,
        ] {
            assert!(
                refused(&[("p/l", kind, "../x")]).contains("p/l"),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn a_refused_entry_after_good_ones_leaves_nothing_runnable_outside() {
        let message = refused(&[
            ("p/ok", tar::EntryType::Regular, ""),
            ("p/link", tar::EntryType::Symlink, "/etc"),
            ("p/link/passwd", tar::EntryType::Regular, ""),
        ]);
        assert!(message.contains("p/link"));
    }

    #[test]
    fn a_good_archive_unpacks_read_only_and_resolves_package_inputs() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = tempfile::tempdir().unwrap();
        let dest = scratch.path().join("package");
        let bytes = archive(&[
            ("./p/", tar::EntryType::Directory, ""),
            ("p/data/a.cfg", tar::EntryType::Regular, ""),
        ]);
        unpack_tar(&bytes, &dest).unwrap();
        let file = dest.join("p/data/a.cfg");
        assert_eq!(std::fs::read(&file).unwrap(), b"x");
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o222,
            0
        );
        assert_eq!(
            std::fs::metadata(dest.join("p/data"))
                .unwrap()
                .permissions()
                .mode()
                & 0o222,
            0
        );
        let mut inputs = serde_json::json!({
            "printer": { "package": "p/data" }, "n": 3, "list": [{ "package": "p/data/a.cfg" }],
            "plain": { "package": "p/data", "other": 1 }
        });
        resolve_inputs(&mut inputs, &dest).unwrap();
        let root = std::fs::canonicalize(&dest).unwrap();
        assert_eq!(inputs["printer"], root.join("p/data").to_str().unwrap());
        assert_eq!(
            inputs["list"][0],
            root.join("p/data/a.cfg").to_str().unwrap()
        );
        assert_eq!(inputs["n"], 3);
        assert_eq!(
            inputs["plain"]["other"], 1,
            "only an exact object is a reference"
        );
        for bad in ["../x", "/etc", "p/missing", ""] {
            let mut inputs = serde_json::json!({ "v": { "package": bad } });
            assert!(resolve_inputs(&mut inputs, &dest).is_err(), "{bad}");
        }
        restore_write(&dest);
    }
}
