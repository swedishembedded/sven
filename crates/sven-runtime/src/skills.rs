// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Skill discovery and parsing for skill package trees.
//!
//! ## Skill packages
//!
//! A skill is a **directory** that contains a `SKILL.md` file.  Subdirectories
//! of a skill package that also contain `SKILL.md` files are **sub-skills** and
//! are discovered automatically — no frontmatter declaration is needed.
//!
//! Example layout:
//! ```text
//! .sven/skills/
//! ├── sven/
//! │   ├── SKILL.md          → slash command: /sven
//! │   ├── scripts/          → bundled scripts (not a sub-skill — no SKILL.md)
//! │   ├── plan/
//! │   │   └── SKILL.md      → slash command: /sven/plan
//! │   ├── implement/
//! │   │   ├── SKILL.md      → slash command: /sven/implement
//! │   │   └── research/
//! │   │       └── SKILL.md  → slash command: /sven/implement/research
//! │   └── review/
//! │       └── SKILL.md      → slash command: /sven/review
//! └── git-workflow/
//!     └── SKILL.md          → slash command: /git-workflow
//! ```
//!
//! Each skill in the tree has a unique **command** string derived from its path
//! relative to the skills root (e.g. `"sven/plan"`).  Commands are the keys
//! used with the `load_skill` tool.
//!
//! ## Discovery order (later sources take precedence on command collision)
//!
//! Discovery uses a **unified ancestor walk**: two chains are collected —
//! one from the project root (or CWD) up to `/`, one from `~` up to `/` —
//! deduplicated and sorted by depth (shallowest = lowest precedence).  At
//! every directory in the merged chain, four config dirs are checked in order:
//!
//! ```text
//! <dir>/.agents/skills/   (lowest within a level)
//! <dir>/.claude/skills/
//! <dir>/.codex/skills/
//! <dir>/.cursor/skills/
//! <dir>/.sven/skills/     (highest within a level)
//! ```
//!
//! Because the chain runs from `/` down to the project root, entries closer to
//! the project root always override farther ancestors.  In practice:
//!
//! - `~/.cursor/skills/`      — found because home is one of the walk roots
//! - `/workspace/.cursor/skills/` — found when the git root is a subdirectory
//!   of the workspace, even though the workspace has no `.git`
//! - `<project>/.sven/skills/` — highest precedence of all
//!
//! The `SKILL.md` filename is matched case-insensitively, so `skill.md`,
//! `Skill.md`, and `SKILL.md` are all accepted.
//!
//! ## SKILL.md format
//!
//! ```markdown
//! ---
//! description: |
//!   This skill should be used when the user asks to "do X", "configure Y".
//! name: My Skill       # optional — falls back to directory name
//! version: 0.1.0       # optional
//! sven:                # optional sven-specific block
//!   always: false
//!   requires_bins: [ffmpeg]
//!   requires_env: [API_KEY]
//!   user_invocable_only: false
//! ---
//!
//! # Skill body here…
//! ```

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::shared::Shared;

// ── Public types ──────────────────────────────────────────────────────────────

/// Sven-specific metadata block parsed from the `sven:` key in frontmatter.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SvenSkillMeta {
    /// Always include this skill's metadata in the system prompt, regardless
    /// of token budget or availability checks.
    #[serde(default)]
    pub always: bool,

    /// Names of binaries that must exist (`which <bin>`) for this skill to be
    /// included.  If any binary is absent the skill is silently skipped.
    #[serde(default)]
    pub requires_bins: Vec<String>,

    /// Names of environment variables that must be set for this skill to be
    /// included.  If any variable is unset the skill is silently skipped.
    #[serde(default)]
    pub requires_env: Vec<String>,

    /// When `true` the skill is excluded from the model's `<available_skills>`
    /// list but still registered as a TUI slash command.  Useful for skills
    /// that should only be invoked explicitly by the user.
    #[serde(default)]
    pub user_invocable_only: bool,
}

