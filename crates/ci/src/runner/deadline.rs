// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for CI jobs that hang on an agent
// run nobody is watching for its clients. If your team needs expertise in
// running coding agents unattended in CI then you can procure our services by
// sending an email to info@swedishembedded.com.
//! The wall-clock bound of a headless single-prompt run.

use std::time::{Duration, Instant};

/// The wall-clock bound of a single-prompt run. The run bounds every wait the
/// kernel can make - a model that never answers, a tool that never returns, a
/// gate whose handler never replies - so it is what keeps a CI job from
/// hanging.
///
/// An explicit limit (the per-run one, else the per-step one) wins and `0`
/// opts out: unlimited. Without one the configured
/// `agent.max_run_timeout_secs` applies, whose default is four hours and whose
/// `0` is unlimited. Returns the instant the run must end by, counted from
/// `started`.
pub(super) fn run_deadline(
    started: Instant,
    explicit_secs: Option<u64>,
    configured_max_run_secs: u64,
) -> Option<Instant> {
    let secs = explicit_secs.unwrap_or(configured_max_run_secs);
    (secs > 0).then(|| started + Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use sven_bootstrap::AgentConfig;

    use super::*;

    fn limit(explicit: Option<u64>, configured: u64) -> Option<Duration> {
        let started = Instant::now();
        run_deadline(started, explicit, configured).map(|by| by - started)
    }

    #[test]
    fn a_run_without_any_limit_gets_the_configured_default_deadline() {
        let default = AgentConfig::default().max_run_timeout_secs;
        assert!(
            default > 0,
            "an unattended run must not default to unlimited"
        );
        assert_eq!(limit(None, default), Some(Duration::from_secs(default)));
    }

    #[test]
    fn an_explicit_limit_beats_the_configured_one_and_zero_opts_out() {
        assert_eq!(limit(Some(30), 900), Some(Duration::from_secs(30)));
        assert_eq!(limit(Some(0), 900), None);
        assert_eq!(limit(None, 0), None);
    }
}
