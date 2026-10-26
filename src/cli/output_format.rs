// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use clap::ValueEnum;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum OutputFormatArg {
    /// Full conversation format (## User / ## Sven / ## Tool / ## Tool Result).
    /// Output is valid sven conversation markdown and fully pipeable.
    #[default]
    Conversation,
    /// Structured JSON: the run's full ATIF trajectory document (schema
    /// version, agent profile, and every step), pretty-printed to stdout.
    /// Not designed for piping between sven instances; use --output-format
    /// jsonl for that, or --output-trace/--trace to write the same
    /// document to a file.
    Json,
    /// Compact plain text: only the final agent response for each step.
    /// Matches the legacy pre-enhancement behaviour.
    Compact,
    /// Full-fidelity JSONL: one ATIF trajectory step (TraceStep) JSON object
    /// per line, streamed as each step is known complete (messages,
    /// thinking, tool calls all folded into their turn-shaped step).
    /// Designed for piping between sven instances:
    ///   sven 'task 1' --output-format jsonl | sven --stdin 'task 2'
    /// The receiving sven instance automatically detects and loads the
    /// history. See --output-trace/--trace to persist the equivalent full
    /// trajectory document to a file instead of streaming step-by-step.
    Jsonl,
}
