// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The command-line spelling of the context assembly mode.

use clap::ValueEnum;

/// The spelling of [`sven_bootstrap::AssemblyMode`] on the command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ContextAssemblyArg {
    /// The conversation is sent as it is.
    Off,
    /// Tool results of older rounds are sent in a smaller form and can be
    /// expanded back.
    Demote,
    /// `demote`, and a resumed session starts from a rebuilt history.
    Reconstruct,
}

impl From<ContextAssemblyArg> for sven_bootstrap::AssemblyMode {
    fn from(arg: ContextAssemblyArg) -> Self {
        match arg {
            ContextAssemblyArg::Off => Self::Off,
            ContextAssemblyArg::Demote => Self::Demote,
            ContextAssemblyArg::Reconstruct => Self::Reconstruct,
        }
    }
}
