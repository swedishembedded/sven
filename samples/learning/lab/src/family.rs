// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

//! L0 - the task contract, and the check that it says what it scores.
//!
//! A family declares a request and the predicates an episode is judged by. The
//! failure this module is built to catch is a predicate that traces to
//! neither the request nor documented environment behaviour: an undisclosed
//! requirement. It is quiet and expensive. The agent is marked wrong for not
//! doing something nobody asked of it, the trajectory is recorded as a
//! failure, and a model trained on the result learns to guess at requirements
//! rather than read them - which is the exact habit these tasks exist to train
//! out of it.
//!
//! So every predicate carries a justification naming where it comes from, and
//! a family whose predicates are not all justified does not load.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::PredicateSet;

/// A loaded, validated task family.
#[derive(Clone, Debug)]
pub struct Family {
    id: String,
    request: String,
    predicates: PredicateSet,
    live_choices: Vec<String>,
    tool_calls: u32,
    root: PathBuf,
}

/// Why a family did not load.
#[derive(Debug)]
pub enum FamilyError {
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    Contract(String),
}

impl std::fmt::Display for FamilyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FamilyError::Read { path, source } => write!(f, "{}: {source}", path.display()),
            FamilyError::Parse { path, source } => write!(f, "{}: {source}", path.display()),
            FamilyError::Contract(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for FamilyError {}

#[derive(Deserialize)]
struct Wire {
    id: String,
    request: String,
    variation: WireVariation,
    completion: WireCompletion,
    limits: WireLimits,
}

#[derive(Deserialize)]
struct WireVariation {
    live_deployment: Vec<String>,
}

#[derive(Deserialize)]
struct WireCompletion {
    predicates: Vec<String>,
}

#[derive(Deserialize)]
struct WireLimits {
    tool_calls: u32,
}

impl Family {
    /// Load `<root>/family.toml` and check it against itself.
    pub fn load(root: &Path) -> Result<Family, FamilyError> {
        let path = root.join("family.toml");
        let text = std::fs::read_to_string(&path).map_err(|source| FamilyError::Read {
            path: path.clone(),
            source,
        })?;
        let wire: Wire = toml::from_str(&text).map_err(|source| FamilyError::Parse {
            path: path.clone(),
            source,
        })?;

        if wire.request.trim().is_empty() {
            return Err(FamilyError::Contract(format!(
                "{}: the request is empty, so nothing the verifier checks could be traced to it",
                path.display()
            )));
        }
        if wire.variation.live_deployment.len() < 2 {
            return Err(FamilyError::Contract(format!(
                "{}: a paired-world family needs at least two choices of hidden state; with one, \
                 an agent that never asks scores the same as one that does",
                path.display()
            )));
        }

        let predicates = PredicateSet::new(wire.completion.predicates.clone())
            .map_err(|e| FamilyError::Contract(format!("{}: {e}", path.display())))?;

        Self::check_predicates_are_justified(&path, &text, predicates.names())?;

        Ok(Family {
            id: wire.id,
            request: wire.request.trim().to_string(),
            predicates,
            live_choices: wire.variation.live_deployment,
            tool_calls: wire.limits.tool_calls,
            root: root.to_path_buf(),
        })
    }

    /// Every predicate must be justified by a comment immediately above it in
    /// the manifest.
    ///
    /// Using the comment is deliberate. The alternative - matching predicate
    /// names against words in the request - accepts
    /// `active_deployment_validates` because the request happens to contain
    /// "deployment", which is pattern-matching rather than justification. A
    /// sentence a person had to write is weak evidence on its own, but it is
    /// evidence that somebody asked the question, and it is reviewable in the
    /// diff that introduces the predicate.
    fn check_predicates_are_justified(
        path: &Path,
        text: &str,
        predicates: &[String],
    ) -> Result<(), FamilyError> {
        let lines: Vec<&str> = text.lines().collect();
        let mut unjustified = BTreeSet::new();

        for predicate in predicates {
            let quoted = format!("\"{predicate}\"");
            let Some(index) = lines.iter().position(|l| l.contains(&quoted)) else {
                continue;
            };
            let justified = lines[..index]
                .iter()
                .rev()
                .take_while(|l| l.trim_start().starts_with('#'))
                .any(|l| l.trim_start().trim_start_matches('#').trim().len() > 10);
            if !justified {
                unjustified.insert(predicate.clone());
            }
        }

        if !unjustified.is_empty() {
            return Err(FamilyError::Contract(format!(
                "{}: {} predicate(s) have no comment saying where they come from: {}.\n\
                 Every predicate must trace to a phrase in the request or to behaviour the \
                 workspace documents. One that traces to neither is a requirement the agent was \
                 never given, and scoring it teaches the model to guess at requirements instead \
                 of reading them.",
                path.display(),
                unjustified.len(),
                unjustified.into_iter().collect::<Vec<_>>().join(", ")
            )));
        }
        Ok(())
    }

    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn request(&self) -> &str {
        &self.request
    }
    pub fn predicates(&self) -> &PredicateSet {
        &self.predicates
    }
    pub fn live_choices(&self) -> &[String] {
        &self.live_choices
    }
    pub fn tool_call_budget(&self) -> u32 {
        self.tool_calls
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, body: &str) -> PathBuf {
        std::fs::create_dir_all(dir).expect("dir");
        std::fs::write(dir.join("family.toml"), body).expect("write");
        dir.to_path_buf()
    }

    fn valid() -> String {
        r#"
id = "t"
request = "Do the thing."
[variation]
live_deployment = ["a", "b"]
[completion]
predicates = [
    # "Do the thing" - the thing must be done.
    "thing_done",
]
[limits]
tool_calls = 10
"#
        .to_string()
    }

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lab-family-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_justified_family_loads() {
        let dir = write(&tmp("ok"), &valid());
        let family = Family::load(&dir).expect("loads");
        assert_eq!(family.id(), "t");
        assert_eq!(family.predicates().names(), ["thing_done"]);
        assert_eq!(family.tool_call_budget(), 10);
    }

    #[test]
    fn a_predicate_with_no_justification_is_refused() {
        let body = valid().replace("    # \"Do the thing\" - the thing must be done.\n", "");
        let dir = write(&tmp("unjustified"), &body);
        let err = Family::load(&dir).expect_err("an unjustified predicate must not load");
        let text = err.to_string();
        assert!(text.contains("thing_done"), "{text}");
        assert!(
            text.contains("never given"),
            "the message must say why it matters: {text}"
        );
    }

    #[test]
    fn a_family_with_one_hidden_state_is_refused() {
        let body = valid().replace(r#"["a", "b"]"#, r#"["a"]"#);
        let dir = write(&tmp("single"), &body);
        let err = Family::load(&dir).expect_err("one choice is not a paired world");
        assert!(err.to_string().contains("never asks"), "{err}");
    }

    #[test]
    fn a_family_with_no_predicates_is_refused() {
        let body = valid().replace(
            "    # \"Do the thing\" - the thing must be done.\n    \"thing_done\",\n",
            "",
        );
        let dir = write(&tmp("nopreds"), &body);
        assert!(
            Family::load(&dir).is_err(),
            "doing nothing would satisfy it"
        );
    }

    #[test]
    fn the_real_config_discovery_family_loads_and_is_justified() {
        // The shipped family is held to its own rule.
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("learning dir")
            .join("tasks/config-discovery");
        let family = Family::load(&root).expect("the shipped family must satisfy its own contract");
        assert_eq!(family.id(), "config-discovery");
        assert_eq!(family.predicates().names().len(), 3);
        assert_eq!(family.live_choices(), ["staging", "production"]);
    }
}
