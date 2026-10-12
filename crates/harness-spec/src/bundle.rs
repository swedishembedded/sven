// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Harness bundles: a harness document plus its helpers, one digest.
//!
//! A harness that uses `transform` references helper programs by name. The
//! helper bytes are part of the program's meaning — a changed helper is a
//! changed program — so the bundle is the unit of pinning: a run pins the
//! bundle digest, not just the document's, and a bundle whose digest no
//! longer matches a checkpoint's is rejected on resume (ADR 0004).

use serde::{Deserialize, Serialize};

use crate::error::HarnessError;
use crate::plan::HarnessPlan;

/// A harness document plus every helper it references.
///
/// Serialized canonically (keys sorted, no whitespace) for its digest, so
/// the digest is invariant to YAML key order exactly as a lone document's
/// is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarnessBundle {
    /// The main harness document, raw YAML.
    pub document: String,
    /// Helper programs by name. `transform` steps name them; a helper that
    /// no step references is a bundle error, and so is a reference without
    /// a helper.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub helpers: std::collections::BTreeMap<String, Helper>,
}

/// A helper program: the sandboxed bytes a `transform` step runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Helper {
    /// The helper ABI version the bytes were written against. The runtime
    /// adapter refuses a version it does not implement (ADR 0004: versioned
    /// ABI, no fs/network, fuel- and memory-capped).
    pub abi: String,
    /// The helper's language (e.g. `wasm`).
    pub language: String,
    /// The helper's bytes (a WASM module's bytes, base64 in YAML).
    pub content: String,
}

impl HarnessBundle {
    /// Parses the main document and compiles it against the helper set.
    ///
    /// # Errors
    ///
    /// Everything [`crate::HarnessDocument::compile`] can produce, plus bundle
    /// errors: a `transform` step naming a helper the bundle does not carry,
    /// or a helper no `transform` step references.
    pub fn compile(&self) -> Result<CompiledBundle, HarnessError> {
        let document = crate::HarnessDocument::parse(&self.document)?;
        let spec = document.spec();
        let used: std::collections::BTreeSet<&str> = spec
            .steps
            .values()
            .filter_map(|step| match step {
                crate::StepSpec::Transform(t) => Some(t.helper.as_str()),
                _ => None,
            })
            .collect();
        for name in &used {
            if !self.helpers.contains_key(*name) {
                return Err(HarnessError::Invalid(format!(
                    "step helper `{name}` is not in the bundle's `helpers`"
                )));
            }
        }
        for name in self.helpers.keys() {
            if !used.contains(name.as_str()) {
                return Err(HarnessError::Invalid(format!(
                    "bundle helper `{name}` is not referenced by any `transform` step"
                )));
            }
        }
        let plan = document.compile()?;
        Ok(CompiledBundle {
            plan,
            digest: self.digest(),
        })
    }

    /// The bundle's content digest: the canonical-JSON digest over the
    /// document's own canonical digest and the helpers' canonical bytes.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Parse`] when the document does not parse (the digest
    /// needs its canonical form).
    pub fn digest(&self) -> String {
        let plan_digest = crate::HarnessDocument::parse(&self.document)
            .ok()
            .and_then(|d| d.compile().ok())
            .map(|p| p.digest)
            .unwrap_or_default();
        let helpers: std::collections::BTreeMap<&String, &Helper> = self.helpers.iter().collect();
        let value = serde_json::json!({
            "format": BUNDLE_FORMAT_VERSION,
            "plan": plan_digest,
            "helpers": helpers,
        });
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
    /// The bundle digest to pin a run to.
    pub digest: String,
}
