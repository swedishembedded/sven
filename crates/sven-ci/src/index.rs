// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Repository context index — `sven index`.
//!
//! Builds a compact, searchable index of the codebase stored in `.sven/index/`.
//!
//! The index captures:
//! - **File tree**: all tracked files with sizes and modification times.
//! - **Symbol signatures**: public functions, structs, traits, types, classes.
//! - **Import graph**: which files import which (derived from `use`/`import` statements).
//!
//! ## Usage
//!
//! ```bash
//! sven index build                     # build or rebuild the index
//! sven index query "authentication"    # find symbols related to authentication
//! sven index stats                     # show index statistics
//! ```
//!
//! The index is language-aware through configurable regex patterns that
//! extract signatures from common languages without requiring tree-sitter.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

// ── Data model ────────────────────────────────────────────────────────────────

/// A single entry in the file tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// Path relative to the repository root.
    pub path: String,
    /// File size in bytes.
    pub size: u64,
    /// UNIX modification timestamp.
    pub modified: u64,
    /// Language detected from extension.
    pub language: String,
}

/// A symbol extracted from a source file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    /// File the symbol lives in (relative path).
    pub file: String,
    /// Line number (1-based).
    pub line: u32,
    /// Symbol kind: "fn", "struct", "trait", "enum", "impl", "class", "def", etc.
    pub kind: String,
    /// The symbol name.
    pub name: String,
    /// Visibility: "pub", "pub(crate)", "private", or empty.
    pub visibility: String,
    /// Short signature (first line of the declaration).
    pub signature: String,
}

/// Complete repository index stored in `.sven/index/index.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RepoIndex {
    /// ISO-8601 timestamp of when this index was built.
    pub built_at: String,
    /// Repository root path.
    pub repo_root: String,
    /// Total number of files indexed.
    pub file_count: usize,
    /// All indexed files.
    pub files: Vec<FileEntry>,
    /// Extracted symbols, keyed by symbol name (lowercase) for fast lookup.
    pub symbols: Vec<Symbol>,
    /// Simple import graph: file → list of imported modules/paths.
    pub imports: HashMap<String, Vec<String>>,
}

impl RepoIndex {
    /// Search symbols by name (case-insensitive substring match).
    pub fn search_symbols(&self, query: &str) -> Vec<&Symbol> {
        let q = query.to_lowercase();
        self.symbols
            .iter()
            .filter(|s| {
                s.name.to_lowercase().contains(&q) || s.signature.to_lowercase().contains(&q)
            })
            .collect()
    }

    /// Find all symbols in a specific file.
    pub fn symbols_in_file(&self, file_path: &str) -> Vec<&Symbol> {
        self.symbols
            .iter()
            .filter(|s| s.file == file_path)
            .collect()
    }

    /// Find files that import a given module or path fragment.
    pub fn files_importing(&self, module: &str) -> Vec<&str> {
        let q = module.to_lowercase();
        self.imports
            .iter()
            .filter(|(_, imports)| imports.iter().any(|i| i.to_lowercase().contains(&q)))
            .map(|(file, _)| file.as_str())
            .collect()
    }
}

// ── Builder ───────────────────────────────────────────────────────────────────

/// Build an index for the given repository root.
pub fn build_index(repo_root: &Path) -> anyhow::Result<RepoIndex> {
    let root = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());

    let mut files = Vec::new();
    let mut symbols = Vec::new();
    let mut imports: HashMap<String, Vec<String>> = HashMap::new();

    // Walk the repository, respecting .gitignore via the `ignore` crate-style
    // heuristics.  We do a simple recursive walk and skip known noise dirs.
    walk_dir(&root, &root, &mut files, &mut symbols, &mut imports);

    let file_count = files.len();

    Ok(RepoIndex {
        built_at: chrono::Utc::now().to_rfc3339(),
        repo_root: root.to_string_lossy().to_string(),
        file_count,
        files,
        symbols,
        imports,
    })
}

// ── Index persistence ─────────────────────────────────────────────────────────

/// Default path for the index file.
pub fn index_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".sven").join("index").join("index.json")
}

