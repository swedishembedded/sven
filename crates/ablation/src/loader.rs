// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Reads the raw records of a benchmark output directory.

use std::path::{Path, PathBuf};

use crate::record::{RunRecord, RUN_SCHEMA};

/// The files `runs/<task>/<variant>/record.json` under `dir`, in path order.
fn record_paths(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut found = Vec::new();
    let runs = dir.join("runs");
    let read = |p: &Path| -> Result<Vec<PathBuf>, String> {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(p)
            .map_err(|e| format!("{}: {e}", p.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .collect();
        entries.sort();
        Ok(entries)
    };
    for task in read(&runs)? {
        for variant in read(&task)? {
            let record = variant.join("record.json");
            if record.exists() {
                found.push(record);
            }
        }
    }
    Ok(found)
}

/// Every run record under `dir`.
///
/// # Errors
///
/// A record that cannot be read or is of another schema. A bad record is an
/// error and is never skipped: a report over fewer runs than were made would
/// quietly change what it says.
pub fn load_runs(dir: &Path) -> Result<Vec<RunRecord>, String> {
    record_paths(dir)?
        .into_iter()
        .map(|path| {
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let record: RunRecord =
                serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            if record.schema != RUN_SCHEMA {
                return Err(format!(
                    "{}: run schema {:?} is not {RUN_SCHEMA:?}",
                    path.display(),
                    record.schema
                ));
            }
            Ok(record)
        })
        .collect()
}
