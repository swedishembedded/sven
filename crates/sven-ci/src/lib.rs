mod runner;
mod output;
mod conversation;
pub mod context;
pub mod template;
#[cfg(test)]
mod tests;

pub use runner::{CiRunner, CiOptions, OutputFormat, EXIT_SUCCESS, EXIT_AGENT_ERROR, EXIT_VALIDATION_ERROR, EXIT_TIMEOUT, EXIT_INTERRUPT};
pub use conversation::{ConversationRunner, ConversationOptions};
pub use context::{find_project_root, detect_ci_context, collect_git_context, load_project_context_file, ci_template_vars, GitContext};
