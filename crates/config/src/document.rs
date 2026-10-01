// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The configuration document: every layer found on disk, environment
//! placeholders expanded, merged into one YAML mapping.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::de::DeserializeOwned;
use tracing::{debug, warn};

use crate::Schema;

/// Ordered list of config file locations searched from lowest to highest priority.
/// Later files override earlier ones.
fn config_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // 1. System-wide default.  /etc/ is a Linux convention; macOS and Windows
    //    use dirs::config_dir() for system-wide configuration instead.
    #[cfg(target_os = "linux")]
    {
        paths.push(PathBuf::from("/etc/sven/config.yaml"));
        paths.push(PathBuf::from("/etc/sven/config.yml"));
    }

    // 2. XDG / home.  dirs::config_dir() returns:
    //    Linux:   $XDG_CONFIG_HOME or ~/.config
    //    macOS:   ~/Library/Application Support
    //    Windows: %APPDATA% (C:\Users\<user>\AppData\Roaming)
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

/// Every configuration layer sven found, merged: later layers override
/// earlier ones key by key, and `${VAR}` placeholders are already expanded.
///
/// Holds the document as written, not yet read into any section's type, so
/// each section's owner reads its part ([`Self::decode`]) and the program
/// reports what none of them recognises ([`Self::unknown_keys`]).
#[derive(Clone, Debug)]
pub struct ConfigDocument {
    root: serde_yaml::Value,
}

impl Default for ConfigDocument {
    fn default() -> Self {
        Self {
            root: serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        }
    }
}

impl ConfigDocument {
    /// Merges every configuration file found - system, user, then project -
    /// and then `extra` (the `--config` flag), which overrides them all.
    ///
    /// # Errors
    ///
    /// A file that exists but cannot be read or is not YAML, and an `extra`
    /// path that cannot be read.
    pub fn load(extra: Option<&Path>) -> anyhow::Result<Self> {
        let mut document = Self::default();
        for path in config_search_paths() {
            if path.is_file() {
                debug!(path = %path.display(), "loading config layer");
                document.merge_file(&path)?;
            }
        }
        if let Some(path) = extra {
            debug!(path = %path.display(), "loading explicit config");
            document.merge_file(path)?;
        }
        Ok(document)
    }

    /// A document of `text` alone, its placeholders expanded.
    ///
    /// # Errors
    ///
    /// `text` is not YAML.
    pub fn from_yaml(text: &str) -> anyhow::Result<Self> {
        let mut document = Self::default();
        document.merge_text(text, "inline configuration")?;
        Ok(document)
    }

    fn merge_file(&mut self, path: &Path) -> anyhow::Result<()> {
        let raw =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        self.merge_text(&raw, &path.display().to_string())
    }

    fn merge_text(&mut self, raw: &str, source: &str) -> anyhow::Result<()> {
        let text = expand_env_vars(raw, source);
        let layer: serde_yaml::Value =
            serde_yaml::from_str(&text).with_context(|| format!("parsing {source}"))?;
        merge_yaml(&mut self.root, layer);
        Ok(())
    }

    /// Whether the document sets `key` at its top level.
    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.root.get(key).is_some()
    }

    /// One message for every key `schema` does not recognise, and one for
    /// every section present that it accepts but ignores.
    #[must_use]
    pub fn unknown_keys(&self, schema: &Schema) -> Vec<String> {
        let mut warnings = Vec::new();
        schema.report(&self.root, "", &mut warnings);
        warnings
    }

    /// Logs a warning for each of [`Self::unknown_keys`]: a key sven does not
    /// read is otherwise dropped in silence when the document is decoded.
    pub fn warn_unknown_keys(&self, schema: &Schema) {
        for warning in self.unknown_keys(schema) {
            warn!("{warning}");
        }
    }

    /// The document read as `T`: its defaults when nothing is configured, and
    /// also - said loudly - when the document cannot be read as `T`.
    ///
    /// Falling back is deliberate: one bad value must not stop sven starting.
    /// It is not done in silence, though; a configuration the schema cannot
    /// decode is discarded whole, and without the warning nothing would tell
    /// the user that the file they just edited is being ignored.
    #[must_use]
    pub fn decode<T: DeserializeOwned + Default>(&self) -> T {
        self.read().unwrap_or_else(|e| {
            warn_ignored(&e);
            T::default()
        })
    }

    /// The document read as two types that each read their own sections, `A`
    /// and `B`. Defaults as [`Self::decode`] does, and for both at once: a
    /// document one of them cannot read is ignored in full, not in part.
    #[must_use]
    pub fn decode_pair<A, B>(&self) -> (A, B)
    where
        A: DeserializeOwned + Default,
        B: DeserializeOwned + Default,
    {
        self.read::<A>()
            .and_then(|a| Ok((a, self.read::<B>()?)))
            .unwrap_or_else(|e| {
                warn_ignored(&e);
                (A::default(), B::default())
            })
    }

    fn read<T: DeserializeOwned + Default>(&self) -> Result<T, serde_yaml::Error> {
        if matches!(&self.root, serde_yaml::Value::Mapping(m) if m.is_empty()) {
            return Ok(T::default());
        }
        serde_yaml::from_value(self.root.clone())
    }
}

