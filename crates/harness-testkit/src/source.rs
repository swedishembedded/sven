// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Reading a harness file: a bare document or a bundle, compiled offline.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in auditing what
//! an agent workflow may do before it runs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::path::{Path, PathBuf};

use sven_harness_spec::{HarnessBundle, HarnessPlan};

use crate::error::TestkitError;

/// A harness file, read and compiled.
pub(crate) struct Source {
    pub path: PathBuf,
    /// The file's text, as the engine loads it.
    pub text: String,
    pub plan: HarnessPlan,
    /// The bundle, when the file is one.
    pub bundle: Option<HarnessBundle>,
    /// The bundle's content digest, when the file is a bundle.
    pub bundle_digest: Option<String>,
}

impl Source {
    /// Reads and compiles `path`. A top-level `document` key makes it a
    /// bundle, the same test the SDK applies.
    pub(crate) fn read(path: &Path) -> Result<Self, TestkitError> {
        let text = std::fs::read_to_string(path).map_err(|source| TestkitError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let harness =
            |message: String| TestkitError::Harness(format!("{}: {message}", path.display()));
        let value: serde_yaml::Value =
            serde_yaml::from_str(&text).map_err(|e| harness(format!("not valid YAML: {e}")))?;
        let is_bundle = value
            .as_mapping()
            .is_some_and(|m| m.contains_key("document"));
        if is_bundle {
            let bundle: HarnessBundle = serde_yaml::from_value(value)
                .map_err(|e| harness(format!("not a valid bundle: {e}")))?;
            let compiled = bundle.compile().map_err(|e| harness(e.to_string()))?;
            Ok(Self {
                path: path.to_path_buf(),
                text,
                plan: compiled.plan,
                bundle_digest: Some(compiled.digest),
                bundle: Some(bundle),
            })
        } else {
            let plan = sven_harness_spec::compile_str(&text).map_err(|e| harness(e.to_string()))?;
            Ok(Self {
                path: path.to_path_buf(),
                text,
                plan,
                bundle: None,
                bundle_digest: None,
            })
        }
    }
}