/// Save the index to disk.
pub fn save_index(repo_root: &Path, index: &RepoIndex) -> anyhow::Result<()> {
    let path = index_path(repo_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(index)?;
    std::fs::write(&path, json)?;
    Ok(())
}

/// Load the index from disk.  Returns `None` if it does not exist yet.
pub fn load_index(repo_root: &Path) -> anyhow::Result<Option<RepoIndex>> {
    let path = index_path(repo_root);
    if !path.exists() {
        return Ok(None);
    }
    let json = std::fs::read_to_string(&path)?;
    let index = serde_json::from_str(&json)?;
    Ok(Some(index))
}

// ── CLI commands ──────────────────────────────────────────────────────────────

/// `sven index build` — build or rebuild the repository index.
pub fn cmd_build(repo_root: &Path, quiet: bool) -> anyhow::Result<()> {
    if !quiet {
        eprintln!("[sven:index] Scanning {} ...", repo_root.display());
    }
    let index = build_index(repo_root)?;
    let symbol_count = index.symbols.len();
    let import_count = index.imports.values().map(|v| v.len()).sum::<usize>();
    save_index(repo_root, &index)?;
    if !quiet {
        eprintln!(
            "[sven:index] Built: {} files, {} symbols, {} imports → {}",
            index.file_count,
            symbol_count,
            import_count,
            index_path(repo_root).display()
        );
    }
    // Emit structured summary to stdout for piping.
    println!(
        "{{\"files\":{},\"symbols\":{},\"imports\":{}}}",
        index.file_count, symbol_count, import_count
    );
    Ok(())
}

/// `sven index query QUERY` — search the index for matching symbols.
pub fn cmd_query(repo_root: &Path, query: &str, limit: usize) -> anyhow::Result<()> {
    let index = load_index(repo_root)?
        .ok_or_else(|| anyhow::anyhow!("No index found. Run 'sven index build' first."))?;

    let results = index.search_symbols(query);
    let shown = results.iter().take(limit);
    let total = results.len();

    println!("Symbols matching {:?} ({total} found):", query);
    println!("{}", "-".repeat(72));
    for sym in shown {
        println!(
            "  {:<8}  {:<40}  {}:{}",
            sym.kind, sym.name, sym.file, sym.line
        );
        if !sym.signature.is_empty() && sym.signature != sym.name {
            let preview: String = sym.signature.chars().take(80).collect();
            println!("           {preview}");
        }
    }
    if total > limit {
        println!(
            "  ... and {} more (use --limit to show more)",
            total - limit
        );
    }
    Ok(())
}

/// `sven index stats` — show statistics about the current index.
pub fn cmd_stats(repo_root: &Path) -> anyhow::Result<()> {
    let index = load_index(repo_root)?
        .ok_or_else(|| anyhow::anyhow!("No index found. Run 'sven index build' first."))?;

    println!("Repository Index");
    println!("  Root:    {}", index.repo_root);
    println!("  Built:   {}", index.built_at);
    println!("  Files:   {}", index.file_count);
    println!("  Symbols: {}", index.symbols.len());
    println!(
        "  Imports: {}",
        index.imports.values().map(|v| v.len()).sum::<usize>()
    );

    // Language breakdown.
    let mut by_lang: HashMap<&str, usize> = HashMap::new();
    for f in &index.files {
        *by_lang.entry(f.language.as_str()).or_insert(0) += 1;
    }
    let mut lang_vec: Vec<_> = by_lang.iter().collect();
    lang_vec.sort_by(|a, b| b.1.cmp(a.1));
    println!("\nTop languages:");
    for (lang, count) in lang_vec.iter().take(10) {
        println!("  {:<16} {count} files", lang);
    }

    // Top symbol kinds.
    let mut by_kind: HashMap<&str, usize> = HashMap::new();
    for sym in &index.symbols {
        *by_kind.entry(sym.kind.as_str()).or_insert(0) += 1;
    }
    let mut kind_vec: Vec<_> = by_kind.iter().collect();
    kind_vec.sort_by(|a, b| b.1.cmp(a.1));
    println!("\nSymbol kinds:");
    for (kind, count) in kind_vec.iter().take(10) {
        println!("  {:<16} {count}", kind);
    }

    Ok(())
}

// ── Directory walker ──────────────────────────────────────────────────────────

const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "dist",
    "__pycache__",
    ".sven-worktrees",
    ".sven/index",
    "vendor",
    ".cargo",
];

