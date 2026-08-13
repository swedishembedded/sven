//! Workspace tooling for sven.
//!
//! `cargo run -p xtask -- arch [--bless] [--profile <name>]` enforces the
//! crate-tier architecture ratchet described in `architecture.toml` at the
//! workspace root. See
//! `.claude/skills/programming/rust/architecture.md` in the outer monorepo
//! for the method this implements.
//!
//! Design notes (see architecture.toml's header for the user-facing rules):
//!   - Only *normal* (non-dev, non-build) internal dependencies are checked
//!     against the tier matrix and the dead-dependency scan. Dev-dependencies
//!     are exempt by design: they legitimately reach upward for end-to-end
//!     tests.
//!   - The dead-dependency scan is a comment-aware text scan of a crate's
//!     `src/` tree, not a real parse. It strips `//`/`///`/`//!` line-comment
//!     text (a simple heuristic, not fully string-literal-aware) before
//!     looking for the dependency's snake_case identifier as a whole word.
//!     This is deliberately stricter than `cargo-machete`, which would treat
//!     an intra-doc link like `` /// [`sven_executors::Foo`] `` as a real use.
//!   - `--bless` regenerates only the `[[allow.large_file]]` section from the
//!     live tree. It does not touch `[[allow.upward]]` / `[[allow.dead_dep]]`
//!     / `[[same_layer]]`, which carry human-authored `why` text that must be
//!     reviewed, not machine-generated. Note that blessing re-serializes the
//!     whole TOML file and will drop any `#`-comments outside those `why`
//!     fields.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("arch") => {
            let bless = args.iter().any(|a| a == "--bless");
            let profile = args
                .iter()
                .position(|a| a == "--profile")
                .and_then(|i| args.get(i + 1))
                .cloned();
            let workspace_root = workspace_root()?;
            if bless {
                bless_large_files(&workspace_root)?;
                println!("architecture.toml: [[allow.large_file]] regenerated from the live tree.");
                return Ok(());
            }
            let violations = run_arch(&workspace_root, profile.as_deref())?;
            if violations.is_empty() {
                println!("architecture: OK (all internal edges, dependencies, and file sizes within the ratchet).");
                Ok(())
            } else {
                for v in &violations {
                    eprintln!("{v}\n");
                }
                bail!("{} architecture violation(s)", violations.len());
            }
        }
        _ => {
            eprintln!("usage: cargo run -p xtask -- arch [--bless] [--profile <name>]");
            std::process::exit(2);
        }
    }
}

fn workspace_root() -> Result<PathBuf> {
    // xtask/Cargo.toml lives directly under the workspace root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .context("xtask has no parent directory")
}

// ─────────────────────────────────────────────────────────────────────────
// architecture.toml model
// ─────────────────────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize)]
struct ArchConfig {
    tiers: Tiers,
    crates: BTreeMap<String, String>,
    #[serde(default)]
    same_layer: Vec<Pair>,
    #[serde(default)]
    allow: AllowSections,
}

#[derive(Deserialize, Serialize)]
struct Tiers {
    order: Vec<String>,
}

#[derive(Deserialize, Serialize, Clone)]
struct Pair {
    from: String,
    to: String,
    #[serde(default)]
    why: String,
}

#[derive(Deserialize, Serialize, Clone)]
struct DeadDep {
    #[serde(rename = "crate")]
    crate_: String,
    dep: String,
    #[serde(default)]
    why: String,
}

#[derive(Deserialize, Serialize, Clone)]
struct LargeFile {
    path: String,
    lines: usize,
}

#[derive(Deserialize, Serialize, Default)]
struct AllowSections {
    #[serde(default)]
    upward: Vec<Pair>,
    #[serde(default)]
    dead_dep: Vec<DeadDep>,
    #[serde(default)]
    large_file: Vec<LargeFile>,
}

