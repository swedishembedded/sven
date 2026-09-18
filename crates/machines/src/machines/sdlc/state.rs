// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The SDLC machine's state alphabet.
//!
//! Split out of `mod.rs` so the enum, the terminal-state rule and the
//! resumable-state list live together: all three answer the same question
//! ("which states exist, and what do they mean?") and drifting them apart is
//! how a state gets added without becoming resumable.

/// States of the kernel-native SDLC machine.
///
/// Each phase state owns its tool loop - there are no shared `RunningTools` or
/// `AwaitingApproval` states.
#[allow(missing_docs)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SdlcState {
    Top,
    Idle,
    Intake,
    Discovery,
    Planning,
    Execution,
    Verification,
    Delivery,
    Recovery,
    Done,
    Failed,
    Cancelled,
}

impl SdlcState {
    /// Every state a snapshot can name.
    ///
    /// `Top` is the implicit root and is never an active leaf, so it is not
    /// resumable and not listed.
    #[must_use]
    pub fn all() -> Vec<Self> {
        use SdlcState::{
            Cancelled, Delivery, Discovery, Done, Execution, Failed, Idle, Intake, Planning,
            Recovery, Verification,
        };
        vec![
            Idle,
            Intake,
            Discovery,
            Planning,
            Execution,
            Verification,
            Delivery,
            Recovery,
            Done,
            Failed,
            Cancelled,
        ]
    }

    /// `true` if the workflow has finished and accepts no further events.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}