fn should_skip_dir(name: &str) -> bool {
    SKIP_DIRS.contains(&name)
}

const SUPPORTED_EXTENSIONS: &[(&str, &str)] = &[
    ("rs", "rust"),
    ("py", "python"),
    ("ts", "typescript"),
    ("tsx", "typescript"),
    ("js", "javascript"),
    ("jsx", "javascript"),
    ("go", "go"),
    ("c", "c"),
    ("h", "c"),
    ("cpp", "cpp"),
    ("cc", "cpp"),
    ("cxx", "cpp"),
    ("hpp", "cpp"),
    ("java", "java"),
    ("kt", "kotlin"),
    ("swift", "swift"),
    ("rb", "ruby"),
    ("sh", "shell"),
    ("bash", "shell"),
    ("yaml", "yaml"),
    ("yml", "yaml"),
    ("toml", "toml"),
    ("json", "json"),
    ("md", "markdown"),
];

fn detect_language(path: &Path) -> &'static str {
    path.extension()
        .and_then(|e| e.to_str())
        .and_then(|ext| {
            SUPPORTED_EXTENSIONS
                .iter()
                .find(|(e, _)| e.eq_ignore_ascii_case(ext))
                .map(|(_, lang)| *lang)
        })
        .unwrap_or("unknown")
}

fn walk_dir(
    base: &Path,
    dir: &Path,
    files: &mut Vec<FileEntry>,
    symbols: &mut Vec<Symbol>,
    imports: &mut HashMap<String, Vec<String>>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        if path.is_dir() {
            if !should_skip_dir(name) && !name.starts_with('.') {
                walk_dir(base, &path, files, symbols, imports);
            }
            continue;
        }

        let lang = detect_language(&path);
        if lang == "unknown" {
            continue;
        }

        let rel_path = path
            .strip_prefix(base)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string_lossy().to_string());

        let metadata = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let size = metadata.len();
        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        files.push(FileEntry {
            path: rel_path.clone(),
            size,
            modified,
            language: lang.to_string(),
        });

        // Only parse text files under 512 KiB to keep indexing fast.
        if size < 512 * 1024 {
            if let Ok(content) = std::fs::read_to_string(&path) {
                extract_symbols_and_imports(lang, &rel_path, &content, symbols, imports);
            }
        }
    }
}

// ── Symbol extractor ──────────────────────────────────────────────────────────

fn extract_symbols_and_imports(
    lang: &str,
    file: &str,
    content: &str,
    symbols: &mut Vec<Symbol>,
    imports: &mut HashMap<String, Vec<String>>,
) {
    match lang {
        "rust" => extract_rust(file, content, symbols, imports),
        "python" => extract_python(file, content, symbols, imports),
        "typescript" | "javascript" => extract_ts_js(file, content, symbols, imports),
        "go" => extract_go(file, content, symbols, imports),
        "c" | "cpp" => extract_c_cpp(file, content, symbols, imports),
        _ => {}
    }
}

fn push_symbol(
    symbols: &mut Vec<Symbol>,
    file: &str,
    line: u32,
    kind: &str,
    name: &str,
    visibility: &str,
    signature: &str,
) {
    if name.is_empty() {
        return;
    }
    symbols.push(Symbol {
        file: file.to_string(),
        line,
        kind: kind.to_string(),
        name: name.to_string(),
        visibility: visibility.to_string(),
        signature: signature.trim().chars().take(120).collect(),
    });
}

// ── Rust extractor ────────────────────────────────────────────────────────────

