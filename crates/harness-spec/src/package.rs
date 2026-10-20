// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Packages: a main workflow and the workflows it imports, compiled and
//! pinned together.
//!
//! An import is a path relative to the importing document's directory. The
//! rules are the same wherever a package comes from, a directory on disk
//! ([`load_package`]) or a bundle that carries the files ([`compile_package`]):
//! paths are relative and stay inside the package (no absolute path, no URL,
//! no `..` out of the root, no symlink out of it when read from disk), an
//! import cycle is an error naming the chain, two documents never share a
//! name, every file carried must be reachable, and the size, depth and number
//! of imports are bounded. Each imported document is itself a version 2
//! workflow; a `use` task sees only its interface (inputs, outputs and
//! capabilities), which is checked when the importer compiles.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in composing long-running agent
//! work from reviewed, reusable parts then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use crate::bundle::HarnessBundle;
use crate::error::HarnessError;
use crate::plan::HarnessPlan;
use crate::spec::HarnessSpec;
use crate::v2::spec::Imported;
use crate::HarnessDocument;

/// The most documents a package may carry besides its main one.
pub const MAX_IMPORTS: usize = 64;
/// The deepest chain of imports.
pub const MAX_IMPORT_DEPTH: usize = 8;
/// The largest imported document.
pub const MAX_IMPORT_BYTES: u64 = 1024 * 1024;
/// The main document's file name in a package directory.
pub const MAIN_DOCUMENT: &str = "harness.yaml";

fn invalid(message: String) -> HarnessError {
    HarnessError::Invalid(message)
}

/// The package-relative path an import `rel` of the document in directory
/// `dir` ("" for the package root) names.
///
/// # Errors
///
/// An absolute path, a URL, an empty or non-YAML path, or one that climbs out
/// of the package.
pub fn resolve_import(dir: &str, rel: &str) -> Result<String, HarnessError> {
    if rel.contains("://") || rel.starts_with('~') {
        return Err(invalid(format!(
            "import `{rel}`: imports are files of the package; remote imports are not supported"
        )));
    }
    let path = Path::new(rel);
    if rel.is_empty() || path.is_absolute() {
        return Err(invalid(format!(
            "import `{rel}` must be a relative path inside the package"
        )));
    }
    let mut parts: Vec<&str> = if dir.is_empty() {
        Vec::new()
    } else {
        dir.split('/').collect()
    };
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(
                part.to_str()
                    .ok_or_else(|| invalid(format!("import `{rel}` is not valid UTF-8")))?,
            ),
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.pop().is_none() {
                    return Err(invalid(format!("import `{rel}` climbs out of the package")));
                }
            }
            _ => {
                return Err(invalid(format!(
                    "import `{rel}` must be a relative path inside the package"
                )))
            }
        }
    }
    let resolved = parts.join("/");
    if !(resolved.ends_with(".yaml") || resolved.ends_with(".yml")) {
        return Err(invalid(format!(
            "import `{rel}` must be a .yaml or .yml document"
        )));
    }
    Ok(resolved)
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// What compiling a package produced.
#[derive(Debug, Clone)]
pub struct CompiledPackage {
    /// The main document's lowered specification.
    pub spec: HarnessSpec,
    /// The main document's plan.
    pub plan: HarnessPlan,
    /// Every imported document's plan with its package path, each after the
    /// documents it imports itself.
    pub imports: Vec<(String, HarnessPlan)>,
}

struct Walk<'a> {
    files: &'a BTreeMap<String, String>,
    done: BTreeMap<String, Imported>,
    plans: Vec<(String, HarnessPlan)>,
    stack: Vec<String>,
    names: BTreeMap<String, String>,
}

impl Walk<'_> {
    /// Compiles the document `text` at `path` (`None` for the main one) once
    /// its imports are resolved.
    fn compile(
        &mut self,
        path: Option<&str>,
        text: &str,
    ) -> Result<(HarnessSpec, HarnessPlan), HarnessError> {
        let dir = path.map_or("", dir_of);
        let mut resolved = BTreeMap::new();
        for (alias, rel) in HarnessDocument::imports_of(text)? {
            let target = resolve_import(dir, &rel)?;
            resolved.insert(alias, self.import(&target)?);
        }
        let document = HarnessDocument::parse_in(text, resolved)?;
        let plan = document.compile()?;
        let place = path.unwrap_or("the main document").to_string();
        if let Some(other) = self.names.insert(plan.name.clone(), place.clone()) {
            return Err(invalid(format!(
                "two documents of the package are named `{}` ({other} and {place}); names are \
                 what `delegate` resolves, so they must be unique",
                plan.name
            )));
        }
        Ok((document.spec().clone(), plan))
    }

    fn import(&mut self, path: &str) -> Result<Imported, HarnessError> {
        if let Some(known) = self.done.get(path) {
            return Ok(known.clone());
        }
        if let Some(at) = self.stack.iter().position(|p| p == path) {
            let chain: Vec<&str> = self.stack[at..]
                .iter()
                .map(String::as_str)
                .chain([path])
                .collect();
            return Err(invalid(format!("circular import: {}", chain.join(" -> "))));
        }
        if self.stack.len() >= MAX_IMPORT_DEPTH {
            return Err(invalid(format!(
                "imports nest deeper than {MAX_IMPORT_DEPTH} levels at {path}"
            )));
        }
        let text = self.files.get(path).ok_or_else(|| {
            invalid(format!(
                "import {path} is not part of the package (a bundle carries every imported file)"
            ))
        })?;
        self.stack.push(path.to_string());
        let (spec, plan) = self.compile(Some(path), text)?;
        self.stack.pop();
        if spec.api_version != crate::WORKFLOW_API_VERSION {
            return Err(invalid(format!(
                "import {path} is a version {} document; only version 2 workflows can be imported",
                spec.api_version
            )));
        }
        let interface = Imported {
            name: spec.name.clone(),
            inputs: spec.inputs.clone().unwrap_or_default(),
            outputs: spec.outputs.clone().unwrap_or_default(),
            capabilities: spec
                .capabilities
                .iter()
                .flatten()
                .map(|c| c.as_str().to_string())
                .collect(),
        };
        self.done.insert(path.to_string(), interface.clone());
        self.plans.push((path.to_string(), plan));
        Ok(interface)
    }
}

