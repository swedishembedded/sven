// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Slash command parser.
//!
//! Parses the input buffer and identifies what stage of slash-command entry
//! the user is at.  The result drives the completion overlay and, on Enter,
//! command execution.
//!
//! Supported syntax:
//!   /command
//!   /command arg1 arg2
//!   /command "arg with spaces"
//!
//! The input is parsed character-by-character so we can correctly identify
//! the cursor position within arguments for completion purposes.

/// The current state of slash-command parsing for a given input string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedCommand {
    /// Input does not start with `/` — not a slash command at all.
    NotCommand,

    /// User has typed `/` or `/partial_name` but hasn't finished the command
    /// name yet (no trailing space after the name).
    PartialCommand {
        /// What has been typed so far after the `/` (may be empty).
        partial: String,
    },

    /// Command name is complete; user is now typing argument `arg_index`.
    CompletingArgs {
        /// The complete command name (without `/`).
        command: String,
        /// Zero-based index of the argument being typed.
        arg_index: usize,
        /// What has been typed so far for this argument (may be empty).
        partial: String,
    },

    /// Input is a fully-specified command ready for execution.
    Complete {
        command: String,
        args: Vec<String>,
    },
}

/// Parse the input string and return the current command state.
///
/// This function is designed to be called after every keystroke; it is pure
/// and has no side effects.
pub fn parse(input: &str) -> ParsedCommand {
    if !input.starts_with('/') {
        return ParsedCommand::NotCommand;
    }

    let body = &input[1..]; // everything after the '/'

    // Tokenise the body (handles quoted strings)
    let tokens = tokenise(body);

    if tokens.is_empty() {
        // Just "/"
        return ParsedCommand::PartialCommand { partial: String::new() };
    }

    let command_name = &tokens[0];

    // If the body ends without a trailing space and we only have one token,
    // the user is still completing the command name.
    let body_ends_with_space = body.ends_with(' ');

    if tokens.len() == 1 && !body_ends_with_space {
        return ParsedCommand::PartialCommand { partial: command_name.clone() };
    }

    // Command name is done.  Now check args.
    let args = &tokens[1..];

    if args.is_empty() {
        // Typed "/command " — completing first argument, nothing typed yet
        return ParsedCommand::CompletingArgs {
            command: command_name.clone(),
            arg_index: 0,
            partial: String::new(),
        };
    }

    // If the body ends with a space the last arg is complete and the user is
    // starting a new one.
    if body_ends_with_space {
        // If this is a single-arg command (most built-ins are), the command
        // is complete.  The caller/registry decides if more args are expected.
        return ParsedCommand::Complete {
            command: command_name.clone(),
            args: args.to_vec(),
        };
    }

    // The user is still typing the last argument
    let partial = args.last().cloned().unwrap_or_default();
    let arg_index = args.len() - 1;

    // If there's only one arg and no trailing space, it could still be
    // completing.  We treat it as CompletingArgs until Enter is pressed.
    ParsedCommand::CompletingArgs {
        command: command_name.clone(),
        arg_index,
        partial,
    }
}


/// Tokenise a command body: splits on whitespace, respects double-quoted
/// strings, collapses multiple spaces.
///
/// Returns owned tokens; quoted strings have their quotes stripped.
pub(super) fn tokenise(s: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut prev_was_space = true;

    for ch in s.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                if !in_quotes && !current.is_empty() {
                    // closing quote — the token is now complete even without a space
                }
            }
            ' ' | '\t' if !in_quotes => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
                prev_was_space = true;
            }
            _ => {
                current.push(ch);
                prev_was_space = false;
            }
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    let _ = prev_was_space; // suppress unused warning
    tokens
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_slash_input_is_not_command() {
        assert_eq!(parse("hello"), ParsedCommand::NotCommand);
        assert_eq!(parse(""), ParsedCommand::NotCommand);
        assert_eq!(parse("model"), ParsedCommand::NotCommand);
    }

    #[test]
    fn bare_slash_is_partial_command() {
        assert_eq!(parse("/"), ParsedCommand::PartialCommand { partial: "".into() });
    }

    #[test]
    fn partial_command_name() {
        assert_eq!(parse("/mod"), ParsedCommand::PartialCommand { partial: "mod".into() });
        assert_eq!(parse("/mode"), ParsedCommand::PartialCommand { partial: "mode".into() });
    }

    #[test]
    fn command_with_trailing_space_starts_completing_first_arg() {
        assert_eq!(
            parse("/model "),
            ParsedCommand::CompletingArgs {
                command: "model".into(),
                arg_index: 0,
                partial: "".into(),
            }
        );
    }

    #[test]
    fn command_with_partial_arg() {
        assert_eq!(
            parse("/model gpt"),
            ParsedCommand::CompletingArgs {
                command: "model".into(),
                arg_index: 0,
                partial: "gpt".into(),
            }
        );
    }

    #[test]
    fn command_with_complete_arg_and_space() {
        assert_eq!(
            parse("/model gpt-4o "),
            ParsedCommand::Complete {
                command: "model".into(),
                args: vec!["gpt-4o".into()],
            }
        );
    }

    #[test]
    fn command_with_no_args_needed_e_g_quit_partial() {
        // "/quit" with no space: still PartialCommand (user might keep typing)
        assert_eq!(
            parse("/quit"),
            ParsedCommand::PartialCommand { partial: "quit".into() }
        );
    }

    #[test]
    fn command_with_trailing_space_is_completing_first_arg() {
        // "/quit " with trailing space: start completing arg 0 (no args typed yet)
        assert_eq!(
            parse("/quit "),
            ParsedCommand::CompletingArgs {
                command: "quit".into(),
                arg_index: 0,
                partial: "".into(),
            }
        );
    }

    #[test]
    fn quoted_argument_parsed_as_single_token() {
        // "/model \"my custom provider\""
        let input = "/model \"my provider\"";
        match parse(input) {
            ParsedCommand::CompletingArgs { command, arg_index, partial } => {
                assert_eq!(command, "model");
                assert_eq!(arg_index, 0);
                assert_eq!(partial, "my provider");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn tokenise_splits_on_spaces() {
        assert_eq!(tokenise("a b c"), vec!["a", "b", "c"]);
    }

    #[test]
    fn tokenise_collapses_multiple_spaces() {
        assert_eq!(tokenise("a  b"), vec!["a", "b"]);
    }

    #[test]
    fn tokenise_handles_quoted_strings() {
        assert_eq!(tokenise("\"hello world\" foo"), vec!["hello world", "foo"]);
    }

    #[test]
    fn tokenise_empty_string() {
        assert_eq!(tokenise(""), Vec::<String>::new());
    }
}
