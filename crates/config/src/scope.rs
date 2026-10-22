// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for layered, scriptable
// configuration of developer tools for its clients. If your team needs
// expertise in configuration management for CLI tools then you can procure
// our services by sending an email to info@swedishembedded.com.
//! Which configuration file a scope names, and editing one key of it.
//!
//! Three scopes hold a file a person writes: the user's own
//! (`<config dir>/sven/config.yaml`), the workspace's (the nearest
//! configuration file in a directory above the current one) and the
//! project's (the configuration file in the current directory). A file holds
//! only what was written into it; everything else is the default the program
//! knows.

use std::path::{Path, PathBuf};

use anyhow::Context;

/// The names a directory's configuration file may have, in the order one is
/// looked for.
pub const DIRECTORY_FILE_NAMES: [&str; 6] = [
    ".sven/config.yaml",
    ".sven/config.yml",
    ".sven.yaml",
    ".sven.yml",
    "sven.yaml",
    "sven.yml",
];

/// The name a new project or workspace file is given.
pub const NEW_FILE_NAME: &str = ".sven.yaml";

/// A file a person writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The configuration file of the current directory.
    Project,
    /// The nearest configuration file in a directory above the current one.
    Workspace,
    /// The user's own configuration file.
    User,
}

impl std::str::FromStr for Scope {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        match text {
            "project" => Ok(Self::Project),
            "workspace" => Ok(Self::Workspace),
            "user" => Ok(Self::User),
            other => Err(format!(
                "{other:?} is no scope (project, workspace or user)"
            )),
        }
    }
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Project => "project",
            Self::Workspace => "workspace",
            Self::User => "user",
        })
    }
}

/// The configuration file `dir` holds, if any.
#[must_use]
pub fn directory_file(dir: &Path) -> Option<PathBuf> {
    DIRECTORY_FILE_NAMES
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
}

/// The nearest configuration file in a directory above `dir`.
#[must_use]
pub fn workspace_file(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().skip(1).find_map(directory_file)
}

/// The user's own configuration file (it need not exist).
#[must_use]
pub fn user_file() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("sven").join("config.yaml"))
}

/// The file `scope` names from `dir`: the one that exists, else where a new
/// one goes. `None` for a workspace with no configuration above `dir`, which
/// has no place to be made: a workspace is a directory that already holds one.
#[must_use]
pub fn scope_file(scope: Scope, dir: &Path) -> Option<PathBuf> {
    match scope {
        Scope::Project => Some(directory_file(dir).unwrap_or_else(|| dir.join(NEW_FILE_NAME))),
        Scope::Workspace => workspace_file(dir),
        Scope::User => user_file(),
    }
}

/// A key path written `a.b.c`; a segment holding a dot is quoted,
/// `providers.or.models."z-ai/glm-5.3".max_tokens`.
///
/// # Errors
///
/// An empty path or segment, or an unclosed quote.
pub fn parse_path(text: &str) -> Result<Vec<String>, String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    let mut quoted = false;
    while let Some(c) = chars.next() {
        match c {
            '"' if current.is_empty() && !quoted => quoted = true,
            '"' if quoted => {
                quoted = false;
                if chars.peek().is_some_and(|&next| next != '.') {
                    return Err(format!("{text:?}: a quoted segment ends at a dot"));
                }
            }
            '.' if !quoted => segments.push(std::mem::take(&mut current)),
            c => current.push(c),
        }
    }
    if quoted {
        return Err(format!("{text:?}: a quote is not closed"));
    }
    segments.push(current);
    if segments.iter().any(String::is_empty) {
        return Err(format!("{text:?} is not a key path such as model.name"));
    }
    Ok(segments)
}

/// The value at `path` in `root`, if it sets one.
#[must_use]
pub fn value_at<'a>(root: &'a serde_yaml::Value, path: &[String]) -> Option<&'a serde_yaml::Value> {
    path.iter().try_fold(root, |at, key| at.get(key.as_str()))
}

/// `root` with `value` at `path`, every mapping on the way made as needed.
///
/// # Errors
///
/// A segment on the way is a value that is not a mapping.
pub fn with_value_at(
    mut root: serde_yaml::Value,
    path: &[String],
    value: serde_yaml::Value,
) -> Result<serde_yaml::Value, String> {
    let mut at = &mut root;
    for (depth, key) in path.iter().enumerate() {
        if at.is_null() {
            *at = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        }
        let serde_yaml::Value::Mapping(map) = at else {
            return Err(format!(
                "{} holds a value, not a section",
                path[..depth].join(".")
            ));
        };
        let key = serde_yaml::Value::String(key.clone());
        if depth + 1 == path.len() {
            map.insert(key, value);
            return Ok(root);
        }
        at = map.entry(key).or_insert(serde_yaml::Value::Null);
    }
    Ok(root)
}

/// The document `path` holds; a file that does not exist holds nothing.
///
/// # Errors
///
/// The file cannot be read or is not YAML.
pub fn read_file(path: &Path) -> anyhow::Result<serde_yaml::Value> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => {
            Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
        }
        Ok(text) => {
            serde_yaml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
        }
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Write `document` to `path` whole, replacing the file in one rename so a
/// reader never sees half of it. A file that may hold a key stays readable by
/// its owner only.
///
/// # Errors
///
/// The directory cannot be made or the file written.
pub fn write_file(path: &Path, document: &serde_yaml::Value) -> anyhow::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let text = serde_yaml::to_string(document)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let aside = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options
            .open(&aside)
            .with_context(|| format!("writing {}", aside.display()))?;
        std::io::Write::write_all(&mut file, text.as_bytes())?;
    }
    std::fs::rename(&aside, path).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_path_splits_at_dots_outside_quotes() {
        assert_eq!(parse_path("model.name").unwrap(), ["model", "name"]);
        assert_eq!(
            parse_path(r#"providers.or.models."z-ai/glm-5.3".max_tokens"#).unwrap(),
            ["providers", "or", "models", "z-ai/glm-5.3", "max_tokens"]
        );
        for bad in ["", "model.", ".name", "a..b", r#"a."b"#, r#"a."b"c"#] {
            assert!(parse_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_value_is_set_making_sections_and_keeping_the_rest() {
        let root: serde_yaml::Value = serde_yaml::from_str("model:\n  name: a\n").unwrap();
        let path = parse_path("model.temperature").unwrap();
        let set = with_value_at(root, &path, 0.3.into()).unwrap();
        assert_eq!(value_at(&set, &path), Some(&serde_yaml::Value::from(0.3)));
        assert_eq!(set["model"]["name"], "a");
        let deep = with_value_at(
            serde_yaml::Value::Null,
            &parse_path("a.b.c").unwrap(),
            1.into(),
        )
        .unwrap();
        assert_eq!(deep["a"]["b"]["c"], 1);
        assert!(with_value_at(set, &parse_path("model.name.x").unwrap(), 1.into()).is_err());
    }

    #[test]
    fn the_workspace_is_the_nearest_configuration_above_and_never_the_directory_itself() {
        let root = std::env::temp_dir().join(format!("sven-scope-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let project = root.join("ws/a/proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(root.join("ws/.sven.yaml"), "{}").unwrap();
        std::fs::write(project.join(".sven.yaml"), "{}").unwrap();
        assert_eq!(workspace_file(&project), Some(root.join("ws/.sven.yaml")));
        assert_eq!(directory_file(&project), Some(project.join(".sven.yaml")));
        assert_eq!(scope_file(Scope::Workspace, &root), workspace_file(&root));
        let _ = std::fs::remove_dir_all(&root);
    }
}
