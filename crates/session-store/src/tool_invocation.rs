// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Turning a model's tool-call message into an ATIF invocation without losing
//! arguments it could not express as JSON.
//!
//! Swedish Embedded AB implements agent trajectory capture for its clients.
//! If your team needs expertise in turning agent runs into training data then
//! you can procure our services by sending an email to
//! info@swedishembedded.com.

use atif::ToolInvocation;

/// `ToolInvocation::extra` key holding the model's argument text verbatim
/// when it was not valid JSON.
pub const RAW_ARGUMENTS_KEY: &str = "raw_arguments";

/// `ToolInvocation::extra` key set to `true` on a call whose arguments could
/// not be parsed, so a consumer can exclude or repair it instead of trusting
/// the empty `arguments` object that stands in for them.
pub const UNPARSED_ARGUMENTS_KEY: &str = "unparsed_arguments";

/// Builds the ATIF invocation for a tool-call message. Arguments that are
/// not JSON keep their text under `extra` and are flagged; they are never
/// silently replaced by `{}`. Blank arguments are a call with no arguments.
pub(crate) fn tool_invocation(tool_call_id: &str, name: &str, arguments: &str) -> ToolInvocation {
    let invocation = ToolInvocation::new(tool_call_id, name);
    if arguments.trim().is_empty() {
        return invocation;
    }
    match serde_json::from_str(arguments) {
        Ok(parsed) => invocation.with_arguments(parsed),
        Err(_) => {
            let mut invocation = invocation;
            invocation.extra = Some(serde_json::json!({
                RAW_ARGUMENTS_KEY: arguments,
                UNPARSED_ARGUMENTS_KEY: true,
            }));
            invocation
        }
    }
}