/// A fully parsed and validated skill.
#[derive(Debug, Clone)]
pub struct SkillInfo {
    /// Slash-command key derived from the directory path relative to the skills
    /// root.  Top-level skills use the directory name (e.g. `"sven"`); nested
    /// skills use `/`-separated segments (e.g. `"sven/plan"`).  This is the
    /// value passed to `load_skill(command)`.
    pub command: String,
    /// Human-readable display name.  Comes from the `name:` frontmatter field;
    /// falls back to the last segment of `command` when not set.
    pub name: String,
    /// Description from frontmatter (should contain trigger phrases).
    pub description: String,
    /// Optional semver version string.
    pub version: Option<String>,
    /// Absolute path to the `SKILL.md` file.
    pub skill_md_path: PathBuf,
    /// Absolute path to the skill directory (parent of `SKILL.md`).
    pub skill_dir: PathBuf,
    /// SKILL.md body — everything after the closing `---` fence.
    pub content: String,
    /// Optional sven-specific metadata.
    pub sven_meta: Option<SvenSkillMeta>,
}

/// A shared, live-refreshable collection of discovered skills.
///
/// Both the TUI command registry and the running agent hold a clone of the same
/// `SharedSkills` instance.  Calling [`SharedSkills::refresh`] atomically
/// replaces the inner skill slice so the next agent turn and the next TUI
/// command lookup both see the updated skills without restarting.
pub type SharedSkills = Shared<SkillInfo>;

impl Shared<SkillInfo> {
    /// Re-run skill discovery and atomically replace the skill list.
    ///
    /// Callers (e.g. the `/refresh` slash command) should also rebuild any
    /// derived state such as TUI slash commands after calling this.
    pub fn refresh(&self, project_root: Option<&Path>) {
        self.set(discover_skills(project_root));
    }
}

// ── Internal frontmatter schema ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct RawFrontmatter {
    /// Optional display name; falls back to the directory name when omitted.
    #[serde(default)]
    name: Option<String>,
    description: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    sven: Option<SvenSkillMeta>,
}

// ── Parsing ───────────────────────────────────────────────────────────────────

/// Parsed frontmatter fields plus the SKILL.md body.
pub struct ParsedSkill {
    /// Optional display name from frontmatter.
    pub name: Option<String>,
    pub description: String,
    pub version: Option<String>,
    pub sven_meta: Option<SvenSkillMeta>,
    /// Everything after the closing `---` fence, with leading whitespace trimmed.
    pub body: String,
}

/// Parse a raw SKILL.md string into its frontmatter fields and body.
///
/// The `description` field is required.  The `name` field is optional; callers
/// should fall back to the directory name when it is absent.
///
/// Returns `None` when the frontmatter is missing, malformed, or lacks a
/// non-empty `description`.
#[must_use]
pub fn parse_skill_file(raw: &str) -> Option<ParsedSkill> {
    let rest = raw.trim_start_matches('\n');

    // ── Frontmatter path ─────────────────────────────────────────────────────
    if let Some(after_open) = rest.strip_prefix("---") {
        let close = after_open.find("\n---")?;
        let yaml_block = &after_open[..close];
        // Body starts after "\n---" (4 bytes). Strip one leading newline if present.
        let body = after_open[close + 4..].trim_start_matches('\n').to_string();

        let fm: RawFrontmatter = serde_yaml::from_str(yaml_block).ok()?;

        if fm.description.trim().is_empty() {
            return None;
        }

        return Some(ParsedSkill {
            name: fm.name.filter(|n| !n.trim().is_empty()),
            description: fm.description,
            version: fm.version,
            sven_meta: fm.sven,
            body,
        });
    }

    // ── Frontmatter-free path ─────────────────────────────────────────────────
    // A SKILL.md with no `---` fence is accepted as a plain-markdown skill.
    // The description is synthesised from the first non-empty, non-heading line
    // (or from the text of the first `# Heading` if no plain line comes first).
    // The entire file becomes the body.
    if rest.trim().is_empty() {
        return None;
    }

    let description = rest
        .lines()
        .find_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            // Strip a leading `#` heading marker so "# Sven" → "Sven".
            let text = line.trim_start_matches('#').trim();
            if text.is_empty() {
                None
            } else {
                Some(text.to_string())
            }
        })
        .unwrap_or_else(|| "Skill".to_string());

    Some(ParsedSkill {
        name: None,
        description,
        version: None,
        sven_meta: None,
        body: rest.to_string(),
    })
}

// ── Requirements checking ─────────────────────────────────────────────────────

/// Return `true` when all `requires_bins` entries can be found on `PATH`.
fn bins_available(bins: &[String]) -> bool {
    bins.iter().all(|bin| which_available(bin))
}

