// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements delegated-task coding agents that measure
// what their fine-tuned models actually retained. If your team needs
// expertise in evaluation harnesses for model interfaces, you can procure
// our services by sending an email to info@swedishembedded.com.

//! Fact evaluation: a facts dataset becomes a scored report.
//!
//! `eval-facts` reads the same JSONL schema `learn` and `explore` write
//! (user message = question, assistant message = reference answer), asks
//! the configured model each question exactly like `ask` does, and judges
//! every reply against its reference. A record is correct when TWO ways
//! of comparing agree:
//!
//! 1. normalized text: lowercase, whitespace collapsed, punctuation
//!    stripped and unit-spacing variance erased ("10.5 Mbit/s" equals
//!    "10.5Mbps");
//! 2. numeric-tolerant: every number in the reference reappears in the
//!    answer with the same value (exact for integers, 1% relative
//!    tolerance otherwise) carrying the same unit token.
//!
//! A reference without numbers is judged by the text way only. A reply
//! that is not exactly one `{"answer": string}` object is a parse
//! failure, scored wrong - a salvaged half-answer would inflate a score
//! the promotion gate trusts.

use crate::store::write_atomic;
use anyhow::Context;

/// Everything one evaluation needs, as configured by the caller.
#[derive(Clone, Debug)]
pub struct EvalOptions {
    /// The JSONL facts dataset to evaluate.
    pub dataset: std::path::PathBuf,
    /// The report file, written atomically at the end.
    pub out: std::path::PathBuf,
    /// Same model selection `run` and `ask` read.
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub local: Option<crate::provider::LocalWeights>,
    /// Shuffle the records (with a time-seeded LCG, no dependency) before
    /// evaluating, so `--limit N` samples rather than truncates.
    pub shuffle: bool,
    /// Evaluate at most N records.
    pub limit: Option<usize>,
}

/// One judged question.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Entry {
    pub question: String,
    pub expected: String,
    pub got: String,
    pub correct: bool,
    /// Which comparison decided: the text way for numberless references,
    /// the numeric way when numbers disagreed, "parse_failure" when the
    /// reply was never an answer at all.
    pub basis: String,
}

/// What one evaluation produced, for the CLI's summary line and the report.
#[derive(Debug, serde::Serialize)]
pub struct Report {
    pub total: usize,
    pub correct: usize,
    pub incorrect: usize,
    pub parse_failures: usize,
    pub accuracy: f64,
    pub entries: Vec<Entry>,
    pub model: String,
}

/// One dataset record: the question and its reference answer.
#[derive(Debug, PartialEq, Eq)]
struct Record {
    question: String,
    expected: String,
}

/// Reads the facts dataset: one JSON object per line, the schema `learn`
/// and `explore` write - user message is the question, assistant message
/// the reference answer.
fn read_dataset(path: &std::path::Path) -> anyhow::Result<Vec<Record>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let mut records = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(line)
            .with_context(|| format!("{} line {}: not a JSON object", path.display(), n + 1))?;
        let messages = value
            .get("messages")
            .and_then(|m| m.as_array())
            .with_context(|| format!("{} line {}: no \"messages\" array", path.display(), n + 1))?;
        let content = |role: &str| -> anyhow::Result<String> {
            messages
                .iter()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some(role))
                .and_then(|m| m.get("content").and_then(|c| c.as_str()))
                .map(str::to_string)
                .with_context(|| {
                    format!("{} line {}: no {role} message", path.display(), n + 1)
                })
        };
        records.push(Record {
            question: content("user")?,
            expected: content("assistant")?,
        });
    }
    Ok(records)
}

/// Time-seeded Fisher-Yates shuffle. No dependency: a fact evaluation
/// needs reproducible-enough sampling, not cryptography.
fn shuffle<T>(items: &mut [T]) {
    if items.len() < 2 {
        return;
    }
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x2545F4914F6CDD1D)
        | 1;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for i in (1..items.len()).rev() {
        let j = (next() % (i as u64 + 1)) as usize;
        items.swap(i, j);
    }
}

