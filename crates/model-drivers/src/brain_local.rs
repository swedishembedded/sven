// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for connecting coding agents to
// the model servers running beside them, with no keys or ports to copy by
// hand, for its clients. If your team needs expertise in local LLM serving
// and agent integration then you can procure our services by sending an email
// to info@swedishembedded.com.
//! A brain server running on this host, found from the connection record it
//! publishes.
//!
//! Every `brain serve` that binds an HTTP surface writes
//! `<runtime dir>/serve-<pid>.json` once it is listening and removes it when
//! it stops: `{"pid": N, "surfaces": [{"provider": "openai", "url":
//! "http://127.0.0.1:8788", "key": "sk-brain-…"}]}`. The runtime dir is
//! `$XDG_RUNTIME_DIR/brain`, else `<tmpdir>/brain-<uid>`. sven's `brain` driver
//! speaks the OpenAI dialect, so it takes an `openai` surface and its key.
//!
//! A record is trusted only when this user owns it and its directory and
//! nobody else may write either: the URL in it is where sven sends the
//! conversation. A record whose process is gone (a crash leaves it behind) is
//! skipped.

use std::path::{Path, PathBuf};

/// One brain server's OpenAI surface: where sven's `brain` driver talks to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalBrain {
    /// The OpenAI-compatible base URL, ending in `/v1`.
    pub base_url: String,
    /// The key that surface requires.
    pub key: String,
    /// The serving process.
    pub pid: u32,
}

/// The port brain's OpenAI surface binds by default; a server on it is the
/// one a user started with plain `brain serve`, and wins over private ones.
const DEFAULT_PORT: u16 = 8788;

/// Where brain publishes its connection records for this user.
#[must_use]
pub fn runtime_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
        Some(dir) => PathBuf::from(dir).join("brain"),
        None => std::env::temp_dir().join(format!("brain-{}", current_uid())),
    }
}

/// The brain server to use: the one whose base URL is `base_url` when given,
/// else the one on brain's default port, else the most recently started.
#[must_use]
pub fn find(base_url: Option<&str>) -> Option<LocalBrain> {
    find_in(&runtime_dir(), base_url)
}

/// [`find`] over the records in `dir`.
pub(crate) fn find_in(dir: &Path, base_url: Option<&str>) -> Option<LocalBrain> {
    let servers = servers_in(dir);
    if let Some(wanted) = base_url {
        let wanted = wanted.trim_end_matches('/');
        return servers.into_iter().find(|s| s.base_url == wanted);
    }
    servers
        .iter()
        .find(|s| port_of(&s.base_url) == Some(DEFAULT_PORT))
        .or_else(|| servers.first())
        .cloned()
}

/// Every live brain server with an OpenAI surface recorded in `dir`, newest
/// first. Empty when the directory is missing or not this user's alone.
pub(crate) fn servers_in(dir: &Path) -> Vec<LocalBrain> {
    if !private_to_me(dir, true) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(std::time::SystemTime, LocalBrain)> = entries
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with("serve-") && name.ends_with(".json")
        })
        .filter_map(|e| {
            let path = e.path();
            if !private_to_me(&path, false) {
                return None;
            }
            let modified = e.metadata().ok()?.modified().ok()?;
            let server = parse_record(&std::fs::read_to_string(&path).ok()?)?;
            process_alive(server.pid).then_some((modified, server))
        })
        .collect();
    found.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    found.into_iter().map(|(_, s)| s).collect()
}

/// The OpenAI surface a record names, when it names one.
fn parse_record(text: &str) -> Option<LocalBrain> {
    let record: serde_json::Value = serde_json::from_str(text).ok()?;
    let pid = u32::try_from(record.get("pid")?.as_u64()?).ok()?;
    let surface = record
        .get("surfaces")?
        .as_array()?
        .iter()
        .find(|s| s.get("provider").and_then(|p| p.as_str()) == Some("openai"))?;
    let url = surface.get("url")?.as_str()?.trim_end_matches('/');
    let key = surface.get("key")?.as_str()?;
    if key.is_empty() || !url.starts_with("http://") {
        return None;
    }
    Some(LocalBrain {
        base_url: format!("{url}/v1"),
        key: key.to_string(),
        pid,
    })
}

