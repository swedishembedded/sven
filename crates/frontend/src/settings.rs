// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The configuration file as the `sven` program reads it: the sections a
//! session runs under ([`Config`]) and the interactive UI's own `tui:`
//! section ([`TuiConfig`]).
//!
//! The runtime sections are owned by the crates that read them and put
//! together by [`sven_bootstrap::Config`]; the `tui:` section is owned here,
//! because this is the crate the interactive UI is wired through. [`Settings`]
//! is the whole file - what [`Settings::load`] reads, reports unknown keys
//! against, and settles. A program that has no UI (an embedded agent, the
//! MCP server) reads only [`Config::load`].
//!
//! Swedish Embedded AB implements configuration for agent frontends for its
//! clients. If your team needs expertise in layered, validated configuration
//! for language-model agents then you can procure our services by sending an
//! email to info@swedishembedded.com.

use std::path::Path;

use serde::{Deserialize, Serialize};
use sven_bootstrap::Config;
use sven_config::{ConfigDocument, Schema};

/// The `tui:` section of the configuration file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TuiConfig {
    /// Colour theme: "dark" | "light" | "solarized"
    pub theme: String,
    /// Show line numbers in code blocks
    pub code_line_numbers: bool,
    /// Width used for markdown wrapping (0 = auto)
    pub wrap_width: u16,
    /// Use plain ASCII borders/indicators instead of Unicode box-drawing and
    /// Braille characters.  Enable this when the terminal font lacks wide
    /// Unicode support (the font renders replacement glyphs / "gibberish").
    /// Can also be forced with the SVEN_ASCII_BORDERS=1 environment variable.
    #[serde(default)]
    pub ascii_borders: bool,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            theme: "dark".into(),
            code_line_numbers: false,
            wrap_width: 0,
            ascii_borders: false,
        }
    }
}

impl TuiConfig {
    /// The keys of the `tui:` section.
    #[must_use]
    pub fn schema() -> Schema {
        Schema::keys(&["theme", "code_line_numbers", "wrap_width", "ascii_borders"])
    }
}

/// Every section of the configuration file: what a session runs under, and
/// how the interactive UI looks.
///
/// Serializes in the layout of the file: the runtime sections and `tui:` side
/// by side.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Settings {
    /// The sections a session is built from.
    #[serde(flatten)]
    pub runtime: Config,
    /// The interactive UI.
    pub tui: TuiConfig,
}

/// The one section of the file that is the UI's, as the document is read.
#[derive(Deserialize, Default)]
#[serde(default)]
struct TuiSection {
    tui: TuiConfig,
}

impl Settings {
    /// The keys of every section of the file.
    #[must_use]
    pub fn schema() -> Schema {
        Config::schema().with("tui", TuiConfig::schema())
    }

    /// Loads the configuration file the way [`Config::load`] does, with the
    /// `tui:` section read as well.
    ///
    /// # Errors
    ///
    /// As [`ConfigDocument::load`].
    pub fn load(extra: Option<&Path>) -> anyhow::Result<Self> {
        let document = ConfigDocument::load(extra)?;
        document.warn_unknown_keys(&Self::schema());
        Ok(Self::from_document(&document))
    }

    /// The settings `document` holds, with the model settled
    /// ([`Config::settle`]).
    ///
    /// A document that cannot be read as settings is ignored whole, runtime
    /// sections and `tui:` alike (see [`ConfigDocument::decode`]).
    #[must_use]
    pub fn from_document(document: &ConfigDocument) -> Self {
        let (mut runtime, TuiSection { tui }): (Config, TuiSection) = document.decode_pair();
        runtime.settle(document.contains("model"));
        Self { runtime, tui }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tui_section_is_read_beside_the_runtime_sections() {
        let document = ConfigDocument::from_yaml(
            "agent: {max_tool_rounds: 5}\ntui: {wrap_width: 100, ascii_borders: true}\n",
        )
        .unwrap();
        let settings = Settings::from_document(&document);
        assert_eq!(settings.runtime.agent.max_tool_rounds, 5);
        assert_eq!(settings.tui.wrap_width, 100);
        assert!(settings.tui.ascii_borders);
        assert_eq!(settings.tui.theme, "dark");
    }

    #[test]
    fn a_bad_tui_value_discards_the_whole_file_as_it_always_has() {
        let document =
            ConfigDocument::from_yaml("agent: {max_tool_rounds: 5}\ntui: {wrap_width: wide}\n")
                .unwrap();
        let settings = Settings::from_document(&document);
        assert_eq!(settings.runtime.agent.max_tool_rounds, 200);
    }

    #[test]
    fn the_tui_section_is_reported_against_its_own_keys() {
        let document = ConfigDocument::from_yaml("tui: {colour: red}\n").unwrap();
        assert_eq!(
            document.unknown_keys(&Settings::schema()),
            ["Unrecognised config field `tui.colour` - check spelling or update sven"]
        );
        assert!(document
            .unknown_keys(&Config::schema().with("tui", Schema::value()))
            .is_empty());
    }

    #[test]
    fn the_settings_serialize_in_the_layout_of_the_file() {
        let value = serde_json::to_value(Settings::default()).unwrap();
        for section in ["model", "agent", "tools", "providers", "mcp_servers", "tui"] {
            assert!(value.get(section).is_some(), "{section}");
        }
    }
}
