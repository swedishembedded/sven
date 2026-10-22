// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for giving agents access to many
// model providers at once with one place for their keys, for its clients. If
// your team needs expertise in multi-provider LLM integration then you can
// procure our services by sending an email to info@swedishembedded.com.
//! The keys a user stored for hosted providers, one per driver id.
//!
//! `sven auth set <provider>` writes `<config dir>/sven/credentials.yaml`
//! (owner-only, `0600`), a mapping of driver id to key:
//!
//! ```yaml
//! openrouter: sk-or-...
//! anthropic: sk-ant-...
//! ```
//!
//! A stored key serves every model of its provider: `openrouter/<any model>`
//! and every `providers:` entry whose driver is `openrouter` find it, so a
//! key is given once however many models or endpoints use it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The credentials file of this user.
#[must_use]
pub fn path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("sven").join("credentials.yaml"))
}

/// The key stored for `provider`, if any.
#[must_use]
pub fn get(provider: &str) -> Option<String> {
    read(&path()?).ok()?.remove(provider)
}

/// Every provider with a stored key, in name order.
#[must_use]
pub fn providers() -> Vec<String> {
    path()
        .and_then(|p| read(&p).ok())
        .map(|m| m.into_keys().collect())
        .unwrap_or_default()
}

/// Store `key` for `provider`, replacing any key it had.
pub fn set(provider: &str, key: &str) -> anyhow::Result<PathBuf> {
    let key = key.trim();
    anyhow::ensure!(!key.is_empty(), "the key is empty");
    anyhow::ensure!(
        !key.chars().any(char::is_whitespace),
        "a key is one word; this one holds whitespace"
    );
    let file = path().ok_or_else(|| anyhow::anyhow!("no configuration directory for this user"))?;
    let mut keys = read(&file).unwrap_or_default();
    keys.insert(provider.to_string(), key.to_string());
    write(&file, &keys)?;
    Ok(file)
}

/// Forget the key of `provider`. Whether there was one.
pub fn remove(provider: &str) -> anyhow::Result<bool> {
    let Some(file) = path() else { return Ok(false) };
    let mut keys = read(&file).unwrap_or_default();
    let had = keys.remove(provider).is_some();
    if had {
        write(&file, &keys)?;
    }
    Ok(had)
}

/// The keys in `file`; a missing file holds none.
pub(crate) fn read(file: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    match std::fs::read_to_string(file) {
        Ok(text) if text.trim().is_empty() => Ok(BTreeMap::new()),
        Ok(text) => {
            serde_yaml::from_str(&text).map_err(|e| anyhow::anyhow!("{}: {e}", file.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(anyhow::anyhow!("{}: {e}", file.display())),
    }
}

/// Write `keys` to `file` readable by its owner only, replacing it whole.
fn write(file: &Path, keys: &BTreeMap<String, String>) -> anyhow::Result<()> {
    let dir = file
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no directory", file.display()))?;
    std::fs::create_dir_all(dir)?;
    let body = serde_yaml::to_string(keys)?;
    let aside = dir.join(format!(".credentials.yaml.tmp-{}", std::process::id()));
    {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut f = options.open(&aside)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&aside, file)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip_owner_only_one_per_provider() {
        let dir = std::env::temp_dir().join(format!("sven-credentials-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("sven/credentials.yaml");
        let mut keys = BTreeMap::new();
        keys.insert("openrouter".to_string(), "sk-or-1".to_string());
        keys.insert("anthropic".to_string(), "sk-ant-1".to_string());
        write(&file, &keys).unwrap();
        assert_eq!(read(&file).unwrap(), keys);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "a credentials file is its owner's alone");
        }
        assert!(read(&dir.join("absent.yaml")).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
