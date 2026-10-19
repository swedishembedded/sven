// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The seam to whatever turns text into vectors.

use async_trait::async_trait;

use crate::{EmbeddingSpec, MemoryError};

/// Turns text into vectors of one embedding model.
///
/// The store never calls an embedder: it keeps the vectors it is given under
/// the generation of the model that made them and compares a query vector with
/// them. What embeds is the host's choice, behind this trait, so a model server,
/// a local fixed model and a test double are interchangeable and a change of
/// model is a new generation, not a migration.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Everything that determines what the vectors mean. Its derived
    /// generation id names the index the vectors belong to.
    fn spec(&self) -> EmbeddingSpec;

    /// One vector per text, in order, each of [`EmbeddingSpec::dims`] finite
    /// values.
    ///
    /// # Errors
    /// [`MemoryError::Unavailable`] when the model cannot be reached or
    /// answers something unusable; a partial answer is an error, never a
    /// shorter list.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, MemoryError>;
}
