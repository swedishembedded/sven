// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Files of the workspace read and written only through the session's
//! [`PathScope`], never by a path handed to a library.
//!
//! Where commands are confined (a served run) a confined process can swap a
//! directory for a link while a tool works; a path opened later may then
//! lead out of the workspace. The scope opens beneath the workspace without
//! following such a link (`PathScope::race_free`), so the document tools
//! read their source and write their results through it, and give the
//! converter private copies in a directory of their own.

use std::io::{Read, Write};

use sven_tool_api::{OpenError, OpenMode, PathScope};

/// The largest document read.
pub(crate) const MAX_DOCUMENT_BYTES: u64 = 512 * 1024 * 1024;

/// The bytes of `requested`, read through `scope`; `Ok(None)` when it does
/// not exist and `missing_ok`. An error is why, as the model reads it.
pub(crate) async fn read(
    scope: &PathScope,
    requested: &str,
    missing_ok: bool,
) -> Result<Option<Vec<u8>>, String> {
    let (scope, requested_owned) = (scope.clone(), requested.to_owned());
    let opened = tokio::task::spawn_blocking(move || -> Result<Option<Vec<u8>>, OpenError> {
        let file = match scope.open(&requested_owned, OpenMode::Read) {
            Ok(file) => file,
            Err(OpenError::Io(e)) if missing_ok && e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None)
            }
            Err(e) => return Err(e),
        };
        let mut bytes = Vec::new();
        file.take(MAX_DOCUMENT_BYTES + 1).read_to_end(&mut bytes)?;
        Ok(Some(bytes))
    })
    .await;
    match opened {
        Ok(Ok(Some(bytes))) if bytes.len() as u64 > MAX_DOCUMENT_BYTES => Err(format!(
            "{requested} is larger than {MAX_DOCUMENT_BYTES} bytes"
        )),
        Ok(Ok(found)) => Ok(found),
        Ok(Err(e)) => Err(format!("cannot read {requested}: {}", why(e))),
        Err(e) => Err(format!("reading {requested} failed: {e}")),
    }
}

/// Write `bytes` to `requested` through `scope`, creating its directories.
pub(crate) async fn write(
    scope: &PathScope,
    requested: &str,
    bytes: Vec<u8>,
) -> Result<(), String> {
    let (scope, requested_owned) = (scope.clone(), requested.to_owned());
    let written = tokio::task::spawn_blocking(move || -> Result<(), OpenError> {
        let mut file = scope.open(&requested_owned, OpenMode::Truncate)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        Ok(())
    })
    .await;
    match written {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(format!("cannot write {requested}: {}", why(e))),
        Err(e) => Err(format!("writing {requested} failed: {e}")),
    }
}

/// Why an open failed, as the model reads it.
fn why(error: OpenError) -> String {
    match error {
        OpenError::Refused(refused) => refused.to_string(),
        OpenError::Io(e) => e.to_string(),
    }
}