/// One number found in text, with the unit token attached to it.
#[derive(Debug, PartialEq, Clone)]
struct Number {
    value: f64,
    /// Parsed from the digits, so "168" stays exact and "10.5" is
    /// known-fractional; the tolerance rule keys off this.
    integer_written: bool,
    /// Lowercased unit right after the number ("/" dropped); empty when
    /// the number stands bare.
    unit: String,
}

/// Extracts every number with its attached unit from `text`. A unit is
/// the alphanumeric run after the number, across one run of whitespace
/// ("42 Mbit/s" and "42Mbit/s" both read unit "mbits"); punctuation ends
/// the scan, so "16-bit" reads unit "bit" and "3." reads unit "".
fn extract_numbers(text: &str) -> Vec<Number> {
    let chars: Vec<char> = text.chars().collect();
    let mut numbers = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        let mut integer_written = true;
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
        if i + 1 < chars.len() && chars[i] == '.' && chars[i + 1].is_ascii_digit() {
            integer_written = false;
            i += 1;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
        }
        let digits: String = chars[start..i].iter().collect();
        let value: f64 = digits.parse().unwrap_or(f64::INFINITY);
        // The unit: whitespace, then alphanumerics (µ included) and '/'.
        let mut j = i;
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        let mut unit = String::new();
        while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '/') {
            unit.push(chars[j].to_ascii_lowercase());
            j += 1;
        }
        if unit.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            unit.clear(); // digits after the number are more digits, not a unit
        }
        numbers.push(Number {
            value,
            integer_written,
            unit: unit.replace('/', ""),
        });
    }
    numbers
}

/// Normalized text for comparison 1: lowercase, punctuation stripped
/// (except the decimal point between digits), whitespace collapsed and
/// the space between a number and its unit removed - so "10.5 Mbit/s"
/// and "10.5Mbps" normalize to the same string.
pub(crate) fn normalize_answer(text: &str) -> String {
    let chars: Vec<char> = text.to_lowercase().chars().collect();
    let mut out = String::new();
    let mut pending_space = false;
    for (i, &c) in chars.iter().enumerate() {
        if c.is_whitespace() {
            pending_space = out.chars().next_back().is_some();
            continue;
        }
        let decimal = c == '.'
            && out.chars().next_back().is_some_and(|p| p.is_ascii_digit())
            && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit());
        if !c.is_alphanumeric() && !decimal {
            pending_space = out.chars().next_back().is_some_and(|p| p.is_ascii_digit());
            continue;
        }
        // Erase unit-spacing variance: no space between digit and letter.
        let prev_digit = out.chars().next_back().is_some_and(|p| p.is_ascii_digit());
        if pending_space && !(prev_digit && c.is_alphabetic()) {
            out.push(' ');
        }
        pending_space = false;
        out.push(c);
    }
    // Unit synonyms: "Mbit/s" and "Mbps" are one unit written two ways.
    // Only the "/s"-family collapses; nothing else is silently rewritten.
    out.replace("mbits", "mbps")
        .replace("kbits", "kbps")
        .replace("gbits", "gbps")
        .replace("bits", "bps")
}

/// Comparison 1: the normalized texts are equal.
fn text_matches(expected: &str, got: &str) -> bool {
    normalize_answer(expected) == normalize_answer(got)
}

/// Comparison 2: every reference number appears in the answer with the
/// same value and unit. An integer-written reference demands an exact
/// match; a fractional one allows 1% relative tolerance. Matching is
/// one-to-one, so a reference saying "3" twice is not satisfied by one.
fn numeric_matches(expected: &str, got: &str) -> bool {
    let answer = extract_numbers(got);
    let mut used = vec![false; answer.len()];
    for want in extract_numbers(expected) {
        let found = answer
            .iter()
            .zip(&used)
            .position(|(have, &used)| {
                !used
                    && have.unit == want.unit
                    && if want.integer_written {
                        have.value == want.value
                    } else {
                        (have.value - want.value).abs() <= 0.01 * want.value.abs()
                    }
            });
        match found {
            Some(index) => used[index] = true,
            None => return false,
        }
    }
    true
}

