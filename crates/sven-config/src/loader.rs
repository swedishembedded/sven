// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use std::path::{Path, PathBuf};

use anyhow::Context;
use tracing::debug;

use crate::Config;

/// Ordered list of config file locations searched from lowest to highest priority.
/// Later files override earlier ones.
fn config_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // 1. System-wide default
    paths.push(PathBuf::from("/etc/sven/config.yaml"));
    paths.push(PathBuf::from("/etc/sven/config.yml"));

    // 2. XDG / home
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".config/sven/config.yaml"));
        paths.push(home.join(".config/sven/config.yml"));
    }
    if let Some(cfg) = dirs::config_dir() {
        paths.push(cfg.join("sven/config.yaml"));
        paths.push(cfg.join("sven/config.yml"));
    }

    // 3. Workspace-local
    paths.push(PathBuf::from(".sven/config.yaml"));
    paths.push(PathBuf::from(".sven/config.yml"));
    paths.push(PathBuf::from(".sven.yaml"));
    paths.push(PathBuf::from(".sven.yml"));
    paths.push(PathBuf::from("sven.yaml"));
    paths.push(PathBuf::from("sven.yml"));

    paths
}

/// Load configuration by merging all discovered YAML files.
/// The `extra` argument may provide an explicit path (e.g. `--config` CLI flag).
pub fn load(extra: Option<&Path>) -> anyhow::Result<Config> {
    let mut merged = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());

    for path in config_search_paths() {
        if path.is_file() {
            debug!(path = %path.display(), "loading config layer");
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let layer: serde_yaml::Value = serde_yaml::from_str(&text)
                .with_context(|| format!("parsing {}", path.display()))?;
            merge_yaml(&mut merged, layer);
        }
    }

    if let Some(p) = extra {
        debug!(path = %p.display(), "loading explicit config");
        let text =
            std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
        let layer: serde_yaml::Value =
            serde_yaml::from_str(&text).with_context(|| format!("parsing {}", p.display()))?;
        merge_yaml(&mut merged, layer);
    }

    // Track whether the merged config contains an explicit model section before
    // deserialisation so we can skip auto-detection for users who have
    // configured their model explicitly.
    let has_model_config = merged.get("model").is_some();

    // Deserialize the merged YAML value into Config, falling back to defaults
    // when the merged value is empty (no config files found).
    let mut config: Config = if matches!(merged, serde_yaml::Value::Mapping(ref m) if m.is_empty())
    {
        Config::default()
    } else {
        serde_yaml::from_value(merged).unwrap_or_default()
    };

    // When no model has been explicitly configured, auto-select the best
    // available provider based on the API keys present in the environment.
    // Priority: Anthropic > OpenAI (both are excellent; Anthropic is ranked
    // first because claude-sonnet-4-6 is the recommended default).
    if !has_model_config {
        if std::env::var("ANTHROPIC_API_KEY").is_ok() {
            config.model.provider = "anthropic".into();
            config.model.name = "claude-sonnet-4-6".into();
        } else if std::env::var("OPENAI_API_KEY").is_ok() {
            config.model.provider = "openai".into();
            config.model.name = "gpt-5.2".into();
        }
        // If neither key is available the defaults remain, and from_config()
        // will produce a clear error when the provider is actually invoked.
    }

    Ok(config)
}

/// Deep-merge `src` into `dst`; src wins on scalar conflicts.
fn merge_yaml(dst: &mut serde_yaml::Value, src: serde_yaml::Value) {
    match (dst, src) {
        (serde_yaml::Value::Mapping(d), serde_yaml::Value::Mapping(s)) => {
            for (k, v) in s {
                let entry = d
                    .entry(k)
                    .or_insert(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
                merge_yaml(entry, v);
            }
        }
        (dst, src) => *dst = src,
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn val(s: &str) -> serde_yaml::Value {
        serde_yaml::from_str(s).unwrap()
    }

    #[test]
    fn merge_scalar_src_wins() {
        let mut dst = val("x: 1");
        let src = val("x: 2");
        merge_yaml(&mut dst, src);
        assert_eq!(dst["x"].as_i64(), Some(2));
    }

    #[test]
    fn merge_preserves_keys_not_in_src() {
        let mut dst = val("a: 1\nb: 2");
        let src = val("b: 99");
        merge_yaml(&mut dst, src);
        assert_eq!(dst["a"].as_i64(), Some(1));
        assert_eq!(dst["b"].as_i64(), Some(99));
    }

    #[test]
    fn merge_nested_tables() {
        let mut dst = val("model:\n  provider: openai\n  name: gpt-4o");
        let src = val("model:\n  name: gpt-4o-mini");
        merge_yaml(&mut dst, src);
        assert_eq!(dst["model"]["provider"].as_str(), Some("openai"));
        assert_eq!(dst["model"]["name"].as_str(), Some("gpt-4o-mini"));
    }

    #[test]
    fn load_returns_error_when_explicit_path_missing() {
        let result = load(Some(Path::new("/tmp/sven_nonexistent_config_xyz.yaml")));
        assert!(result.is_err());
    }

    #[test]
    fn load_with_no_extra_path_returns_valid_config() {
        // The provider may be auto-detected from env-vars (ANTHROPIC_API_KEY
        // or OPENAI_API_KEY), so we only assert that the result is a non-empty
        // provider string rather than a fixed value.
        let cfg = load(None).unwrap();
        assert!(!cfg.model.provider.is_empty());
        assert!(!cfg.model.name.is_empty());
    }

    #[test]
    fn load_explicit_file_overrides_defaults() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "model:\n  provider: anthropic\n  name: test-model").unwrap();
        let cfg = load(Some(f.path())).unwrap();
        assert_eq!(cfg.model.provider, "anthropic");
        assert_eq!(cfg.model.name, "test-model");
    }
}
