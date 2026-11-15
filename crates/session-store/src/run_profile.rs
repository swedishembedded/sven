// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for recording which agent
// configuration produced an episode for its clients. If your team needs
// expertise in reproducible agent trajectories then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Recording the run profile that produced a trajectory.

use atif::AgentProfile;
use serde_json::{json, Value};

/// `agent` with `agent.extra.sven.profile` set to the run profile's
/// identifier (for example `compact/1`), so a consumer can tell which fixed
/// request shape produced the episode. A run with no fixed profile
/// (`profile` is `None`) leaves `agent` as it was.
#[must_use]
pub fn with_run_profile(mut agent: AgentProfile, profile: Option<&str>) -> AgentProfile {
    let Some(profile) = profile else {
        return agent;
    };
    let mut extra = match agent.extra.take() {
        Some(Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    let mut sven = match extra.remove("sven") {
        Some(Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    sven.insert("profile".to_string(), json!(profile));
    extra.insert("sven".to_string(), Value::Object(sven));
    agent.extra = Some(Value::Object(extra));
    agent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_profile_is_recorded_beside_what_the_agent_already_says() {
        let mut agent = AgentProfile::new("sven", "1");
        agent.extra = Some(json!({"sven": {"other": 1}, "kept": true}));
        let stamped = with_run_profile(agent, Some("compact/1"));
        assert_eq!(
            stamped.extra,
            Some(json!({"sven": {"other": 1, "profile": "compact/1"}, "kept": true}))
        );
    }

    #[test]
    fn a_run_without_a_fixed_profile_records_nothing() {
        let agent = with_run_profile(AgentProfile::new("sven", "1"), None);
        assert_eq!(agent.extra, None);
    }
}