/// The two-way judgment: correct when both ways agree, with the basis
/// naming which comparison decided. A numberless reference is text-only.
fn judge(expected: &str, got: &str) -> (bool, &'static str) {
    let has_numbers = !extract_numbers(expected).is_empty();
    let by_text = text_matches(expected, got);
    if !has_numbers {
        return (by_text, "text");
    }
    if !by_text {
        return (false, "text");
    }
    // Both ways must agree; the numeric way decided (numbers are the
    // decisive content when a reference carries any).
    (numeric_matches(expected, got), "numeric")
}

/// Runs the whole evaluation: read, ask, judge, report.
pub(crate) fn run(options: EvalOptions) -> anyhow::Result<Report> {
    let mut records = read_dataset(&options.dataset)?;
    anyhow::ensure!(
        !records.is_empty(),
        "{}: no records found",
        options.dataset.display()
    );
    if options.shuffle {
        shuffle(&mut records);
    }
    if let Some(limit) = options.limit {
        records.truncate(limit);
        anyhow::ensure!(!records.is_empty(), "--limit must be at least 1");
    }

    let model = match &options.model {
        Some(spec) => spec.clone(),
        None => format!(
            "brain/{}",
            crate::runner::local_model_name_of(options.local.as_ref().ok_or_else(|| {
                anyhow::anyhow!("no local weights configured")
            })?)
        ),
    };

    // One provider for the whole evaluation: the load is the expensive
    // step and every question wants the same model anyway - the same
    // shape `explore` uses.
    let explore_options = crate::explore::ExploreOptions {
        file: std::path::PathBuf::new(),
        out: std::path::PathBuf::new(),
        chunk_lines: None,
        model: options.model.clone(),
        base_url: options.base_url.clone(),
        api_key: options.api_key.clone(),
        local: options.local.clone(),
    };
    let provider = crate::explore::provider_from_shared(&explore_options)?;
    let rt = tokio::runtime::Runtime::new()?;

    let mut entries = Vec::new();
    let mut parse_failures = 0usize;
    for record in &records {
        let result: anyhow::Result<String> = rt.block_on(async {
            crate::explore::complete_text(provider.as_ref(), &crate::ask::prompt(&record.question))
                .await
        });
        let (correct, basis, got) = match result
            .ok()
            .and_then(|reply| crate::explore::parse_answer_reply(&reply).ok())
        {
            Some(answer) => {
                let (correct, basis) = judge(&record.expected, &answer);
                (correct, basis.to_string(), answer)
            }
            None => {
                parse_failures += 1;
                (false, "parse_failure".to_string(), String::new())
            }
        };
        entries.push(Entry {
            question: record.question.clone(),
            expected: record.expected.clone(),
            got,
            correct,
            basis,
        });
    }

    let correct = entries.iter().filter(|e| e.correct).count();
    let report = Report {
        total: entries.len(),
        correct,
        incorrect: entries.len() - correct,
        parse_failures,
        accuracy: if entries.is_empty() {
            0.0
        } else {
            correct as f64 / entries.len() as f64
        },
        entries,
        model,
    };
    // Atomic write: a crash mid-report leaves no half-file a gate could
    // read as a finished evaluation.
    write_atomic(&options.out, &serde_json::to_string_pretty(&report)?)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unit-spacing variance must not fail the text way.
    #[test]
    fn unit_spacing_variance_normalizes_away() {
        assert_eq!(normalize_answer("10.5 Mbit/s"), normalize_answer("10.5Mbps"));
        assert_eq!(normalize_answer("3.3 V"), normalize_answer("3.3V"));
        assert!(text_matches("10.5 Mbit/s", "10.5Mbps"));
    }

    /// Case, punctuation and whitespace collapse.
    #[test]
    fn case_punctuation_and_whitespace_collapse() {
        assert!(text_matches("The max clock is 168 MHz.", "the max clock is 168 mhz"));
        assert!(text_matches("Three SPI controllers", "three   spi controllers"));
        // A decimal point inside a number survives; one between words does not.
        assert!(text_matches("10.5 Mbps", "10.5Mbps"));
        assert!(text_matches("1. stop", "1 stop"));
    }

    /// Number tolerance: exact for integers, 1% for fractional.
    #[test]
    fn numeric_tolerance_follows_the_reference_form() {
        // Integer reference: 168.001 is a different answer, not tolerance.
        assert!(numeric_matches("168 MHz", "168 MHz"));
        assert!(!numeric_matches("168 MHz", "168.001 MHz"));
        // Fractional reference: 1% relative tolerance applies.
        assert!(numeric_matches("10.5 Mbps", "10.51 Mbps"));
        assert!(!numeric_matches("10.5 Mbps", "10.7 Mbps"));
        // Units must match, not merely values.
        assert!(!numeric_matches("10.5 Mbps", "10.5 kBd"));
    }

    /// A number missing from the answer, or a wrong one, fails it.
    #[test]
    fn missing_and_wrong_numbers_fail() {
        assert!(!numeric_matches("4 timers and 168 MHz", "168 MHz"));
        assert!(!numeric_matches("3.3 V", "5.0 V"));
        // Two references need two answer numbers: one-to-one.
        assert!(!numeric_matches("2 buses, 2 clocks", "2 buses"));
        // Extra answer numbers do not rescue a mismatch.
        assert!(!numeric_matches("16-bit timer", "32-bit timer"));
    }

    /// A numberless reference is judged by text only - the numeric way
    /// would find nothing to check and must not be the gate.
    #[test]
    fn numberless_references_judge_by_text() {
        assert_eq!(judge("Yes, the HSI is trimmable", "yes the hsi is trimmable"), (true, "text"));
        assert_eq!(judge("Yes", "No"), (false, "text"));
    }

    /// The fence-stripping parser accepts fenced answers and refuses
    /// prose-wrapped JSON - the same strict gate `ask` applies.
    #[test]
    fn fenced_answers_parse_and_prose_wrapped_json_is_refused() {
        assert_eq!(
            crate::explore::parse_answer_reply("```json\n{\"answer\": \"42 Mbit/s\"}\n```").unwrap(),
            "42 Mbit/s"
        );
        assert!(
            crate::explore::parse_answer_reply("The answer is {\"answer\": \"42 Mbit/s\"}").is_err(),
            "prose around the object is a parse failure"
        );
    }

    /// A dataset in the learn/explore schema reads as question/answer.
    #[test]
    fn dataset_records_read_from_the_learn_schema() {
        let dir = std::env::temp_dir().join(format!("loop-eval-ds-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("facts.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"messages\":[{\"role\":\"user\",\"content\":\"Q1\",\"train\":false},",
                "{\"role\":\"assistant\",\"content\":\"A1\",\"train\":true}]}\n",
                "\n",
                "{\"messages\":[{\"role\":\"user\",\"content\":\"Q2\",\"train\":false},",
                "{\"role\":\"assistant\",\"content\":\"A2\",\"train\":true}]}\n",
            ),
        )
        .unwrap();
        let records = read_dataset(&path).unwrap();
        assert_eq!(
            records,
            vec![
                Record { question: "Q1".into(), expected: "A1".into() },
                Record { question: "Q2".into(), expected: "A2".into() },
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The shuffle must not lose or duplicate anything.
    #[test]
    fn shuffle_is_a_permutation() {
        let mut items: Vec<u32> = (0..64).collect();
        shuffle(&mut items);
        items.sort_unstable();
        assert_eq!(items, (0..64).collect::<Vec<_>>());
    }
}
