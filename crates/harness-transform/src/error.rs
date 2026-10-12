// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The helper runtime's error vocabulary. Every failure is a typed value —
//! a `transform` step fails predictably, never by panicking and never with
//! an unwrapped message that hides which boundary broke.

use std::fmt;

/// Why a transform step's helper execution failed.
#[derive(Debug, Clone, PartialEq)]
pub enum TransformError {
    /// The helper's bytes do not decode, or decode to a module the sandbox
    /// refuses (imports, tables, a start section — anything that could
    /// reach out). The message names the refusal.
    Decode(String),
    /// The helper's declared ABI or language is not one this runtime
    /// implements.
    Abi(String),
    /// The helper exhausted its instruction budget.
    Fuel,
    /// The helper's linear memory exceeded its page cap.
    Memory,
    /// The helper trapped at runtime (a trap the compiler would have
    /// rejected statically is a decode refusal; these are the dynamic
    /// ones: division by zero, out-of-bounds access, a bad conversion).
    Trap(String),
    /// The input or the output failed the JSON Schema the transform step
    /// declared for it.
    Schema {
        /// Which boundary failed.
        stage: SchemaStage,
        /// The schema's own complaint (first error reported).
        message: String,
    },
    /// The module does not honor the helper ABI (a missing export, a
    /// wrong signature, an output the host cannot read back).
    Contract(String),
}

/// Which boundary of a transform a schema violation happened at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaStage {
    /// The value the step passed in.
    Input,
    /// The value the helper produced.
    Output,
}

impl fmt::Display for SchemaStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SchemaStage::Input => "input",
            SchemaStage::Output => "output",
        })
    }
}

impl fmt::Display for TransformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransformError::Decode(m) => write!(f, "helper module rejected: {m}"),
            TransformError::Abi(m) => write!(f, "helper abi rejected: {m}"),
            TransformError::Fuel => f.write_str("helper exhausted its fuel budget"),
            TransformError::Memory => f.write_str("helper exceeded its memory budget"),
            TransformError::Trap(m) => write!(f, "helper trapped: {m}"),
            TransformError::Schema { stage, message } => {
                write!(f, "helper {stage} failed its schema: {message}")
            }
            TransformError::Contract(m) => write!(f, "helper contract violated: {m}"),
        }
    }
}

impl std::error::Error for TransformError {}
