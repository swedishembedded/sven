// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
use clap::Subcommand;

/// `sven doctor` subcommands: find out why something does not work.
#[derive(Subcommand, Debug)]
pub enum DoctorCommands {
    /// Report what the decision runtime would do: the selected model, whether
    /// it is ready, the question types it supports, its input limit, its
    /// validation status, a latency profile from a few tiny decisions, and
    /// which decision points are inactive and why. Contacts the provider
    /// even when `decision.enabled` is false, so it can be run before
    /// turning decisions on; exits non-zero when decisions are unavailable.
    ///
    ///   sven doctor decisions
    ///   sven doctor decisions --json
    Decisions {
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Check that the model sven would use can be reached: the provider
    /// builds, its endpoint answers with the key, it serves the model, and the
    /// model can call tools. A hosted provider is asked for its model list
    /// only, which costs nothing. Honours `--model` and the configuration
    /// files; exits non-zero when the model cannot be used.
    ///
    ///   sven doctor provider
    ///   sven -M brain/qwen3 doctor provider --json
    Provider {
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// `sven decisions` subcommands.
#[derive(Subcommand, Debug)]
pub enum DecisionsCommands {
    /// List the decision models the provider knows: state, validation,
    /// supported question types and input limit. Loads no model.
    ///
    ///   sven decisions models
    Models {
        /// Print the models as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[cfg(test)]
mod tests {
    use crate::cli::{Cli, Commands};
    use clap::Parser;

    #[test]
    fn the_decision_subcommands_parse() {
        let doctor = Cli::try_parse_from(["sven", "doctor", "decisions", "--json"]).unwrap();
        assert!(matches!(doctor.command, Some(Commands::Doctor { .. })));
        let models = Cli::try_parse_from(["sven", "decisions", "models"]).unwrap();
        assert!(matches!(models.command, Some(Commands::Decisions { .. })));
    }
}
