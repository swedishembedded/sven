// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in tool retrieval for
// large agent tool sets then you can procure our services by sending an
// email to info@swedishembedded.com.
//! How a search is shown to the model: the best matches with their summaries,
//! and the names of the next few, so that a near miss is one `load` away.

use std::collections::HashMap;

use crate::descriptor::ToolDescriptor;
use crate::search::RankedTool;

/// The names listed after the matches that are shown with a summary.
///
/// Eight names cost about 50 estimated tokens against about 180 for the eight
/// matches before them, and raise held-out recall from 95.2% to 98.8% for
/// paraphrased queries and from 90.2% to 96.5% for indirect ones (see the
/// tool disclosure page): a missed tool costs another request carrying the
/// whole prompt, which is worth far more than that.
pub const RELATED_NAMES: usize = 8;

/// The text of a successful `search`: `ranked` is non-empty, `pool` holds the
/// tools it ranked, and `limit` of the matches are shown in full.
///
/// The first line and the lines of matches are read back by the ablation
/// benchmark (`crates/ablation`), which stops at the line that starts with
/// "Load the ones": keep the related names after it.
pub fn present_matches(
    query: &str,
    pool: &[ToolDescriptor],
    ranked: &[RankedTool],
    limit: usize,
) -> String {
    let by_name: HashMap<&str, &ToolDescriptor> =
        pool.iter().map(|t| (t.name.as_str(), t)).collect();
    let shown: Vec<&ToolDescriptor> = ranked
        .iter()
        .take(limit)
        .filter_map(|r| by_name.get(r.name.as_str()).copied())
        .collect();
    let mut out = format!(
        "{} of {} matches for {query:?} (best first):\n",
        shown.len(),
        ranked.len()
    );
    for tool in &shown {
        out.push_str(&tool.listing_line());
        out.push('\n');
    }
    out.push_str("Load the ones you need with action=load and their names.");
    let related: Vec<&str> = ranked
        .iter()
        .skip(limit)
        .take(RELATED_NAMES)
        .map(|r| r.name.as_str())
        .collect();
    if !related.is_empty() {
        out.push_str(&format!(
            "\nAlso related (names only): {}.",
            related.join(", ")
        ));
    }
    out
}