fn extract_rust(
    file: &str,
    content: &str,
    symbols: &mut Vec<Symbol>,
    imports: &mut HashMap<String, Vec<String>>,
) {
    let mut file_imports = Vec::new();

    for (i, line) in content.lines().enumerate() {
        let ln = (i + 1) as u32;
        let trimmed = line.trim();

        // Imports
        if trimmed.starts_with("use ") {
            let imp = trimmed
                .trim_start_matches("use ")
                .trim_end_matches(';')
                .to_string();
            file_imports.push(imp);
            continue;
        }

        // Visibility prefix
        let (vis, rest) = extract_visibility(trimmed);

        // pub fn / fn
        if let Some(name) = extract_name_after(rest, "fn ") {
            push_symbol(symbols, file, ln, "fn", &name, vis, trimmed);
        }
        // pub struct / struct
        else if let Some(name) = extract_name_after(rest, "struct ") {
            push_symbol(symbols, file, ln, "struct", &name, vis, trimmed);
        }
        // pub enum / enum
        else if let Some(name) = extract_name_after(rest, "enum ") {
            push_symbol(symbols, file, ln, "enum", &name, vis, trimmed);
        }
        // pub trait / trait
        else if let Some(name) = extract_name_after(rest, "trait ") {
            push_symbol(symbols, file, ln, "trait", &name, vis, trimmed);
        }
        // pub type / type
        else if let Some(name) = extract_name_after(rest, "type ") {
            push_symbol(symbols, file, ln, "type", &name, vis, trimmed);
        }
        // impl
        else if trimmed.starts_with("impl ") || trimmed.starts_with("impl<") {
            let name = trimmed
                .trim_start_matches("impl")
                .trim_start_matches('<')
                .split_whitespace()
                .find(|s| !s.is_empty() && !s.starts_with('<'))
                .unwrap_or("")
                .trim_end_matches('{')
                .to_string();
            if !name.is_empty() {
                push_symbol(symbols, file, ln, "impl", &name, "", trimmed);
            }
        }
        // const
        else if let Some(name) = extract_name_after(rest, "const ") {
            push_symbol(symbols, file, ln, "const", &name, vis, trimmed);
        }
        // static
        else if let Some(name) = extract_name_after(rest, "static ") {
            let name = name.trim_start_matches("mut ").to_string();
            push_symbol(symbols, file, ln, "static", &name, vis, trimmed);
        }
        // mod
        else if let Some(name) = extract_name_after(rest, "mod ") {
            push_symbol(symbols, file, ln, "mod", &name, vis, trimmed);
        }
    }

    if !file_imports.is_empty() {
        imports.insert(file.to_string(), file_imports);
    }
}

fn extract_visibility(s: &str) -> (&'static str, &str) {
    if let Some(rest) = s.strip_prefix("pub(crate) ") {
        ("pub(crate)", rest)
    } else if let Some(rest) = s.strip_prefix("pub(super) ") {
        ("pub(super)", rest)
    } else if let Some(rest) = s.strip_prefix("pub ") {
        ("pub", rest)
    } else {
        ("", s)
    }
}

fn extract_name_after(s: &str, prefix: &str) -> Option<String> {
    let rest = s.strip_prefix(prefix)?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

// ── Python extractor ──────────────────────────────────────────────────────────

fn extract_python(
    file: &str,
    content: &str,
    symbols: &mut Vec<Symbol>,
    imports: &mut HashMap<String, Vec<String>>,
) {
    let mut file_imports = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let ln = (i + 1) as u32;
        let trimmed = line.trim();

        if trimmed.starts_with("import ") || trimmed.starts_with("from ") {
            file_imports.push(trimmed.to_string());
        } else if let Some(name) = extract_name_after(trimmed, "def ") {
            push_symbol(symbols, file, ln, "def", &name, "", trimmed);
        } else if let Some(name) = extract_name_after(trimmed, "class ") {
            let name = name.split('(').next().unwrap_or(&name).to_string();
            push_symbol(symbols, file, ln, "class", &name, "", trimmed);
        } else if let Some(name) = extract_name_after(trimmed, "async def ") {
            push_symbol(symbols, file, ln, "async def", &name, "", trimmed);
        }
    }
    if !file_imports.is_empty() {
        imports.insert(file.to_string(), file_imports);
    }
}

// ── TypeScript / JavaScript extractor ────────────────────────────────────────