fn which_available(name: &str) -> bool {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in path_var.split(':') {
            if PathBuf::from(dir).join(name).exists() {
                return true;
            }
        }
    }
    false
}

/// Return `true` when all `requires_env` entries are set (non-empty).
fn env_vars_set(vars: &[String]) -> bool {
    vars.iter()
        .all(|v| std::env::var(v).map(|s| !s.is_empty()).unwrap_or(false))
}

// ── Directory scanning ────────────────────────────────────────────────────────

pub(crate) const MAX_SKILL_FILE_BYTES: u64 = 256 * 1024; // 256 KB

/// Compute the slash-command key for `skill_dir` relative to the skills `root`.
///
/// Path separators are normalised to `/` and each component is kept as-is
/// (caller is responsible for sanitising component names if needed).
fn command_from_path(root: &Path, skill_dir: &Path) -> String {
    skill_dir
        .strip_prefix(root)
        .unwrap_or(skill_dir)
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Try to load and validate one skill directory.
///
/// Returns `None` and emits a warning when the file is oversized, unreadable,
/// or has invalid frontmatter.
fn try_load_skill(
    skill_dir: &Path,
    skill_md: &Path,
    command: &str,
    source: &str,
) -> Option<SkillInfo> {
    let size = skill_md.metadata().map(|m| m.len()).unwrap_or(0);
    if size > MAX_SKILL_FILE_BYTES {
        warn!(
            source,
            path = %skill_md.display(),
            size,
            max = MAX_SKILL_FILE_BYTES,
            "skipping oversized SKILL.md"
        );
        return None;
    }

    let raw = match std::fs::read_to_string(skill_md) {
        Ok(s) => s,
        Err(e) => {
            warn!(source, path = %skill_md.display(), error = %e, "failed to read SKILL.md");
            return None;
        }
    };

    let parsed = match parse_skill_file(&raw) {
        Some(p) => p,
        None => {
            warn!(source, path = %skill_md.display(), "failed to parse SKILL.md frontmatter — skipping");
            return None;
        }
    };

    // Check runtime requirements before accepting the skill.
    if let Some(ref meta) = parsed.sven_meta {
        if !bins_available(&meta.requires_bins) {
            return None;
        }
        if !env_vars_set(&meta.requires_env) {
            return None;
        }
    }

    // Display name: frontmatter `name:` if provided, otherwise the last
    // path segment (directory name) of `command`.
    let name = parsed
        .name
        .unwrap_or_else(|| command.rsplit('/').next().unwrap_or(command).to_string());

    Some(SkillInfo {
        command: command.to_string(),
        name,
        description: parsed.description,
        version: parsed.version,
        skill_md_path: skill_md.to_path_buf(),
        skill_dir: skill_dir.to_path_buf(),
        content: parsed.body,
        sven_meta: parsed.sven_meta,
    })
}

/// Recursively scan `dir` under `root` for skill packages.
///
/// Any subdirectory (at any depth) that contains a `SKILL.md` file is
/// considered a skill.  Subdirectories without `SKILL.md` are still traversed
/// so that nested sub-skills can be discovered; they are simply not registered
/// as skills themselves.
/// Find the `SKILL.md` file inside `dir`, accepting any capitalisation.
///
/// Checks for the canonical `SKILL.md` first (fast path), then falls back to a
/// case-insensitive scan of the directory entries so that `skill.md`, `Skill.md`,
/// etc. are also accepted.
fn find_skill_md(dir: &Path) -> Option<PathBuf> {
    let canonical = dir.join("SKILL.md");
    if canonical.is_file() {
        return Some(canonical);
    }
    std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let p = e.path();
        if p.is_file()
            && p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.to_lowercase() == "skill.md")
                .unwrap_or(false)
        {
            Some(p)
        } else {
            None
        }
    })
}

fn scan_recursive(root: &Path, dir: &Path, source: &str, out: &mut Vec<SkillInfo>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let child = entry.path();
        if !child.is_dir() {
            continue;
        }

        if let Some(skill_md) = find_skill_md(&child) {
            let command = command_from_path(root, &child);
            if let Some(skill) = try_load_skill(&child, &skill_md, &command, source) {
                out.push(skill);
            }
        }

        // Recurse unconditionally — a skill directory may have sub-skill
        // subdirectories even if no SKILL.md exists at this level.
        scan_recursive(root, &child, source, out);
    }
}

