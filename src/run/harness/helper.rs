// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven harness bundle` and `sven harness helper run`: the authoring half of
//! helpers. A helper is built with whatever toolchain emits a module for the
//! `helper-run/1` ABI; these commands put it in a bundle and run it in the
//! same bounded runtime a harness uses, so a helper can be tried before a
//! harness depends on it.
//!
//! Swedish Embedded AB implements sandboxed, fuel-metered data helpers for
//! agent workflows for its clients. If your team needs expertise in running
//! untrusted transformation code safely then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::collections::BTreeMap;
use std::path::Path;

use base64::Engine as _;
use sven_harness_spec::{HarnessBundle, Helper};
use sven_harness_transform::{Limits, Transform, HELPER_ABI};

use super::{print_json, usage, Failure, Outcome};
use crate::cli::HelperCommands;

fn read(path: &Path) -> Result<Vec<u8>, Failure> {
    std::fs::read(path).map_err(|e| usage(format!("cannot read {}: {e}", path.display())))
}

/// Composes `document` and the `NAME=FILE` helpers into a bundle, checking
/// the whole as `sven harness validate` would.
pub(super) fn bundle(document: &Path, helpers: &[String], output: Option<&Path>) -> Outcome {
    let text = String::from_utf8(read(document)?)
        .map_err(|e| usage(format!("{} is not UTF-8: {e}", document.display())))?;
    let mut carried = BTreeMap::new();
    for spec in helpers {
        let (name, file) = spec
            .split_once('=')
            .filter(|(name, file)| !name.is_empty() && !file.is_empty())
            .ok_or_else(|| usage(format!("--helper wants NAME=FILE, got `{spec}`")))?;
        let bytes = read(Path::new(file))?;
        Transform::compile(&bytes, None, None)
            .map_err(|e| usage(format!("helper `{name}` ({file}) is not usable: {e}")))?;
        let content = base64::engine::general_purpose::STANDARD.encode(bytes);
        if carried
            .insert(
                name.to_string(),
                Helper {
                    abi: HELPER_ABI.to_string(),
                    language: "wasm".to_string(),
                    content,
                },
            )
            .is_some()
        {
            return Err(usage(format!("helper `{name}` is given twice")));
        }
    }
    let bundle = HarnessBundle {
        document: text,
        helpers: carried,
    };
    bundle
        .compile()
        .map_err(|e| usage(format!("the bundle does not compile: {e}")))?;
    let yaml = serde_yaml::to_string(&bundle).map_err(|e| Failure {
        code: 5,
        message: format!("cannot serialize the bundle: {e}"),
    })?;
    match output {
        Some(path) => std::fs::write(path, yaml)
            .map_err(|e| usage(format!("cannot write {}: {e}", path.display())))?,
        None => print!("{yaml}"),
    }
    Ok(0)
}

pub(super) fn run_helper(command: &HelperCommands) -> Outcome {
    let HelperCommands::Run {
        path,
        helper,
        input,
        fuel,
        max_pages,
    } = command;
    let bytes = read(path)?;
    let wasm = match helper {
        None => bytes,
        Some(name) => {
            let bundle: HarnessBundle = serde_yaml::from_slice(&bytes)
                .map_err(|e| usage(format!("{} is not a bundle: {e}", path.display())))?;
            let found = bundle
                .helpers
                .get(name)
                .ok_or_else(|| usage(format!("the bundle has no helper `{name}`")))?;
            base64::engine::general_purpose::STANDARD
                .decode(&found.content)
                .map_err(|e| usage(format!("helper `{name}` is not base64: {e}")))?
        }
    };
    let input: serde_json::Value =
        serde_json::from_str(input).map_err(|e| usage(format!("--input is not JSON: {e}")))?;
    let transform = Transform::compile(&wasm, None, None)
        .map_err(|e| usage(format!("the module is not usable: {e}")))?;
    let limits = Limits {
        fuel: *fuel,
        max_pages: *max_pages,
    };
    match transform.run(&input, &limits) {
        Ok(output) => {
            print_json(&output)?;
            Ok(0)
        }
        Err(e) => {
            eprintln!("helper failed: {e}");
            Ok(1)
        }
    }
}
