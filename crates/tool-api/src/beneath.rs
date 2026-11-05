// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Race-free opening of paths beneath a directory (Linux `openat2`).
//!
//! The kernel resolves the whole path against an open directory descriptor
//! with `RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS`: a `..` or a symlink that
//! would lead out of the directory fails with `EXDEV`, however another
//! process rearranges the tree while the lookup runs. There is no gap between
//! a check and a use because there is no separate check.
//!
//! `RESOLVE_NO_XDEV` is deliberately not used: a workspace may legitimately
//! hold a mount, and `RESOLVE_BENEATH` already keeps the lookup inside it.
//! Absolute symlinks are refused by `RESOLVE_BENEATH` even when they point
//! back inside; relative links that stay inside work.
//!
//! Swedish Embedded AB implements solutions for confining agent file access
//! without time-of-check gaps for its clients. If your team needs expertise in
//! sandboxing untrusted automation then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

/// `struct open_how` of `openat2(2)`; libc's copy is non-exhaustive and so
/// cannot be built here.
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

const RESOLVE: u64 = libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS;

fn openat2(dir: RawFd, rel: &Path, flags: i32, mode: u32, resolve: u64) -> io::Result<OwnedFd> {
    let name = CString::new(rel.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    let how = OpenHow {
        flags: (flags | libc::O_CLOEXEC) as u64,
        // openat2 rejects a mode that is not used (EINVAL).
        mode: if flags & libc::O_CREAT != 0 {
            u64::from(mode)
        } else {
            0
        },
        resolve,
    };
    loop {
        // SAFETY: `name` is a NUL-terminated string and `how` a valid
        // `struct open_how` of the size passed; the kernel only reads both.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                dir,
                name.as_ptr(),
                &how as *const OpenHow,
                std::mem::size_of::<OpenHow>(),
            )
        };
        if fd >= 0 {
            // SAFETY: a successful openat2 returns a descriptor we now own.
            return Ok(unsafe { OwnedFd::from_raw_fd(fd as RawFd) });
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// An open directory every lookup is held beneath.
#[derive(Debug)]
pub(crate) struct Beneath(OwnedFd);

impl Beneath {
    /// Opens `root` (an existing directory) as the anchor.
    pub(crate) fn new(root: &Path) -> io::Result<Self> {
        let cwd = libc::AT_FDCWD;
        // The root itself is resolved the ordinary way, once, at setup.
        openat2(cwd, root, libc::O_PATH | libc::O_DIRECTORY, 0, 0).map(Self)
    }

    /// Opens `rel` (relative to the anchor) with `flags`. Fails with `EXDEV`
    /// when the lookup would leave the anchor.
    pub(crate) fn open(&self, rel: &Path, flags: i32, mode: u32) -> io::Result<File> {
        openat2(self.0.as_raw_fd(), rel, flags, mode, RESOLVE).map(File::from)
    }

    /// Creates each directory of `rel` that is missing, never following a
    /// symlink on the way (the caller passes a resolved path, so a link there
    /// means something swapped it).
    pub(crate) fn create_dir_all(&self, rel: &Path) -> io::Result<()> {
        let mut current = openat2(
            self.0.as_raw_fd(),
            Path::new("."),
            libc::O_PATH | libc::O_DIRECTORY,
            0,
            RESOLVE,
        )?;
        for component in rel.components() {
            let Component::Normal(name) = component else {
                continue;
            };
            let c_name = CString::new(name.as_bytes())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"))?;
            // SAFETY: valid descriptor and NUL-terminated single component.
            let made = unsafe { libc::mkdirat(current.as_raw_fd(), c_name.as_ptr(), 0o777) };
            if made != 0 {
                let err = io::Error::last_os_error();
                if err.kind() != io::ErrorKind::AlreadyExists {
                    return Err(err);
                }
            }
            current = openat2(
                current.as_raw_fd(),
                Path::new(name),
                libc::O_PATH | libc::O_DIRECTORY,
                0,
                RESOLVE | libc::RESOLVE_NO_SYMLINKS,
            )?;
        }
        Ok(())
    }
}

/// Where the kernel says the open descriptor `file` really is. The deny rules
/// are applied to this, so they judge the object that was opened.
pub(crate) fn path_of(file: &File) -> io::Result<PathBuf> {
    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
}