/// Compiles `main` against the files it may import.
///
/// # Errors
///
/// Everything compiling a document can produce, and the package rules: a
/// missing or unreachable file, a cycle, a duplicate name, a version 1 import,
/// or a limit exceeded.
pub fn compile_package(
    main: &str,
    files: &BTreeMap<String, String>,
) -> Result<CompiledPackage, HarnessError> {
    if files.len() > MAX_IMPORTS {
        return Err(invalid(format!(
            "a package carries at most {MAX_IMPORTS} imported documents, this one {}",
            files.len()
        )));
    }
    let mut walk = Walk {
        files,
        done: BTreeMap::new(),
        plans: Vec::new(),
        stack: Vec::new(),
        names: BTreeMap::new(),
    };
    let (spec, plan) = walk.compile(None, main)?;
    let reached: BTreeSet<&str> = walk.plans.iter().map(|(p, _)| p.as_str()).collect();
    if let Some(stray) = files.keys().find(|p| !reached.contains(p.as_str())) {
        return Err(invalid(format!(
            "package file {stray} is not imported by any document"
        )));
    }
    Ok(CompiledPackage {
        spec,
        plan,
        imports: walk.plans,
    })
}

/// Reads the package in directory `dir`: `harness.yaml` and everything it
/// imports, transitively, as a bundle that pins them.
///
/// # Errors
///
/// An unreadable or oversized file, an import that leaves the directory
/// (through `..` or through a symlink), and the package rules of
/// [`compile_package`] (checked by compiling the result).
pub fn load_package(dir: &Path) -> Result<HarnessBundle, HarnessError> {
    load_package_file(&dir.join(MAIN_DOCUMENT))
}

/// Reads the package whose main document is the file `main`: the directory
/// holding it is the package root, and everything it imports, transitively, is
/// read from there.
///
/// # Errors
///
/// As [`load_package`].
pub fn load_package_file(main: &Path) -> Result<HarnessBundle, HarnessError> {
    let dir = main
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let io =
        |what: &Path, e: std::io::Error| invalid(format!("cannot read {}: {e}", what.display()));
    let root = dir.canonicalize().map_err(|e| io(dir, e))?;
    let read = |path: &Path| -> Result<String, HarnessError> {
        let real = path.canonicalize().map_err(|e| io(path, e))?;
        if !real.starts_with(&root) {
            return Err(invalid(format!(
                "{} leads outside the package directory",
                path.display()
            )));
        }
        let meta = std::fs::metadata(&real).map_err(|e| io(&real, e))?;
        if !meta.is_file() || meta.len() > MAX_IMPORT_BYTES {
            return Err(invalid(format!(
                "{} must be a file of at most {MAX_IMPORT_BYTES} bytes",
                real.display()
            )));
        }
        std::fs::read_to_string(&real).map_err(|e| io(&real, e))
    };
    let file_name = main
        .file_name()
        .ok_or_else(|| invalid(format!("{} is not a file", main.display())))?;
    let main = read(&root.join(file_name))?;
    let mut imports: BTreeMap<String, String> = BTreeMap::new();
    let mut pending: Vec<(String, String)> = vec![(String::new(), main.clone())];
    while let Some((path, text)) = pending.pop() {
        for rel in HarnessDocument::imports_of(&text)?.into_values() {
            let target = resolve_import(dir_of(&path), &rel)?;
            if imports.contains_key(&target) {
                continue;
            }
            if imports.len() >= MAX_IMPORTS {
                return Err(invalid(format!(
                    "a package carries at most {MAX_IMPORTS} imported documents"
                )));
            }
            let imported = read(&root.join(&target))?;
            imports.insert(target.clone(), imported.clone());
            pending.push((target, imported));
        }
    }
    Ok(HarnessBundle {
        document: main,
        helpers: BTreeMap::new(),
        imports,
    })
}
