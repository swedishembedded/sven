// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The hint a search tool appends to an empty result.
//!
//! "(no matches)" alone leaves a model with nothing to try next: it repeats
//! the call, or guesses a file name from another language's convention
//! (`*_test.py` in a Lua tree). Naming what is actually under the directory
//! searched turns the dead end into a map. The hint is bounded, so a huge
//! tree costs a fixed number of tokens.

use std::collections::VecDeque;
use std::fs;
use std::path::Path;

/// The most names listed in each of the two parts of the hint.
pub const MAX_HINT_NAMES: usize = 40;

/// How many directory levels below the searched directory the file list
/// reaches; enough to see a package directory and its modules.
const MAX_DEPTH: usize = 3;

/// Directories never listed: version control, build output and caches.
const SKIPPED_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".cargo",
    "__pycache__",
    ".venv",
];

/// What to append to an empty search result for the directory `root`: its
/// top-level entries and the files near it, at most [`MAX_HINT_NAMES`] of
/// each, as a short bracketed note. Empty when `root` is not a readable
/// directory (a search of one file has no neighbourhood to describe).
#[must_use]
pub fn no_match_hint(root: &Path) -> String {
    let Some(top) = top_level(root) else {
        return String::new();
    };
    let files = files_near(root);
    let mut out = format!(
        "\n[nothing matched; the search root holds: {}]",
        listed(&top.names, top.total)
    );
    if !files.names.is_empty() {
        out.push_str(&format!(
            "\n[files near it: {}]",
            listed(&files.names, files.total)
        ));
    }
    out
}

struct Names {
    names: Vec<String>,
    /// How many there were before the list was cut to [`MAX_HINT_NAMES`].
    total: usize,
}

fn listed(names: &[String], total: usize) -> String {
    let mut text = names.join(", ");
    if total > names.len() {
        text.push_str(&format!(", and {} more", total - names.len()));
    }
    text
}

/// Sorted entries of `root` that are not hidden or skipped; directories end
/// in `/`.
fn top_level(root: &Path) -> Option<Names> {
    let mut entries: Vec<String> = fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || SKIPPED_DIRS.contains(&name.as_str()) {
                return None;
            }
            let is_dir = entry.file_type().ok()?.is_dir();
            Some(if is_dir { format!("{name}/") } else { name })
        })
        .collect();
    entries.sort();
    Some(cut(entries))
}

/// The files within [`MAX_DEPTH`] levels of `root`, nearest first (all of one
/// level in name order before the next), as paths relative to it.
fn files_near(root: &Path) -> Names {
    let mut files = Vec::new();
    let mut level: VecDeque<(String, usize)> = VecDeque::from([(String::new(), 0)]);
    while let Some((dir, depth)) = level.pop_front() {
        let Ok(read) = fs::read_dir(root.join(&dir)) else {
            continue;
        };
        let mut children: Vec<(String, bool)> = read
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') || SKIPPED_DIRS.contains(&name.as_str()) {
                    return None;
                }
                Some((name, entry.file_type().ok()?.is_dir()))
            })
            .collect();
        children.sort();
        for (name, is_dir) in children {
            let path = if dir.is_empty() {
                name
            } else {
                format!("{dir}/{name}")
            };
            if !is_dir {
                files.push(path);
            } else if depth + 1 < MAX_DEPTH {
                level.push_back((path, depth + 1));
            }
        }
    }
    // Nearest first: a breadth-first walk already orders by depth within
    // each directory, but not across siblings, so order the whole list.
    files.sort_by_key(|path| (path.matches('/').count(), path.clone()));
    cut(files)
}

fn cut(mut names: Vec<String>) -> Names {
    let total = names.len();
    names.truncate(MAX_HINT_NAMES);
    Names { names, total }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for file in files {
            let path = dir.path().join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "x").unwrap();
        }
        dir
    }

    #[test]
    fn an_empty_result_names_the_top_level_and_the_source_files_nearby() {
        let dir = tree(&[
            "src/breaker.lua",
            "src/util.lua",
            "tests/test_breaker.lua",
            "README.md",
            ".git/config",
            "target/debug/x",
        ]);
        let hint = no_match_hint(dir.path());
        assert!(hint.contains("README.md, src/, tests/"), "{hint}");
        assert!(
            hint.contains("README.md, src/breaker.lua, src/util.lua, tests/test_breaker.lua"),
            "files nearest first: {hint}"
        );
        assert!(!hint.contains(".git") && !hint.contains("debug"), "{hint}");
    }

    #[test]
    fn the_hint_is_bounded_whatever_the_tree() {
        let names: Vec<String> = (0..MAX_HINT_NAMES * 3)
            .map(|i| format!("pkg/m{i:03}.py"))
            .collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let dir = tree(&refs);
        let hint = no_match_hint(dir.path());
        assert!(
            hint.contains(&format!("and {} more", MAX_HINT_NAMES * 2)),
            "{hint}"
        );
        assert!(hint.matches(".py").count() <= MAX_HINT_NAMES, "{hint}");
    }

    #[test]
    fn a_file_or_missing_path_has_no_neighbourhood_to_describe() {
        let dir = tree(&["a.txt"]);
        assert_eq!(no_match_hint(&dir.path().join("a.txt")), "");
        assert_eq!(no_match_hint(&dir.path().join("absent")), "");
    }
}
