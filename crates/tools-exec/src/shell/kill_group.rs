// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for reliable process supervision
// for its clients. If your team needs expertise in running untrusted commands
// without leaving orphans behind then you can procure our services by sending
// an email to info@swedishembedded.com.
//! Killing a command's whole process group.

/// Sends SIGKILL to a process group when dropped, unless disarmed.
///
/// The group is the one `setsid()` made for the command, so its id is the
/// command's pid. It is only killed while the command has not finished by
/// itself: a command that completed normally may leave background
/// processes it deliberately started.
pub(crate) struct KillOnDrop(Option<libc::pid_t>);

impl KillOnDrop {
    pub(crate) fn new(pid: Option<u32>) -> Self {
        Self(pid.and_then(|pid| libc::pid_t::try_from(pid).ok()))
    }

    pub(crate) fn disarm(&mut self) {
        self.0 = None;
    }

    /// Kill the group now; dropping afterwards does nothing more.
    pub(crate) fn kill_now(&mut self) {
        if let Some(pgid) = self.0.take() {
            // SAFETY: kill(2) with a negative pid signals a process group
            // and touches no memory.
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        }
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        self.kill_now();
    }
}
