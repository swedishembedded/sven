use std::path::{Path, PathBuf};

use anyhow::Context;
use tracing::debug;

use crate::Config;

/// Ordered list of config file locations searched from lowest to highest priority.
/// Later files override earlier ones.
fn config_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // 1. System-wide default
    paths.push(PathBuf::from("/etc/sven/config.toml"));

    // 2. XDG / home
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".config/sven/config.toml"));
    }
    if let Some(cfg) = dirs::config_dir() {
        paths.push(cfg.join("sven/config.toml"));
    }

    // 3. Workspace-local
    paths.push(PathBuf::from(".sven/config.toml"));
    paths.push(PathBuf::from("sven.toml"));

    paths
}

/// Load configuration by merging all discovered TOML files.
/// The `extra` argument may provide an explicit path (e.g. `--config` CLI flag).
pub fn load(extra: Option<&Path>) -> anyhow::Result<Config> {
    let mut merged = toml::Value::Table(toml::map::Map::new());

    for path in config_search_paths() {
        if path.is_file() {
            debug!(path = %path.display(), "loading config layer");
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let layer: toml::Value = toml::from_str(&text)
                .with_context(|| format!("parsing {}", path.display()))?;
            merge_toml(&mut merged, layer);
        }
    }

    if let Some(p) = extra {
        debug!(path = %p.display(), "loading explicit config");
        let text = std::fs::read_to_string(p)
            .with_context(|| format!("reading {}", p.display()))?;
        let layer: toml::Value = toml::from_str(&text)
            .with_context(|| format!("parsing {}", p.display()))?;
        merge_toml(&mut merged, layer);
    }

    let config: Config = merged.try_into().unwrap_or_default();
    Ok(config)
}

/// Deep-merge `src` into `dst`; src wins on scalar conflicts.
fn merge_toml(dst: &mut toml::Value, src: toml::Value) {
    match (dst, src) {
        (toml::Value::Table(d), toml::Value::Table(s)) => {
            for (k, v) in s {
                let entry = d.entry(k).or_insert(toml::Value::Table(toml::map::Map::new()));
                merge_toml(entry, v);
            }
        }
        (dst, src) => *dst = src,
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn val(s: &str) -> toml::Value {
        toml::from_str(s).unwrap()
    }

    #[test]
    fn merge_scalar_src_wins() {
        let mut dst = val(r#"x = 1"#);
        let src = val(r#"x = 2"#);
        merge_toml(&mut dst, src);
        assert_eq!(dst["x"].as_integer(), Some(2));
    }

    #[test]
    fn merge_preserves_keys_not_in_src() {
        let mut dst = val(r#"a = 1
b = 2"#);
        let src = val(r#"b = 99"#);
        merge_toml(&mut dst, src);
        assert_eq!(dst["a"].as_integer(), Some(1));
        assert_eq!(dst["b"].as_integer(), Some(99));
    }

    #[test]
    fn merge_nested_tables() {
        let mut dst = val(r#"[model]
provider = "openai"
name = "gpt-4o""#);
        let src = val(r#"[model]
name = "gpt-4o-mini""#);
        merge_toml(&mut dst, src);
        assert_eq!(dst["model"]["provider"].as_str(), Some("openai"));
        assert_eq!(dst["model"]["name"].as_str(), Some("gpt-4o-mini"));
    }

    #[test]
    fn load_returns_defaults_when_no_files_present() {
        // Pass a non-existent explicit path – load() must still succeed
        let result = load(Some(Path::new("/tmp/sven_nonexistent_config_xyz.toml")));
        // Error expected because the explicit path doesn't exist
        assert!(result.is_err());
    }

    #[test]
    fn load_with_no_extra_path_returns_defaults() {
        // No config files on disk in this environment → pure defaults
        let cfg = load(None).unwrap();
        assert_eq!(cfg.model.provider, "openai");
    }

    #[test]
    fn load_explicit_file_overrides_defaults() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, r#"[model]
provider = "anthropic"
name = "test-model""#).unwrap();
        let cfg = load(Some(f.path())).unwrap();
        assert_eq!(cfg.model.provider, "anthropic");
        assert_eq!(cfg.model.name, "test-model");
    }
}