/// Scan one skills root directory and return all valid skills found within it
/// (including those nested at any depth).
fn scan_skills_dir(dir: &Path, source: &str) -> Vec<SkillInfo> {
    let mut skills = Vec::new();
    scan_recursive(dir, dir, source, &mut skills);
    skills
}

// ── Public discovery API ──────────────────────────────────────────────────────

/// Walk up the filesystem from `start` to `/`, collecting every directory.
///
/// Returns the directories in **root-first** order so callers can load them
/// lowest-precedence first.
pub(crate) fn ancestor_chain(start: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut cur = start.to_path_buf();
    loop {
        dirs.push(cur.clone());
        match cur.parent() {
            Some(p) if p != cur => cur = p.to_path_buf(),
            _ => break,
        }
    }
    dirs.reverse(); // root first, start last
    dirs
}

/// Build the merged, depth-sorted list of directories to scan for config files.
///
/// Combines the ancestor chains of `project_root` (or CWD) and `~`, deduplicates,
/// and returns them shallowest-first so that deeper directories (closer to the
/// project root) always win on command name collisions.
pub(crate) fn build_sorted_search_dirs(project_root: Option<&Path>) -> Vec<PathBuf> {
    let home = dirs::home_dir();
    let base = project_root
        .map(|p| p.to_path_buf())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/"));

    let mut all_dirs: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for dir in ancestor_chain(&base) {
        all_dirs.insert(dir);
    }
    if let Some(ref h) = home {
        for dir in ancestor_chain(h) {
            all_dirs.insert(dir);
        }
    }

    let mut sorted: Vec<PathBuf> = all_dirs.into_iter().collect();
    sorted.sort_by(|a, b| {
        let da = a.components().count();
        let db = b.components().count();
        da.cmp(&db).then_with(|| a.cmp(b))
    });
    sorted
}

/// Derive the slash-command key for a `*.md` `path` relative to `root`.
///
/// The extension (`.md` / `.MD`) is stripped; path components are joined with
/// `/`.  Used by both command and agent discovery.
pub(crate) fn md_key_from_path(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let raw: String = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    raw.strip_suffix(".md")
        .or_else(|| raw.strip_suffix(".MD"))
        .map(|s| s.to_string())
        .unwrap_or(raw)
}

/// Recursively enumerate all `*.md` files under `dir`, returning
/// `(key, path)` pairs where `key` is the path relative to `root` with the
/// `.md` extension stripped.
///
/// Files are yielded in deterministic (sorted) order.  Both command discovery
/// and agent discovery use this helper so the scanning logic is implemented
/// exactly once.
pub(crate) fn enumerate_md_files_recursive(root: &Path, dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    enumerate_md_inner(root, dir, &mut out);
    out
}

fn enumerate_md_inner(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.path());

    for entry in entries {
        let path = entry.path();
        if path.is_file() {
            let is_md = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("md"))
                .unwrap_or(false);
            if is_md {
                out.push((md_key_from_path(root, &path), path));
            }
        } else if path.is_dir() {
            enumerate_md_inner(root, &path, out);
        }
    }
}

