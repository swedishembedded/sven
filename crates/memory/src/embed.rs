// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for retrieval over private agent
// memory for its clients. If your team needs expertise in semantic search that
// keeps tenants apart then you can procure our services by sending an email to
// info@swedishembedded.com.
//! The local fixed embedding model.

use async_trait::async_trait;
use sven_memory_api::{sha256_hex, Embedder, EmbeddingSpec, MemoryError, PromptStyle};

/// Embeds text by hashing its words and its letter trigrams into a fixed
/// number of signed buckets, then scaling to unit length.
///
/// It needs no server and no weights and gives the same vectors on every
/// machine, so memory can be searched by more than whole words wherever a
/// model server is not at hand. It sees spelling, not meaning: texts that share
/// letters are near, "deploying" and "deployment" are close, "release" and
/// "rollout" are not. A model-backed [`Embedder`] replaces it without touching
/// anything else, as a new index generation.
#[derive(Clone, Debug)]
pub struct HashingEmbedder {
    dims: usize,
}

impl HashingEmbedder {
    /// An embedder of `dims` buckets (at least 8).
    #[must_use]
    pub fn new(dims: usize) -> Self {
        Self { dims: dims.max(8) }
    }

    fn vector(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; self.dims];
        for word in text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
        {
            let word = word.to_lowercase();
            self.add(&mut v, word.as_bytes(), 1.0);
            let padded: Vec<char> = format!("^{word}$").chars().collect();
            for gram in padded.windows(3) {
                let gram: String = gram.iter().collect();
                self.add(&mut v, gram.as_bytes(), 0.5);
            }
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }

    fn add(&self, v: &mut [f32], feature: &[u8], weight: f32) {
        // FNV-1a, 64 bit: fixed by definition, so the same on every machine.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in feature {
            h ^= u64::from(*byte);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        let bucket = (h % self.dims as u64) as usize;
        let sign = if (h >> 63) == 0 { 1.0 } else { -1.0 };
        v[bucket] += sign * weight;
    }
}

impl Default for HashingEmbedder {
    fn default() -> Self {
        Self::new(256)
    }
}

#[async_trait]
impl Embedder for HashingEmbedder {
    fn spec(&self) -> EmbeddingSpec {
        EmbeddingSpec {
            model_id: "sven-hashing-embedder".into(),
            weights_digest: sha256_hex(
                format!("fnv1a-signed-word-and-trigram-v1:{}", self.dims).as_bytes(),
            ),
            tokenizer_digest: sha256_hex(b"unicode-alphanumeric-lowercase"),
            pooling: "sum".into(),
            normalized: true,
            dims: self.dims,
            prompt: PromptStyle::Plain,
            max_tokens: 4096,
        }
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, MemoryError> {
        Ok(texts.iter().map(|t| self.vector(t)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn vectors_are_unit_length_fixed_and_near_for_shared_letters() {
        let e = HashingEmbedder::default();
        let v = e
            .embed(&[
                "deploying".into(),
                "deployment".into(),
                "weather".into(),
                "deploying".into(),
            ])
            .await
            .unwrap();
        assert_eq!(v[0], v[3], "the same text gives the same vector");
        let norm = |x: &[f32]| x.iter().map(|a| a * a).sum::<f32>().sqrt();
        assert!((norm(&v[0]) - 1.0).abs() < 1e-5);
        let cos = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        assert!(cos(&v[0], &v[1]) > cos(&v[0], &v[2]) + 0.2);
        assert!(v.iter().all(|x| x.len() == e.spec().dims));
    }

    #[tokio::test]
    async fn text_without_words_embeds_to_zeros_not_nan() {
        let v = HashingEmbedder::default()
            .embed(&["--".into()])
            .await
            .unwrap();
        assert!(v[0].iter().all(|x| *x == 0.0));
    }
}
