// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Deterministic resolution of a loose model name ("deepseek ocr v2") to one
//! of the OCR models that can actually be reached.
//!
//! The caller supplies the candidates (configured aliases, the catalog, what
//! the provider reports it is serving). Matching is by word: every word of the
//! query must be a word of the candidate; among those the candidate with the
//! fewest extra words wins, and a tie is an error rather than a guess.

use std::collections::BTreeSet;

/// One model that can be named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// What `--model` takes, such as `brain/deepseek-ai/DeepSeek-OCR-2`.
    pub spec: String,
    /// A short name the user configured for it.
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MatchError {
    #[error("no OCR model matches '{query}'. Available: {}", list(.choices))]
    NoMatch { query: String, choices: Vec<String> },
    #[error("'{query}' matches several OCR models; name one of: {}", list(.choices))]
    Ambiguous { query: String, choices: Vec<String> },
}

fn list(choices: &[String]) -> String {
    if choices.is_empty() {
        "none (configure `tools.document.ocr_models` or serve an OCR model)".into()
    } else {
        choices.join(", ")
    }
}

pub fn match_model<'a>(
    query: &str,
    candidates: &'a [Candidate],
) -> Result<&'a Candidate, MatchError> {
    let wanted = query.trim().to_lowercase();
    let choices = || {
        candidates
            .iter()
            .map(|c| c.spec.clone())
            .collect::<Vec<_>>()
    };
    if let Some(exact) = candidates.iter().find(|c| {
        c.spec.to_lowercase() == wanted
            || c.alias.as_deref().map(str::to_lowercase) == Some(wanted.clone())
    }) {
        return Ok(exact);
    }
    let query_words = words(query);
    if query_words.is_empty() {
        return Err(MatchError::NoMatch {
            query: query.into(),
            choices: choices(),
        });
    }
    let mut scored: Vec<(usize, &Candidate)> = candidates
        .iter()
        .filter_map(|c| {
            let mut have = words(&c.spec);
            if let Some(alias) = &c.alias {
                have.extend(words(alias));
            }
            query_words
                .is_subset(&have)
                .then(|| (have.len() - query_words.len(), c))
        })
        .collect();
    scored.sort_by_key(|(extra, c)| (*extra, c.spec.clone()));
    match scored.as_slice() {
        [] => Err(MatchError::NoMatch {
            query: query.into(),
            choices: choices(),
        }),
        [(_, only)] => Ok(only),
        [(a, best), (b, _), ..] if a < b => Ok(best),
        tied => {
            let best = tied[0].0;
            Err(MatchError::Ambiguous {
                query: query.into(),
                choices: tied
                    .iter()
                    .filter(|(e, _)| *e == best)
                    .map(|(_, c)| c.spec.clone())
                    .collect(),
            })
        }
    }
}

/// Lower-case words, split at punctuation and between letters and digits;
/// the version marker "v" in "v2" is dropped.
fn words(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut current = String::new();
    let mut digit = false;
    let mut flush = |current: &mut String| {
        if !current.is_empty() && current != "v" {
            out.insert(std::mem::take(current));
        }
        current.clear();
    };
    for c in text.chars().flat_map(char::to_lowercase) {
        if !c.is_alphanumeric() {
            flush(&mut current);
            continue;
        }
        if !current.is_empty() && c.is_ascii_digit() != digit {
            flush(&mut current);
        }
        digit = c.is_ascii_digit();
        current.push(c);
    }
    flush(&mut current);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(spec: &str) -> Candidate {
        Candidate {
            spec: spec.into(),
            alias: None,
        }
    }

    fn catalog() -> Vec<Candidate> {
        vec![
            c("brain/deepseek-ai/DeepSeek-OCR"),
            c("brain/deepseek-ai/DeepSeek-OCR-2"),
            c("openrouter/qwen/qwen2.5-vl-72b-instruct"),
        ]
    }

    #[test]
    fn loose_names_resolve_to_one_model() {
        let all = catalog();
        assert_eq!(
            match_model("deepseek ocr v2", &all).unwrap().spec,
            "brain/deepseek-ai/DeepSeek-OCR-2"
        );
        assert_eq!(
            match_model("DeepSeek OCR 2", &all).unwrap().spec,
            "brain/deepseek-ai/DeepSeek-OCR-2"
        );
        assert_eq!(
            match_model("deepseek ocr", &all).unwrap().spec,
            "brain/deepseek-ai/DeepSeek-OCR"
        );
        assert_eq!(
            match_model("qwen vl", &all).unwrap().spec,
            "openrouter/qwen/qwen2.5-vl-72b-instruct"
        );
    }

    #[test]
    fn unknown_and_ambiguous_names_list_the_choices() {
        let all = catalog();
        let err = match_model("gpt vision", &all).unwrap_err();
        assert!(
            err.to_string().contains("brain/deepseek-ai/DeepSeek-OCR-2"),
            "{err}"
        );
        let twins = vec![c("x/foo-ocr"), c("y/bar-ocr")];
        let err = match_model("ocr", &twins).unwrap_err();
        assert!(matches!(err, MatchError::Ambiguous { .. }), "{err}");
    }

    #[test]
    fn an_alias_or_exact_spec_wins_outright() {
        let all = vec![
            Candidate {
                spec: "brain/deepseek-ai/DeepSeek-OCR-2".into(),
                alias: Some("fast".into()),
            },
            c("brain/deepseek-ai/DeepSeek-OCR"),
        ];
        assert_eq!(
            match_model("fast", &all).unwrap().spec,
            "brain/deepseek-ai/DeepSeek-OCR-2"
        );
        assert_eq!(
            match_model("brain/deepseek-ai/deepseek-ocr", &all)
                .unwrap()
                .spec,
            "brain/deepseek-ai/DeepSeek-OCR"
        );
    }
}