fn warn_ignored(error: &serde_yaml::Error) {
    warn!(
        %error,
        "config could not be decoded and is being IGNORED IN FULL; using defaults"
    );
}

// ── Environment variable expansion ───────────────────────────────────────────

/// Expand `${VAR}` and `${VAR:-default}` placeholders in config file text.
///
/// Uses [`shellexpand`] so the full bash-style variable syntax is supported:
///
/// | Syntax              | Behaviour                                              |
/// |---------------------|--------------------------------------------------------|
/// | `${VAR}`            | Replaced with `$VAR`; empty string + WARN if not set  |
/// | `${VAR:-default}`   | Replaced with `$VAR`; falls back to `default` silently |
/// | `$$`                | Literal `$` (escape sequence)                          |
///
/// `source_desc` is a human-readable label used in warning messages (typically
/// the config file path).
fn expand_env_vars(text: &str, source_desc: &str) -> String {
    // First pass: expand all set variables and handle `${VAR:-default}` for
    // unset ones.  Unset variables *without* a default remain as `${VAR}`.
    let first: Cow<str> =
        shellexpand::env_with_context_no_errors(text, |name| -> Option<Cow<str>> {
            std::env::var(name).ok().map(Cow::Owned)
        });

    // Second pass: any `${VAR}` placeholders that survived the first pass are
    // unset variables with no default.  Warn and substitute an empty string so
    // the YAML remains valid.
    let second: Cow<str> =
        shellexpand::env_with_context_no_errors(&*first, |name| -> Option<Cow<str>> {
            warn!(
                var = name,
                source = source_desc,
                "config env var is not set; substituting empty string"
            );
            Some(Cow::Borrowed(""))
        });

    second.into_owned()
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
    use std::io::Write;

    use super::*;

    fn val(s: &str) -> serde_yaml::Value {
        serde_yaml::from_str(s).unwrap()
    }

    fn file(text: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{text}").unwrap();
        f
    }

    #[derive(Debug, Default, PartialEq, serde::Deserialize)]
    #[serde(default)]
    struct Sample {
        model: SampleModel,
        rounds: u32,
    }

    #[derive(Debug, Default, PartialEq, serde::Deserialize)]
    #[serde(default)]
    struct SampleModel {
        provider: String,
        name: String,
    }

    #[test]
    fn merge_scalar_src_wins() {
        let mut dst = val("x: 1");
        merge_yaml(&mut dst, val("x: 2"));
        assert_eq!(dst["x"].as_i64(), Some(2));
    }

    #[test]
    fn merge_preserves_keys_not_in_src() {
        let mut dst = val("a: 1\nb: 2");
        merge_yaml(&mut dst, val("b: 99"));
        assert_eq!(dst["a"].as_i64(), Some(1));
        assert_eq!(dst["b"].as_i64(), Some(99));
    }

    #[test]
    fn merge_nested_tables() {
        let mut dst = val("model:\n  provider: openai\n  name: gpt-4o");
        merge_yaml(&mut dst, val("model:\n  name: gpt-4o-mini"));
        assert_eq!(dst["model"]["provider"].as_str(), Some("openai"));
        assert_eq!(dst["model"]["name"].as_str(), Some("gpt-4o-mini"));
    }

    #[test]
    fn an_explicit_path_that_cannot_be_read_is_an_error() {
        assert!(ConfigDocument::load(Some(Path::new("sven_nonexistent_config_xyz.yaml"))).is_err());
    }

    #[test]
    fn the_explicit_file_overrides_what_it_names() {
        let f = file("model:\n  provider: anthropic\n  name: test-model\n");
        let doc = ConfigDocument::load(Some(f.path())).unwrap();
        let sample: Sample = doc.decode();
        assert_eq!(sample.model.provider, "anthropic");
        assert_eq!(sample.model.name, "test-model");
        assert!(doc.contains("model"));
        assert!(!doc.contains("rounds"));
    }

    #[test]
    fn an_empty_file_or_a_bare_separator_decodes_to_the_defaults() {
        for text in ["", "---\n"] {
            let doc = ConfigDocument::load(Some(file(text).path())).unwrap();
            assert_eq!(doc.decode::<Sample>(), Sample::default(), "{text:?}");
        }
    }

    #[test]
    fn an_undecodable_document_decodes_to_the_defaults() {
        let doc = ConfigDocument::from_yaml("model:\n  - foo\n  - bar\nrounds: 7\n").unwrap();
        assert_eq!(doc.decode::<Sample>(), Sample::default());
    }

    #[test]
    fn two_readers_of_one_document_default_together() {
        #[derive(Debug, Default, PartialEq, serde::Deserialize)]
        #[serde(default)]
        struct Other {
            other: u32,
        }
        let doc = ConfigDocument::from_yaml("rounds: 7\nother: 3\n").unwrap();
        let (sample, other): (Sample, Other) = doc.decode_pair();
        assert_eq!((sample.rounds, other.other), (7, 3));

        let doc = ConfigDocument::from_yaml("rounds: 7\nother: three\n").unwrap();
        let (sample, other): (Sample, Other) = doc.decode_pair();
        assert_eq!((sample, other), (Sample::default(), Other::default()));
    }

    #[test]
    fn deeply_nested_yaml_does_not_overflow_the_stack() {
        let nested: String = "a:\n".to_string() + &"  ".repeat(200).repeat(200);
        let _ = ConfigDocument::load(Some(file(&nested).path()));
    }

    #[test]
    fn unknown_keys_are_reported_against_the_schema() {
        let doc = ConfigDocument::from_yaml("model: {provider: x}\nrounds: 1\nextra: 2\n").unwrap();
        let schema = Schema::fields([
            ("model", Schema::keys(&["provider", "name"])),
            ("rounds", Schema::value()),
        ]);
        assert_eq!(
            doc.unknown_keys(&schema),
            ["Unrecognised config field `.extra` - check spelling or update sven"]
        );
    }

    // ── expand_env_vars ───────────────────────────────────────────────────────

    #[test]
    fn expand_env_vars_substitutes_set_variable() {
        std::env::set_var("SVEN_TEST_EXPAND_VAR", "hello");
        let result = expand_env_vars("key: ${SVEN_TEST_EXPAND_VAR}", "test");
        assert_eq!(result, "key: hello");
        std::env::remove_var("SVEN_TEST_EXPAND_VAR");
    }

    #[test]
    fn expand_env_vars_uses_default_for_unset_variable() {
        std::env::remove_var("SVEN_TEST_MISSING_VAR");
        let result = expand_env_vars("key: ${SVEN_TEST_MISSING_VAR:-fallback}", "test");
        assert_eq!(result, "key: fallback");
    }

    #[test]
    fn expand_env_vars_replaces_unset_required_var_with_empty() {
        std::env::remove_var("SVEN_TEST_REQUIRED_VAR");
        let result = expand_env_vars("key: ${SVEN_TEST_REQUIRED_VAR}", "test");
        assert_eq!(result, "key: ");
    }

    #[test]
    fn expand_env_vars_leaves_plain_text_unchanged() {
        let text = "model:\n  provider: openai\n  name: gpt-4o\n";
        assert_eq!(expand_env_vars(text, "test"), text);
    }

    #[test]
    fn load_expands_env_vars_in_config_file() {
        std::env::set_var("SVEN_TEST_PROVIDER", "anthropic");
        std::env::set_var("SVEN_TEST_MODEL", "claude-opus-4-5");
        let f = file("model:\n  provider: ${SVEN_TEST_PROVIDER}\n  name: ${SVEN_TEST_MODEL}\n");
        let sample: Sample = ConfigDocument::load(Some(f.path())).unwrap().decode();
        assert_eq!(sample.model.provider, "anthropic");
        assert_eq!(sample.model.name, "claude-opus-4-5");
        std::env::remove_var("SVEN_TEST_PROVIDER");
        std::env::remove_var("SVEN_TEST_MODEL");
    }

    #[test]
    fn adversarial_env_expansion_of_undefined_var_produces_empty_string() {
        std::env::remove_var("SVEN_ADV_TOTALLY_NONEXISTENT_XYZZY");
        let result = expand_env_vars("key: ${SVEN_ADV_TOTALLY_NONEXISTENT_XYZZY}", "test");
        assert_eq!(result, "key: ");
    }

    #[test]
    fn adversarial_env_expansion_value_containing_var_ref_does_not_produce_deep_value() {
        // OUTER is set to the literal text of a reference to INNER; expansion
        // must not follow it.
        std::env::set_var("SVEN_ADV_OUTER_VAR2", "${SVEN_ADV_INNER_VAR2}");
        std::env::set_var("SVEN_ADV_INNER_VAR2", "inner_value");
        let result = expand_env_vars("key: ${SVEN_ADV_OUTER_VAR2}", "test");
        assert!(
            !result.contains("inner_value"),
            "deep expansion must not occur; result was: {result:?}"
        );
        std::env::remove_var("SVEN_ADV_OUTER_VAR2");
        std::env::remove_var("SVEN_ADV_INNER_VAR2");
    }
}
