/// Persistent conversation history management.
///
/// Conversations are stored as markdown files in `~/.local/share/sven/history/`
/// (XDG data directory).  File names follow the pattern:
///
///   `<YYYY-MM-DDTHH-MM-SSZ>_<slug>.md`
///
/// where the slug is derived from the first user message.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use sven_model::Message;

use crate::{parse_conversation, serialize_conversation, ParsedConversation};

// ─── Directory ───────────────────────────────────────────────────────────────

/// Returns the directory where sven stores conversation history.
///
/// Defaults to `$XDG_DATA_HOME/sven/history` (i.e. `~/.local/share/sven/history`).
pub fn history_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local")
                .join("share")
        })
        .join("sven")
        .join("history")
}

/// Creates the history directory if it does not exist and returns its path.
pub fn ensure_history_dir() -> Result<PathBuf> {
    let dir = history_dir();
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating history directory {}", dir.display()))?;
    Ok(dir)
}

// ─── Save ────────────────────────────────────────────────────────────────────

/// Writes `messages` as a new conversation file in the history directory.
///
/// Returns the path of the written file.  Does nothing and returns an error if
/// `messages` is empty.
pub fn save(messages: &[Message]) -> Result<PathBuf> {
    if messages.is_empty() {
        anyhow::bail!("cannot save empty conversation");
    }

    let dir = ensure_history_dir()?;

    let first_user = messages
        .iter()
        .find(|m| matches!(m.role, sven_model::Role::User))
        .and_then(|m| m.as_text())
        .unwrap_or("conversation");

    // Derive a human-readable title from the first user message.
    let title = make_title(first_user);
    let filename = make_filename(first_user);
    let path = dir.join(&filename);

    // Embed the title as an H1 so the file is self-describing.
    let content = serialize_conversation(Some(&title), messages);
    fs::write(&path, &content)
        .with_context(|| format!("writing conversation to {}", path.display()))?;

    Ok(path)
}

/// Overwrites an existing conversation file with the given messages.
///
/// Preserves the H1 title already present in the file (if any); otherwise
/// derives one from the first user message, consistent with `save()`.
pub fn save_to(path: &Path, messages: &[Message]) -> Result<()> {
    if messages.is_empty() {
        return Ok(());
    }

    // Preserve an existing title so repeated saves don't lose it.
    let existing_title: Option<String> = fs::read_to_string(path)
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("# ") && !l.starts_with("## "))
                .map(|l| l[2..].trim().to_string())
        });

    let title = existing_title.or_else(|| {
        messages
            .iter()
            .find(|m| matches!(m.role, sven_model::Role::User))
            .and_then(|m| m.as_text())
            .map(make_title)
    });

    let content = serialize_conversation(title.as_deref(), messages);
    fs::write(path, &content)
        .with_context(|| format!("writing conversation to {}", path.display()))
}

// ─── List ────────────────────────────────────────────────────────────────────

/// A summary of a saved conversation shown when listing history.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    /// File stem used as an ID for `--resume` (filename without `.md`).
    pub id: String,
    /// Full path to the conversation file.
    pub path: PathBuf,
    /// ISO-8601 timestamp string extracted from the filename.
    pub timestamp: String,
    /// Human-readable title (H1 from file or derived from the first user message).
    pub title: String,
    /// Number of user/assistant turn pairs in the file.
    pub turns: usize,
}

/// Lists all conversations in the history directory, most recent first.
pub fn list(limit: Option<usize>) -> Result<Vec<HistoryEntry>> {
    let dir = history_dir();
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries: Vec<HistoryEntry> = Vec::new();
    for entry in fs::read_dir(&dir).context("reading history directory")? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();

        let (timestamp, title) = parse_stem_and_title(&stem, &path);
        let turns = count_turns(&path);

        entries.push(HistoryEntry { id: stem, path, timestamp, title, turns });
    }

    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    if let Some(n) = limit {
        entries.truncate(n);
    }
    Ok(entries)
}

// ─── Resolve / Load ──────────────────────────────────────────────────────────