fn extract_ts_js(
    file: &str,
    content: &str,
    symbols: &mut Vec<Symbol>,
    imports: &mut HashMap<String, Vec<String>>,
) {
    let mut file_imports = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let ln = (i + 1) as u32;
        let trimmed = line.trim();

        if trimmed.starts_with("import ") {
            file_imports.push(trimmed.to_string());
        } else if trimmed.starts_with("export function ")
            || trimmed.starts_with("export async function ")
        {
            let after = trimmed
                .trim_start_matches("export async function ")
                .trim_start_matches("export function ");
            if let Some(name) = extract_name_after(after, "") {
                push_symbol(symbols, file, ln, "function", &name, "export", trimmed);
            }
        } else if trimmed.starts_with("export class ")
            || trimmed.starts_with("export default class ")
        {
            let after = trimmed
                .trim_start_matches("export default class ")
                .trim_start_matches("export class ");
            if let Some(name) = extract_name_after(after, "") {
                push_symbol(symbols, file, ln, "class", &name, "export", trimmed);
            }
        } else if trimmed.starts_with("export interface ") {
            let after = trimmed.trim_start_matches("export interface ");
            if let Some(name) = extract_name_after(after, "") {
                push_symbol(symbols, file, ln, "interface", &name, "export", trimmed);
            }
        } else if trimmed.starts_with("export type ") {
            let after = trimmed.trim_start_matches("export type ");
            if let Some(name) = extract_name_after(after, "") {
                push_symbol(symbols, file, ln, "type", &name, "export", trimmed);
            }
        } else if trimmed.starts_with("function ") {
            if let Some(name) = extract_name_after(trimmed, "function ") {
                push_symbol(symbols, file, ln, "function", &name, "", trimmed);
            }
        } else if trimmed.starts_with("class ") {
            if let Some(name) = extract_name_after(trimmed, "class ") {
                push_symbol(symbols, file, ln, "class", &name, "", trimmed);
            }
        }
    }
    if !file_imports.is_empty() {
        imports.insert(file.to_string(), file_imports);
    }
}

// ── Go extractor ──────────────────────────────────────────────────────────────

fn extract_go(
    file: &str,
    content: &str,
    symbols: &mut Vec<Symbol>,
    imports: &mut HashMap<String, Vec<String>>,
) {
    let mut in_import_block = false;
    let mut file_imports = Vec::new();

    for (i, line) in content.lines().enumerate() {
        let ln = (i + 1) as u32;
        let trimmed = line.trim();

        if trimmed == "import (" {
            in_import_block = true;
            continue;
        }
        if in_import_block && trimmed == ")" {
            in_import_block = false;
            continue;
        }
        if in_import_block {
            file_imports.push(trimmed.trim_matches('"').to_string());
            continue;
        }
        if trimmed.starts_with("import ") {
            file_imports.push(
                trimmed
                    .trim_start_matches("import ")
                    .trim_matches('"')
                    .to_string(),
            );
        } else if trimmed.starts_with("func ") {
            let after = trimmed.trim_start_matches("func ");
            // Handle receiver: func (r Recv) MethodName(...)
            let name = if after.starts_with('(') {
                // Skip receiver, get method name
                after
                    .split(')')
                    .nth(1)
                    .unwrap_or("")
                    .trim()
                    .split('(')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string()
            } else {
                extract_name_after(after, "").unwrap_or_default()
            };
            if !name.is_empty() {
                push_symbol(symbols, file, ln, "func", &name, "", trimmed);
            }
        } else if trimmed.starts_with("type ") {
            if let Some(name) = extract_name_after(trimmed, "type ") {
                let kind = if trimmed.contains("interface") {
                    "interface"
                } else if trimmed.contains("struct") {
                    "struct"
                } else {
                    "type"
                };
                push_symbol(symbols, file, ln, kind, &name, "", trimmed);
            }
        }
    }
    if !file_imports.is_empty() {
        imports.insert(file.to_string(), file_imports);
    }
}

// ── C/C++ extractor ───────────────────────────────────────────────────────────

