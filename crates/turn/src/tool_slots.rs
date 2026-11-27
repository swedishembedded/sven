// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! [`AccumSlot`] accumulates one streamed call's id, name and argument text
//! for [`crate::stream_turn`], and [`unique_call_id`] keeps the calls of one
//! turn apart. Dispatch is the HSM kernel's `ToolExecutor`'s.

/// `id` if no call of this turn has it yet, else `id` with a numeric suffix
/// that no call has. Some servers send the same id (`call_0`) for every
/// parallel call; ids are how results are paired with calls and how a call is
/// marked unparsed, so two calls sharing one would answer for each other.
pub(crate) fn unique_call_id(issued: &mut std::collections::HashSet<String>, id: &str) -> String {
    if !id.is_empty() && issued.insert(id.to_owned()) {
        return id.to_owned();
    }
    let stem = if id.is_empty() { "call" } else { id };
    (1..)
        .map(|n| format!("{stem}_{n}"))
        .find(|candidate| issued.insert(candidate.clone()))
        .expect("an unbounded range always yields a free id")
}

/// One tool call's id, name and argument text as they stream in. The text is
/// kept whole as it arrives: it is the call as the model wrote it.
#[derive(Default)]
pub(crate) struct AccumSlot {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) args: String,
}

impl AccumSlot {
    pub(crate) fn feed(&mut self, id: &str, name: &str, args_chunk: &str) {
        if !id.is_empty() {
            self.id = id.to_owned();
        }
        if !name.is_empty() {
            self.name = name.to_owned();
        }
        self.args.push_str(args_chunk);
    }
}