fn load_config(workspace_root: &Path) -> Result<ArchConfig> {
    let path = workspace_root.join("architecture.toml");
    let text = fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

// ─────────────────────────────────────────────────────────────────────────
// cargo metadata
// ─────────────────────────────────────────────────────────────────────────

struct Package {
    name: String,
    manifest_dir: PathBuf,
    /// (dep name, is_normal_dependency)
    deps: Vec<(String, bool)>,
}

fn cargo_metadata(workspace_root: &Path) -> Result<Vec<Package>> {
    let out = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(workspace_root)
        .output()
        .context("running `cargo metadata`")?;
    if !out.status.success() {
        bail!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).context("parsing cargo metadata JSON")?;
    let mut packages = Vec::new();
    for p in v["packages"].as_array().context("packages[] missing")? {
        let name = p["name"].as_str().context("package.name missing")?.to_string();
        let manifest_path = p["manifest_path"]
            .as_str()
            .context("package.manifest_path missing")?;
        let manifest_dir = PathBuf::from(manifest_path)
            .parent()
            .context("manifest_path has no parent")?
            .to_path_buf();
        let mut deps = Vec::new();
        for d in p["dependencies"].as_array().context("dependencies[] missing")? {
            let dep_name = d["name"].as_str().context("dependency.name missing")?.to_string();
            // kind is null for a normal dependency, "dev" or "build" otherwise.
            let is_normal = d["kind"].is_null();
            deps.push((dep_name, is_normal));
        }
        packages.push(Package {
            name,
            manifest_dir,
            deps,
        });
    }
    Ok(packages)
}

// ─────────────────────────────────────────────────────────────────────────
// checks
// ─────────────────────────────────────────────────────────────────────────

fn run_arch(workspace_root: &Path, profile: Option<&str>) -> Result<Vec<String>> {
    let cfg = load_config(workspace_root)?;
    let packages = cargo_metadata(workspace_root)?;

    let tier_index: BTreeMap<&str, usize> = cfg
        .tiers
        .order
        .iter()
        .enumerate()
        .map(|(i, t)| (t.as_str(), i))
        .collect();

    let mut violations = Vec::new();

    // Check: every workspace member has a declared tier.
    for p in &packages {
        if !cfg.crates.contains_key(&p.name) {
            violations.push(format!(
                "error[ARCH-001]: undeclared crate\n  {} has no entry in architecture.toml [crates]\n  = help: add `{} = \"<tier>\"`",
                p.name, p.name
            ));
        }
    }

    let same_layer: BTreeSet<(String, String)> = cfg
        .same_layer
        .iter()
        .map(|p| (p.from.clone(), p.to.clone()))
        .collect();
    let allow_upward: BTreeSet<(String, String)> = cfg
        .allow
        .upward
        .iter()
        .map(|p| (p.from.clone(), p.to.clone()))
        .collect();
    let allow_dead: BTreeSet<(String, String)> = cfg
        .allow
        .dead_dep
        .iter()
        .map(|d| (d.crate_.clone(), d.dep.clone()))
        .collect();

    let mut seen_upward = BTreeSet::new();
    let mut seen_dead = BTreeSet::new();
    let mut seen_same_layer = BTreeSet::new();

    for p in &packages {
        let Some(&from_tier) = cfg.crates.get(&p.name).and_then(|t| tier_index.get(t.as_str()))
        else {
            continue; // already reported as ARCH-001
        };

        for (dep_name, is_normal) in &p.deps {
            if !is_normal {
                continue; // dev/build deps are exempt from the tier + dead-dep checks
            }
            let Some(dep_tier_name) = cfg.crates.get(dep_name) else {
                continue; // not a workspace crate we track (external dep)
            };
            let Some(&to_tier) = tier_index.get(dep_tier_name.as_str()) else {
                continue;
            };

            // Tier legality.
            if to_tier > from_tier {
                let key = (p.name.clone(), dep_name.clone());
                seen_upward.insert(key.clone());
                if !allow_upward.contains(&key) {
                    violations.push(format!(
                        "error[ARCH-002]: illegal tier edge\n  {} ({}) depends on {} ({})\n  = help: this is an upward edge with no [[allow.upward]] entry -- either the edge is a bug, or it needs a reviewed exception in architecture.toml",
                        p.name, cfg.crates[&p.name], dep_name, dep_tier_name
                    ));
                }
            } else if to_tier == from_tier && p.name != *dep_name {
                let key = (p.name.clone(), dep_name.clone());
                seen_same_layer.insert(key.clone());
                if !same_layer.contains(&key) {
                    violations.push(format!(
                        "error[ARCH-003]: undeclared same-tier edge\n  {} and {} are both tier \"{}\"\n  = help: add a [[same_layer]] entry with a `why`, or move one crate to a different tier",
                        p.name, dep_name, dep_tier_name
                    ));
                }
            }

            // Dead-dependency scan.
            let dep_ident = dep_name.replace('-', "_");
            let src_dir = p.manifest_dir.join("src");
            let used = src_dir.is_dir() && dir_uses_ident(&src_dir, &dep_ident)?;
            if !used {
                let key = (p.name.clone(), dep_name.clone());
                seen_dead.insert(key.clone());
                if !allow_dead.contains(&key) {
                    violations.push(format!(
                        "error[ARCH-004]: unused internal dependency\n  {} declares {} as a normal dependency\n  = note: no real `{}::` reference found under {}\n  = help: remove the dependency, or move it to [dev-dependencies] if only tests use it",
                        p.name, dep_name, dep_ident, src_dir.display()
                    ));
                }
            }
        }
    }

    // Stale allow entries: named but no longer a real violation.
    for key in &allow_upward {
        if !seen_upward.contains(key) {
            violations.push(format!(
                "error[ARCH-005]: stale [[allow.upward]] entry\n  {} -> {} is listed as an allowed upward edge but is no longer an upward edge (or no longer exists)\n  = help: delete this entry from architecture.toml",
                key.0, key.1
            ));
        }
    }
    for key in &same_layer {
        if !seen_same_layer.contains(key) {
            violations.push(format!(
                "error[ARCH-009]: stale [[same_layer]] entry\n  {} -> {} is listed as a legitimate same-tier edge but is no longer a same-tier edge (or no longer exists)\n  = help: delete this entry from architecture.toml, or if the crate moved tier, update the entry",
                key.0, key.1
            ));
        }
    }
    for key in &allow_dead {
        if !seen_dead.contains(key) {
            violations.push(format!(
                "error[ARCH-006]: stale [[allow.dead_dep]] entry\n  {} -> {} is listed as a dead dependency but is now used (or no longer declared)\n  = help: delete this entry from architecture.toml",
                key.0, key.1
            ));
        }
    }

    // File-size ratchet.
    violations.extend(check_large_files(workspace_root, &cfg.allow.large_file)?);

    if let Some(profile) = profile {
        violations.extend(check_profile(workspace_root, profile)?);
    }

    Ok(violations)
}

/// Scans `dir` recursively for `.rs` files and checks whether any, after
/// stripping `//`/`///`/`//!` line-comment text, contains `ident` as a whole
/// word (i.e. `ident::...` or a bare `ident` path segment, not a substring of
/// a longer identifier and not inside a stripped comment).
fn dir_uses_ident(dir: &Path, ident: &str) -> Result<bool> {
    for path in walk_rs_files(dir)? {
        let text = fs::read_to_string(&path).unwrap_or_default();
        let stripped = strip_line_comments(&text);
        if contains_ident(&stripped, ident) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn walk_rs_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk_rs_files_into(dir, &mut out)?;
    Ok(out)
}

fn walk_rs_files_into(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()), // directory doesn't exist; nothing to scan
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if path.is_dir() {
            if file_name == "target" || file_name == ".git" {
                continue;
            }
            walk_rs_files_into(&path, out)?;
        } else if file_name.ends_with(".rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// Heuristic comment stripper: drops `//...` to end of line unless the `//`
/// appears inside a `"..."` string literal on that line. Not aware of raw
/// strings, char literals, or escaped quotes -- good enough to distinguish a
/// real `use` site from a doc-comment-only mention, not a full Rust parser.
fn strip_line_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let bytes = line.as_bytes();
        let mut in_str = false;
        let mut cut = line.len();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'"' => in_str = !in_str,
                b'/' if !in_str && i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                    cut = i;
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        out.push_str(&line[..cut]);
        out.push('\n');
    }
    out
}

fn contains_ident(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let bytes = hay.as_bytes();
    let mut start = 0;
    while let Some(pos) = hay[start..].find(needle) {
        let idx = start + pos;
        let before_ok = idx == 0 || !is_ident_byte(bytes[idx - 1]);
        let after_idx = idx + needle.len();
        let after_ok = after_idx >= bytes.len() || !is_ident_byte(bytes[after_idx]);
        if before_ok && after_ok {
            return true;
        }
        start = idx + 1;
    }
    false
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn check_large_files(workspace_root: &Path, allow: &[LargeFile]) -> Result<Vec<String>> {
    let mut violations = Vec::new();
    let allow_map: BTreeMap<&str, usize> =
        allow.iter().map(|f| (f.path.as_str(), f.lines)).collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    for path in walk_rs_files(workspace_root)? {
        let rel = path
            .strip_prefix(workspace_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let lines = count_lines(&path)?;
        if lines <= 800 {
            continue;
        }
        seen.insert(rel.clone());
        match allow_map.get(rel.as_str()) {
            None => violations.push(format!(
                "error[ARCH-007]: file size\n  {rel}  {lines} lines > 800\n  = help: not in [[allow.large_file]] -- split the file, or add an entry with the current line count"
            )),
            Some(&blessed) => {
                if lines > blessed {
                    violations.push(format!(
                        "error[ARCH-007]: file size grew\n  {rel}  {lines} > {blessed} (blessed high-water mark)\n  = help: an allowlisted file may shrink but never grow -- split it, or run --bless if this growth is reviewed and intentional"
                    ));
                }
                // lines <= blessed and lines > 800: fine, still within the ratchet.
            }
        }
    }

    for f in allow {
        if !seen.contains(&f.path) {
            violations.push(format!(
                "error[ARCH-008]: stale [[allow.large_file]] entry\n  {}  allowlisted at {} lines but is now <= 800 lines (or missing)\n  = help: delete this entry from architecture.toml",
                f.path, f.lines
            ));
        }
    }

    Ok(violations)
}

fn count_lines(path: &Path) -> Result<usize> {
    let text = fs::read_to_string(path).unwrap_or_default();
    Ok(text.lines().count())
}

/// `--profile <name>` support: asserts the resolved dependency closure for a
/// named Cargo feature profile contains none of a forbidden set of
/// third-party crates. Currently a stub -- wired up in full once the
/// `minimal` feature profile exists on sven-cli (refactor plan Phase 6).
fn check_profile(_workspace_root: &Path, profile: &str) -> Result<Vec<String>> {
    eprintln!("note: --profile {profile} is not yet implemented (refactor plan Phase 6.3); skipping.");
    Ok(Vec::new())
}

// ─────────────────────────────────────────────────────────────────────────
// --bless
// ─────────────────────────────────────────────────────────────────────────

/// Rewrites only the `[[allow.large_file]]` array in architecture.toml,
/// leaving every other line -- comments, `why` prose, `[[same_layer]]` /
/// `[[allow.upward]]` / `[[allow.dead_dep]]` entries -- byte-for-byte
/// untouched. This is a targeted text edit rather than a parse-and-
/// re-serialize round trip specifically so blessing a routine line-count
/// bump never drops the hand-authored rationale elsewhere in the file.
/// Requires `[[allow.large_file]]` to be the LAST section in the file (the
/// header comment documents this).
fn bless_large_files(workspace_root: &Path) -> Result<()> {
    let path = workspace_root.join("architecture.toml");
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;

    // Find the first line that IS (after trimming) exactly the table-array
    // header, not a substring match -- a substring search would also match
    // this very marker appearing inside the header comment's prose above.
    let marker = "[[allow.large_file]]";
    let mut cut = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if line.trim() == marker {
            cut = Some(offset);
            break;
        }
        offset += line.len();
    }
    let cut = cut.context("architecture.toml has no [[allow.large_file]] section to bless")?;
    let preamble = &text[..cut];

    let mut fresh = Vec::new();
    for rs_path in walk_rs_files(workspace_root)? {
        let rel = rs_path
            .strip_prefix(workspace_root)
            .unwrap_or(&rs_path)
            .to_string_lossy()
            .replace('\\', "/");
        let lines = count_lines(&rs_path)?;
        if lines > 800 {
            fresh.push(LargeFile { path: rel, lines });
        }
    }
    fresh.sort_by(|a, b| b.lines.cmp(&a.lines).then_with(|| a.path.cmp(&b.path)));

    let mut out = String::from(preamble);
    for f in &fresh {
        out.push_str("[[allow.large_file]]\n");
        out.push_str(&format!("path = \"{}\"\n", toml_escape(&f.path)));
        out.push_str(&format!("lines = {}\n", f.lines));
    }

    fs::write(&path, out).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Escapes a string for a TOML basic string. Workspace-relative `.rs` paths
/// never contain control characters, so this only needs to handle backslash
/// and double-quote.
fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