fn extract_c_cpp(
    file: &str,
    content: &str,
    symbols: &mut Vec<Symbol>,
    imports: &mut HashMap<String, Vec<String>>,
) {
    let mut file_imports = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let ln = (i + 1) as u32;
        let trimmed = line.trim();

        if trimmed.starts_with("#include ") {
            file_imports.push(trimmed.to_string());
        } else if trimmed.starts_with("struct ") || trimmed.starts_with("typedef struct ") {
            let after = trimmed
                .trim_start_matches("typedef struct ")
                .trim_start_matches("struct ");
            let name = after
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .next()
                .unwrap_or("")
                .to_string();
            if !name.is_empty() {
                push_symbol(symbols, file, ln, "struct", &name, "", trimmed);
            }
        } else if trimmed.starts_with("enum ") {
            let after = trimmed.trim_start_matches("enum ");
            let name = after
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .next()
                .unwrap_or("")
                .to_string();
            if !name.is_empty() {
                push_symbol(symbols, file, ln, "enum", &name, "", trimmed);
            }
        }
    }
    if !file_imports.is_empty() {
        imports.insert(file.to_string(), file_imports);
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn rust_symbol_extraction() {
        let mut symbols = Vec::new();
        let mut imports = HashMap::new();
        extract_rust(
            "src/lib.rs",
            "pub fn hello() {}\npub struct Foo;\npub(crate) trait Bar {}",
            &mut symbols,
            &mut imports,
        );
        assert_eq!(symbols.len(), 3);
        assert_eq!(symbols[0].kind, "fn");
        assert_eq!(symbols[0].name, "hello");
        assert_eq!(symbols[0].visibility, "pub");
        assert_eq!(symbols[1].kind, "struct");
        assert_eq!(symbols[1].name, "Foo");
        assert_eq!(symbols[2].kind, "trait");
        assert_eq!(symbols[2].visibility, "pub(crate)");
    }

    #[test]
    fn rust_import_extraction() {
        let mut symbols = Vec::new();
        let mut imports = HashMap::new();
        extract_rust(
            "src/main.rs",
            "use std::io;\nuse anyhow::Context;\n",
            &mut symbols,
            &mut imports,
        );
        let file_imports = imports.get("src/main.rs").unwrap();
        assert!(file_imports.iter().any(|i| i.contains("std::io")));
        assert!(file_imports.iter().any(|i| i.contains("anyhow")));
    }

    #[test]
    fn python_symbol_extraction() {
        let mut symbols = Vec::new();
        let mut imports = HashMap::new();
        extract_python(
            "app.py",
            "def greet(name):\n    pass\nclass Foo:\n    pass\nasync def run():\n    pass",
            &mut symbols,
            &mut imports,
        );
        assert!(symbols.iter().any(|s| s.name == "greet" && s.kind == "def"));
        assert!(symbols.iter().any(|s| s.name == "Foo" && s.kind == "class"));
        assert!(symbols
            .iter()
            .any(|s| s.name == "run" && s.kind == "async def"));
    }

    #[test]
    fn search_symbols_case_insensitive() {
        let mut index = RepoIndex::default();
        index.symbols.push(Symbol {
            file: "src/auth.rs".to_string(),
            line: 10,
            kind: "fn".to_string(),
            name: "authenticate_user".to_string(),
            visibility: "pub".to_string(),
            signature: "pub fn authenticate_user(token: &str) -> bool".to_string(),
        });
        let results = index.search_symbols("AUTH");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "authenticate_user");
    }

    #[test]
    fn build_index_on_small_dir() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("main.rs"), "pub fn main() {}").unwrap();
        std::fs::write(dir.path().join("lib.py"), "def greet(): pass").unwrap();
        let index = build_index(dir.path()).unwrap();
        assert!(index.file_count >= 2);
        assert!(index.symbols.iter().any(|s| s.name == "main"));
        assert!(index.symbols.iter().any(|s| s.name == "greet"));
    }

    #[test]
    fn index_roundtrip() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("foo.rs"), "pub fn foo() {}").unwrap();
        let index = build_index(dir.path()).unwrap();
        save_index(dir.path(), &index).unwrap();
        let loaded = load_index(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.file_count, index.file_count);
        assert_eq!(loaded.symbols.len(), index.symbols.len());
    }
}
