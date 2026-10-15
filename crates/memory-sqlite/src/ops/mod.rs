// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The store operations as synchronous functions over one connection.
//!
//! The async trait implementation runs each of them on a blocking thread, so
//! a long query never stalls the async runtime.

mod annotate;
mod generations;
mod ingest;
mod retrieve;

pub(crate) use annotate::{annotation_head, annotations, append_annotation};
pub(crate) use generations::{put_embeddings, register_generation, set_generation_state};
pub(crate) use ingest::{delete_source, ingest};
pub(crate) use retrieve::{get_unit, list_sources, retrieve};
