// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Harness bundles: a workflow plus the workflows it imports, one digest.
//!
//! A workflow that imports others is more than its own document: a changed
//! import is a changed program. The bundle is the unit of pinning: a run pins
//! the bundle digest, not just the document's, and a bundle whose digest no
//! longer matches a checkpoint's is rejected on resume (ADR 0004).

use serde::{Deserialize, Serialize};

use crate::error::HarnessError;
use crate::plan::HarnessPlan;

/// A harness document plus every workflow it imports.
///
/// Serialized canonically (keys sorted, no whitespace) for its digest, so
/// the digest is invariant to YAML key order exactly as a lone document's
/// is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HarnessBundle {
    /// The main harness document, raw YAML.
    pub document: String,
    /// The workflows the main document imports, transitively, by their path
    /// relative to the package root. Their contents are pinned by the
    /// bundle's digest; a file nothing imports is a bundle error.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub imports: std::collections::BTreeMap<String, String>,
}

impl HarnessBundle {
    /// Parses the main document and compiles it against the helper set.
    ///
    /// # Errors
    ///
    /// Everything [`crate::HarnessDocument::compile`] can produce, plus the
    /// package's own errors (an import that is missing or that nothing uses).
    pub fn compile(&self) -> Result<CompiledBundle, HarnessError> {
        let package = crate::package::compile_package(&self.document, &self.imports)?;
        Ok(CompiledBundle {
            plan: package.plan,
            imports: package.imports.into_iter().map(|(_, plan)| plan).collect(),
            digest: self.digest(),
        })
    }

    /// The bundle's content digest: the canonical-JSON digest over the
    /// document's own canonical digest and its imports' digests.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Parse`] when the document does not parse (the digest
    /// needs its canonical form).
    pub fn digest(&self) -> String {
        let package = crate::package::compile_package(&self.document, &self.imports).ok();
        let plan_digest = package
            .as_ref()
            .map(|p| p.plan.digest.clone())
            .unwrap_or_default();
        let mut value = serde_json::json!({
            "format": BUNDLE_FORMAT_VERSION,
            "plan": plan_digest,
        });
        if !self.imports.is_empty() {
            // Each imported file by the digest of its compiled plan, so that
            // what it does is pinned and how it is formatted is not. A
            // package that does not compile has no digest to pin to.
            let pinned: std::collections::BTreeMap<&str, &str> = package
                .iter()
                .flat_map(|p| p.imports.iter())
                .map(|(path, plan)| (path.as_str(), plan.digest.as_str()))
                .collect();
            value["imports"] = serde_json::json!(pinned);
        }
        let mut hasher = sha2::Sha256::new();
        use sha2::Digest;
        hasher.update(serde_json::to_string(&value).unwrap_or_default());
        format!("{:x}", hasher.finalize())
    }
}

/// The bundle format version, stamped into the digest input.
pub const BUNDLE_FORMAT_VERSION: &str = "harness-bundle/1";

/// A bundle whose main document has been compiled: the plan plus the
/// bundle's pinned digest.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledBundle {
    /// The compiled, validated plan.
    pub plan: HarnessPlan,
    /// The plans of the imported workflows, each after those it imports
    /// itself: what a `use` task delegates to.
    pub imports: Vec<HarnessPlan>,
    /// The bundle digest to pin a run to.
    pub digest: String,
}
