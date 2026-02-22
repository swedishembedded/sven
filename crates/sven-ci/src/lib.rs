mod runner;
mod output;
mod conversation;
pub mod context;
pub mod template;
#[cfg(test)]
mod tests;

pub use runner::{CiRunner, CiOptions, OutputFormat, EXIT_SUCCESS, EXIT_AGENT_ERROR, EXIT_VALIDATION_ERROR, EXIT_TIMEOUT, EXIT_INTERRUPT};
pub use conversation::{ConversationRunner, ConversationOptions};
// Re-export runtime detection utilities for callers that import from sven_ci
pub use sven_runtime::{find_project_root, detect_ci_context, collect_git_context, load_project_context_file, ci_template_vars, GitContext};
