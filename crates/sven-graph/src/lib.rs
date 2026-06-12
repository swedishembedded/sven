// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Sven Workflow DSL — pure model and evaluation layer.
//!
//! This crate defines the data model for sven's hierarchical state-machine
//! workflow DSL and the associated evaluation utilities. It is deliberately kept
//! dependency-free of higher sven crates so it can be tested and reasoned about
//! in isolation.
//!
//! # Layers
//!
//! * **[`model`]** — [`Graph`], [`NodeData`], [`EdgeData`], [`NodeId`],
//!   [`NodeKind`], [`LoopSpec`], [`EffectTmpl`], [`EventPattern`].
//! * **[`native`]** — [`NativeRegistry`] / [`NativeFn`] / [`NativeOutcome`]
//!   escape-hatch types for imperative logic.
//! * **[`guard`]** — [`GuardExpr`] / [`GuardCtx`] — pure guard evaluation.
//! * **[`template`]** — `{{ path }}` prompt / effect template interpolation.
//! * **[`compile`]** — [`GraphBuilder`] / [`CompileError`] — graph validation.
//! * **[`render_dot`]** — one-way graphviz dot rendering for visualization.

pub mod compile;
pub mod guard;
pub mod model;
pub mod native;
pub mod render_dot;
pub mod template;

// Convenience re-exports.
pub use compile::{CompileError, GraphBuilder};
pub use guard::{GuardCtx, GuardError};
pub use model::{
    EdgeData, EffectTmpl, EventPattern, Graph, GraphError, LoopSpec, NodeData, NodeId, NodeKind,
    SchemaRef, ToolsSpec,
};
pub use native::{NativeArgs, NativeFn, NativeOutcome, NativeRegistry};
pub use template::{TemplateCtx, TemplateError};