/// Discover all skills from the standard search hierarchy.
///
/// The search is a **unified ancestor walk**: every directory in the path from
/// `/` down to the project root (and from `/` down to `~`) is checked for
/// `.agents/skills/`, `.claude/skills/`, `.codex/skills/`, `.cursor/skills/`,
/// and `.sven/skills/` (in that order within each directory, so `.sven/` beats
/// `.cursor/` at the same level).  Because directories are loaded farthest-first, entries closer
/// to the project root override entries from parent directories.
///
/// This means:
/// - `/home/user/.cursor/skills/` is found because home is one of the walk roots.
/// - `/data/.cursor/skills/` is found when the project root is a subdirectory
///   of `/data/` (e.g. `/data/repo/`), even though `/data/` has no `.git`.
/// - Skills at the project root itself always have the highest precedence.
///
/// When `project_root` is `None`, the current working directory is used as the
/// walk base so workspace-level skills are still found.
#[must_use]
pub fn discover_skills(project_root: Option<&Path>) -> Vec<SkillInfo> {
    // Keyed by command; later insertions (higher-precedence sources) win.
    let mut map: HashMap<String, SkillInfo> = HashMap::new();

    let mut load = |dir: PathBuf, source: &str| {
        for skill in scan_skills_dir(&dir, source) {
            map.insert(skill.command.clone(), skill);
        }
    };

    // At each directory, load all five config dir types.  The within-level
    // order (.agents < .claude < .codex < .cursor < .sven) means .sven/ wins
    // on collision at the same directory depth.  .codex/ is included for
    // compatibility with Codex-based tooling (mirrors Cursor's compat list).
    for dir in &build_sorted_search_dirs(project_root) {
        let label = dir.to_string_lossy();
        load(
            dir.join(".agents").join("skills"),
            &format!("{label}/.agents"),
        );
        load(
            dir.join(".claude").join("skills"),
            &format!("{label}/.claude"),
        );
        load(
            dir.join(".codex").join("skills"),
            &format!("{label}/.codex"),
        );
        load(
            dir.join(".cursor").join("skills"),
            &format!("{label}/.cursor"),
        );
        load(dir.join(".sven").join("skills"), &format!("{label}/.sven"));
    }

    let mut result: Vec<SkillInfo> = map.into_values().collect();
    result.sort_by(|a, b| a.command.cmp(&b.command));
    result
}

// ── Command discovery (`.cursor/commands/` etc.) ──────────────────────────────

/// Try to load one command markdown file.
///
/// Returns `None` when the file is oversized, unreadable, or produces no
/// usable content.
fn try_load_command(md_path: &Path, command: &str, source: &str) -> Option<SkillInfo> {
    let size = md_path.metadata().map(|m| m.len()).unwrap_or(0);
    if size > MAX_SKILL_FILE_BYTES {
        warn!(
            source,
            path = %md_path.display(),
            size,
            max = MAX_SKILL_FILE_BYTES,
            "skipping oversized command file"
        );
        return None;
    }

    let raw = match std::fs::read_to_string(md_path) {
        Ok(s) => s,
        Err(e) => {
            warn!(source, path = %md_path.display(), error = %e, "failed to read command file");
            return None;
        }
    };

    let parsed = match parse_skill_file(&raw) {
        Some(p) => p,
        None => {
            warn!(source, path = %md_path.display(), "could not extract content from command file — skipping");
            return None;
        }
    };

    let name = parsed
        .name
        .unwrap_or_else(|| command.rsplit('/').next().unwrap_or(command).to_string());

    Some(SkillInfo {
        command: command.to_string(),
        name,
        description: parsed.description,
        version: parsed.version,
        skill_md_path: md_path.to_path_buf(),
        skill_dir: md_path.parent().unwrap_or(md_path).to_path_buf(),
        content: parsed.body,
        sven_meta: parsed.sven_meta,
    })
}

/// Scan one commands root directory and return all valid commands found within
/// it (including those nested at any depth).
fn scan_commands_dir(dir: &Path, source: &str) -> Vec<SkillInfo> {
    enumerate_md_files_recursive(dir, dir)
        .into_iter()
        .filter_map(|(key, path)| try_load_command(&path, &key, source))
        .collect()
}