/// Resolves a conversation ID to its file path.
///
/// Accepts:
/// - Exact file stem (filename without `.md`)
/// - Unique timestamp prefix (e.g. `2026-02-20`)
/// - Absolute or relative filesystem path to a `.md` file
pub fn resolve(id: &str) -> Result<PathBuf> {
    let p = PathBuf::from(id);
    if p.is_absolute() || id.contains('/') {
        if p.exists() {
            return Ok(p);
        }
        anyhow::bail!("file not found: {}", p.display());
    }

    let dir = history_dir();

    let with_ext = dir.join(format!("{id}.md"));
    if with_ext.exists() {
        return Ok(with_ext);
    }

    if dir.exists() {
        let mut matches: Vec<PathBuf> = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(id) && name.ends_with(".md") {
                matches.push(entry.path());
            }
        }
        matches.sort();
        match matches.len() {
            1 => return Ok(matches.remove(0)),
            n if n > 1 => {
                let ids: Vec<String> = matches
                    .iter()
                    .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()))
                    .collect();
                anyhow::bail!(
                    "ambiguous id '{}' matches {} conversations:\n  {}\nBe more specific.",
                    id,
                    n,
                    ids.join("\n  ")
                );
            }
            _ => {}
        }
    }

    anyhow::bail!(
        "no conversation found with id '{}'. Use 'sven chats' to list saved conversations.",
        id
    )
}

/// Loads and parses a conversation by ID.  Returns both the parsed conversation
/// and the resolved file path (needed for subsequent saves).
pub fn load(id: &str) -> Result<(ParsedConversation, PathBuf)> {
    let path = resolve(id)?;
    let content = fs::read_to_string(&path)
        .with_context(|| format!("reading conversation file {}", path.display()))?;
    let parsed = parse_conversation(&content)
        .with_context(|| format!("parsing conversation file {}", path.display()))?;
    Ok((parsed, path))
}

// ─── Internal helpers ────────────────────────────────────────────────────────

/// Builds a filename for a new conversation file.
pub fn make_filename(first_user_message: &str) -> String {
    let ts = Utc::now().format("%Y-%m-%dT%H-%M-%SZ").to_string();
    let slug = slugify(first_user_message, 60);
    if slug.is_empty() {
        format!("{ts}.md")
    } else {
        format!("{ts}_{slug}.md")
    }
}

/// Derives a human-readable title (capitalised, up to ~80 chars) from a
/// free-form text string — used as the H1 title in saved conversation files.
pub fn make_title(text: &str) -> String {
    // Take up to the first sentence (stop at '.', '!', '?') or 80 chars.
    let trimmed = text.trim();
    let sentence_end = trimmed
        .char_indices()
        .find(|(_, c)| matches!(*c, '.' | '!' | '?'))
        .map(|(i, _)| i + 1)
        .unwrap_or(trimmed.len());
    let raw: String = trimmed.chars().take(sentence_end.min(80)).collect();
    let raw = raw.trim_end_matches(|c: char| c == '.' || c == '!' || c == '?').trim();
    if raw.is_empty() {
        return "Conversation".to_string();
    }
    // Capitalise first character.
    let mut chars = raw.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

fn slugify(s: &str, max_chars: usize) -> String {
    s.split_whitespace()
        .take(10)
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .take(max_chars)
        .collect()
}

fn parse_stem_and_title(stem: &str, path: &Path) -> (String, String) {
    let (ts, slug_hint) = if let Some(idx) = stem.find('_') {
        (&stem[..idx], stem[idx + 1..].replace('-', " "))
    } else {
        (stem, String::new())
    };

    let title = read_title_from_file(path).unwrap_or(slug_hint);
    (ts.to_string(), title)
}

fn read_title_from_file(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;

    // Prefer an explicit H1 title line.
    for line in content.lines() {
        if line.starts_with("## ") {
            break;
        }
        if let Some(t) = line.strip_prefix("# ") {
            return Some(t.trim().to_string());
        }
    }

    // Fall back to the first non-empty line of the first ## User section.
    let mut in_user = false;
    for line in content.lines() {
        if line.trim() == "## User" {
            in_user = true;
            continue;
        }
        if in_user {
            if line.starts_with("## ") {
                break;
            }
            if !line.trim().is_empty() {
                let s: String = line.chars().take(60).collect();
                return Some(if line.len() > 60 { format!("{s}…") } else { s });
            }
        }
    }
    None
}

fn count_turns(path: &Path) -> usize {
    let Ok(content) = fs::read_to_string(path) else {
        return 0;
    };
    content
        .lines()
        .filter(|l| l.trim() == "## User")
        .count()
}
