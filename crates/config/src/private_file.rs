// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping credentials on disk
// safe from other users and from half-written files for its clients. If your
// team needs expertise in secure local secret storage then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Writing a file that holds a secret.

use std::path::Path;

use anyhow::Context;

/// Replace `file` with `body`, readable by its owner only, atomically: a
/// reader sees the old file or the new one, never part of either. For any
/// file that holds a secret (a sign-in, an API key, an OAuth token).
///
/// # Errors
///
/// The directory cannot be made, or the file written or renamed into place.
pub fn write_private(file: &Path, body: &str) -> anyhow::Result<()> {
    let dir = file
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no directory", file.display()))?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("secret");
    let aside = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut f = options
            .open(&aside)
            .with_context(|| format!("writing {}", aside.display()))?;
        // The mode above applies only when the file is created; an aside left
        // by a crashed writer keeps whatever mode it had.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&aside, file).with_context(|| format!("writing {}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_file_is_replaced_whole_and_its_owners_alone() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("nested/secret.json");
        write_private(&file, "one").unwrap();
        write_private(&file, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "two");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let leftovers: Vec<_> = std::fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers.len(), 1, "no aside file is left behind");
    }
}