fn port_of(url: &str) -> Option<u16> {
    reqwest::Url::parse(url).ok()?.port()
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

/// Owned by this user, not a symlink, and writable by nobody else; a file
/// must also be unreadable by anybody else, since it holds a key.
#[cfg(unix)]
fn private_to_me(path: &Path, is_dir: bool) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    let closed = if is_dir { 0o022 } else { 0o077 };
    meta.is_dir() == is_dir && meta.uid() == current_uid() && meta.mode() & closed == 0
}

#[cfg(not(unix))]
fn private_to_me(path: &Path, is_dir: bool) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_dir() == is_dir)
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists and may be
    // signalled; nothing is delivered.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    true
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("sven-brain-local-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(dir)
        }

        fn record(&self, pid: u32, url: &str, key: &str) -> PathBuf {
            let path = self.0.join(format!("serve-{pid}.json"));
            let body = serde_json::json!({"pid": pid, "surfaces": [
                {"provider": "anthropic", "url": "http://127.0.0.1:1", "key": "sk-other"},
                {"provider": "openai", "url": url, "key": key},
            ]});
            std::fs::write(&path, body.to_string()).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            path
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn dead_pid() -> u32 {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    #[test]
    fn a_live_server_is_found_by_its_openai_surface() {
        let dir = Dir::new("live");
        dir.record(
            std::process::id(),
            "http://127.0.0.1:41234",
            "sk-brain-live",
        );
        let found = find_in(&dir.0, None).expect("the live record");
        assert_eq!(found.base_url, "http://127.0.0.1:41234/v1");
        assert_eq!(found.key, "sk-brain-live");
        assert_eq!(
            find_in(&dir.0, Some("http://127.0.0.1:41234/v1/")).map(|s| s.key),
            Some("sk-brain-live".to_string()),
            "a configured base URL selects its own server"
        );
        assert_eq!(find_in(&dir.0, Some("http://127.0.0.1:9/v1")), None);
    }

    #[test]
    fn a_record_whose_process_is_gone_is_skipped() {
        let dir = Dir::new("dead");
        dir.record(dead_pid(), "http://127.0.0.1:41235", "sk-brain-dead");
        assert_eq!(find_in(&dir.0, None), None);
    }

    #[test]
    fn a_record_others_could_read_or_plant_is_not_trusted() {
        let dir = Dir::new("open");
        let path = dir.record(
            std::process::id(),
            "http://127.0.0.1:41236",
            "sk-brain-open",
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(find_in(&dir.0, None), None, "a key others can read");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(
            find_in(&dir.0, None),
            None,
            "a directory others can write into"
        );
    }

    #[test]
    fn the_server_on_the_default_port_wins_over_a_private_one() {
        let dir = Dir::new("default-port");
        dir.record(
            std::process::id(),
            "http://127.0.0.1:8788",
            "sk-brain-default",
        );
        // A parent of this test process is alive too: a second live server.
        let parent = std::os::unix::process::parent_id();
        dir.record(parent, "http://127.0.0.1:41237", "sk-brain-private");
        assert_eq!(
            find_in(&dir.0, None).map(|s| s.key),
            Some("sk-brain-default".to_string())
        );
    }

    #[test]
    fn a_record_without_an_openai_surface_names_no_server() {
        assert_eq!(
            parse_record(
                r#"{"pid": 1, "surfaces": [{"provider": "anthropic", "url": "http://x:1", "key": "k"}]}"#
            ),
            None
        );
        assert_eq!(parse_record("not json"), None);
        assert_eq!(
            parse_record(
                r#"{"pid": 1, "surfaces": [{"provider": "openai", "url": "http://x:1", "key": ""}]}"#
            ),
            None
        );
    }
}