/// Discover user-invocable commands from the standard `commands/` directories.
///
/// Uses the same ancestor-walk strategy as [`discover_skills`] but scans
/// `commands/` subdirectories instead of `skills/`.  Each `.md` file found
/// inside a `commands/` directory becomes one slash command whose name is the
/// file path relative to the commands root with the `.md` extension removed.
/// Hyphens in filenames are preserved (`review-code.md` → `/review-code`),
/// matching the Cursor commands convention.
///
/// Scanned config directories (lowest to highest precedence within a level):
///
/// ```text
/// <dir>/.agents/commands/
/// <dir>/.claude/commands/
/// <dir>/.codex/commands/
/// <dir>/.cursor/commands/   ← primary Cursor location
/// <dir>/.sven/commands/     ← highest precedence
/// ```
///
/// Example layout:
/// ```text
/// .cursor/commands/
/// ├── implement.md           → slash command `/implement`
/// ├── review-code.md         → slash command `/review-code`
/// └── sven/
///     ├── plan.md            → slash command `/sven/plan`
///     └── task-makefile.md   → slash command `/sven/task-makefile`
/// ```
///
/// Discovery priority mirrors skill discovery: entries closer to the project
/// root override those from parent directories.
#[must_use]
pub fn discover_commands(project_root: Option<&Path>) -> Vec<SkillInfo> {
    let mut map: HashMap<String, SkillInfo> = HashMap::new();

    let mut load = |dir: PathBuf, source: &str| {
        for cmd in scan_commands_dir(&dir, source) {
            map.insert(cmd.command.clone(), cmd);
        }
    };

    for dir in &build_sorted_search_dirs(project_root) {
        let label = dir.to_string_lossy();
        load(
            dir.join(".agents").join("commands"),
            &format!("{label}/.agents"),
        );
        load(
            dir.join(".claude").join("commands"),
            &format!("{label}/.claude"),
        );
        load(
            dir.join(".codex").join("commands"),
            &format!("{label}/.codex"),
        );
        load(
            dir.join(".cursor").join("commands"),
            &format!("{label}/.cursor"),
        );
        load(
            dir.join(".sven").join("commands"),
            &format!("{label}/.sven"),
        );
    }

    let mut result: Vec<SkillInfo> = map.into_values().collect();
    result.sort_by(|a, b| a.command.cmp(&b.command));
    result
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Write a SKILL.md into `dir/<command>/SKILL.md`, creating all parent
    /// directories.  `command` may be slash-separated (e.g. `"sven/plan"`).
    fn write_skill(
        dir: &Path,
        command: &str,
        description: &str,
        extra_frontmatter: &str,
        body: &str,
    ) {
        let skill_dir = command
            .split('/')
            .fold(dir.to_path_buf(), |acc, seg| acc.join(seg));
        fs::create_dir_all(&skill_dir).unwrap();
        let frontmatter =
            format!("---\ndescription: |\n  {description}\n{extra_frontmatter}---\n\n{body}");
        fs::write(skill_dir.join("SKILL.md"), frontmatter).unwrap();
    }

    // ── parse_skill_file ──────────────────────────────────────────────────────

    #[test]
    fn parse_skill_file_valid() {
        let raw = "---\ndescription: A test skill.\n---\n\nBody here.";
        let parsed = parse_skill_file(raw).expect("should parse");
        assert!(parsed.name.is_none());
        assert_eq!(parsed.description.trim(), "A test skill.");
        assert_eq!(parsed.body, "Body here.");
        assert!(parsed.version.is_none());
        assert!(parsed.sven_meta.is_none());
    }

    #[test]
    fn parse_skill_file_with_explicit_name() {
        let raw = "---\nname: My Skill\ndescription: A test skill.\n---\n\nBody here.";
        let parsed = parse_skill_file(raw).expect("should parse");
        assert_eq!(parsed.name.as_deref(), Some("My Skill"));
    }

    #[test]
    fn parse_skill_file_body_preserved_with_dashes() {
        // A body that itself contains a horizontal-rule `---` must not be truncated.
        let raw = "---\ndescription: Desc.\n---\n\nParagraph one.\n\n---\n\nParagraph two.";
        let parsed = parse_skill_file(raw).expect("should parse");
        assert!(
            parsed.body.contains("Paragraph one."),
            "body: {:?}",
            parsed.body
        );
        assert!(
            parsed.body.contains("Paragraph two."),
            "body: {:?}",
            parsed.body
        );
    }

    #[test]
    fn parse_skill_file_with_version_and_sven_block() {
        let raw = "---\ndescription: Git helper.\nversion: 1.2.3\nsven:\n  always: true\n  requires_bins:\n    - git\n  user_invocable_only: false\n---\n\nBody.";
        let parsed = parse_skill_file(raw).expect("should parse");
        assert_eq!(parsed.version.as_deref(), Some("1.2.3"));
        let meta = parsed.sven_meta.unwrap();
        assert!(meta.always);
        assert_eq!(meta.requires_bins, vec!["git"]);
        assert!(!meta.user_invocable_only);
    }

    #[test]
    fn parse_skill_file_missing_description_returns_none() {
        let raw = "---\nname: Something\n---\n\nBody.";
        assert!(parse_skill_file(raw).is_none());
    }

    #[test]
    fn parse_skill_file_no_frontmatter_uses_first_line_as_description() {
        let raw = "# Just a heading\n\nNo frontmatter here.";
        let parsed = parse_skill_file(raw).expect("plain-markdown skill should parse");
        // Heading marker stripped, first non-empty line becomes description.
        assert_eq!(parsed.description, "Just a heading");
        assert!(parsed.body.contains("Just a heading"));
    }

    #[test]
    fn parse_skill_file_no_frontmatter_plain_line_description() {
        let raw = "You are an embedded engineer.";
        let parsed = parse_skill_file(raw).expect("plain body should parse");
        assert_eq!(parsed.description, "You are an embedded engineer.");
    }

    #[test]
    fn parse_skill_file_no_frontmatter_empty_returns_none() {
        assert!(parse_skill_file("").is_none());
        assert!(parse_skill_file("   \n  \n").is_none());
    }

    #[test]
    fn parse_skill_file_empty_description_returns_none() {
        let raw = "---\ndescription: \"\"\n---\n\nBody.";
        assert!(parse_skill_file(raw).is_none());
    }

    // ── command_from_path ─────────────────────────────────────────────────────

    #[test]
    fn command_from_path_top_level() {
        let root = Path::new("/skills");
        let skill = Path::new("/skills/sven");
        assert_eq!(command_from_path(root, skill), "sven");
    }

    #[test]
    fn command_from_path_nested() {
        let root = Path::new("/skills");
        let skill = Path::new("/skills/sven/plan");
        assert_eq!(command_from_path(root, skill), "sven/plan");
    }

    #[test]
    fn command_from_path_deep() {
        let root = Path::new("/skills");
        let skill = Path::new("/skills/sven/implement/research");
        assert_eq!(command_from_path(root, skill), "sven/implement/research");
    }

    // ── discover_skills ───────────────────────────────────────────────────────

    #[test]
    fn discover_skills_empty_dir_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let result = discover_skills(Some(tmp.path()));
        assert!(result.is_empty());
    }

    #[test]
    fn discover_skills_single_skill() {
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join(".sven").join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        write_skill(&skills_dir, "git-workflow", "Git helper.", "", "## Section");

        let skills = discover_skills(Some(tmp.path()));
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].command, "git-workflow");
        assert_eq!(skills[0].name, "git-workflow"); // falls back to dir name
        assert!(skills[0].description.contains("Git helper."));
        assert!(skills[0].content.contains("## Section"));
        assert!(skills[0].skill_dir.ends_with("git-workflow"));
    }

    #[test]
    fn discover_skills_name_from_frontmatter() {
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join(".sven").join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        let skill_dir = skills_dir.join("git-workflow");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: Git Workflow\ndescription: Git helper.\n---\n\nbody",
        )
        .unwrap();

        let skills = discover_skills(Some(tmp.path()));
        assert_eq!(skills[0].command, "git-workflow");
        assert_eq!(skills[0].name, "Git Workflow"); // from frontmatter
    }

    #[test]
    fn discover_skills_multiple_sorted_by_command() {
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join(".sven").join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        write_skill(&skills_dir, "zebra", "Z skill.", "", "");
        write_skill(&skills_dir, "apple", "A skill.", "", "");
        write_skill(&skills_dir, "mango", "M skill.", "", "");

        let skills = discover_skills(Some(tmp.path()));
        assert_eq!(skills.len(), 3);
        assert_eq!(skills[0].command, "apple");
        assert_eq!(skills[1].command, "mango");
        assert_eq!(skills[2].command, "zebra");
    }

    #[test]
    fn discover_skills_subskills_via_directory_structure() {
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join(".sven").join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        write_skill(
            &skills_dir,
            "sven",
            "Top-level sven.",
            "",
            "Orchestrator body.",
        );
        write_skill(
            &skills_dir,
            "sven/plan",
            "Planning phase.",
            "",
            "Plan body.",
        );
        write_skill(
            &skills_dir,
            "sven/implement",
            "Implementation phase.",
            "",
            "Impl body.",
        );

        let skills = discover_skills(Some(tmp.path()));
        assert_eq!(skills.len(), 3);

        let cmds: Vec<&str> = skills.iter().map(|s| s.command.as_str()).collect();
        assert!(cmds.contains(&"sven"), "top-level skill");
        assert!(cmds.contains(&"sven/plan"), "sub-skill");
        assert!(cmds.contains(&"sven/implement"), "sub-skill");
    }

    #[test]
    fn discover_skills_deeply_nested_subskills() {
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join(".sven").join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        write_skill(
            &skills_dir,
            "sven/implement/research",
            "Research sub-skill.",
            "",
            "Research body.",
        );

        let skills = discover_skills(Some(tmp.path()));
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].command, "sven/implement/research");
    }

    #[test]
    fn discover_skills_non_skill_subdirs_ignored() {
        // A subdirectory without SKILL.md (scripts/, docs/) must not be treated
        // as a sub-skill, but nested sub-skills underneath it must still be found.
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join(".sven").join("skills");
        let sven_dir = skills_dir.join("sven");
        fs::create_dir_all(&sven_dir).unwrap();
        fs::write(
            sven_dir.join("SKILL.md"),
            "---\ndescription: Sven.\n---\n\nbody",
        )
        .unwrap();

        // scripts/ has no SKILL.md → not a sub-skill
        let scripts_dir = sven_dir.join("scripts");
        fs::create_dir_all(&scripts_dir).unwrap();
        fs::write(scripts_dir.join("helper.sh"), "#!/bin/sh\necho hi").unwrap();

        let skills = discover_skills(Some(tmp.path()));
        let cmds: Vec<&str> = skills.iter().map(|s| s.command.as_str()).collect();
        assert!(cmds.contains(&"sven"), "parent skill registered");
        assert!(!cmds.contains(&"sven/scripts"), "scripts/ not a sub-skill");
        assert_eq!(skills.len(), 1);
    }

    #[test]
    fn discover_skills_project_sven_overrides_same_command() {
        let tmp = TempDir::new().unwrap();
        let agents_dir = tmp.path().join(".agents").join("skills");
        fs::create_dir_all(&agents_dir).unwrap();
        write_skill(&agents_dir, "deploy", "Agents version.", "", "Agents body.");

        let sven_dir = tmp.path().join(".sven").join("skills");
        fs::create_dir_all(&sven_dir).unwrap();
        write_skill(&sven_dir, "deploy", "Sven version.", "", "Sven body.");

        let skills = discover_skills(Some(tmp.path()));
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description.trim(), "Sven version.");
    }

    #[test]
    fn discover_skills_size_cap_skips_oversized() {
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join(".sven").join("skills");
        let skill_dir = skills_dir.join("big-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        let big_content = format!(
            "---\ndescription: Too big.\n---\n\n{}",
            "x".repeat(260 * 1024)
        );
        fs::write(skill_dir.join("SKILL.md"), big_content).unwrap();

        let skills = discover_skills(Some(tmp.path()));
        assert!(skills.is_empty(), "oversized skill should be skipped");
    }

    #[test]
    fn discover_skills_requires_bins_missing_skips_skill() {
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join(".sven").join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        write_skill(
            &skills_dir,
            "needs-nonexistent",
            "Requires missing bin.",
            "sven:\n  requires_bins:\n    - __sven_test_nonexistent_binary__\n",
            "Body.",
        );

        let skills = discover_skills(Some(tmp.path()));
        assert!(
            skills.is_empty(),
            "skill with missing binary should be skipped"
        );
    }

    #[test]
    fn discover_skills_requires_bins_present_includes_skill() {
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join(".sven").join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        write_skill(
            &skills_dir,
            "needs-sh",
            "Requires sh.",
            "sven:\n  requires_bins:\n    - sh\n",
            "Body.",
        );

        let skills = discover_skills(Some(tmp.path()));
        assert_eq!(skills.len(), 1);
    }

    #[test]
    fn discover_skills_dir_without_skill_md_skipped() {
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join(".sven").join("skills");
        let no_skill = skills_dir.join("not-a-skill");
        fs::create_dir_all(&no_skill).unwrap();
        fs::write(no_skill.join("README.md"), "not a skill").unwrap();

        let skills = discover_skills(Some(tmp.path()));
        assert!(skills.is_empty());
    }

    #[test]
    fn skill_info_content_strips_frontmatter() {
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join(".sven").join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        write_skill(
            &skills_dir,
            "example",
            "Example skill.",
            "",
            "## Usage\n\nDo things.",
        );

        let skills = discover_skills(Some(tmp.path()));
        let content = &skills[0].content;
        assert!(
            !content.contains("description:"),
            "content should not include frontmatter"
        );
        assert!(content.contains("## Usage"));
    }
}
